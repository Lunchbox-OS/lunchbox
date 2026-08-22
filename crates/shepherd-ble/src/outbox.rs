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
//!
//! # Backlog is the enemy
//!
//! Reads are capped at 512 bytes (Android's per-read ceiling) and cost a
//! GATT round trip each, so the drain rate is only ~10–20 KiB/s. Every
//! byte sitting here when a companion connects is time the companion
//! spends draining before it can talk — see
//! `docs/ai/history/2026-08-01 001 ble-connect-drain-unbounded.md`, where
//! an uncapped backlog of superseded snapshots stalled `connect()`
//! indefinitely. Two mechanisms keep it small: [`Outbox::push_coalesced`]
//! (a fresh `StateChanged` supersedes the queued one instead of queueing
//! behind it) and a deliberately tight capacity.

use std::collections::VecDeque;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Bytes drained in one continuous backlog before we log it at info.
/// Sized so an ordinary request/response never trips it but a genuine
/// reconnect backlog always does — that log line is the only server-side
/// evidence that a companion spent its connect budget draining.
const BACKLOG_LOG_THRESHOLD: usize = 8 * 1024;

/// Marks messages that supersede one another in the queue. Pushing under
/// a key discards any *queued* message with the same key, so a slow
/// reader sees the latest value rather than every intermediate one.
pub type CoalesceKey = &'static str;

/// Coalescing key for `StateChanged`: a full snapshot of service state,
/// which is idempotent — the newest one makes every older one redundant.
pub const COALESCE_STATE_CHANGED: CoalesceKey = "state_changed";

/// Coalesce key for `EventPayload::DiagnosticsChanged` (issue #143).
///
/// Qualifies for the same reason `StateChanged` does: the payload is the whole
/// diagnostic set rather than a raise/clear delta, so a newer one makes every
/// queued older one redundant. This is what keeps a probe sweep that flips
/// several conditions at once from queueing several frames the companion has to
/// drain before its first RPC.
pub const COALESCE_DIAGNOSTICS: CoalesceKey = "diagnostics_changed";

pub struct Outbox {
    name: &'static str,
    max_bytes: usize,
    state: Mutex<OutboxState>,
}

struct QueuedFrame {
    bytes: Vec<u8>,
    key: Option<CoalesceKey>,
}

#[derive(Default)]
struct OutboxState {
    queue: VecDeque<QueuedFrame>,
    head_offset: usize,
    total_bytes: usize,
    /// Bytes and reads spent on the current continuous backlog. Reset
    /// each time a read leaves the queue empty, so the backlog log below
    /// fires once per drain rather than once per read.
    drained_bytes: usize,
    drained_reads: usize,
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
        self.push_inner(framed, None).await;
    }

    /// Append one framed message, first discarding every *queued*
    /// message previously pushed under the same `key`.
    ///
    /// This is the backlog control for idempotent messages: a
    /// `StateChanged` snapshot supersedes the one before it, so a
    /// companion that reconnects after an hour of churn drains one
    /// current snapshot instead of hundreds of stale ones. Without it the
    /// events outbox sits pinned at capacity and the companion's
    /// post-connect drain can outlast any sane timeout.
    ///
    /// A mid-delivery head is never coalesced away — the client is
    /// already partway through those bytes, and dropping them would jump
    /// its byte stream forward and desync the reassembler.
    pub async fn push_coalesced(&self, framed: Vec<u8>, key: CoalesceKey) {
        self.push_inner(framed, Some(key)).await;
    }

    async fn push_inner(&self, framed: Vec<u8>, key: Option<CoalesceKey>) {
        let mut s = self.state.lock().await;
        // Reject an oversize frame *before* coalescing: superseding the
        // queued snapshot and then dropping its replacement would leave
        // the client with neither.
        if framed.len() > self.max_bytes {
            warn!(
                outbox = self.name,
                len = framed.len(),
                cap = self.max_bytes,
                "frame exceeds outbox capacity; dropping",
            );
            return;
        }
        if let Some(k) = key {
            let first_evictable = usize::from(s.head_offset > 0);
            let mut superseded = 0usize;
            let mut freed = 0usize;
            let mut i = first_evictable;
            while i < s.queue.len() {
                if s.queue[i].key == Some(k) {
                    let dropped = s.queue.remove(i).expect("index is in range");
                    s.total_bytes -= dropped.bytes.len();
                    freed += dropped.bytes.len();
                    superseded += 1;
                } else {
                    i += 1;
                }
            }
            if superseded > 0 {
                debug!(
                    outbox = self.name,
                    key = k,
                    superseded,
                    freed_bytes = freed,
                    "outbox coalesced superseded messages",
                );
            }
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
            s.total_bytes -= dropped.bytes.len();
            warn!(
                outbox = self.name,
                dropped_len = dropped.bytes.len(),
                "outbox full; dropped oldest message",
            );
        }
        s.total_bytes += framed.len();
        s.queue.push_back(QueuedFrame { bytes: framed, key });
    }

    /// Queued messages and undelivered bytes, for logging. Cheap enough
    /// to call on connect, which is the moment the depth actually
    /// predicts something: how long the companion's drain will take.
    pub async fn depth(&self) -> (usize, usize) {
        let s = self.state.lock().await;
        (s.queue.len(), s.total_bytes)
    }

    /// Drain up to `max_chunk` bytes from the head of the queue.
    /// Returns an empty vector when the queue is empty.
    pub async fn read(&self, max_chunk: usize) -> Vec<u8> {
        if max_chunk == 0 {
            return Vec::new();
        }
        let mut s = self.state.lock().await;
        let head_len = match s.queue.front() {
            Some(h) => h.bytes.len(),
            None => return Vec::new(),
        };
        let available = head_len - s.head_offset;
        let n = max_chunk.min(available);
        let chunk = s.queue.front().unwrap().bytes[s.head_offset..s.head_offset + n].to_vec();
        s.head_offset += n;
        s.total_bytes -= n;
        s.drained_bytes += n;
        s.drained_reads += 1;
        if s.head_offset >= head_len {
            s.queue.pop_front();
            s.head_offset = 0;
        }
        // A backlog just finished draining. This is the one server-side
        // signal that a companion spent real time here rather than
        // connecting instantly, so it's worth an info line — the failure
        // it diagnoses (connect stalling on the drain) is otherwise
        // completely silent in the journal.
        if s.queue.is_empty() {
            if s.drained_bytes >= BACKLOG_LOG_THRESHOLD {
                info!(
                    outbox = self.name,
                    bytes = s.drained_bytes,
                    reads = s.drained_reads,
                    "BLE outbox backlog drained",
                );
            }
            s.drained_bytes = 0;
            s.drained_reads = 0;
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
        s.drained_bytes = 0;
        s.drained_reads = 0;
    }

    /// Discard the queue, but only while the client is not partway
    /// through a frame. Returns what was dropped, or `None` if the head
    /// was mid-delivery and nothing was touched.
    ///
    /// This is what makes a *connect-time* wipe safe. Everything queued
    /// while nobody was connected is stale by definition — the companion
    /// discards its whole post-connect drain and then asks for a fresh
    /// `service_state` anyway — so shipping it costs a GATT round trip
    /// per 512 bytes and buys nothing. Measured on the reporter's box:
    /// ~4 KiB of queued snapshots turned a 1.5 s reconnect into 3.3 s.
    ///
    /// The alignment check is the safety. A peer that has already begun
    /// reading is holding a partial frame, and dropping it underneath
    /// would jump its byte stream forward mid-frame and desync the
    /// length-prefix reassembler — the same reason [`Outbox::push_inner`]
    /// spares a mid-delivery head. In that case we leave the queue alone
    /// and let the drain do its job.
    pub async fn clear_if_aligned(&self) -> Option<(usize, usize)> {
        let mut s = self.state.lock().await;
        if s.head_offset > 0 {
            return None;
        }
        let dropped = (s.queue.len(), s.total_bytes);
        s.queue.clear();
        s.total_bytes = 0;
        s.drained_bytes = 0;
        s.drained_reads = 0;
        Some(dropped)
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

    #[tokio::test]
    async fn push_coalesced_supersedes_queued_messages_with_the_same_key() {
        let ob = Outbox::new("test", 1024);
        ob.push_coalesced(vec![1, 1], COALESCE_STATE_CHANGED).await;
        ob.push(vec![9, 9]).await; // unkeyed: must survive
        ob.push_coalesced(vec![2, 2], COALESCE_STATE_CHANGED).await;
        ob.push_coalesced(vec![3, 3], COALESCE_STATE_CHANGED).await;

        // Only the unkeyed message and the newest snapshot remain, and
        // the newest is at the back — coalescing supersedes in place but
        // does not reorder what's left.
        assert_eq!(ob.pending_bytes().await, 4);
        let mut drained = Vec::new();
        loop {
            let chunk = ob.read(10).await;
            if chunk.is_empty() {
                break;
            }
            drained.extend(chunk);
        }
        assert_eq!(drained, vec![9, 9, 3, 3]);
    }

    #[tokio::test]
    async fn push_coalesced_spares_a_mid_delivery_head() {
        let ob = Outbox::new("test", 1024);
        ob.push_coalesced(vec![1, 2, 3, 4], COALESCE_STATE_CHANGED)
            .await;
        let _ = ob.read(2).await; // head is now mid-delivery

        // The client is partway through the head's bytes; superseding it
        // would jump their stream forward mid-frame. It stays, and the
        // newer snapshot queues behind it.
        ob.push_coalesced(vec![5, 6], COALESCE_STATE_CHANGED).await;
        assert_eq!(ob.read(10).await, vec![3, 4]);
        assert_eq!(ob.read(10).await, vec![5, 6]);
        assert!(ob.read(10).await.is_empty());
    }

    #[tokio::test]
    async fn clear_if_aligned_drops_a_stale_queue() {
        let ob = Outbox::new("test", 1024);
        ob.push(vec![1, 2, 3]).await;
        ob.push(vec![4, 5]).await;

        assert_eq!(ob.clear_if_aligned().await, Some((2, 5)));
        assert_eq!(ob.pending_bytes().await, 0);
        assert!(ob.read(10).await.is_empty());
    }

    /// The safety property: a client partway through a frame must not
    /// have the ground moved under it, or its reassembler desyncs.
    #[tokio::test]
    async fn clear_if_aligned_spares_a_mid_delivery_head() {
        let ob = Outbox::new("test", 1024);
        ob.push(vec![1, 2, 3, 4]).await;
        assert_eq!(ob.read(2).await, vec![1, 2]);

        assert_eq!(ob.clear_if_aligned().await, None);
        // The rest of the frame is still there, in order.
        assert_eq!(ob.read(10).await, vec![3, 4]);
    }

    #[tokio::test]
    async fn push_coalesced_keeps_the_queue_bounded_under_churn() {
        // The regression this whole mechanism exists for: snapshots
        // arriving while nobody is polling must not accumulate.
        let ob = Outbox::new("test", 64 * 1024);
        for i in 0..500u16 {
            ob.push_coalesced(i.to_le_bytes().to_vec(), COALESCE_STATE_CHANGED)
                .await;
        }
        assert_eq!(ob.pending_bytes().await, 2);
        assert_eq!(ob.read(10).await, 499u16.to_le_bytes().to_vec());
    }
}
