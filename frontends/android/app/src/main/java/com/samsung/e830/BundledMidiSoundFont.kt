package com.samsung.e830

import android.content.Context
import android.util.AtomicFile
import java.io.File

internal object BundledMidiSoundFont {
    private const val name = "default.sf2"

    @Synchronized
    fun prepare(context: Context): String? {
        if (context.assets.list("midi")?.contains(name) != true) return null
        val apkVersion = File(context.applicationInfo.sourceDir).lastModified()
        val file = File(context.noBackupFilesDir, name)
        if (!file.isFile || file.lastModified() != apkVersion) {
            val atomic = AtomicFile(file)
            context.assets.open("midi/$name").use { input ->
                val output = atomic.startWrite()
                try {
                    input.copyTo(output)
                    atomic.finishWrite(output)
                } catch (error: Exception) {
                    atomic.failWrite(output)
                    throw error
                }
            }
            file.setLastModified(apkVersion)
        }
        return file.absolutePath
    }
}
