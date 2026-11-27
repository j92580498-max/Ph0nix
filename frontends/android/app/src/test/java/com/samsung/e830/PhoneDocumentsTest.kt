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
    fun documentsCannotCrossPhonesOrFollowAReplacementCard() {
        val provider = PhoneDocumentsProvider()
        val phone = "phones/b310d433-2f28-4efe-b6b7-195f175b8f8a"
        val other = "phones/4d97e4e1-b294-4a50-adc8-f6f484c3324b"
        val card = "$phone/card:original.img"
        assertTrue(provider.isChildDocument(phone, card))
        assertTrue(provider.isChildDocument(card, "$card/Music/song.mp3"))
        for (id in listOf("$card/../other.img/song.mp3", "$card/Music\\song.mp3",
            "$other/card:original.img/song.mp3", "$phone/card:replacement.img/song.mp3", "$card/", card)) {
            assertTrue("Expected rejection: $id", !provider.isChildDocument(card, id))
        }
    }

    @Test
    fun inboxOnlyImportsSyncedCopiesForTheSelectedPhone() {
        val phone = PhoneInbox.directory(temporary.newFolder("phone"))
        val other = PhoneInbox.directory(temporary.newFolder("other"))
        val item = PhoneInbox.create(phone, "song.mp3")
        val writer = PhoneInbox.begin(phone, item.id)
        val otherItem = PhoneInbox.create(other, "song.mp3")
        val token = byteArrayOf(9, 0, -5)
        val callback = DocumentWriteCallback(writer.staging, truncate = true, signal = null,
            commit = { PhoneInbox.publish(writer) }, cleanup = { PhoneInbox.finish(writer) }, sealAfterSync = true)
        try {
            callback.onWrite(0, token.size, token)
            assertTrue(PhoneInbox.ready(phone).isEmpty())
            assertTrue(PhoneInbox.ready(other).isEmpty())
            callback.onFsync()
            assertEquals(OsConstants.EPERM, assertThrows(ErrnoException::class.java) {
                callback.onWrite(0, 1, byteArrayOf(0))
            }.errno)
            assertEquals(item, PhoneInbox.claim(phone, emptySet()))
            assertEquals(null, PhoneInbox.claim(phone, emptySet()))
            assertThrows(IOException::class.java) { PhoneInbox.delete(phone, item.id) }
            assertThrows(IOException::class.java) { PhoneInbox.get(other, item.id) }
            assertArrayEquals(token, item.ready.readBytes())
            assertEquals("song (1).mp3", PhoneInbox.create(phone, "song.mp3").name)
            assertEquals("song.mp3", otherItem.name)
        } finally {
            callback.onRelease()
            PhoneInbox.release(item)
        }
        // A failed transfer merely releases the claim. A saved one removes the payload.
        assertEquals(item, PhoneInbox.claim(phone, emptySet()))
        assertTrue(item.ready.delete())
        PhoneInbox.release(item)
        assertTrue(!item.folder.exists())
    }

    @Test
    fun closingAnInboxWriteWithoutSyncDoesNotImportIt() {
        val phone = PhoneInbox.directory(temporary.newFolder())
        val writer = PhoneInbox.create(phone, "broken.mp3").let { PhoneInbox.begin(phone, it.id) }
        val callback = DocumentWriteCallback(writer.staging, truncate = true, signal = null,
            commit = { PhoneInbox.publish(writer) }, cleanup = { PhoneInbox.finish(writer) }, sealAfterSync = true)
        callback.onWrite(0, 1, byteArrayOf(0))
        callback.onRelease()
        assertTrue(PhoneInbox.ready(phone).isEmpty())
        assertTrue(!writer.folder.exists())
    }
}
