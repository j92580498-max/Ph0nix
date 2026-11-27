package com.samsung.e830

import android.graphics.Bitmap
import android.graphics.Canvas
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import android.os.Looper
import android.view.View
import android.widget.LinearLayout
import androidx.appcompat.app.AppCompatActivity
import androidx.appcompat.app.AppCompatDelegate
import androidx.appcompat.widget.SwitchCompat
import androidx.core.os.LocaleListCompat
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.ConscryptMode
import org.robolectric.annotation.GraphicsMode
import org.robolectric.shadows.ShadowDialog
import java.util.Locale

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 35])
@ConscryptMode(ConscryptMode.Mode.OFF)
@GraphicsMode(GraphicsMode.Mode.LEGACY)
class SettingsRenderingTest {
    @Test
    @Config(qualifiers = "en")
    fun languagePickerSwitchesBetweenEnglishRussianAndSystem() {
        Robolectric.buildActivity(LanguageTestActivity::class.java).setup().use { controller ->
            fun choose(index: Int) {
                controller.get().languageSetting().performClick()
                val dialog = ShadowDialog.getLatestDialog() as androidx.appcompat.app.AlertDialog
                dialog.listView.performItemClick(null, index, index.toLong())
                shadowOf(Looper.getMainLooper()).idle()
            }
            try {
                assertEquals("App language: System default", controller.get().languageSetting().text.toString())
                choose(2)
                assertEquals("ru", AppCompatDelegate.getApplicationLocales().toLanguageTags())
                controller.recreate()
                assertTrue(controller.get().languageSetting().text.endsWith("Русский"))
                if (Build.VERSION.SDK_INT < 33) {
                    assertEquals("Язык приложения: Русский", controller.get().languageSetting().text.toString())
                    assertEquals("1 ГиБ", controller.get().getString(R.string.size_gib, 1))
                }
                choose(1)
                assertEquals("en", AppCompatDelegate.getApplicationLocales().toLanguageTags())
                controller.recreate()
                assertEquals("App language: English", controller.get().languageSetting().text.toString())
                choose(0)
                assertTrue(AppCompatDelegate.getApplicationLocales().isEmpty)
                controller.recreate()
                assertEquals("App language: System default", controller.get().languageSetting().text.toString())
            } finally {
                AppCompatDelegate.setApplicationLocales(LocaleListCompat.getEmptyLocaleList())
            }
        }
    }

    @Test
    fun russianResourcesRenderFormattedLabels() {
        val activity = Robolectric.buildActivity(LanguageTestActivity::class.java).get()
        val localized = activity.createConfigurationContext(Configuration(activity.resources.configuration).apply {
            setLocale(Locale.forLanguageTag("ru"))
        })
        assertEquals("Настройки", localized.getString(R.string.settings))
        assertEquals("Язык приложения: Русский", localized.getString(R.string.language_setting, "Русский"))
        assertEquals("1 ГиБ", localized.getString(R.string.size_gib, 1))
        assertEquals("Работает K750i · 1,5 с", localized.getString(R.string.emulator_running, "K750i", 1.5))
    }

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

class LanguageTestActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        setTheme(R.style.Theme_SamsungE830)
        super.onCreate(savedInstanceState)
        setContentView(LinearLayout(this))
    }
}
