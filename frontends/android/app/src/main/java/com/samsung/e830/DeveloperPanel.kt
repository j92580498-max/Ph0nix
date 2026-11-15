package com.samsung.e830

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Typeface
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import com.google.android.material.button.MaterialButton
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale

/** A live dialog keeps the phone running while its metrics and faults are inspected. */
internal class DeveloperPanel(private val context: Context, private val onClose: () -> Unit) {
    private var snapshot = JSONObject()
    private var tab = "Metrics"
    private val text = TextView(context).apply {
        setTextIsSelectable(true)
        typeface = Typeface.MONOSPACE
        textSize = 12f
        setPadding(dp(12), dp(12), dp(12), dp(12))
    }
    private val dialog: AlertDialog
    val isShowing get() = dialog.isShowing

    init {
        val layout = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
        val tabs = LinearLayout(context)
        listOf("Metrics", "Logs", "Errors").forEach { label ->
            tabs.addView(MaterialButton(context, null,
                com.google.android.material.R.attr.materialButtonOutlinedStyle).apply {
                this.text = label; isAllCaps = false; textSize = 12f
                setOnClickListener { tab = label; render() }
            }, LinearLayout.LayoutParams(0, dp(48), 1f))
        }
        layout.addView(tabs)
        layout.addView(ScrollView(context).apply { addView(text) },
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(340)))
        dialog = MaterialAlertDialogBuilder(context)
            .setTitle("ph0nix developer mode")
            .setView(layout)
            .setPositiveButton("Close") { _, _ -> }
            .setNeutralButton("Copy report", null)
            .create()
        dialog.setOnDismissListener { onClose() }
        dialog.setOnShowListener {
            dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setOnClickListener {
                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                clipboard.setPrimaryClip(ClipData.newPlainText("ph0nix diagnostics", snapshot.toString(2)))
                Toast.makeText(context, "Diagnostic report copied", Toast.LENGTH_SHORT).show()
            }
        }
    }

    fun show(value: JSONObject) { snapshot = value; render(); dialog.show() }
    fun update(value: JSONObject) { snapshot = value; if (isShowing) render() }
    fun dismiss() = dialog.dismiss()

    private fun render() {
        val content = when (tab) {
            "Logs" -> logs(snapshot.optJSONArray("logs"))
            "Errors" -> {
                val errors = snapshot.optJSONArray("error_logs")
                if (errors != null) logs(errors)
                else logs(snapshot.optJSONArray("logs"), onlyErrors = true)
            }
            else -> metrics()
        }
        if (text.text.toString() != content) text.text = content
    }

    private fun metrics() = buildString {
        appendLine("Presentation: ${number("presentation_fps")} FPS")
        appendLine("Firmware LCD: ${number("lcd_update_hz")} updates/s")
        appendLine("Guest speed: ${number("guest_speed")} × real time")
        appendLine("Polling: ${number("poll_hz")} Hz")
        appendLine("Missed deadlines: ${snapshot.optLong("missed_poll_deadlines")}")
        appendLine("CPU app / engine: ${number("app_cpu_percent")}% / ${number("engine_cpu_percent")}%")
        appendLine("Memory app / engine: ${mib("app_memory_bytes")} / ${mib("engine_memory_bytes")} MiB")
        appendLine("PC: 0x${snapshot.optLong("pc").toString(16).padStart(8, '0')}")
        appendLine("Instructions/s: ${number("instructions_per_second")}")
        appendLine()
        listOf("qmp_ms" to "QMP", "qmp_receive_ms" to "QMP receive", "qmp_decode_ms" to "QMP decoding",
            "lcd_status_ms" to "LCD status", "lcd_cached_ms" to "Cached LCD", "frame_decode_ms" to "LCD decoding", "render_ms" to "Rendering",
            "cpu_quantum_ms" to "CPU quantum", "audio_receive_ms" to "Audio receive").forEach { (key, label) ->
            snapshot.optJSONObject(key)?.let { timing ->
                if (timing.optLong("samples") == 0L) appendLine("$label: —")
                else {
                    appendLine("$label mean / p95 / max:")
                    appendLine("  ${format(timing.optDouble("mean_ms"))} / ${format(timing.optDouble("p95_ms"))} / ${format(timing.optDouble("max_ms"))} ms")
                }
            }
        }
        appendLine()
        appendLine("Audio frames: ${snapshot.optLong("audio_frames")}")
        appendLine("Audio dropped: ${snapshot.optLong("audio_dropped_frames")}")
        appendLine("Audio invalid: ${snapshot.optLong("audio_invalid_connections")}")
        snapshot.optJSONObject("engine_components")?.let { components ->
            appendLine()
            appendLine("Engine timers (sampled host costs):")
            for (name in components.keys()) {
                val component = components.optJSONObject(name) ?: continue
                appendLine("$name: ${component.optLong("service_calls")} calls, ${format(component.optDouble("service_mean_us"))} µs/call")
                appendLine("  schedule ${component.optLong("reschedules")}, unchanged ${component.optLong("unchanged_deadlines")}")
            }
        }
        appendLine()
        append("CPU is measured per process; 100% uses one core. Firmware update rate depends on the game.")
    }

    private fun logs(entries: JSONArray?, onlyErrors: Boolean = false): String = buildString {
        if (entries != null) for (index in 0 until entries.length()) {
            val entry = entries.optJSONObject(index) ?: continue
            val level = entry.optString("level")
            if (onlyErrors && level != "error" && level != "warn") continue
            appendLine("${entry.optLong("timestamp_ms")} ms [$level] ${entry.optString("component")}")
            appendLine(entry.optString("message"))
            appendLine()
        }
        if (isEmpty()) append(if (onlyErrors || tab == "Errors") "No errors recorded." else "No log entries yet.")
    }

    private fun number(key: String) = format(snapshot.optDouble(key, Double.NaN))
    private fun mib(key: String) = if (snapshot.isNull(key)) "—" else format(snapshot.optLong(key) / 1048576.0)
    private fun format(value: Double) = if (value.isFinite()) String.format(Locale.ROOT, "%.1f", value) else "—"
    private fun dp(value: Int) = (value * context.resources.displayMetrics.density + 0.5f).toInt()
}
