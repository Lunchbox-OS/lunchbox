//! Byte queue that backs the read-poll Response / Events characteristics.
//!
//! The companion app drains these characteristics with GATT reads instead
//! of subscribing for notifications. The notify path proved unreliable on
//! bonded reconnects: BlueZ caches CCCD state at the bond level, Android
//! short-circuits subsequent CCCD writes, and the server-side notify task
//! from the *first* session ends up the only subscriber the wire ever
//! reaches — every later reopen of the companion silently loses
//! responses. Read-poll sidesteps that machinery entirely.
//!
//! Each `Outbox` is a FIFO of length-prefixed framed messages plus a
//! "head offset" tracking how many bytes of the front message have
//! already been drained. Reads slice up to `max_chunk` bytes from the
//! head, advancing the offset; when the front message is exhausted it's
//! popped. Reads therefore never tear a frame across calls — bytes are
//! delivered in order, byte-for-byte, with no synchronisation needed on
//! the client beyond its existing length-prefix reassembler.

use std::collections::VecDeque;
use tokio::sync::Mutex;
use tracing::warn;

pub struct Outbox {
    name: &'static str,
    max_bytes: usize,
    state: Mutex<OutboxState>,
}

#[derive(Default)]
struct OutboxState {
    queue: VecDeque<Vec<u8>>,
    head_offset: usize,
    total_bytes: usize,
}

impl Outbox {
    pub fn new(name: &'static str, max_bytes: usize) -> Self {
        Self {
            name,
            max_bytes,
            state: Mutex::new(OutboxState::default()),
        }
    }

    /// Append one framed message. Oldest *whole* messages are evicted
    /// from the front to make room — never the in-progress head, since
    /// that would leave the client mid-frame with no way to resync.
    pub async fn push(&self, framed: Vec<u8>) {
        let mut s = self.state.lock().await;
        if framed.len() > self.max_bytes {
            warn!(
                outbox = self.name,
                len = framed.len(),
                cap = self.max_bytes,
                "frame exceeds outbox capacity; dropping",
            );
            return;
        }
        while s.total_bytes + framed.len() > self.max_bytes {
            // Don't drop the head if it's already mid-delivery — popping
            // it now would jump the client's byte stream forward and
            // desync the length-prefix reassembler. Wait until the
            // current head finishes draining.
            if s.head_offset > 0 {
                warn!(
                    outbox = self.name,
                    head_offset = s.head_offset,
                    "outbox full but head is mid-delivery; dropping incoming frame",
                );
                return;
            }
            let dropped = s
                .queue
                .pop_front()
                .expect("queue must be nonempty when over capacity");
            s.total_bytes -= dropped.len();
            warn!(
                outbox = self.name,
                dropped_len = dropped.len(),
                "outbox full; dropped oldest message",
            );
        }
        s.total_bytes += framed.len();
        s.queue.push_back(framed);
    }

    /// Drain up to `max_chunk` bytes from the head of the queue.
    /// Returns an empty vector when the queue is empty.
    pub async fn read(&self, max_chunk: usize) -> Vec<u8> {
        if max_chunk == 0 {
            return Vec::new();
        }
        let mut s = self.state.lock().await;
        let head_len = match s.queue.front() {
            Some(h) => h.len(),
            None => return Vec::new(),
        };
        let available = head_len - s.head_offset;
        let n = max_chunk.min(available);
        let chunk = s.queue.front().unwrap()[s.head_offset..s.head_offset + n].to_vec();
        s.head_offset += n;
        s.total_bytes -= n;
        if s.head_offset >= head_len {
            s.queue.pop_front();
            s.head_offset = 0;
        }
        chunk
    }

    /// Discard everything currently queued and any partially-delivered
    /// head. Used when the BLE peer disconnects — leaving a mid-frame
    /// head in place would desync the next session's reassembler.
    pub async fn clear(&self) {
        let mut s = self.state.lock().await;
        s.queue.clear();
        s.head_offset = 0;
        s.total_bytes = 0;
    }

    #[cfg(test)]
    pub async fn pending_bytes(&self) -> usize {
        self.state.lock().await.total_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_drains_in_chunks_then_pops() {
        let ob = Outbox::new("test", 1024);
        ob.push(vec![1, 2, 3, 4, 5]).await;
        ob.push(vec![10, 20]).await;

        // First read takes 3 bytes off the head.
        let a = ob.read(3).await;
        assert_eq!(a, vec![1, 2, 3]);
        assert_eq!(ob.pending_bytes().await, 4);

        // Second read takes the rest of the head (2 bytes), exhausting it.
        let b = ob.read(10).await;
        assert_eq!(b, vec![4, 5]);

        // Third read drains the second message.
        let c = ob.read(10).await;
        assert_eq!(c, vec![10, 20]);

        // Empty afterwards.
        let d = ob.read(10).await;
        assert!(d.is_empty());
    }

    #[tokio::test]
    async fn push_evicts_oldest_when_full() {
        let ob = Outbox::new("test", 4);
        ob.push(vec![1, 2]).await; // 2/4
        ob.push(vec![3, 4]).await; // 4/4
        ob.push(vec![5, 6]).await; // would be 6/4 → evict the first

        let mut drained = Vec::new();
        loop {
            let chunk = ob.read(10).await;
            if chunk.is_empty() {
                break;
            }
            drained.extend(chunk);
        }
        assert_eq!(drained, vec![3, 4, 5, 6]);
    }

    #[tokio::test]
    async fn push_dropped_when_oversize() {
        let ob = Outbox::new("test", 4);
        ob.push(vec![1; 10]).await; // dropped
        assert_eq!(ob.pending_bytes().await, 0);
    }

    #[tokio::test]
    async fn push_dropped_when_head_mid_delivery() {
        let ob = Outbox::new("test", 4);
        ob.push(vec![1, 2, 3, 4]).await; // 4/4
        let _ = ob.read(2).await; // head_offset = 2, total = 2
        // Adding a 4-byte frame would overflow — and we can't evict the
        // head because it's mid-delivery (popping it would jump the
        // client's byte stream forward and desync the reassembler), so
        // the new frame is dropped instead.
        ob.push(vec![5, 6, 7, 8]).await;
        let rest = ob.read(10).await;
        assert_eq!(rest, vec![3, 4]);
        assert!(ob.read(10).await.is_empty());
    }
}
