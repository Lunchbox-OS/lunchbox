package com.armeafamily.shepherd.companion.ble

/**
 * `u16` little-endian length-prefix framing, matching
 * `crates/shepherd-ble/src/framing.rs`.
 *
 * On the wire each logical frame is `[len: u16 LE][payload: len bytes]`,
 * fragmented across ATT writes/notifies. Writes are chunked by the
 * caller; notifies are reassembled by [FrameAssembler].
 */
object Framing {

    /** The largest frame the app will produce or accept. */
    const val MAX_FRAME_BYTES: Int = Protocol.MAX_FRAME_BYTES

    /**
     * Prepend the 2-byte length header to [payload], producing the bytes
     * to write to the Request characteristic (before MTU chunking).
     */
    fun encode(payload: ByteArray): ByteArray {
        require(payload.size <= 0xFFFF) { "frame too large: ${payload.size}" }
        val out = ByteArray(2 + payload.size)
        out[0] = (payload.size and 0xFF).toByte()
        out[1] = ((payload.size ushr 8) and 0xFF).toByte()
        payload.copyInto(out, destinationOffset = 2)
        return out
    }

    /**
     * Split a framed byte array into ATT-sized chunks. [chunkSize] should
     * be the negotiated `MTU - 3`; never below 20 (BLE 4.0 default).
     */
    fun chunk(framed: ByteArray, chunkSize: Int): List<ByteArray> {
        val size = chunkSize.coerceAtLeast(20)
        if (framed.size <= size) return listOf(framed)
        val chunks = ArrayList<ByteArray>((framed.size + size - 1) / size)
        var offset = 0
        while (offset < framed.size) {
            val end = minOf(offset + size, framed.size)
            chunks.add(framed.copyOfRange(offset, end))
            offset = end
        }
        return chunks
    }
}

/**
 * Reassembles logical frames from a stream of notification payloads.
 *
 * The server emits one or more notifications per frame; [push] appends
 * raw notification bytes and returns every complete frame that became
 * available. Not thread-safe; confine to a single collector coroutine.
 * [reset] on disconnect to drop any partial frame.
 */
class FrameAssembler(
    private val maxFrameBytes: Int = Framing.MAX_FRAME_BYTES,
) {
    // Backing buffer of bytes received but not yet sliced into frames.
    private var buffer = ByteArray(0)

    fun reset() {
        buffer = ByteArray(0)
    }

    /**
     * Append [data] and return any frames that are now complete.
     *
     * @throws FramingException if a length prefix exceeds [maxFrameBytes].
     */
    fun push(data: ByteArray): List<ByteArray> {
        if (data.isEmpty()) return emptyList()
        buffer += data

        val frames = ArrayList<ByteArray>()
        while (buffer.size >= 2) {
            val len = (buffer[0].toInt() and 0xFF) or ((buffer[1].toInt() and 0xFF) shl 8)
            if (len > maxFrameBytes) {
                throw FramingException("declared frame length $len exceeds cap $maxFrameBytes")
            }
            val total = 2 + len
            if (buffer.size < total) break // wait for more notifications
            frames.add(buffer.copyOfRange(2, total))
            buffer = buffer.copyOfRange(total, buffer.size)
        }
        return frames
    }
}

class FramingException(message: String) : Exception(message)
