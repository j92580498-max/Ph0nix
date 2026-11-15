package com.samsung.e830

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Process
import android.util.Log
import java.util.concurrent.LinkedBlockingDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** Bounded PCM playback; a blocked speaker never blocks native polling or input. */
internal class AudioPlayer(private val onError: (String) -> Unit = {}) : AutoCloseable {
    private val stopped = AtomicBoolean(false)
    private val packets = LinkedBlockingDeque<ByteArray>(10)
    @Volatile private var track: AudioTrack? = null
    private val thread = Thread({ play() }, "phone-android-audio").apply {
        isDaemon = true
        start()
    }

    fun enqueue(packet: ByteArray) {
        if (stopped.get() || packet.size < 4 || packet.size > 3844 || (packet.size - 4) % 4 != 0) return
        if (!packets.offerLast(packet)) {
            packets.clear()
            packet[0] = (packet[0].toInt() or 1).toByte() // Flush after dropping queued samples.
            packets.offerLast(packet)
        }
    }

    private fun play() {
        Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO)
        var output: AudioTrack? = null
        try {
            val minimum = AudioTrack.getMinBufferSize(
                48000, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT,
            )
            if (minimum <= 0) error("48 kHz stereo output is unavailable")
            output = AudioTrack.Builder()
                .setAudioAttributes(AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_GAME)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
                .setAudioFormat(AudioFormat.Builder().setSampleRate(48000)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
                .setBufferSizeInBytes(maxOf(minimum, 4800 * 4))
                .setTransferMode(AudioTrack.MODE_STREAM).build()
            if (output.state != AudioTrack.STATE_INITIALIZED) error("AudioTrack could not initialize")
            track = output
            if (stopped.get()) return
            output.play()
            while (!stopped.get()) {
                val packet = packets.pollFirst(40, TimeUnit.MILLISECONDS) ?: continue
                val flags = packet[0].toInt() and 0xff
                if (flags and 1 != 0) { output.pause(); output.flush(); output.play() }
                var offset = 4
                while (offset < packet.size && !stopped.get()) {
                    val written = output.write(packet, offset, packet.size - offset, AudioTrack.WRITE_BLOCKING)
                    if (written < 0) error("AudioTrack write failed: $written")
                    if (written == 0) break
                    offset += written
                }
                // EOS leaves the written tail to drain. A new epoch carries
                // a flush flag; lifecycle shutdown flushes and releases.
            }
        } catch (error: Exception) {
            if (!stopped.get()) {
                Log.e("ph0nix", "Phone audio stopped", error)
                onError(error.message ?: error.toString())
            }
        } finally {
            stopped.set(true)
            packets.clear()
            track = null
            runCatching { output?.pause() }
            runCatching { output?.flush() }
            output?.release()
        }
    }

    override fun close() {
        stopped.set(true)
        packets.clear()
        // Interrupt a blocking AudioTrack write without waiting on the UI.
        runCatching { track?.pause() }
        runCatching { track?.flush() }
        thread.interrupt()
    }
}
