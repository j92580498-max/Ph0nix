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

/** A live dialog keeps the phone running while its metrics and faults are inspected. */
internal class DeveloperPanel(private val context: Context, private val onClose: () -> Unit) {
    private var snapshot = JSONObject()
    private var tab = R.string.diagnostics_metrics
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
        listOf(R.string.diagnostics_metrics, R.string.diagnostics_logs, R.string.diagnostics_errors).forEach { label ->
            tabs.addView(MaterialButton(context, null,
                com.google.android.material.R.attr.materialButtonOutlinedStyle).apply {
                setText(label); isAllCaps = false; textSize = 12f
                setOnClickListener { tab = label; render() }
            }, LinearLayout.LayoutParams(0, dp(48), 1f))
        }
        layout.addView(tabs)
        layout.addView(ScrollView(context).apply { addView(text) },
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(340)))
        dialog = MaterialAlertDialogBuilder(context)
            .setTitle(R.string.diagnostics_title)
            .setView(layout)
            .setPositiveButton(R.string.close) { _, _ -> }
            .setNeutralButton(R.string.diagnostics_copy, null)
            .create()
        dialog.setOnDismissListener { onClose() }
        dialog.setOnShowListener {
            dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setOnClickListener {
                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                clipboard.setPrimaryClip(ClipData.newPlainText(context.getString(R.string.diagnostics_report), snapshot.toString(2)))
                Toast.makeText(context, R.string.diagnostics_copied, Toast.LENGTH_SHORT).show()
            }
        }
    }

    fun show(value: JSONObject) { snapshot = value; render(); dialog.show() }
    fun update(value: JSONObject) { snapshot = value; if (isShowing) render() }
    fun dismiss() = dialog.dismiss()

    private fun render() {
        val content = when (tab) {
            R.string.diagnostics_logs -> logs(snapshot.optJSONArray("logs"))
            R.string.diagnostics_errors -> {
                val errors = snapshot.optJSONArray("error_logs")
                if (errors != null) logs(errors)
                else logs(snapshot.optJSONArray("logs"), onlyErrors = true)
            }
            else -> metrics()
        }
        if (text.text.toString() != content) text.text = content
    }

    private fun metrics() = buildString {
        appendLine(context.getString(R.string.metric_presentation, number("presentation_fps")))
        appendLine(context.getString(R.string.metric_lcd, number("lcd_update_hz")))
        appendLine(context.getString(R.string.metric_speed, number("guest_speed")))
        appendLine(context.getString(R.string.metric_poll, number("poll_hz")))
        appendLine(context.getString(R.string.metric_missed, snapshot.optLong("missed_poll_deadlines")))
        appendLine(context.getString(R.string.metric_cpu, number("app_cpu_percent"), number("engine_cpu_percent")))
        appendLine(context.getString(R.string.metric_memory, mib("app_memory_bytes"), mib("engine_memory_bytes")))
        appendLine("PC: 0x${snapshot.optLong("pc").toString(16).padStart(8, '0')}")
        appendLine(context.getString(R.string.metric_instructions, number("instructions_per_second")))
        appendLine()
        listOf("qmp_ms" to R.string.metric_qmp, "qmp_receive_ms" to R.string.metric_qmp_receive, "qmp_decode_ms" to R.string.metric_qmp_decode,
            "lcd_status_ms" to R.string.metric_lcd_status, "lcd_cached_ms" to R.string.metric_lcd_cached, "frame_decode_ms" to R.string.metric_lcd_decode, "render_ms" to R.string.metric_render,
            "cpu_quantum_ms" to R.string.metric_cpu_quantum, "audio_receive_ms" to R.string.metric_audio_receive).forEach { (key, labelId) ->
            val label = context.getString(labelId)
            snapshot.optJSONObject(key)?.let { timing ->
                if (timing.optLong("samples") == 0L) appendLine("$label: —")
                else {
                    appendLine(context.getString(R.string.metric_timings, label))
                    appendLine(context.getString(R.string.metric_timing_values, format(timing.optDouble("mean_ms")), format(timing.optDouble("p95_ms")), format(timing.optDouble("max_ms"))))
                }
            }
        }
        appendLine()
        appendLine(context.getString(R.string.metric_audio_frames, snapshot.optLong("audio_frames")))
        appendLine(context.getString(R.string.metric_audio_dropped, snapshot.optLong("audio_dropped_frames")))
        appendLine(context.getString(R.string.metric_audio_invalid, snapshot.optLong("audio_invalid_connections")))
        snapshot.optJSONObject("engine_components")?.let { components ->
            appendLine()
            appendLine(context.getString(R.string.metric_engine_timers))
            for (name in components.keys()) {
                val component = components.optJSONObject(name) ?: continue
                appendLine(context.getString(R.string.metric_engine_calls, name, component.optLong("service_calls"), format(component.optDouble("service_mean_us"))))
                appendLine(context.getString(R.string.metric_engine_schedule, component.optLong("reschedules"), component.optLong("unchanged_deadlines")))
            }
        }
        appendLine()
        append(context.getString(R.string.metric_description))
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
        if (isEmpty()) {
            append(context.getString(if (onlyErrors || tab == R.string.diagnostics_errors) R.string.diagnostics_no_errors else R.string.diagnostics_no_logs))
        }
    }

    private fun number(key: String) = format(snapshot.optDouble(key, Double.NaN))
    private fun mib(key: String) = if (snapshot.isNull(key)) "—" else format(snapshot.optLong(key) / 1048576.0)
    private fun format(value: Double) = if (value.isFinite()) String.format(context.resources.configuration.locales[0], "%.1f", value) else "—"
    private fun dp(value: Int) = (value * context.resources.displayMetrics.density + 0.5f).toInt()
}
