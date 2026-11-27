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
class PhoneDocumentsTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test
    fun syncPublishesCompleteFileBeforeCopyCanReportSuccess() {
        val staging = temporary.newFile()
        val installed = temporary.newFile().apply { writeText("previous") }
        var saves = 0
        val callback = DocumentWriteCallback(staging, truncate = true, signal = null, commit = {
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
        val callback = DocumentWriteCallback(staging, truncate = true, signal = null, commit = {
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
        val callback = DocumentWriteCallback(staging, truncate = true, signal = signal,
            commit = { error("An incomplete write was saved") }, cleanup = { staging.delete() })
        try {
            callback.onWrite(0, 1, byteArrayOf(1))
            signal.cancel()
            assertEquals(OsConstants.ECANCELED, assertThrows(ErrnoException::class.java) { callback.onFsync() }.errno)
        } finally {
            callback.onRelease()
        }
        val large = temporary.newFile()
        val largeCallback = DocumentWriteCallback(large, truncate = true, signal = null,
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
    fun documentsKeepEachCardUnderItsPhoneAndOutOfTheFatPath() {
        val provider = PhoneDocumentsProvider()
        val phone = "phones/b310d433-2f28-4efe-b6b7-195f175b8f8a"
        val other = "phones/4d97e4e1-b294-4a50-adc8-f6f484c3324b"
        val list = "$phone/card"
        val card = "$list/original.img"
        val second = "$list/replacement.img"
        val song = "$card/Music/song.mp3"
        assertTrue(provider.isChildDocument(phone, list))
        assertTrue(provider.isChildDocument(list, card))
        assertTrue(provider.isChildDocument(list, second))
        assertTrue(provider.isChildDocument(card, song))
        assertEquals(listOf("Music", "song.mp3"), StorageDocument.parse(song).fatPath)
        assertTrue(StorageDocument.parse(card).fatPath.isEmpty())
        assertThrows(IOException::class.java) { StorageDocument.parse("$phone/card:original.img") }
        for (id in listOf("$card/../other.img/song.mp3", "$card/Music\\song.mp3",
            "$other/card/original.img/song.mp3", "$second/song.mp3", "$card/", card)) {
            assertTrue("Expected rejection: $id", !provider.isChildDocument(card, id))
        }
    }

    @Test
    fun internalMemoryUsesTheSelectedPhoneAndFolder() {
        val provider = PhoneDocumentsProvider()
        val phone = "phones/b310d433-2f28-4efe-b6b7-195f175b8f8a"
        val other = "phones/4d97e4e1-b294-4a50-adc8-f6f484c3324b"
        val internal = "$phone/internal"
        val picture = "$internal/image/photo.jpg"
        val parsed = StorageDocument.parse(picture)
        assertTrue(parsed.isInternalEntry)
        assertEquals(listOf("image", "photo.jpg"), parsed.internalPath)
        assertTrue(provider.isChildDocument(internal, picture))
        assertTrue(!provider.isChildDocument(internal, "$other/internal/image/photo.jpg"))
        assertThrows(IOException::class.java) { StorageDocument.parse("$internal/../card") }
        assertThrows(IOException::class.java) { StorageDocument.parse("$phone/inbox") }
    }

    @Test
    fun selectedCardDocumentResolvesOnlyItsOwnImage() {
        val cards = temporary.newFolder("cards")
        val first = cards.resolve("first.img").apply { writeText("first") }
        val second = cards.resolve("second.IMG").apply { writeText("second") }
        cards.resolve(".import.tmp").writeText("copy is unfinished")
        cards.resolve("folder.img").mkdir()
        assertEquals(listOf(first, second), MemoryStickImages.list(cards))

        val chosen = StorageDocument.parse("phones/b310d433-2f28-4efe-b6b7-195f175b8f8a/card/second.IMG/Other/photo.jpg")
        val name = requireNotNull(chosen.cardName)
        assertEquals(second, MemoryStickImages.find(cards, name))
        assertTrue(second.delete())
        assertEquals(null, MemoryStickImages.find(cards, name))
        assertEquals(first, MemoryStickImages.list(cards).single())
    }
}
