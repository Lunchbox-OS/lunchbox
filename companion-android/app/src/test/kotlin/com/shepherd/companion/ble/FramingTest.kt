package com.shepherd.companion.ble

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class FramingTest {

    @Test
    fun `encode prepends little-endian u16 length`() {
        val framed = Framing.encode(byteArrayOf(0xAA.toByte(), 0xBB.toByte()))
        // len = 2 -> 0x02 0x00, then the payload
        assertArrayEquals(byteArrayOf(0x02, 0x00, 0xAA.toByte(), 0xBB.toByte()), framed)
    }

    @Test
    fun `chunk splits into mtu-sized pieces and preserves bytes`() {
        val framed = ByteArray(50) { it.toByte() }
        val chunks = Framing.chunk(framed, chunkSize = 20)
        assertEquals(3, chunks.size)
        assertEquals(20, chunks[0].size)
        assertEquals(20, chunks[1].size)
        assertEquals(10, chunks[2].size)
        assertArrayEquals(framed, chunks.reduce { a, b -> a + b })
    }

    @Test
    fun `assembler reassembles a frame split across notifications`() {
        val payload = "hello world".encodeToByteArray()
        val framed = Framing.encode(payload)
        val assembler = FrameAssembler()

        // Feed it one byte at a time; only the last push yields the frame.
        val collected = mutableListOf<ByteArray>()
        for (i in framed.indices) {
            collected += assembler.push(byteArrayOf(framed[i]))
        }
        assertEquals(1, collected.size)
        assertArrayEquals(payload, collected.single())
    }

    @Test
    fun `assembler yields multiple frames from one push`() {
        val a = Framing.encode("one".encodeToByteArray())
        val b = Framing.encode("two".encodeToByteArray())
        val frames = FrameAssembler().push(a + b)
        assertEquals(2, frames.size)
        assertEquals("one", frames[0].decodeToString())
        assertEquals("two", frames[1].decodeToString())
    }

    @Test
    fun `assembler keeps a partial frame buffered`() {
        val framed = Framing.encode("abcd".encodeToByteArray())
        val assembler = FrameAssembler()
        assertTrue(assembler.push(framed.copyOfRange(0, 3)).isEmpty())
        val frames = assembler.push(framed.copyOfRange(3, framed.size))
        assertEquals("abcd", frames.single().decodeToString())
    }

    @Test
    fun `oversize length prefix is rejected`() {
        // Declare more bytes than the configured cap. We instantiate a
        // FrameAssembler with a deliberately tight cap so the test
        // doesn't have to allocate the full Protocol.MAX_FRAME_BYTES
        // (64 KiB) just to assert this guard.
        val bogus = byteArrayOf(0xFF.toByte(), 0xFF.toByte())
        assertThrows(FramingException::class.java) {
            FrameAssembler(maxFrameBytes = 1024).push(bogus)
        }
    }

    @Test
    fun `reset drops a partial frame`() {
        val framed = Framing.encode("xyz".encodeToByteArray())
        val assembler = FrameAssembler()
        assembler.push(framed.copyOfRange(0, 2))
        assembler.reset()
        // After reset the leftover header bytes are gone, so a fresh frame
        // parses cleanly.
        val frames = assembler.push(framed)
        assertEquals("xyz", frames.single().decodeToString())
    }
}
