package com.samsung.e830

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import java.io.File
import java.io.DataInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

internal fun Context.writeDocumentExport(uri: Uri, source: File?) {
    publishDocumentExport(
        source,
        openDestination = {
            if (uri.authority == "$packageName.storage") throw IOException(getString(R.string.export_outside_phone))
            contentResolver.openOutputStream(uri, "wt")
        },
        readDestination = { contentResolver.openInputStream(uri) },
        discardDestination = {
            if (!DocumentsContract.deleteDocument(contentResolver, uri)) {
                throw IOException("Could not remove the unfinished export.")
            }
        },
    )
}

internal fun publishDocumentExport(
    source: File?,
    openDestination: () -> OutputStream?,
    readDestination: () -> InputStream?,
    discardDestination: () -> Unit,
) {
    try {
        if (source == null || !source.isFile) throw IOException("The export source is missing.")
        val expected = source.length()
        if (expected == 0L) throw IOException("The export source is empty.")
        source.inputStream().use { input ->
            val destination = openDestination() ?: throw IOException("Could not open the export destination.")
            destination.use { output ->
                if (input.copyTo(output, 64 * 1024) != expected) throw IOException("The export source size changed.")
            }
        }
        val saved = readDestination() ?: throw IOException("Could not reopen the saved export.")
        DataInputStream(saved.buffered()).use { actual ->
            DataInputStream(source.inputStream().buffered()).use { input ->
                val sourceBytes = ByteArray(64 * 1024)
                val savedBytes = ByteArray(sourceBytes.size)
                var remaining = expected
                while (remaining > 0) {
                    val count = minOf(remaining, sourceBytes.size.toLong()).toInt()
                    input.readFully(sourceBytes, 0, count)
                    actual.readFully(savedBytes, 0, count)
                    if (!sourceBytes.contentEquals(savedBytes)) throw IOException("The saved export differs from the source.")
                    remaining -= count
                }
                if (actual.read() != -1 || input.read() != -1) throw IOException("The export size changed.")
            }
        }
    } catch (error: Exception) {
        try {
            discardDestination()
        } catch (cleanup: Exception) {
            error.addSuppressed(cleanup)
        }
        throw error
    }
}
