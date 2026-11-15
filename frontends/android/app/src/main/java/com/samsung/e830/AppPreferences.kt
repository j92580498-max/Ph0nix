package com.samsung.e830

import android.content.Context

internal class AppPreferences(context: Context) {
    private val values = context.getSharedPreferences("featurephone", Context.MODE_PRIVATE)
    var showFps: Boolean
        get() = values.getBoolean("show_fps", true)
        set(value) { values.edit().putBoolean("show_fps", value).apply() }
    var sound: Boolean
        get() = values.getBoolean("sound", true)
        set(value) { values.edit().putBoolean("sound", value).apply() }
    var fullscreen: Boolean
        get() = values.getBoolean("fullscreen", false)
        set(value) { values.edit().putBoolean("fullscreen", value).apply() }
    var resume: Boolean
        get() = values.getBoolean("resume", true)
        set(value) { values.edit().putBoolean("resume", value).apply() }
    var keepAwake: Boolean
        get() = values.getBoolean("keep_awake", true)
        set(value) { values.edit().putBoolean("keep_awake", value).apply() }
    var grid: Boolean
        get() = values.getBoolean("grid", true)
        set(value) { values.edit().putBoolean("grid", value).apply() }
    var sortByDevice: Boolean
        get() = values.getBoolean("sort_by_device", false)
        set(value) { values.edit().putBoolean("sort_by_device", value).apply() }
}
