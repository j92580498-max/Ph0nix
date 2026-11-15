package com.samsung.e830

import android.graphics.Bitmap
import android.graphics.Canvas
import android.view.View
import android.widget.LinearLayout
import androidx.appcompat.widget.SwitchCompat
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.ConscryptMode
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 35])
@ConscryptMode(ConscryptMode.Mode.OFF)
@GraphicsMode(GraphicsMode.Mode.LEGACY)
class SettingsRenderingTest {
    @Test
    fun settingsSwitchesRenderAndPersistInDayTheme() = checkSwitches()

    @Test
    @Config(qualifiers = "night")
    fun settingsSwitchesRenderAndPersistInNightTheme() = checkSwitches()

    private fun checkSwitches() {
        // Attach the real activity without starting its unrelated native phone inventory.
        val activity = Robolectric.buildActivity(MainActivity::class.java).get()
        activity.setTheme(R.style.Theme_SamsungE830)
        val preferences = AppPreferences(activity)
        val changes = mutableListOf<Boolean>()
        val change: (Boolean) -> Unit = { enabled ->
            preferences.sound = enabled
            changes += enabled
        }
        val method = MainActivity::class.java.getDeclaredMethod(
            "settingSwitch", String::class.java, Boolean::class.javaPrimitiveType,
            kotlin.jvm.functions.Function1::class.java,
        ).apply { isAccessible = true }
        val control = method.invoke(activity, "Sound", preferences.sound, change) as SwitchCompat
        val settings = LinearLayout(activity).apply { addView(control) }

        fun drawSettings() {
            settings.measure(
                View.MeasureSpec.makeMeasureSpec(1080, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(1920, View.MeasureSpec.AT_MOST),
            )
            settings.layout(0, 0, settings.measuredWidth, settings.measuredHeight)
            val bitmap = Bitmap.createBitmap(1080, 1920, Bitmap.Config.ARGB_8888)
            settings.draw(Canvas(bitmap))
            bitmap.recycle()
        }

        assertTrue(changes.isEmpty())
        drawSettings()
        control.performClick()
        drawSettings()
        assertEquals(listOf(false), changes)
        assertEquals(false, AppPreferences(activity).sound)
        control.performClick()
        drawSettings()
        assertEquals(listOf(false, true), changes)
        assertEquals(true, AppPreferences(activity).sound)
    }
}
