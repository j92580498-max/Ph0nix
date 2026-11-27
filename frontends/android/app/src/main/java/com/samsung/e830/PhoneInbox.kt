package com.samsung.e830

import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.util.Locale
import java.util.UUID

// Only fully written copies in `data` are visible to the emulator. It removes `data`
// after verifying the transfer and saving the guest's flash.
internal object PhoneInbox {
    data class Item(val folder: File, val name: String) {
        val id get() = folder.name
        val ready get() = File(folder, "data")
        val staging get() = File(folder, "upload")
    }

    private val writing = mutableSetOf<File>()
    private val importing = mutableSetOf<File>()
    private const val ABANDONED_MS = 24L * 60 * 60 * 1000

    fun directory(phoneDirectory: File) = File(phoneDirectory, "android-inbox")

    @Synchronized
    fun create(directory: File, displayName: String): Item {
        val name = displayName.trim()
        if (!StorageDocument.validComponent(name) || name.length > 180 || name.endsWith('.') ||
            name.any { it < ' ' || it in "\"*:<>?|" }) throw IOException("Invalid internal storage filename.")
        if (!directory.parentFile!!.isDirectory || (!directory.mkdir() && !directory.isDirectory)) {
            throw IOException("This K750i no longer exists.")
        }
        val entries = items(directory)
        val names = entries.mapTo(mutableSetOf()) { it.name.lowercase(Locale.ROOT) }
        val extension = name.lastIndexOf('.').takeIf { it > 0 } ?: name.length
        val chosenName = generateSequence(0) { it + 1 }.map { index ->
            if (index == 0) name else "${name.substring(0, extension)} ($index)${name.substring(extension)}"
        }.first { it.lowercase(Locale.ROOT) !in names }
        val item = Item(File(directory, UUID.randomUUID().toString()), chosenName)
        if (!item.folder.mkdir()) throw IOException("Could not create an incoming file.")
        try {
            File(item.folder, "name").writeText(item.name)
            if (!item.staging.createNewFile()) throw IOException("Could not stage the incoming file.")
        } catch (error: IOException) {
            remove(item)
            throw error
        }
        return item
    }

    @Synchronized
    fun get(directory: File, id: String): Item {
        if (StorageDocument.validId(id)) item(File(directory, id))?.let { return it }
        throw FileNotFoundException("This incoming file is no longer available.")
    }

    @Synchronized
    fun begin(directory: File, id: String): Item = get(directory, id).also {
        if (!it.staging.isFile || it.ready.isFile || !writing.add(it.folder)) {
            throw IOException("This file has already been opened for copying.")
        }
    }

    @Synchronized
    fun publish(item: Item) {
        if (item.folder !in writing || !item.staging.renameTo(item.ready)) {
            throw IOException("Could not save the incoming file.")
        }
    }

    @Synchronized
    fun finish(item: Item) {
        writing.remove(item.folder)
        // Remove a failed write. A successfully published copy is left for the phone.
        if (!item.ready.exists() && item.folder !in importing) remove(item)
    }

    @Synchronized
    fun delete(directory: File, id: String) {
        val item = get(directory, id)
        if (item.folder in importing) throw IOException("This file is being imported into the phone.")
        if (!remove(item)) throw IOException("Could not delete the incoming file.")
    }

    @Synchronized
    fun ready(directory: File): List<Item> = items(directory).filter { it.ready.isFile }
        .sortedWith(compareBy({ it.ready.lastModified() }, { it.id }))

    @Synchronized
    fun claim(directory: File, skipped: Set<String>): Item? = ready(directory).firstOrNull {
        it.id !in skipped && importing.add(it.folder)
    }

    @Synchronized
    fun release(item: Item) {
        importing.remove(item.folder)
        if (!item.ready.exists() && item.folder !in writing) remove(item)
    }

    private fun items(directory: File): List<Item> = directory.listFiles().orEmpty()
        .filter { it.isDirectory && StorageDocument.validId(it.name) }.mapNotNull { folder ->
            val item = item(folder)
            if (folder in writing || folder in importing) item
            else if (item?.ready?.isFile == true) item
            else if (item != null && System.currentTimeMillis() - item.staging.lastModified() < ABANDONED_MS) item
            else { remove(Item(folder, "")); null }
        }

    private fun item(folder: File): Item? {
        if (!File(folder, "data").isFile && !File(folder, "upload").isFile) return null
        val name = runCatching { File(folder, "name").readText() }.getOrNull() ?: return null
        return Item(folder, name).takeIf { StorageDocument.validComponent(name) }
    }

    private fun remove(item: Item): Boolean {
        for (file in listOf(item.staging, item.ready, File(item.folder, "name"))) {
            if (file.exists() && !file.delete()) return false
        }
        return !item.folder.exists() || item.folder.delete()
    }
}
