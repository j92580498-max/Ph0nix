package com.samsung.e830

import java.io.IOException
import java.io.OutputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class DocumentExportTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test
    fun completeExportKeepsTheDocument() {
        val bytes = byteArrayOf(0, 10, 20, -1)
        val source = temporary.newFile().apply { writeBytes(bytes) }
        val destination = temporary.newFile()
        publishDocumentExport(source, destination::outputStream, destination::inputStream) { error("Discarded a complete export") }
        assertArrayEquals(bytes, destination.readBytes())
    }

    @Test
    fun emptyOrMissingSourceDiscardsDocumentWithoutOpeningIt() {
        for (source in listOf(temporary.newFile(), null)) {
            val destination = temporary.newFile()
            assertThrows(IOException::class.java) {
                publishDocumentExport(source, { error("Opened a document without a valid source") }, destination::inputStream) {
                    destination.delete()
                }
            }
            assertFalse(destination.exists())
        }
    }

    @Test
    fun failedSnapshotDiscardsThePickerDocumentWithoutOpeningIt() {
        val destination = temporary.newFile()
        assertThrows(IOException::class.java) {
            publishPreparedDocumentExport(
                prepareSource = { throw IOException("Card image is in use") },
                openDestination = { error("Opened a document without a snapshot") },
                readDestination = { error("Verified a document without a snapshot") },
                discardDestination = { destination.delete() },
            )
        }
        assertFalse(destination.exists())
    }

    @Test
    fun closeFailureDiscardsIncompleteDocumentAndPreservesTheSource() {
        val source = temporary.newFile().apply { writeText("memory backup") }
        val destination = temporary.newFile()
        val stream = destination.outputStream()
        val failing = object : OutputStream() {
            override fun write(value: Int) = stream.write(value)

            override fun close() {
                stream.close()
                throw IOException("Could not commit the document")
            }
        }
        assertThrows(IOException::class.java) {
            publishDocumentExport(source, { failing }, destination::inputStream) { destination.delete() }
        }
        assertFalse(destination.exists())
        assertTrue(source.length() > 0)
    }

    @Test
    fun anEmptyOrCorruptReadbackCannotReportSuccess() {
        val source = temporary.newFile().apply { writeBytes(byteArrayOf(1, 2, 3)) }
        for (saved in listOf(byteArrayOf(), byteArrayOf(1, 9, 3))) {
            val destination = temporary.newFile()
            assertThrows(IOException::class.java) {
                publishDocumentExport(source, destination::outputStream, { saved.inputStream() }) { destination.delete() }
            }
            assertFalse(destination.exists())
        }
    }
}
