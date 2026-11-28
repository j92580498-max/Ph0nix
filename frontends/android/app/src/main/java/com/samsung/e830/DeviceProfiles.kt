package com.samsung.e830

import android.content.Context
import org.json.JSONArray

internal data class FileRole(val role: String, val extensions: Set<String>, val required: Boolean)
internal data class DeviceProfile(
    val id: String,
    val name: String,
    val roles: List<FileRole>,
    val hasNand: Boolean,
    val displayWidth: Int,
    val displayHeight: Int,
    val selectionHint: String,
    val support: String,
) {
    fun selectionHint(context: Context): String = when (id) {
        "k750i" -> context.getString(R.string.k750_selection_hint)
        else -> selectionHint
    }

    fun support(context: Context): String = when (id) {
        "k750i" -> context.getString(R.string.k750_support)
        else -> support
    }
}

// The native registry supplies phone identities, file formats and LCD dimensions.
internal object DeviceProfiles {
    val all: List<DeviceProfile> by lazy {
        val array = JSONArray(NativeBridge.nativeProfiles())
        List(array.length()) { index ->
            val item = array.getJSONObject(index)
            val info = item.getJSONObject("info")
            val display = info.optJSONObject("display")
            val roles = item.getJSONArray("roles")
            DeviceProfile(
                info.getString("id"), info.getString("name"),
                List(roles.length()) { roleIndex ->
                    val role = roles.getJSONObject(roleIndex)
                    val extensions = role.getJSONArray("extensions")
                    FileRole(role.getString("role"),
                        List(extensions.length()) { extensions.getString(it) }.toSet(),
                        role.getBoolean("required"))
                },
                info.getBoolean("has_nand"), display?.getInt("width") ?: 0,
                display?.getInt("height") ?: 0, item.getString("selection_hint"), info.getString("support"),
            )
        }
    }

    fun find(id: String): DeviceProfile? = all.firstOrNull { it.id == id }

    fun get(id: String): DeviceProfile = find(id)
        ?: throw IllegalArgumentException("Unknown phone profile $id")
}
