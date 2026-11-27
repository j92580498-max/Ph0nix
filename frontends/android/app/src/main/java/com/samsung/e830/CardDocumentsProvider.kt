package com.samsung.e830

import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.os.Build
import android.os.Bundle
import android.os.CancellationSignal
import android.os.Handler
import android.os.HandlerThread
import android.os.ParcelFileDescriptor
import android.os.ProxyFileDescriptorCallback
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.system.ErrnoException
import android.system.OsConstants
import android.util.Log
import android.webkit.MimeTypeMap
import androidx.annotation.RequiresApi
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.io.RandomAccessFile
import java.util.Locale
import java.util.UUID

// The provider is enabled by the manifest only on Android 8.0 and later.
@RequiresApi(26)
class CardDocumentsProvider : DocumentsProvider() {
    private val app: Context get() = requireNotNull(context)
    private val cards: File get() = File(app.filesDir, "cards")
    private val mutations = Any()
    private val writers = mutableMapOf<String, Any>()
    private val writeHandler by lazy {
        Handler(HandlerThread("ph0nix Memory Stick writes").apply { start() }.looper)
    }

    override fun onCreate(): Boolean = true

    override fun queryRoots(projection: Array<out String>?): Cursor = MatrixCursor(projection ?: ROOT_COLUMNS).apply {
        addRow(values(columnNames) { column ->
            when (column) {
                Root.COLUMN_ROOT_ID -> CardDocument.ROOT
                Root.COLUMN_DOCUMENT_ID -> CardDocument.ROOT
                Root.COLUMN_TITLE -> app.getString(R.string.card_documents_title)
                Root.COLUMN_SUMMARY -> app.getString(R.string.card_documents_summary)
                Root.COLUMN_ICON -> R.mipmap.ic_launcher
                Root.COLUMN_FLAGS -> Root.FLAG_LOCAL_ONLY or Root.FLAG_SUPPORTS_CREATE or Root.FLAG_SUPPORTS_IS_CHILD
                else -> null
            }
        })
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor = documentIo {
        val document = CardDocument.parse(documentId)
        MatrixCursor(projection ?: DOCUMENT_COLUMNS).apply { addDocument(document, entry(document)) }
    }

    override fun queryChildDocuments(
        parentDocumentId: String, projection: Array<out String>?, sortOrder: String?,
    ): Cursor = documentIo {
        val parent = CardDocument.parse(parentDocumentId)
        MatrixCursor(projection ?: DOCUMENT_COLUMNS).apply {
            setNotificationUri(app.contentResolver, if (parent.isRoot) cardUri(app, parent) else cardUri(app, parent.card()))
            if (parent.isRoot) {
                cards.listFiles().orEmpty().filter(::isImage).sortedBy { it.name.lowercase(Locale.ROOT) }
                    .forEach { addDocument(parent.child(it.name), null) }
            } else {
                try {
                    val entries = list(parent)
                    for (index in 0 until entries.length()) {
                        val child = entries.getJSONObject(index)
                        addDocument(parent.child(child.getString("name")), child)
                    }
                } catch (error: CardInUse) {
                    setExtras(Bundle().apply { putString(DocumentsContract.EXTRA_ERROR, error.message) })
                }
            }
        }
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean = try {
        val parent = CardDocument.parse(parentDocumentId)
        val child = CardDocument.parse(documentId)
        child.parts.size > parent.parts.size && parent.contains(child)
    } catch (_: FileNotFoundException) {
        false
    }

    override fun findDocumentPath(parentDocumentId: String?, childDocumentId: String): DocumentsContract.Path = documentIo {
        val parent = CardDocument.parse(parentDocumentId ?: CardDocument.ROOT)
        val child = CardDocument.parse(childDocumentId)
        if (!parent.contains(child)) throw FileNotFoundException("Unknown path on the Memory Stick")
        entry(child)
        val ids = (parent.parts.size..child.parts.size).map { child.ancestor(it).id }
        DocumentsContract.Path(if (parentDocumentId == null) CardDocument.ROOT else null, ids)
    }

    override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String = documentIo {
        val parent = CardDocument.parse(parentDocumentId)
        if (parent.isRoot) throw IOException("Select a Memory Stick first.")
        val document = parent.child(displayName.trim())
        if (document.name.last() == '.' || document.name.any { it < ' ' || it in "\"*:<>?|" }) {
            throw IOException("This name is not valid on a Memory Stick.")
        }
        synchronized(mutations) {
            if (mimeType == Document.MIME_TYPE_DIR) {
                request(document, "make_directory", snapshot = false)
            } else {
                val empty = File.createTempFile("card-empty-", ".tmp", app.cacheDir)
                try {
                    import(document, empty, replace = false)
                } finally {
                    empty.delete()
                }
            }
        }
        changed(parent)
        document.id
    }

    override fun deleteDocument(documentId: String) = documentIo {
        val document = CardDocument.parse(documentId)
        if (document.fatPath.isEmpty()) throw IOException("Cannot delete a Memory Stick from Files.")
        synchronized(mutations) {
            request(document, "delete", snapshot = false)
            writers.remove(document.writerKey)
        }
        changed(document.parent())
    }

    override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor = documentIo {
        signal?.throwIfCanceled()
        val document = CardDocument.parse(documentId)
        if (document.fatPath.isEmpty() || entry(document)?.getBoolean("directory") != false) {
            throw IOException("Select a file on the Memory Stick.")
        }
        if (mode == "r") {
            val temp = export(document)
            try {
                ParcelFileDescriptor.open(temp, ParcelFileDescriptor.MODE_READ_ONLY)
            } finally {
                // An open Unix descriptor keeps the snapshot alive until the reader closes it.
                temp.delete()
            }
        } else {
            openWriter(document, mode, signal)
        }
    }

    private fun openWriter(document: CardDocument, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
        val truncate = mode in setOf("w", "wt", "rwt")
        if (!truncate && mode != "rw") throw IOException("Unsupported card file access mode: $mode")
        val token = Any()
        val key = document.writerKey
        val temp = synchronized(mutations) {
            if (writers.containsKey(key)) throw IOException("This card file is already open for writing.")
            val staging = if (truncate) File.createTempFile("card-write-", ".tmp", app.cacheDir) else export(document)
            writers[key] = token
            staging
        }
        val cleanup = {
            temp.delete()
            synchronized(mutations) { if (writers[key] === token) writers.remove(key) }
            Unit
        }
        try {
            val callback = CardWriteCallback(temp, truncate, signal, commit = {
                synchronized(mutations) {
                    if (writers[key] !== token || entry(document)?.getBoolean("directory") != false) {
                        throw IOException("The destination was removed while it was open.")
                    }
                    import(document, temp, replace = true)
                }
                changed(document.parent())
            }, cleanup = cleanup)
            try {
                val access = if (mode.startsWith('r')) ParcelFileDescriptor.MODE_READ_WRITE else ParcelFileDescriptor.MODE_WRITE_ONLY
                val storage = app.getSystemService(StorageManager::class.java) ?: throw IOException("Storage service unavailable")
                return storage.openProxyFileDescriptor(access, callback, writeHandler)
            } catch (error: Exception) {
                callback.abort()
                throw error
            }
        } catch (error: Exception) {
            cleanup()
            throw error
        }
    }

    private fun entry(document: CardDocument): JSONObject? {
        if (document.isRoot) return null
        if (document.fatPath.isEmpty()) {
            image(document)
            return null
        }
        val entries = list(document.parent())
        for (index in 0 until entries.length()) {
            val entry = entries.getJSONObject(index)
            if (entry.getString("name") == document.name) return entry
        }
        throw FileNotFoundException("File is no longer on the Memory Stick: ${document.name}")
    }

    private fun list(document: CardDocument): JSONArray = request(document, "list")
        .getJSONObject("snapshot").getJSONArray("entries")

    private fun import(document: CardDocument, source: File, replace: Boolean) {
        request(document, "import", snapshot = false) { put("host", source.absolutePath).put("replace", replace) }
    }

    private fun export(document: CardDocument): File {
        // Native export creates its destination exclusively.
        val temp = File(app.cacheDir, "card-read-${UUID.randomUUID()}.tmp")
        try {
            request(document, "export", snapshot = false) { put("host", temp.absolutePath) }
            return temp
        } catch (error: Exception) {
            temp.delete()
            throw error
        }
    }

    private fun request(
        document: CardDocument, operation: String, snapshot: Boolean = true, configure: JSONObject.() -> Unit = {},
    ): JSONObject {
        val request = JSONObject().put("image", image(document).absolutePath).put("operation", operation)
            .put("path", JSONArray(document.fatPath)).put("include_snapshot", snapshot).apply(configure)
        val response = JSONObject(NativeBridge.nativeCardRequest(request.toString()))
        if (response.has("error")) {
            val message = response.getString("error")
            if (message.contains("Card image is in use:")) throw CardInUse(app.getString(R.string.card_documents_in_use))
            throw IOException(message)
        }
        return response
    }

    private fun image(document: CardDocument): File {
        val file = File(cards, document.parts.first())
        if (!isImage(file)) throw FileNotFoundException("Memory Stick image no longer exists.")
        return file
    }

    private fun isImage(file: File): Boolean = file.isFile && file.name.endsWith(".img", ignoreCase = true) &&
        file.canonicalFile == File(cards.canonicalFile, file.name)

    private fun MatrixCursor.addDocument(document: CardDocument, entry: JSONObject?) {
        val directory = entry?.getBoolean("directory") ?: true
        addRow(values(columnNames) { column ->
            when (column) {
                Document.COLUMN_DOCUMENT_ID -> document.id
                Document.COLUMN_DISPLAY_NAME -> if (document.isRoot) app.getString(R.string.card_documents_title) else document.name
                Document.COLUMN_MIME_TYPE -> if (directory) Document.MIME_TYPE_DIR else mimeType(document.name)
                Document.COLUMN_FLAGS -> when {
                    document.isRoot -> 0
                    directory -> Document.FLAG_DIR_SUPPORTS_CREATE
                    else -> Document.FLAG_SUPPORTS_WRITE or Document.FLAG_SUPPORTS_DELETE
                }
                Document.COLUMN_SIZE -> entry?.takeIf { !it.isNull("size") }?.getLong("size")
                else -> null
            }
        })
    }

    private fun changed(directory: CardDocument) {
        app.contentResolver.notifyChange(cardUri(app, directory.card()), null)
    }

    private class CardInUse(message: String) : IOException(message)

    private companion object {
        val ROOT_COLUMNS = arrayOf(Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_TITLE,
            Root.COLUMN_SUMMARY, Root.COLUMN_ICON, Root.COLUMN_FLAGS)
        val DOCUMENT_COLUMNS = arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME,
            Document.COLUMN_MIME_TYPE, Document.COLUMN_FLAGS, Document.COLUMN_SIZE)

        fun values(columns: Array<String>, value: (String) -> Any?): Array<Any?> =
            Array(columns.size) { value(columns[it]) }

        fun mimeType(name: String): String = MimeTypeMap.getSingleton()
            .getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase(Locale.ROOT)) ?: "application/octet-stream"

        inline fun <T> documentIo(block: () -> T): T = try {
            block()
        } catch (error: FileNotFoundException) {
            throw error
        } catch (error: IOException) {
            throw FileNotFoundException(error.message).apply { initCause(error) }
        }
    }
}

internal class CardDocument private constructor(val parts: List<String>) {
    val isRoot get() = parts.isEmpty()
    val name get() = parts.last()
    val id get() = if (isRoot) ROOT else "$ROOT/${parts.joinToString("/")}"
    val fatPath get() = parts.drop(1)
    val writerKey get() = id.lowercase(Locale.ROOT)

    fun contains(other: CardDocument) = other.parts.take(parts.size) == parts
    fun ancestor(size: Int) = CardDocument(parts.take(size))
    fun card() = ancestor(1)
    fun parent() = ancestor(parts.size - 1)
    fun child(name: String): CardDocument {
        if (!validComponent(name) || parts.size >= MAX_COMPONENTS) throw FileNotFoundException("Invalid card filename")
        return CardDocument(parts + name)
    }

    companion object {
        const val ROOT = "cards"
        private const val MAX_COMPONENTS = 33 // image filename plus 32 FAT path components

        fun parse(id: String): CardDocument {
            if (id == ROOT) return CardDocument(emptyList())
            if (!id.startsWith("$ROOT/")) throw FileNotFoundException("Unknown card document")
            val parts = id.removePrefix("$ROOT/").split('/')
            if (parts.size > MAX_COMPONENTS || parts.any { !validComponent(it) } ||
                !parts.first().endsWith(".img", ignoreCase = true)) throw FileNotFoundException("Invalid card document")
            return CardDocument(parts)
        }

        private fun validComponent(value: String) = value.isNotEmpty() && value.length <= 255 &&
            value != "." && value != ".." && value.none { it == '/' || it == '\\' || it == '\u0000' }
    }
}

private fun cardUri(context: Context, document: CardDocument) =
    DocumentsContract.buildDocumentUri("${context.packageName}.cards", document.id)

internal fun notifyCardImagesChanged(context: Context, cardName: String? = null) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val root = CardDocument.parse(CardDocument.ROOT)
    val document = if (cardName == null) root else root.child(cardName)
    context.contentResolver.notifyChange(cardUri(context, document), null)
}

@RequiresApi(26)
internal class CardWriteCallback(
    staging: File,
    truncate: Boolean,
    private val signal: CancellationSignal?,
    private val commit: () -> Unit,
    private val cleanup: () -> Unit,
) : ProxyFileDescriptorCallback() {
    private val file = RandomAccessFile(staging, "rw")
    private var dirty = truncate
    private var failed = false

    override fun onGetSize(): Long = io("size") { file.length() }

    override fun onRead(offset: Long, size: Int, data: ByteArray): Int = io("read") {
        file.seek(offset)
        file.read(data, 0, size).coerceAtLeast(0)
    }

    override fun onWrite(offset: Long, size: Int, data: ByteArray): Int {
        if (offset < 0 || size < 0 || offset > MAX_BYTES - size) {
            failed = true
            throw ErrnoException("Memory Stick file exceeds 32 MiB", OsConstants.EFBIG)
        }
        return io("write") {
            file.seek(offset)
            file.write(data, 0, size)
            dirty = true
            size
        }
    }

    override fun onFsync() = io("sync") {
        if (dirty) {
            commit()
            dirty = false
        }
    }

    override fun onRelease() {
        try {
            if (dirty && !failed && signal?.isCanceled != true) onFsync()
        } catch (error: ErrnoException) {
            Log.w("ph0nix", "Could not save a Memory Stick file on close", error)
        } finally {
            abort()
        }
    }

    fun abort() {
        try {
            file.close()
        } catch (error: IOException) {
            Log.w("ph0nix", "Could not close the staged card file", error)
        } finally {
            cleanup()
        }
    }

    private inline fun <T> io(operation: String, action: () -> T): T {
        if (signal?.isCanceled == true) throw ErrnoException(operation, OsConstants.ECANCELED)
        if (failed) throw ErrnoException(operation, OsConstants.EIO)
        try {
            return action()
        } catch (error: Exception) {
            failed = true
            val errno = if (error is FileNotFoundException) OsConstants.ENOENT else OsConstants.EIO
            throw ErrnoException(operation, errno).apply { initCause(error) }
        }
    }

    private companion object {
        const val MAX_BYTES = 32L * 1024 * 1024
    }
}
