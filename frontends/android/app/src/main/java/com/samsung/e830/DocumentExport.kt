package com.samsung.e830

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import java.io.File
import java.io.DataInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID
import org.json.JSONObject

internal fun Context.writeDocumentExport(uri: Uri, source: File?) = writeDocumentExport(uri) { source }

internal fun Context.writeCardImageExport(uri: Uri, name: String?) {
    val staged = File(cacheDir, "card-export-${UUID.randomUUID()}.img")
    try {
        writeDocumentExport(uri) {
            val image = name?.let { MemoryStickImages.find(MemoryStickImages.directory(filesDir), it) }
                ?: throw IOException(getString(R.string.card_missing_or_empty))
            val request = JSONObject().put("operation", "snapshot").put("image", image.absolutePath)
                .put("host", staged.absolutePath)
            val response = JSONObject(NativeBridge.nativeCardRequest(request.toString()))
            if (response.has("error")) throw IOException(response.getString("error"))
            staged
        }
    } finally {
        staged.delete()
    }
}

private fun Context.writeDocumentExport(uri: Uri, prepareSource: () -> File?) {
    publishPreparedDocumentExport(
        prepareSource = {
            if (uri.authority == "$packageName.storage") throw IOException(getString(R.string.export_outside_phone))
            prepareSource()
        },
        openDestination = { contentResolver.openOutputStream(uri, "wt") },
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
) = publishPreparedDocumentExport({ source }, openDestination, readDestination, discardDestination)

internal fun publishPreparedDocumentExport(
    prepareSource: () -> File?,
    openDestination: () -> OutputStream?,
    readDestination: () -> InputStream?,
    discardDestination: () -> Unit,
) {
    try {
        val source = prepareSource()
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
