package com.samsung.e830

import android.content.Context
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import com.google.android.material.button.MaterialButton
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import org.json.JSONArray
import org.json.JSONObject

internal class PhoneFilePanel(
    private val context: Context,
    private val request: (JSONObject) -> Unit,
    private val export: (List<String>, Boolean) -> Unit,
    private val bluetoothSend: (List<String>) -> Unit,
    private val title: String = "Phone files",
) {
    private var path = emptyList<String>()
    private var entries = JSONArray()
    private var rowsKey = ""
    private var busy = false
    private val pathText = TextView(context).apply { setPadding(dp(12), dp(8), dp(12), dp(8)) }
    private val status = TextView(context).apply { setPadding(dp(12), dp(8), dp(12), dp(8)); textSize = 12f }
    private val rows = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
    private val dialog: AlertDialog
    val isShowing get() = dialog.isShowing

    init {
        val layout = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
        val buttons = LinearLayout(context)
        buttons.addView(button("Up") { if (path.isNotEmpty()) list(path.dropLast(1)) }, weighted())
        buttons.addView(button("Refresh") { list(path) }, weighted())
        layout.addView(pathText)
        layout.addView(buttons)
        layout.addView(ScrollView(context).apply { addView(rows) },
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(300)))
        layout.addView(button("New folder…") {
            prompt("New folder", "Folder name") { name ->
                if (name.isNotBlank() && !name.contains('/') && !name.contains('\\'))
                    request(JSONObject().put("operation", "make_directory").put("path", JSONArray(path + name)))
            }
        })
        layout.addView(status)
        dialog = MaterialAlertDialogBuilder(context).setTitle(title).setView(layout)
            .setPositiveButton("Close") { _, _ -> }.create()
    }

    fun show() { dialog.show(); list(path) }
    fun dismiss() = dialog.dismiss()
    fun refresh() = list(path)
    fun currentPath(): List<String> = path.toList()
    fun message(value: String) { status.text = value }

    fun update(snapshot: JSONObject) {
        busy = snapshot.optBoolean("busy")
        val current = snapshot.optJSONArray("path") ?: JSONArray()
        path = List(current.length()) { current.getString(it) }
        pathText.text = "/${path.joinToString("/")}"
        entries = snapshot.optJSONArray("entries") ?: JSONArray()
        val key = "$busy:$path:$entries"
        if (key != rowsKey) { rowsKey = key; rebuildRows() }
        val job = snapshot.optJSONObject("job")
        status.text = when {
            job != null && !job.isNull("error") -> job.optString("error")
            busy -> "${if (job?.optString("phase") == "awaiting_save") "Saving phone state" else job?.optString("phase").orEmpty()} (${job?.optLong("bytes") ?: 0} bytes)"
            !snapshot.optBoolean("attached") -> "Waiting for the phone's transfer service."
            !snapshot.optBoolean("connected") -> "Browse to connect to the phone."
            entries.length() == 0 -> "This folder is empty."
            else -> "${entries.length()} items"
        }
        snapshot.optString("notice").takeIf { it.isNotEmpty() }?.let { status.append("\n$it") }
    }

    private fun list(value: List<String>) {
        request(JSONObject().put("operation", "list").put("path", JSONArray(value)))
    }

    private fun rebuildRows() {
        rows.removeAllViews()
        for (index in 0 until entries.length()) {
            val entry = entries.getJSONObject(index)
            val name = entry.getString("name")
            val directory = entry.optBoolean("directory")
            rows.addView(button(if (directory) "$name/" else "$name · ${if (entry.isNull("size")) "?" else entry.optLong("size")} bytes") {
                if (directory) list(path + name) else fileActions(path + name, false)
            }.apply {
                isEnabled = !busy
                setOnLongClickListener { fileActions(path + name, directory); true }
            })
        }
    }

    private fun fileActions(source: List<String>, directory: Boolean) {
        if (directory) {
            MaterialAlertDialogBuilder(context).setTitle(source.last()).setMessage("Delete this empty folder?")
                .setNegativeButton("Cancel", null).setPositiveButton("Delete") { _, _ ->
                    request(JSONObject().put("operation", "delete").put("path", JSONArray(source)))
                }.show()
            return
        }
        MaterialAlertDialogBuilder(context).setTitle(source.last())
            .setItems(arrayOf("Copy to device…", "Move to device…", "Copy within phone…", "Move within phone…", "Send via Bluetooth…", "Delete…")) { _, action ->
                when (action) {
                    0 -> export(source, false)
                    1 -> export(source, true)
                    2, 3 -> prompt(if (action == 2) "Copy within phone" else "Move within phone", "Full destination path, including file name") { value ->
                        val destination = value.split('/').filter { it.isNotEmpty() }
                        request(JSONObject().put("operation", "copy").put("source", JSONArray(source))
                            .put("destination", JSONArray(destination)).put("replace", false).put("move_source", action == 3))
                    }
                    4 -> bluetoothSend(source)
                    5 -> MaterialAlertDialogBuilder(context).setTitle("Delete ${source.last()}?")
                        .setMessage("This removes the item from the phone.")
                        .setNegativeButton("Cancel", null).setPositiveButton("Delete") { _, _ ->
                            request(JSONObject().put("operation", "delete").put("path", JSONArray(source)))
                        }.show()
                }
            }.show()
    }

    private fun prompt(title: String, hint: String, done: (String) -> Unit) {
        val field = EditText(context).apply { this.hint = hint; isSingleLine = true }
        MaterialAlertDialogBuilder(context).setTitle(title).setView(field)
            .setNegativeButton("Cancel", null).setPositiveButton("OK") { _, _ -> done(field.text.toString().trim()) }.show()
    }

    private fun button(label: String, action: () -> Unit) = MaterialButton(context, null,
        com.google.android.material.R.attr.materialButtonOutlinedStyle).apply {
        text = label; isAllCaps = false; textSize = 12f; setOnClickListener { action() }
    }
    private fun weighted() = LinearLayout.LayoutParams(0, dp(48), 1f)
    private fun dp(value: Int) = (value * context.resources.displayMetrics.density + 0.5f).toInt()
}
