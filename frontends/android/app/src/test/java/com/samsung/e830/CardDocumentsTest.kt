package com.samsung.e830

import android.os.CancellationSignal
import android.system.ErrnoException
import android.system.OsConstants
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.IOException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28])
class CardDocumentsTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test
    fun syncPublishesCompleteFileBeforeCopyCanReportSuccess() {
        val staging = temporary.newFile()
        val installed = temporary.newFile().apply { writeText("previous") }
        var saves = 0
        val callback = CardWriteCallback(staging, truncate = true, signal = null, commit = {
            installed.writeBytes(staging.readBytes())
            saves++
        }, cleanup = { staging.delete() })
        val bytes = byteArrayOf(0, 7, -1, 42)
        try {
            callback.onWrite(0, 2, bytes.copyOfRange(0, 2))
            callback.onWrite(2, 2, bytes.copyOfRange(2, 4))
            assertEquals("previous", installed.readText())
            callback.onFsync()
            assertArrayEquals(bytes, installed.readBytes())
        } finally {
            callback.onRelease()
        }
        assertEquals(1, saves)
        assertTrue(!staging.exists())
    }

    @Test
    fun failedSyncIsReportedAndNotRetriedOnClose() {
        val staging = temporary.newFile()
        var attempts = 0
        val callback = CardWriteCallback(staging, truncate = true, signal = null, commit = {
            attempts++
            throw IOException("Card image is in use")
        }, cleanup = { staging.delete() })
        try {
            callback.onWrite(0, 1, byteArrayOf(1))
            assertEquals(OsConstants.EIO, assertThrows(ErrnoException::class.java) { callback.onFsync() }.errno)
        } finally {
            callback.onRelease()
        }
        assertEquals(1, attempts)
        assertTrue(!staging.exists())
    }

    @Test
    fun canceledOrOversizeWriteCannotPublishPartialData() {
        val signal = CancellationSignal()
        val staging = temporary.newFile()
        val callback = CardWriteCallback(staging, truncate = true, signal = signal,
            commit = { error("An incomplete write was saved") }, cleanup = { staging.delete() })
        try {
            callback.onWrite(0, 1, byteArrayOf(1))
            signal.cancel()
            assertEquals(OsConstants.ECANCELED, assertThrows(ErrnoException::class.java) { callback.onFsync() }.errno)
        } finally {
            callback.onRelease()
        }
        val large = temporary.newFile()
        val largeCallback = CardWriteCallback(large, truncate = true, signal = null,
            commit = { error("An oversize write was saved") }, cleanup = { large.delete() })
        try {
            assertEquals(OsConstants.EFBIG, assertThrows(ErrnoException::class.java) {
                largeCallback.onWrite(32L * 1024 * 1024, 1, byteArrayOf(1))
            }.errno)
        } finally {
            largeCallback.onRelease()
        }
        assertTrue(!staging.exists() && !large.exists())
    }

    @Test
    fun documentTreeCannotEscapeCardImageOrCrossToAnotherCard() {
        val provider = CardDocumentsProvider()
        val card = "cards/test.img"
        assertTrue(provider.isChildDocument(card, "$card/Music/song.mp3"))
        for (id in listOf("$card/../other.img/song.mp3", "$card/Music\\song.mp3", "cards/other.img/song.mp3", "$card/", card)) {
            assertTrue("Expected rejection: $id", !provider.isChildDocument(card, id))
        }
    }
}
