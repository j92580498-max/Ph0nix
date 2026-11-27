package com.samsung.e830

import android.content.Context
import android.text.format.Formatter
import org.json.JSONObject
import java.io.File

/** The same image names are used in Android Files and the K750i boot picker. */
internal object MemoryStickImages {
    private val generatedName = Regex("(card|imported)-([0-9a-f]{8})-[0-9a-f-]{27}[.]img")

    fun directory(files: File) = File(files, "cards")

    fun validName(name: String) = StorageDocument.validComponent(name) &&
        name.length > 4 && name.endsWith(".img", ignoreCase = true)

    fun find(directory: File, name: String): File? {
        if (!validName(name)) return null
        val file = File(directory, name)
        return file.takeIf { it.isFile && it.canonicalFile == File(directory.canonicalFile, name) }
    }

    fun list(directory: File): List<File> = directory.list()
        ?.mapNotNull { find(directory, it) }?.sortedBy { it.name }.orEmpty()

    fun label(image: File): String {
        val generated = generatedName.matchEntire(image.name)
        val name = if (generated == null) image.name else "${generated.groupValues[1]}-${generated.groupValues[2]}"
        return "$name (${image.length() / 1048576} MiB)"
    }

    fun capacitySummary(context: Context, info: JSONObject): String = context.getString(
        R.string.card_capacity_summary,
        Formatter.formatShortFileSize(context, info.getLong("capacity_bytes")),
        Formatter.formatShortFileSize(context, info.getLong("used_bytes")),
        Formatter.formatShortFileSize(context, info.getLong("free_bytes")),
    )
}
