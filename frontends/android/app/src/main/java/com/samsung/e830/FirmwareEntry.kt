package com.samsung.e830

import java.io.File
import org.json.JSONObject

internal data class FirmwareEntry(
    val id: String,
    val name: String,
    val directory: File,
    val device: String,
    val files: Map<String, File>,
) {
    val profile: DeviceProfile get() = DeviceProfiles.get(device)
    val image: File get() = files.getValue("image")
    val onenand: File? get() = files["nand"]

    // Older K750i imports still use their original workspace under files/sessions.
    val memoryWorkspace: File?
        get() = if (device == "k750i") File(directory, K750_MEMORY_DIRECTORY).takeIf(File::isDirectory) else null

    fun selectionJson(): String {
        val companions = JSONObject()
        files.filterKeys { it != "image" }.forEach { (role, file) -> companions.put(role, file.absolutePath) }
        return JSONObject().put("device", device).put("files",
            JSONObject().put("image", image.absolutePath).put("companions", companions)).toString()
    }

    companion object {
        const val K750_MEMORY_DIRECTORY = "internal-memory"
    }
}
