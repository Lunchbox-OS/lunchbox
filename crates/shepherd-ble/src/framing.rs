//! Length-prefix framing on top of ATT writes and reads.
//!
//! BLE writes are byte-stream chunks of up to ATT_MTU-3 bytes each, and
//! reads return up to ATT_MTU-1; the management protocol moves whole
//! JSON frames, which may exceed one chunk. This module wraps that
//! mismatch:
//!
//! - [`FrameReader`] buffers incoming chunks (write fragments or read
//!   replies) and yields complete `Vec<u8>` payloads once a `u16` LE
//!   length prefix worth of bytes has arrived.
//! - [`encode_frame`] prepends the length prefix to a payload. The
//!   read-poll outbox stores already-encoded frames so the read
//!   handler can slice across MTU boundaries cheaply.
//! - [`chunk_payload`] (test-only helper) is a small convenience for
//!   tests that want both steps in one call.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FramingError {
    #[error("frame length {0} exceeds the configured maximum {1}")]
    FrameTooLarge(usize, usize),
}

pub struct FrameReader {
    buf: Vec<u8>,
    max_frame_bytes: usize,
}

impl FrameReader {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_frame_bytes,
        }
    }

    /// Push an ATT chunk into the buffer.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Try to pull a complete frame off the front of the buffer.
    /// Returns `Ok(None)` when more bytes are needed,
    /// `Ok(Some(payload))` for a complete frame, and an error if a
    /// frame's advertised length exceeds the configured cap (the
    /// caller should drop the connection in that case rather than
    /// continue buffering attacker-controlled bytes).
    pub fn pop_frame(&mut self) -> Result<Option<Vec<u8>>, FramingError> {
        if self.buf.len() < 2 {
            return Ok(None);
        }
        let len = u16::from_le_bytes([self.buf[0], self.buf[1]]) as usize;
        if len > self.max_frame_bytes {
            return Err(FramingError::FrameTooLarge(len, self.max_frame_bytes));
        }
        if self.buf.len() < 2 + len {
            return Ok(None);
        }
        let payload = self.buf[2..2 + len].to_vec();
        self.buf.drain(..2 + len);
        Ok(Some(payload))
    }
}

/// Encode `payload` as a length-prefixed wire frame
/// (`[u16 LE len][payload]`). Used by the read-poll outbox: the outbox
/// stores already-framed messages so the read handler can slice across
/// MTU boundaries without re-computing the header.
pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() <= u16::MAX as usize,
        "payload exceeds u16 length prefix"
    );
    let mut buf = Vec::with_capacity(2 + payload.len());
    buf.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Test helper: encode `payload` as a length-prefixed wire frame, then
/// split it into fragments no larger than `chunk_size`.
#[cfg(test)]
fn chunk_payload(payload: &[u8], chunk_size: usize) -> Vec<Vec<u8>> {
    assert!(chunk_size > 0, "chunk_size must be > 0");
    encode_frame(payload)
        .chunks(chunk_size)
        .map(|c| c.to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_chunk_one_frame() {
        let mut r = FrameReader::new(1024);
        let mut wire = Vec::new();
        wire.extend_from_slice(&(3u16).to_le_bytes());
        wire.extend_from_slice(b"abc");
        r.push(&wire);
        assert_eq!(r.pop_frame().unwrap(), Some(b"abc".to_vec()));
        assert_eq!(r.pop_frame().unwrap(), None);
    }

    #[test]
    fn split_across_chunks() {
        let mut r = FrameReader::new(1024);
        let mut wire = Vec::new();
        wire.extend_from_slice(&(5u16).to_le_bytes());
        wire.extend_from_slice(b"hello");
        // Push one byte at a time.
        for byte in &wire {
            r.push(std::slice::from_ref(byte));
            // No complete frame until every byte has arrived.
        }
        assert_eq!(r.pop_frame().unwrap(), Some(b"hello".to_vec()));
    }

    #[test]
    fn multiple_frames_in_buffer() {
        let mut r = FrameReader::new(1024);
        let frames: [&[u8]; 3] = [b"one", b"twotwo", b"three!!"];
        for f in &frames {
            r.push(&(f.len() as u16).to_le_bytes());
            r.push(f);
        }
        for expected in frames {
            assert_eq!(r.pop_frame().unwrap().as_deref(), Some(expected));
        }
        assert_eq!(r.pop_frame().unwrap(), None);
    }

    #[test]
    fn length_only_no_payload_yet() {
        let mut r = FrameReader::new(1024);
        r.push(&(4u16).to_le_bytes());
        assert_eq!(r.pop_frame().unwrap(), None);
        r.push(b"data");
        assert_eq!(r.pop_frame().unwrap(), Some(b"data".to_vec()));
    }

    #[test]
    fn frame_over_max_errors() {
        let mut r = FrameReader::new(8);
        r.push(&(100u16).to_le_bytes());
        assert_eq!(
            r.pop_frame().unwrap_err(),
            FramingError::FrameTooLarge(100, 8)
        );
    }

    #[test]
    fn chunk_small_payload_single_fragment() {
        let frags = chunk_payload(b"abc", 64);
        assert_eq!(frags.len(), 1);
        // Round-trip through FrameReader.
        let mut r = FrameReader::new(64);
        r.push(&frags[0]);
        assert_eq!(r.pop_frame().unwrap(), Some(b"abc".to_vec()));
    }

    #[test]
    fn chunk_large_payload_multiple_fragments() {
        let payload = vec![0xABu8; 500];
        let frags = chunk_payload(&payload, 100);
        // 500 + 2 = 502 bytes total, in 100-byte chunks → 6 fragments.
        assert_eq!(frags.len(), 6);

        let mut r = FrameReader::new(1024);
        for f in &frags {
            r.push(f);
        }
        assert_eq!(r.pop_frame().unwrap(), Some(payload));
    }
}
