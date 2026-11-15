package com.samsung.e830

import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.AtomicFile
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.util.Locale
import java.util.UUID

internal class FirmwareLibrary(context: Context) {
    private val resolver: ContentResolver = context.applicationContext.contentResolver
    private val root = File(context.applicationContext.filesDir, "firmware-library")
    private val indexFile = File(root, "library.json")
    private val index = AtomicFile(indexFile)
    private var recovered = false

    init {
        if (!root.exists() && !root.mkdirs()) throw IOException("Could not create the firmware library.")
    }

    @Synchronized
    fun entries(): List<FirmwareEntry> {
        ensureRecovered()
        return readRecords().mapNotNull { record ->
            val directory = File(root, record.id)
            if (!directory.isDirectory) return@mapNotNull null
            val profile = DeviceProfiles.find(record.device) ?: return@mapNotNull null
            val files = record.files.mapValues { File(directory, it.value) }.filterValues(File::isFile).toMutableMap()
            if (profile.roles.any { it.required && it.role !in files }) return@mapNotNull null
            if (profile.hasNand) {
                File(directory, "physical.onenand.bin").takeIf(File::isFile)?.let { files["nand"] = it }
            }
            FirmwareEntry(record.id, record.name, directory, record.device, files)
        }.sortedBy { it.name.lowercase(Locale.ROOT) }
    }

    @Synchronized
    fun importFirmware(uris: List<Uri>, device: String): FirmwareEntry {
        ensureRecovered()
        val profile = DeviceProfiles.get(device)
        val selected = linkedMapOf<String, Pair<Uri, String>>()
        for (uri in uris) {
            val name = displayName(uri)
            val extension = name.substringAfterLast('.', "").lowercase(Locale.ROOT)
            val role = profile.roles.firstOrNull { extension in it.extensions }
                ?: throw IllegalArgumentException(profile.selectionHint)
            if (selected.putIfAbsent(role.role, uri to name) != null) {
                throw IllegalArgumentException("Select only one ${role.role} file.")
            }
        }
        for (role in profile.roles.filter { it.required }) {
            if (role.role !in selected) throw IllegalArgumentException("The ${role.role} file was not selected.")
        }
        val image = selected.getValue("image")
        val id = UUID.randomUUID().toString()
        val staging = File(root, ".import-$id")
        val destination = File(root, id)
        if (!staging.mkdirs()) throw IOException("Could not stage the firmware import.")
        try {
            val files = selected.mapValues { (role, source) ->
                val extension = source.second.substringAfterLast('.').lowercase(Locale.ROOT)
                copyUri(source.first, File(staging, "$role.$extension"))
            }
            val display = image.second.substringBeforeLast('.', image.second).ifBlank { profile.name }.take(80)
            val staged = FirmwareEntry(id, display, staging, device, files)
            val error = NativeBridge.nativeValidateSelection(staged.selectionJson())
            if (error.isNotBlank()) throw IllegalArgumentException(error)
            if (!staging.renameTo(destination)) throw IOException("Could not finish the firmware import.")
            val names = files.mapValues { it.value.name }
            writeRecords(readRecords() + Record(id, display, device, names))
            return FirmwareEntry(id, display, destination, device, names.mapValues { File(destination, it.value) })
        } catch (error: Throwable) {
            staging.deleteRecursively()
            destination.deleteRecursively()
            throw error
        }
    }

    @Synchronized
    fun attachOneNand(entry: FirmwareEntry, uri: Uri): FirmwareEntry {
        ensureRecovered()
        require(entry.profile.hasNand) { "This phone has no NAND profile." }
        val name = displayName(uri)
        val staging = File(entry.directory, ".physical-onenand.tmp")
        val destination = File(entry.directory, "physical.onenand.bin")
        if (staging.exists() && !staging.delete()) throw IOException("Could not clear a previous NAND import attempt.")
        try {
            copyUri(uri, staging)
            val selected = entry.copy(files = entry.files + ("nand" to staging))
            val error = NativeBridge.nativeValidateSelection(selected.selectionJson())
            if (error.isNotBlank()) throw IllegalArgumentException(error)
            val atomic = AtomicFile(destination)
            val output = atomic.startWrite()
            try {
                staging.inputStream().use { source -> source.copyTo(output) }
                atomic.finishWrite(output)
            } catch (error: Throwable) {
                atomic.failWrite(output)
                throw error
            }
            staging.delete()
            val updated = entry.copy(files = entry.files + ("nand" to destination))
            writeRecords(readRecords().map { if (it.id == updated.id) updated.record() else it })
            return updated
        } catch (error: Throwable) {
            staging.delete()
            throw IllegalArgumentException("Could not use $name: ${error.message ?: "invalid file"}", error)
        }
    }

    @Synchronized
    fun useAutomaticNand(entry: FirmwareEntry): FirmwareEntry {
        ensureRecovered()
        val stored = File(entry.directory, "physical.onenand.bin")
        val relatedFiles = listOfNotNull(
            stored,
            File(entry.directory, "physical.onenand.bin.bak"),
            File(entry.directory, "physical.onenand.bin.new"),
            File(entry.directory, ".physical-onenand.tmp"),
            // A NAND selected during a combined import has a role-based name.
            entry.onenand?.takeIf { it.parentFile == entry.directory },
        )
        AtomicFile(stored).delete()
        relatedFiles.filter { it != stored }.forEach { if (it.exists()) it.delete() }
        if (relatedFiles.any(File::exists)) throw IOException("Could not remove the imported NAND copy.")
        val updated = entry.copy(files = entry.files - "nand")
        writeRecords(readRecords().map { if (it.id == updated.id) updated.record() else it })
        return updated
    }

    @Synchronized
    fun remove(entry: FirmwareEntry) {
        ensureRecovered()
        val remaining = readRecords().filterNot { it.id == entry.id }
        val tombstone = File(root, ".remove-${entry.id}")
        if (tombstone.exists()) throw IOException("A previous firmware removal is still being recovered.")
        if (!entry.directory.renameTo(tombstone)) throw IOException("Could not stage this firmware entry for removal.")
        try {
            writeRecords(remaining)
        } catch (error: Throwable) {
            if (!tombstone.renameTo(entry.directory)) {
                error.addSuppressed(IOException("Could not restore the firmware entry after the removal failed."))
            }
            throw error
        }
        tombstone.deleteRecursively()
    }

    private fun ensureRecovered() {
        if (!recovered) {
            recoverPendingChanges()
            recovered = true
        }
    }

    private fun recoverPendingChanges() {
        val recordIds = readRecords().mapTo(mutableSetOf()) { it.id }
        val pendingFiles = root.listFiles() ?: return
        for (pending in pendingFiles) {
            when {
                pending.name.startsWith(".import-") -> pending.deleteRecursively()
                pending.name.startsWith(".remove-") -> {
                    val id = pending.name.removePrefix(".remove-")
                    if (ID_PATTERN.matches(id) && id in recordIds && !File(root, id).exists()) {
                        if (!pending.renameTo(File(root, id))) {
                            throw IOException("Could not restore a firmware entry after an interrupted removal.")
                        }
                    } else {
                        pending.deleteRecursively()
                    }
                }
                ID_PATTERN.matches(pending.name) && pending.isDirectory -> {
                    if (pending.name !in recordIds) {
                        pending.deleteRecursively()
                    } else {
                        recoverOneNand(pending)
                        File(pending, ".physical-onenand.tmp").delete()
                    }
                }
            }
        }
    }

    private fun recoverOneNand(directory: File) {
        val nand = File(directory, "physical.onenand.bin")
        val backup = File(directory, "physical.onenand.bin.bak")
        val temporary = File(directory, "physical.onenand.bin.new")
        if (nand.exists() || backup.exists()) AtomicFile(nand).openRead().use { }
        temporary.delete()
    }

    private fun displayName(uri: Uri): String {
        return resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (!cursor.moveToFirst()) null else cursor.getString(0)
        }?.takeIf(String::isNotBlank) ?: uri.lastPathSegment?.substringAfterLast('/')?.takeIf(String::isNotBlank)
        ?: throw IOException("The selected document has no file name.")
    }

    private fun copyUri(uri: Uri, destination: File): File {
        val input = resolver.openInputStream(uri) ?: throw IOException("Could not open the selected file.")
        input.use { source ->
            destination.outputStream().use { output ->
                val buffer = ByteArray(64 * 1024)
                var total = 0L
                while (true) {
                    val count = source.read(buffer)
                    if (count < 0) break
                    total += count
                    if (total > MAX_FILE_BYTES) throw IOException("A selected file exceeds the 128 MiB import limit.")
                    output.write(buffer, 0, count)
                }
                if (total == 0L) throw IOException("A selected file is empty.")
            }
        }
        return destination
    }

    private fun readRecords(): List<Record> {
        val backup = File(root, "library.json.bak")
        val temporary = File(root, "library.json.new")
        if (!indexFile.exists() && !backup.exists()) {
            temporary.delete()
            return emptyList()
        }
        val input = index.openRead()
        temporary.delete()
        return input.use { stream ->
            val array = JSONArray(stream.bufferedReader().readText())
            buildList {
                for (position in 0 until array.length()) {
                    val item = array.optJSONObject(position) ?: continue
                    val id = item.optString("id")
                    val name = item.optString("name")
                    if (ID_PATTERN.matches(id) && name.isNotBlank()) {
                        val device = item.optString("device", "e830")
                        val stored = item.optJSONObject("files")
                        // The old E830 index stored only id/name with fixed private filenames.
                        val files = if (stored == null) mapOf(
                            "image" to "firmware.cla", "eeprom" to "eeprom.nsf", "tfs_package" to "filesystem.tfs",
                        ) else buildMap {
                            for (role in stored.keys()) {
                                val file = stored.getString(role)
                                if (File(file).name != file || file in setOf(".", "..")) {
                                    throw IOException("Invalid filename in firmware index.")
                                }
                                put(role, file)
                            }
                        }
                        add(Record(id, name.take(80), device, files))
                    }
                }
            }
        }
    }

    private fun writeRecords(records: List<Record>) {
        val array = JSONArray()
        records.forEach { record ->
            array.put(JSONObject().put("id", record.id).put("name", record.name)
                .put("device", record.device).put("files", JSONObject(record.files)))
        }
        val output = index.startWrite()
        try {
            output.write(array.toString(2).toByteArray(Charsets.UTF_8))
            index.finishWrite(output)
        } catch (error: Throwable) {
            index.failWrite(output)
            throw error
        }
    }

    private data class Record(val id: String, val name: String, val device: String, val files: Map<String, String>)
    private fun FirmwareEntry.record() = Record(id, name, device, files.mapValues { it.value.name })

    private companion object {
        const val MAX_FILE_BYTES = 128L * 1024L * 1024L
        val ID_PATTERN = Regex("^[0-9a-fA-F-]{36}$")
    }
}
