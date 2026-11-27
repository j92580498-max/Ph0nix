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
class PhoneDocumentsProvider : DocumentsProvider() {
    private val app: Context get() = requireNotNull(context)
    private val cards: File get() = File(app.filesDir, "cards")
    private val mutations = Any()
    private val writers = mutableMapOf<String, Any>()
    private val writeHandler by lazy {
        Handler(HandlerThread("ph0nix storage writes").apply { start() }.looper)
    }

    override fun onCreate(): Boolean = true

    override fun queryRoots(projection: Array<out String>?): Cursor = MatrixCursor(projection ?: ROOT_COLUMNS).apply {
        addRow(values(columnNames) { column ->
            when (column) {
                Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID -> StorageDocument.ROOT
                Root.COLUMN_TITLE -> app.getString(R.string.app_name)
                Root.COLUMN_ICON -> R.mipmap.ic_launcher
                Root.COLUMN_FLAGS -> Root.FLAG_LOCAL_ONLY or Root.FLAG_SUPPORTS_CREATE or Root.FLAG_SUPPORTS_IS_CHILD
                else -> null
            }
        })
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor = documentIo {
        val doc = StorageDocument.parse(documentId)
        val phone = phone(doc)
        MatrixCursor(projection ?: DOCUMENT_COLUMNS).apply {
            when {
                doc.isInboxFile -> addInboxFile(doc, PhoneInbox.get(inbox(phone), doc.name))
                doc.isCardRoot -> addDocument(doc, phone, cardIssue = cardIssue(doc))
                else -> addDocument(doc, phone, if (doc.isCardFile) entry(doc) else null)
            }
        }
    }

    override fun queryChildDocuments(
        parentDocumentId: String, projection: Array<out String>?, sortOrder: String?,
    ): Cursor = documentIo {
        val parent = StorageDocument.parse(parentDocumentId)
        val phone = phone(parent)
        MatrixCursor(projection ?: DOCUMENT_COLUMNS).apply {
            setNotificationUri(app.contentResolver, storageChangeUri(app, parent))
            when {
                parent.isRoot -> phones().forEach { addDocument(parent.child(it.id), it) }
                parent.isPhone -> {
                    val machine = requireNotNull(phone)
                    val card = parent.child(StorageDocument.cardComponent(selectedCard(machine)?.name))
                    setExtras(message(DocumentsContract.EXTRA_INFO, R.string.phone_documents_choose_storage))
                    addDocument(card, machine, cardIssue = cardIssue(card))
                    addDocument(parent.child(StorageDocument.INBOX), machine)
                }
                parent.isInbox -> {
                    setExtras(message(DocumentsContract.EXTRA_INFO, R.string.phone_inbox_summary))
                    PhoneInbox.ready(inbox(phone)).forEach { addInboxFile(parent.child(it.id), it) }
                }
                parent.isCard -> try {
                    val entries = list(parent)
                    if (parent.isCardRoot) {
                        setExtras(message(DocumentsContract.EXTRA_INFO, R.string.card_documents_copy_summary))
                    }
                    for (index in 0 until entries.length()) {
                        val child = entries.getJSONObject(index)
                        addDocument(parent.child(child.getString("name")), phone, child)
                    }
                } catch (error: IOException) {
                    setExtras(Bundle().apply { putString(DocumentsContract.EXTRA_ERROR, error.message) })
                }
                else -> throw FileNotFoundException("Not a storage directory.")
            }
        }
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean = try {
        val parent = StorageDocument.parse(parentDocumentId)
        val child = StorageDocument.parse(documentId)
        child.parts.size > parent.parts.size && parent.contains(child)
    } catch (_: FileNotFoundException) {
        false
    }

    override fun findDocumentPath(parentDocumentId: String?, childDocumentId: String): DocumentsContract.Path = documentIo {
        val parent = StorageDocument.parse(parentDocumentId ?: StorageDocument.ROOT)
        val child = StorageDocument.parse(childDocumentId)
        if (!parent.contains(child)) throw FileNotFoundException("Unknown storage path.")
        val phone = phone(child)
        when {
            child.isInboxFile -> PhoneInbox.get(inbox(phone), child.name)
            child.isCardFile -> entry(child)
        }
        val ids = (parent.parts.size..child.parts.size).map { child.ancestor(it).id }
        DocumentsContract.Path(if (parentDocumentId == null) StorageDocument.ROOT else null, ids)
    }

    override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String = documentIo {
        val parent = StorageDocument.parse(parentDocumentId)
        if (parent.isInbox) {
            if (mimeType == Document.MIME_TYPE_DIR) throw IOException(app.getString(R.string.phone_inbox_files_only))
            return@documentIo parent.child(PhoneInbox.create(inbox(phone(parent)), displayName).id).id
        }
        if (!parent.isCard) throw IOException(app.getString(R.string.phone_documents_choose_storage))
        image(parent) // An empty Memory Stick folder has no image to edit.
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
        val document = StorageDocument.parse(documentId)
        if (document.isInboxFile) {
            PhoneInbox.delete(inbox(phone(document)), document.name)
        } else {
            if (!document.isCardFile) throw IOException("Cannot delete storage from Files.")
            synchronized(mutations) {
                val card = image(document)
                request(document, "delete", snapshot = false, card = card)
                writers.remove(writerKey(document, card))
            }
        }
        changed(document.parent())
    }

    override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor = documentIo {
        signal?.throwIfCanceled()
        val document = StorageDocument.parse(documentId)
        if (document.isInboxFile) {
            val directory = inbox(phone(document))
            if (mode == "r") return@documentIo ParcelFileDescriptor.open(
                PhoneInbox.get(directory, document.name).ready, ParcelFileDescriptor.MODE_READ_ONLY,
            )
            if (mode !in setOf("w", "wt", "rw", "rwt")) throw IOException("Unsupported file access mode: $mode")
            val file = PhoneInbox.begin(directory, document.name)
            return@documentIo stagedWriter(file.staging, true, mode, signal, commit = {
                PhoneInbox.publish(file)
                changed(document.parent())
            }, cleanup = { PhoneInbox.finish(file) }, sealAfterSync = true)
        }
        if (!document.isCardFile || entry(document)?.getBoolean("directory") != false) {
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

    private fun openWriter(document: StorageDocument, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
        val truncate = mode in setOf("w", "wt", "rwt")
        if (!truncate && mode != "rw") throw IOException("Unsupported card file access mode: $mode")
        val token = Any()
        val card = image(document)
        val key = writerKey(document, card)
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
        return stagedWriter(temp, truncate, mode, signal, commit = {
            synchronized(mutations) {
                if (writers[key] !== token || image(document) != card || entry(document)?.getBoolean("directory") != false) {
                    throw IOException("The destination was removed while it was open.")
                }
                request(document, "import", snapshot = false, card = card) {
                    put("host", temp.absolutePath).put("replace", true)
                }
            }
            changed(document.parent())
        }, cleanup = cleanup)
    }

    private fun stagedWriter(
        temp: File, truncate: Boolean, mode: String, signal: CancellationSignal?,
        commit: () -> Unit, cleanup: () -> Unit, sealAfterSync: Boolean = false,
    ): ParcelFileDescriptor {
        val callback = try {
            DocumentWriteCallback(temp, truncate, signal, commit, cleanup, sealAfterSync)
        } catch (error: Exception) {
            cleanup()
            throw error
        }
        try {
            val access = if (mode.startsWith('r')) ParcelFileDescriptor.MODE_READ_WRITE else ParcelFileDescriptor.MODE_WRITE_ONLY
            val storage = app.getSystemService(StorageManager::class.java) ?: throw IOException("Storage service unavailable")
            return storage.openProxyFileDescriptor(access, callback, writeHandler)
        } catch (error: Exception) {
            callback.abort()
            throw error
        }
    }

    private fun writerKey(document: StorageDocument, card: File) =
        (card.name + "/" + document.fatPath.joinToString("/")).lowercase(Locale.ROOT)

    private fun entry(document: StorageDocument): JSONObject? {
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

    private fun list(document: StorageDocument): JSONArray = request(document, "list")
        .getJSONObject("snapshot").getJSONArray("entries")

    private fun import(document: StorageDocument, source: File, replace: Boolean) {
        request(document, "import", snapshot = false) { put("host", source.absolutePath).put("replace", replace) }
    }

    private fun export(document: StorageDocument): File {
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
        document: StorageDocument, operation: String, snapshot: Boolean = true, card: File = image(document),
        configure: JSONObject.() -> Unit = {},
    ): JSONObject = synchronized(mutations) {
        val request = JSONObject().put("image", card.absolutePath).put("operation", operation)
            .put("path", JSONArray(document.fatPath)).put("include_snapshot", snapshot).apply(configure)
        val response = JSONObject(NativeBridge.nativeCardRequest(request.toString()))
        if (response.has("error")) {
            val message = response.getString("error")
            throw IOException(if (message.contains("Card image is in use:")) {
                app.getString(R.string.card_documents_in_use)
            } else message)
        }
        response
    }

    private fun cardIssue(document: StorageDocument): String? = try {
        request(document, "info", snapshot = false)
        null
    } catch (error: IOException) {
        error.message ?: app.getString(R.string.card_documents_missing)
    }

    private fun message(key: String, text: Int) = Bundle().apply { putString(key, app.getString(text)) }

    private fun phones() = FirmwareLibrary.get(app).entries().filter { it.device == "k750i" }

    private fun phone(document: StorageDocument): FirmwareEntry? {
        if (document.isRoot) return null
        return phones().firstOrNull { it.id == document.parts.first() }
            ?: throw FileNotFoundException("This K750i no longer exists.")
    }

    private fun inbox(phone: FirmwareEntry?) = PhoneInbox.directory(requireNotNull(phone).directory)

    private fun image(document: StorageDocument): File {
        val machine = requireNotNull(phone(document))
        val card = selectedCard(machine)
        if (card == null || document.cardName != card.name) {
            throw IOException(app.getString(R.string.card_documents_missing))
        }
        return card
    }

    private fun selectedCard(phone: FirmwareEntry): File? {
        val name = AppPreferences(app).phoneCard(phone.id) ?: return null
        if (!StorageDocument.validComponent(name) || !name.endsWith(".img", ignoreCase = true)) return null
        val file = File(cards, name)
        return file.takeIf { it.isFile && it.canonicalFile == File(cards.canonicalFile, name) }
    }

    private fun MatrixCursor.addDocument(
        document: StorageDocument, phone: FirmwareEntry?, entry: JSONObject? = null, cardIssue: String? = null,
    ) {
        val directory = entry?.getBoolean("directory") ?: true
        addRow(values(columnNames) { column ->
            when (column) {
                Document.COLUMN_DOCUMENT_ID -> document.id
                Document.COLUMN_DISPLAY_NAME -> when {
                    document.isRoot -> app.getString(R.string.app_name)
                    document.isPhone -> requireNotNull(phone).name
                    document.isInbox -> app.getString(R.string.phone_inbox_title)
                    document.isCardRoot -> app.getString(R.string.card_documents_title)
                    else -> document.name
                }
                Document.COLUMN_SUMMARY -> when {
                    document.isCardRoot -> cardIssue ?: app.getString(R.string.card_documents_copy_summary)
                    document.isInbox -> app.getString(R.string.phone_inbox_summary)
                    else -> null
                }
                Document.COLUMN_MIME_TYPE -> if (directory) Document.MIME_TYPE_DIR else mimeType(document.name)
                Document.COLUMN_FLAGS -> when {
                    document.isRoot || document.isPhone || cardIssue != null -> 0
                    document.isInbox || directory -> Document.FLAG_DIR_SUPPORTS_CREATE
                    else -> Document.FLAG_SUPPORTS_WRITE or Document.FLAG_SUPPORTS_DELETE
                }
                Document.COLUMN_SIZE -> entry?.takeIf { !it.isNull("size") }?.getLong("size")
                else -> null
            }
        })
    }

    private fun MatrixCursor.addInboxFile(document: StorageDocument, file: PhoneInbox.Item) {
        addRow(values(columnNames) { column ->
            when (column) {
                Document.COLUMN_DOCUMENT_ID -> document.id
                Document.COLUMN_DISPLAY_NAME -> file.name
                Document.COLUMN_MIME_TYPE -> mimeType(file.name)
                Document.COLUMN_FLAGS -> Document.FLAG_SUPPORTS_DELETE or
                    (if (file.ready.isFile) 0 else Document.FLAG_SUPPORTS_WRITE)
                Document.COLUMN_SIZE -> file.ready.length()
                else -> null
            }
        })
    }

    private fun changed(directory: StorageDocument) {
        val cardName = directory.cardName
        if (cardName != null) notifyMemoryStickChanged(app, cardName)
        else app.contentResolver.notifyChange(storageChangeUri(app, directory), null)
    }

    private companion object {
        val ROOT_COLUMNS = arrayOf(Root.COLUMN_ROOT_ID, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_TITLE,
            Root.COLUMN_SUMMARY, Root.COLUMN_ICON, Root.COLUMN_FLAGS)
        val DOCUMENT_COLUMNS = arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME,
            Document.COLUMN_MIME_TYPE, Document.COLUMN_FLAGS, Document.COLUMN_SIZE, Document.COLUMN_SUMMARY)

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

internal class StorageDocument private constructor(val parts: List<String>) {
    val isRoot get() = parts.isEmpty()
    val isPhone get() = parts.size == 1
    private val storage get() = parts.getOrNull(1)
    val isCard get() = storage == CARD || storage?.startsWith("$CARD:") == true
    val cardName get() = storage?.takeIf { it.startsWith("$CARD:") }?.removePrefix("$CARD:")
    val isCardRoot get() = isCard && parts.size == 2
    val isCardFile get() = isCard && parts.size > 2
    val isInbox get() = parts.size == 2 && parts[1] == INBOX
    val isInboxFile get() = parts.size == 3 && parts[1] == INBOX
    val name get() = parts.last()
    val id get() = if (isRoot) ROOT else "$ROOT/${parts.joinToString("/")}"
    val fatPath get() = parts.drop(2)

    fun contains(other: StorageDocument) = other.parts.take(parts.size) == parts
    fun ancestor(size: Int) = StorageDocument(parts.take(size))
    fun parent() = ancestor(parts.size - 1)
    fun child(name: String): StorageDocument {
        if (!validComponent(name)) throw FileNotFoundException("Invalid storage filename.")
        return parse("$id/$name")
    }

    companion object {
        const val ROOT = "phones"
        const val CARD = "card"
        const val INBOX = "inbox"
        private const val MAX_COMPONENTS = 34 // phone ID, storage type and up to 32 FAT components
        private val UUID_PATTERN = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

        fun validId(id: String) = UUID_PATTERN.matches(id)

        fun cardComponent(name: String?) = if (name == null) CARD else "$CARD:$name"

        fun parse(id: String): StorageDocument {
            if (id == ROOT) return StorageDocument(emptyList())
            if (!id.startsWith("$ROOT/")) throw FileNotFoundException("Unknown storage document.")
            val parts = id.removePrefix("$ROOT/").split('/')
            val type = parts.getOrNull(1)
            val validStorage = when (type) {
                null -> true
                CARD -> parts.size == 2
                INBOX -> parts.size == 2 || (parts.size == 3 && validId(parts[2]))
                else -> type.startsWith("$CARD:") && type.endsWith(".img", ignoreCase = true)
            }
            if (!validId(parts.first()) || !validStorage || parts.size > MAX_COMPONENTS ||
                parts.any { !validComponent(it) }) {
                throw FileNotFoundException("Invalid storage document.")
            }
            return StorageDocument(parts)
        }

        fun validComponent(value: String) = value.isNotEmpty() && value.length <= 255 &&
            value != "." && value != ".." && value.none { it == '/' || it == '\\' || it == '\u0000' }
    }
}

private fun storageChangeUri(context: Context, directory: StorageDocument) =
    DocumentsContract.buildChildDocumentsUri("${context.packageName}.storage",
        (if (directory.isCard) directory.ancestor(2) else directory).id)

internal fun notifyPhoneStorageChanged(context: Context, phoneId: String? = null) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val root = StorageDocument.parse(StorageDocument.ROOT)
    val directory = if (phoneId == null) root else root.child(phoneId).child(StorageDocument.INBOX)
    context.contentResolver.notifyChange(storageChangeUri(context, directory), null)
}

internal fun notifyMemoryStickSelectionChanged(context: Context, phoneId: String, previous: String?) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val phone = StorageDocument.parse(StorageDocument.ROOT).child(phoneId)
    context.contentResolver.notifyChange(storageChangeUri(context, phone), null)
    context.contentResolver.notifyChange(storageChangeUri(context, phone.child(StorageDocument.cardComponent(previous))), null)
}

internal fun notifyMemoryStickChanged(context: Context, name: String) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val root = StorageDocument.parse(StorageDocument.ROOT)
    val preferences = AppPreferences(context)
    FirmwareLibrary.get(context).entries().filter { it.device == "k750i" && preferences.phoneCard(it.id) == name }.forEach {
        val card = root.child(it.id).child(StorageDocument.cardComponent(name))
        context.contentResolver.notifyChange(storageChangeUri(context, card.parent()), null)
        context.contentResolver.notifyChange(storageChangeUri(context, card), null)
    }
}

@RequiresApi(26)
internal class DocumentWriteCallback(
    staging: File,
    truncate: Boolean,
    private val signal: CancellationSignal?,
    private val commit: () -> Unit,
    private val cleanup: () -> Unit,
    private val sealAfterSync: Boolean = false,
) : ProxyFileDescriptorCallback() {
    private val file = RandomAccessFile(staging, "rw")
    private var dirty = truncate
    private var failed = false
    private var sealed = false

    override fun onGetSize(): Long = io("size") { file.length() }

    override fun onRead(offset: Long, size: Int, data: ByteArray): Int = io("read") {
        file.seek(offset)
        file.read(data, 0, size).coerceAtLeast(0)
    }

    override fun onWrite(offset: Long, size: Int, data: ByteArray): Int {
        if (sealed) throw ErrnoException("Document already saved", OsConstants.EPERM)
        if (offset < 0 || size < 0 || offset > MAX_BYTES - size) {
            failed = true
            throw ErrnoException("File exceeds 32 MiB", OsConstants.EFBIG)
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
            if (sealAfterSync) file.fd.sync()
            commit()
            sealed = sealAfterSync
            dirty = false
        }
    }

    override fun onRelease() {
        try {
            if (!sealAfterSync && dirty && !failed && signal?.isCanceled != true) onFsync()
        } catch (error: ErrnoException) {
            Log.w("ph0nix", "Could not save a document on close", error)
        } finally {
            abort()
        }
    }

    fun abort() {
        try {
            file.close()
        } catch (error: IOException) {
            Log.w("ph0nix", "Could not close the staged document", error)
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
