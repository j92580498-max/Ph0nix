package com.samsung.e830

import android.content.Context
import android.os.Build
import android.system.Os
import java.io.DataInputStream
import java.io.File
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption

internal object UserMidiSoundFont {
    private const val maxBytes = 128L * 1024 * 1024

    private fun file(context: Context) = File(context.noBackupFilesDir, "user-midi.sf2")

    fun path(context: Context): String? = file(context).takeIf { it.isFile }?.absolutePath

    @Synchronized
    fun install(context: Context, source: InputStream) {
        val input = DataInputStream(source.buffered())
        val header = ByteArray(12).also { input.readFully(it) }
        require(header.decodeToString(0, 4) == "RIFF" && header.decodeToString(8, 12) == "sfbk")
        val riffSize = (4..7).fold(0L) { size, index ->
            size or ((header[index].toLong() and 255L) shl (8 * (index - 4)))
        }
        val totalSize = riffSize + 8
        require(totalSize in 24..maxBytes)

        val destination = file(context)
        val staged = File.createTempFile("midi-", ".tmp", context.noBackupFilesDir)
        try {
            staged.outputStream().use { output ->
                output.write(header)
                val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                var remaining = totalSize - header.size
                while (remaining > 0) {
                    val count = minOf(remaining, buffer.size.toLong()).toInt()
                    input.readFully(buffer, 0, count)
                    output.write(buffer, 0, count)
                    remaining -= count
                }
                require(input.read() == -1)
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                Files.move(staged.toPath(), destination.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
            } else {
                Os.rename(staged.absolutePath, destination.absolutePath)
            }
        } finally {
            staged.delete()
        }
    }

    @Synchronized
    fun clear(context: Context) {
        val bank = file(context)
        check(!bank.exists() || bank.delete())
    }
}
