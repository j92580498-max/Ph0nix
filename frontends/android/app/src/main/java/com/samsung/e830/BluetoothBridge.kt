package com.samsung.e830

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothServerSocket
import android.bluetooth.BluetoothSocket
import android.content.Context
import org.json.JSONObject
import java.io.File
import java.io.InputStream
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** The host adapter transfers OPP objects, then the app imports them through the phone's file service. */
internal class BluetoothBridge(
    private val context: Context,
    private val staging: File,
    private val log: (String, String) -> Unit,
    private val progress: (String) -> Unit,
    private val received: (JSONObject) -> Unit,
) : AutoCloseable {
    private val adapter = (context.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager).adapter
    private val worker = Executors.newSingleThreadExecutor { task -> Thread(task, "ph0nix-bluetooth").apply { isDaemon = true } }
    private val watchdog = Executors.newSingleThreadScheduledExecutor { task -> Thread(task, "ph0nix-bluetooth-timeout").apply { isDaemon = true } }
    private val busy = AtomicBoolean(false)
    private val closed = AtomicBoolean(false)
    private val cancelled = AtomicBoolean(false)
    @Volatile private var socket: BluetoothSocket? = null
    @Volatile private var server: BluetoothServerSocket? = null
    @Volatile private var timeout: ScheduledFuture<*>? = null
    val isBusy get() = busy.get()

    @SuppressLint("MissingPermission") // Activity requests CONNECT before every adapter operation.
    fun peers(): List<BluetoothDevice> {
        check(adapter != null) { context.getString(R.string.bluetooth_no_adapter) }
        check(adapter.isEnabled) { context.getString(R.string.bluetooth_enable) }
        return adapter.bondedDevices.sortedBy { it.name ?: it.address }
    }

    @SuppressLint("MissingPermission")
    fun send(device: BluetoothDevice, file: File) {
        begin {
            var sender = 0L
            try {
                check(adapter != null && adapter.isEnabled) { context.getString(R.string.bluetooth_enable) }
                val connection = device.createRfcommSocketToServiceRecord(OPP_UUID)
                socket = connection
                check(!cancelled.get()) { context.getString(R.string.bluetooth_cancelled) }
                deadline()
                progress(context.getString(R.string.bluetooth_connecting, device.name ?: device.address))
                connection.connect()
                check(!cancelled.get()) { context.getString(R.string.bluetooth_cancelled) }
                sender = NativeBridge.nativeBluetoothSenderOpen(file.absolutePath)
                check(sender != 0L) { context.getString(R.string.bluetooth_sender_failed) }
                var nextProgressNs = 0L
                while (!cancelled.get()) {
                    val packet = NativeBridge.nativeBluetoothSenderNext(sender) ?: break
                    deadline()
                    connection.outputStream.write(packet)
                    connection.outputStream.flush()
                    val error = NativeBridge.nativeBluetoothSenderResponse(sender, readPacket(connection.inputStream))
                    check(error.isEmpty()) { error }
                    val now = System.nanoTime()
                    if (now >= nextProgressNs) {
                        val state = JSONObject(NativeBridge.nativeBluetoothSenderProgress(sender))
                        progress(context.getString(R.string.bluetooth_sending, file.name, state.optLong("bytes"), state.optLong("total")))
                        nextProgressNs = now + 100_000_000L
                    }
                }
                check(!cancelled.get()) { context.getString(R.string.bluetooth_cancelled) }
                val state = JSONObject(NativeBridge.nativeBluetoothSenderProgress(sender))
                check(state.optBoolean("finished")) { context.getString(R.string.bluetooth_unfinished) }
                progress(context.getString(R.string.bluetooth_sent, file.name, state.optLong("bytes")))
                log("info", "Bluetooth sent ${file.name}: ${state.optLong("bytes")} bytes")
                file.delete()
            } finally { if (sender != 0L) NativeBridge.nativeBluetoothSenderClose(sender) }
        }
    }

    @SuppressLint("MissingPermission")
    fun receive() {
        begin {
            var receiver = 0L
            var completed = false
            try {
                check(adapter != null && adapter.isEnabled) { context.getString(R.string.bluetooth_enable) }
                val listener = adapter.listenUsingRfcommWithServiceRecord("ph0nix File Transfer", OPP_UUID)
                server = listener
                check(!cancelled.get()) { context.getString(R.string.bluetooth_cancelled) }
                progress(context.getString(R.string.bluetooth_ready))
                val connection = listener.accept(120_000)
                socket = connection
                check(!cancelled.get()) { context.getString(R.string.bluetooth_cancelled) }
                listener.close(); server = null
                receiver = NativeBridge.nativeBluetoothReceiverOpen(staging.absolutePath)
                check(receiver != 0L) { context.getString(R.string.bluetooth_receiver_failed) }
                progress(context.getString(R.string.bluetooth_receiving, connection.remoteDevice.name ?: connection.remoteDevice.address))
                while (!cancelled.get()) {
                    deadline()
                    val request = readPacket(connection.inputStream)
                    if ((request[0].toInt() and 0xff) == 0x02 || (request[0].toInt() and 0xff) == 0x82) completed = false
                    val response = NativeBridge.nativeBluetoothReceiverPacket(receiver, request)
                        ?: error(context.getString(R.string.bluetooth_receiver_stopped))
                    connection.outputStream.write(response); connection.outputStream.flush()
                    if ((response[0].toInt() and 0xff) >= 0xc0)
                        log("warn", "Bluetooth request rejected: opcode ${(request[0].toInt() and 0xff).toString(16)}, status ${(response[0].toInt() and 0xff).toString(16)}")
                    val result = NativeBridge.nativeBluetoothReceiverTake(receiver)
                    if (result != "null") {
                        completed = true
                        val file = JSONObject(result)
                        log("info", "Bluetooth received ${file.optString("name")}: ${file.optLong("bytes")} bytes, SHA256 ${file.optString("sha256")}")
                        progress(context.getString(R.string.bluetooth_received, file.optString("name"), file.optLong("bytes")))
                        received(file)
                    }
                    if ((request[0].toInt() and 0xff) == 0x81) break
                }
            } catch (error: java.io.IOException) {
                if (!completed) throw error
                log("info", "Bluetooth peer closed after a completed file transfer.")
            } finally { if (receiver != 0L) NativeBridge.nativeBluetoothReceiverClose(receiver) }
        }
    }

    private fun begin(action: () -> Unit) {
        check(!closed.get()) { context.getString(R.string.bluetooth_closed) }
        check(busy.compareAndSet(false, true)) { context.getString(R.string.bluetooth_busy) }
        cancelled.set(false)
        worker.execute {
            try { check(!cancelled.get()) { context.getString(R.string.bluetooth_cancelled) }; action() }
            catch (error: Exception) {
                val message = if (cancelled.get()) context.getString(R.string.bluetooth_cancelled) else context.getString(R.string.bluetooth_failed, error.localizedMessage ?: error.toString())
                progress(message); log(if (cancelled.get()) "info" else "error", message)
            } finally {
                timeout?.cancel(false); timeout = null
                runCatching { socket?.close() }; socket = null
                runCatching { server?.close() }; server = null
                busy.set(false)
            }
        }
    }

    private fun deadline() {
        timeout?.cancel(false)
        val watched = socket
        timeout = watchdog.schedule({ runCatching { watched?.close() } }, 45, TimeUnit.SECONDS)
    }

    fun cancel() {
        cancelled.set(true)
        timeout?.cancel(false)
        runCatching { server?.close() }
        runCatching { socket?.close() }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        cancel(); worker.shutdown(); watchdog.shutdownNow()
    }

    private fun readPacket(input: InputStream): ByteArray {
        val prefix = ByteArray(3)
        readExactly(input, prefix, 0, 3)
        val length = ((prefix[1].toInt() and 0xff) shl 8) or (prefix[2].toInt() and 0xff)
        check(length in 3..1024) { context.getString(R.string.bluetooth_packet_length) }
        return ByteArray(length).also { packet -> prefix.copyInto(packet); readExactly(input, packet, 3, length - 3) }
    }

    private fun readExactly(input: InputStream, bytes: ByteArray, start: Int, length: Int) {
        var offset = start
        while (offset < start + length) {
            val count = input.read(bytes, offset, start + length - offset)
            if (count <= 0) throw java.io.EOFException(context.getString(R.string.bluetooth_disconnected))
            offset += count
        }
    }

    companion object { val OPP_UUID: UUID = UUID.fromString("00001105-0000-1000-8000-00805f9b34fb") }
}
