package com.samsung.e830

import android.app.Application
import java.io.File
import java.io.IOException
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28])
class UserMidiSoundFontTest {
    private val context: Application get() = RuntimeEnvironment.getApplication()

    @Test
    fun copiesReplacesAndRemovesBank() {
        assertNull(UserMidiSoundFont.path(context))
        val original = soundFont(1)
        UserMidiSoundFont.install(context, original.inputStream())
        val installed = File(requireNotNull(UserMidiSoundFont.path(context)))
        assertArrayEquals(original, installed.readBytes())

        val replacement = soundFont(2)
        UserMidiSoundFont.install(context, replacement.inputStream())
        assertArrayEquals(replacement, installed.readBytes())

        UserMidiSoundFont.clear(context)
        assertNull(UserMidiSoundFont.path(context))
    }

    @Test
    fun rejectedImportLeavesExistingBankUntouched() {
        val original = soundFont(1)
        UserMidiSoundFont.install(context, original.inputStream())

        val wrongType = soundFont(2).apply { this[8] = 0 }
        assertThrows(IllegalArgumentException::class.java) { UserMidiSoundFont.install(context, wrongType.inputStream()) }
        val oversized = soundFont(2).apply { this[7] = 8 }
        assertThrows(IllegalArgumentException::class.java) { UserMidiSoundFont.install(context, oversized.inputStream()) }
        val truncated = soundFont(2).copyOf(23)
        assertThrows(IOException::class.java) { UserMidiSoundFont.install(context, truncated.inputStream()) }

        assertArrayEquals(original, File(requireNotNull(UserMidiSoundFont.path(context))).readBytes())
    }

    private fun soundFont(sample: Byte): ByteArray =
        "RIFF\u0010\u0000\u0000\u0000sfbkLIST\u0004\u0000\u0000\u0000INFO".encodeToByteArray().apply { this[23] = sample }
}
