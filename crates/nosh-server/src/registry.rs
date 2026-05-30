//! Session persistence substrate: sequenced output buffer, session registry,
//! and per-identity orphan lifecycle management.
//!
//! # Design decisions (from 05-CONTEXT.md)
//!
//! - **D-03**: A single `last_active: Instant` per slot drives both idle-timeout
//!   and LRU eviction.
//! - **D-04**: State machine: Active (client attached) → Orphaned (transport lost,
//!   PTY kept alive) → reaped (idle-timeout or shell-exit).
//! - **D-05**: Per-identity orphan cap, default 5.
//! - **D-06**: LRU eviction of the least-recently-active orphan (oldest `last_active`)
//!   when the cap is exceeded; the newly-orphaned session is always retained.
//! - **D-07**: Active sessions are NEVER evicted and never count toward the cap.
//! - **D-08**: Idle timeout, default 0 = disabled (Mosh behavior).
//! - **D-10**: Monotonic u64 sequence numbers on every outgoing PTY chunk.
//! - **D-11**: 64 KiB drop-oldest ring with truncation marker; newest chunk always
//!   survives.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use uuid::Uuid;

use crate::session::Session;
use nosh_auth::NoshPublicKey;

// ─── SequencedOutputBuffer (D-10 / D-11) ────────────────────────────────────

/// Default ring capacity: 64 KiB (D-11).
pub const DEFAULT_OUTPUT_BUFFER_BYTES: usize = 64 * 1024;

/// A monotonically-sequenced, bounded ring buffer of PTY output chunks.
///
/// Every outgoing PTY chunk is assigned a `u64` sequence number starting at 0.
/// On overflow the **oldest** chunks are dropped so the buffer never exceeds
/// `max_bytes`, but the **newest** chunk always survives (D-11 invariant).
/// A truncation marker records that bytes were lost and tracks the lowest
/// retained sequence number for Phase 6 replay logic.
pub struct SequencedOutputBuffer {
    next_seq: u64,
    ring: VecDeque<(u64, Bytes)>,
    total_bytes: usize,
    max_bytes: usize,
    /// Lowest sequence number still in the ring (only meaningful when
    /// `truncated == true`).
    lowest_retained_seq: u64,
    /// Set to `true` the first time an overflow forces a chunk to be dropped.
    truncated: bool,
}

impl SequencedOutputBuffer {
    /// Create a new buffer with the given byte capacity.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            next_seq: 0,
            ring: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
            lowest_retained_seq: 0,
            truncated: false,
        }
    }

    /// Push a chunk, assign it the next sequence number, enforce the byte cap,
    /// and return the assigned sequence number.
    ///
    /// CRITICAL (D-11): `ring.len() > 1` guard ensures the newest chunk always
    /// survives even if a single chunk exceeds `max_bytes`.
    pub fn push(&mut self, chunk: &[u8]) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.ring.push_back((seq, Bytes::copy_from_slice(chunk)));
        self.total_bytes += chunk.len();

        // Evict oldest entries until we are within budget.
        // The `ring.len() > 1` guard ensures the chunk we just pushed is never
        // the one evicted — the newest chunk always survives (D-11).
        while self.total_bytes > self.max_bytes && self.ring.len() > 1 {
            if let Some((_, old_chunk)) = self.ring.pop_front() {
                self.total_bytes -= old_chunk.len();
                self.truncated = true;
                // Update the lowest retained seq to the new front.
                if let Some(&(front_seq, _)) = self.ring.front() {
                    self.lowest_retained_seq = front_seq;
                }
            }
        }

        seq
    }

    /// The sequence number that will be assigned to the NEXT pushed chunk.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Total bytes currently retained in the ring.
    pub fn len_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Whether any chunks have been dropped due to overflow.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The sequence number of the oldest retained chunk (only meaningful when
    /// `truncated() == true`; is 0 when nothing has been dropped).
    pub fn lowest_retained_seq(&self) -> u64 {
        self.lowest_retained_seq
    }
}

impl Default for SequencedOutputBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_OUTPUT_BUFFER_BYTES)
    }
}

// ─── SlotState ──────────────────────────────────────────────────────────────

/// The lifecycle state of a session slot.
///
/// - `Active`: a client is currently attached and driving the PTY.
/// - `Orphaned`: the transport was lost; the PTY + shell continue running.
///   The slot is eligible for idle-timeout reaping (D-08) and LRU eviction
///   (D-06); an Active slot is NEVER eligible for either (D-07).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotState {
    Active,
    Orphaned,
}

// ─── SessionSlot ────────────────────────────────────────────────────────────

/// A slot decouples the PTY `Session` from any particular QUIC connection.
///
/// Uses `std::sync::Mutex` throughout (NOT `tokio::sync::Mutex`) — locks are
/// held only for brief field operations, never across `.await`
/// (ARCHITECTURE Anti-Pattern #2).
pub struct SessionSlot {
    pub identity: NoshPublicKey,
    pub session_id: Uuid,
    pub(crate) session: Mutex<Session>,
    output_buf: Mutex<SequencedOutputBuffer>,
    state: Mutex<SlotState>,
    last_active: Mutex<Instant>,
}

impl SessionSlot {
    /// Wrap a `Session` in a new Active slot.
    pub fn new(session: Session) -> Arc<SessionSlot> {
        let identity = session.identity.clone();
        let session_id = session.session_id;
        Arc::new(SessionSlot {
            identity,
            session_id,
            session: Mutex::new(session),
            output_buf: Mutex::new(SequencedOutputBuffer::default()),
            state: Mutex::new(SlotState::Active),
            last_active: Mutex::new(Instant::now()),
        })
    }

    /// Update `last_active` to now. Call cheaply while the client is attached
    /// (D-03). Coarse-grained updates are fine.
    pub fn touch(&self) {
        *self.last_active.lock().unwrap() = Instant::now();
    }

    /// Transition to `Orphaned` and freeze `last_active` at the current time
    /// (D-04). The PTY and shell continue running; the MasterPty stays open.
    pub fn mark_orphaned(&self) {
        *self.last_active.lock().unwrap() = Instant::now();
        *self.state.lock().unwrap() = SlotState::Orphaned;
    }

    /// Current lifecycle state.
    pub fn state(&self) -> SlotState {
        *self.state.lock().unwrap()
    }

    /// Snapshot of `last_active` (set at orphan time and frozen thereafter;
    /// updated by `touch()` while attached).
    pub fn last_active(&self) -> Instant {
        *self.last_active.lock().unwrap()
    }

    /// Assign a sequence number to `chunk` and append it to the output buffer.
    /// Returns the assigned sequence number (D-10).
    pub fn push_output(&self, chunk: &[u8]) -> u64 {
        self.output_buf.lock().unwrap().push(chunk)
    }

    /// Resize the PTY (delegates to `Session::resize`).
    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.session.lock().unwrap().resize(cols, rows)
    }

    /// Non-blocking check whether the shell child has exited. Returns the
    /// exit code if it has, `None` if it is still running.
    ///
    /// Uses `portable_pty::Child::try_wait` which is non-blocking.
    pub fn try_wait(&self) -> Option<i32> {
        let mut guard = self.session.lock().unwrap();
        if let Some(child) = guard.child_mut() {
            match child.try_wait() {
                Ok(Some(status)) => Some(status.exit_code() as i32),
                _ => None,
            }
        } else {
            // Child already taken (moved to wait_task) — treat as not yet exited.
            None
        }
    }

    /// SIGHUP the shell (best-effort).
    pub fn sighup(&self) {
        self.session.lock().unwrap().sighup();
    }
}

// ─── SessionRegistry ────────────────────────────────────────────────────────

/// Default per-identity orphan cap (D-05).
pub const DEFAULT_MAX_PER_IDENTITY: usize = 5;

/// Reaper polling interval (cadence is implementer's choice per CONTEXT).
pub const REAP_INTERVAL: Duration = Duration::from_secs(1);

/// The authoritative store of live and orphaned sessions.
///
/// Keyed by the SSH identity's raw 32-byte key (`NoshPublicKey::key32()`).
/// All mutex locks are held only for brief O(n) operations over per-identity
/// Vec entries — never across I/O or `.await` (Anti-Pattern #2).
pub struct SessionRegistry {
    inner: Mutex<HashMap<[u8; 32], Vec<Arc<SessionSlot>>>>,
    max_per_identity: usize,
    idle_timeout: Duration,
}

impl SessionRegistry {
    /// Create a registry.
    ///
    /// - `max_per_identity`: maximum ORPHANED sessions per identity (D-05).
    /// - `idle_timeout`: orphaned session idle timeout; `Duration::ZERO` =
    ///   disabled (D-08 default).
    pub fn new(max_per_identity: usize, idle_timeout: Duration) -> Arc<SessionRegistry> {
        Arc::new(SessionRegistry {
            inner: Mutex::new(HashMap::new()),
            max_per_identity,
            idle_timeout,
        })
    }

    /// Register a freshly-opened Active slot. Active slots do NOT count toward
    /// the orphan cap (D-07).
    pub fn register_active(&self, slot: Arc<SessionSlot>) {
        let key = *slot.identity.key32();
        self.inner
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(slot);
    }

    /// Transition `slot` to Orphaned and enforce the per-identity cap.
    ///
    /// If the number of Orphaned slots for this identity would exceed
    /// `max_per_identity`, the LEAST-recently-active orphan (oldest
    /// `last_active`, i.e. longest-orphaned) is evicted: SIGHUP'd and reaped.
    /// The just-orphaned slot (most-recently-active) is always retained (D-06).
    ///
    /// Active slots are NEVER evicted (D-07). A warning is logged on eviction
    /// so it is never silent (Pitfall #5).
    pub fn orphan(&self, slot: &Arc<SessionSlot>) {
        slot.mark_orphaned();

        let key = *slot.identity.key32();

        // Collect the victim (if any) under the lock, then reap after releasing.
        let victim: Option<Arc<SessionSlot>> = {
            let mut guard = self.inner.lock().unwrap();
            let slots = guard.entry(key).or_default();

            // Count orphans, excluding the slot we just orphaned (it's already
            // in the Vec as Active; mark_orphaned changed its state in-place).
            let orphan_count = slots
                .iter()
                .filter(|s| s.state() == SlotState::Orphaned)
                .count();

            if orphan_count > self.max_per_identity {
                // Find the least-recently-active orphan EXCLUDING the
                // just-orphaned slot (identified by session_id).
                let victim_idx = slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        s.state() == SlotState::Orphaned
                            && s.session_id != slot.session_id
                    })
                    .min_by_key(|(_, s)| s.last_active())
                    .map(|(i, _)| i);

                if let Some(idx) = victim_idx {
                    let v = slots.remove(idx);
                    tracing::warn!(
                        evicted_session_id = %v.session_id,
                        identity = %v.identity.fingerprint(),
                        "orphan cap exceeded; evicting least-recently-active orphan"
                    );
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        }; // lock released here

        // Reap victim outside the lock (Pitfall #5 / Anti-Pattern #2).
        if let Some(v) = victim {
            v.sighup();
        }
    }

    /// Remove a slot (shell exited or clean client close). Drops the
    /// `Arc<SessionSlot>`, which closes the MasterPty when no other holder
    /// remains.
    pub fn remove(&self, identity_raw: &[u8; 32], session_id: Uuid) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(slots) = guard.get_mut(identity_raw) {
            slots.retain(|s| s.session_id != session_id);
            if slots.is_empty() {
                guard.remove(identity_raw);
            }
        }
    }

    /// Remove a SPECIFIC slot instance (identity by `Arc` pointer identity, not
    /// just `session_id`). Idempotent: if the slot is already gone (e.g. evicted
    /// by the LRU cap, or reaped), this is a no-op.
    ///
    /// This is the event-driven exit path: the orphan-exit watcher spawned in
    /// `run_session` awaits the shell's `wait_task` and then calls this to drop
    /// the dead orphan's slot — releasing the `MasterPty` and freeing the
    /// per-identity cap slot. Keying on the `Arc` instance (via [`Arc::ptr_eq`])
    /// ensures that once Phase 6 reattach swaps a live connection back onto a
    /// slot under the same `session_id`, an exit watcher from a PRIOR orphan
    /// generation can never remove the freshly-reattached slot.
    pub fn remove_slot(&self, slot: &Arc<SessionSlot>) {
        let key = *slot.identity.key32();
        let mut guard = self.inner.lock().unwrap();
        if let Some(slots) = guard.get_mut(&key) {
            slots.retain(|s| !Arc::ptr_eq(s, slot));
            if slots.is_empty() {
                guard.remove(&key);
            }
        }
    }

    /// Number of ORPHANED slots for an identity (test seam).
    pub fn orphan_count(&self, identity_raw: &[u8; 32]) -> usize {
        self.inner
            .lock()
            .unwrap()
            .get(identity_raw)
            .map(|v| v.iter().filter(|s| s.state() == SlotState::Orphaned).count())
            .unwrap_or(0)
    }

    /// Total orphaned slots across all identities (test seam).
    pub fn total_orphans(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .values()
            .flat_map(|v| v.iter())
            .filter(|s| s.state() == SlotState::Orphaned)
            .count()
    }

    // ─── Reaper ─────────────────────────────────────────────────────────────

    /// Perform one reap pass: collect orphaned slots whose shell has exited or
    /// whose idle time exceeds `idle_timeout` (when > 0), then reap them.
    ///
    /// Victims are COLLECTED under the lock but SIGHUPed/dropped AFTER
    /// releasing it (Anti-Pattern #2: no blocking ops under the lock).
    ///
    /// EXIT-DETECTION OWNERSHIP (Phase 5 BLOCKER fix): in the production server
    /// path the shell `Child` is `take_child()`'d out of the `Session` into a
    /// dedicated `wait_task` BEFORE the slot is orphaned, so `slot.try_wait()`
    /// returns `None` for real orphans. Exited-orphan removal is therefore
    /// EVENT-DRIVEN: an exit-watcher task awaits `wait_task` and calls
    /// [`Self::remove_slot`] when the shell exits (see `run_session`). The
    /// `try_wait()` branch below is retained as a genuine BACKSTOP for any slot
    /// that still owns its child (e.g. unit tests, or future code paths that do
    /// not hand the child to a watcher); it is harmless when the child was taken.
    /// The reaper's load-bearing production duty is idle-timeout sweeping (D-08).
    pub fn reap_once(&self) {
        let now = Instant::now();

        // Collect victims under the lock.
        let victims: Vec<Arc<SessionSlot>> = {
            let mut guard = self.inner.lock().unwrap();
            let mut v = Vec::new();
            for slots in guard.values_mut() {
                slots.retain(|slot| {
                    if slot.state() != SlotState::Orphaned {
                        return true; // keep Active slots (D-07)
                    }
                    let shell_exited = slot.try_wait().is_some();
                    let idle_expired = self.idle_timeout > Duration::ZERO
                        && now.duration_since(slot.last_active()) >= self.idle_timeout;
                    if shell_exited || idle_expired {
                        v.push(slot.clone());
                        false // remove from the Vec
                    } else {
                        true // keep
                    }
                });
            }
            // Remove empty identity entries.
            guard.retain(|_, slots| !slots.is_empty());
            v
        }; // lock released here

        // Reap victims outside the lock.
        for slot in victims {
            // Idle-expired orphans need SIGHUP; exited-shell orphans are
            // already dead — sighup is a no-op on a dead pid (best effort).
            if self.idle_timeout > Duration::ZERO
                && now.duration_since(slot.last_active()) >= self.idle_timeout
            {
                tracing::info!(
                    session_id = %slot.session_id,
                    identity = %slot.identity.fingerprint(),
                    "reaping orphan: idle timeout expired"
                );
                slot.sighup();
            } else {
                tracing::info!(
                    session_id = %slot.session_id,
                    identity = %slot.identity.fingerprint(),
                    "reaping orphan: shell exited"
                );
            }
            // Drop the Arc — when the last reference drops, MasterPty closes.
            drop(slot);
        }
    }

    /// Spawn a background task that calls [`Self::reap_once`] every
    /// [`REAP_INTERVAL`].
    pub fn spawn_reaper(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let registry = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAP_INTERVAL).await;
                registry.reap_once();
            }
        })
    }
}

// ─── Session::child_mut seam ─────────────────────────────────────────────────
// We need a way to call `try_wait` on the child from the slot.
// Add `child_mut` to `Session` for non-blocking check.
// This is done via an extension in this module to avoid polluting session.rs
// with registry-specific concerns — but since we can't add methods from outside,
// we add the method to Session directly via the mod here and wire it in
// session.rs.
//
// The plan permits adding a thin `child_mut` method to session.rs.
// See the method below that calls `session.child_mut()`.
// We add `pub fn child_mut(&mut self) -> Option<&mut Box<dyn portable_pty::Child + Send + Sync>>`
// to session.rs in this task.

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SequencedOutputBuffer tests ──────────────────────────────────────────

    #[test]
    fn seq_starts_at_zero_and_increments() {
        let mut buf = SequencedOutputBuffer::new(1024);
        let s0 = buf.push(b"hello");
        let s1 = buf.push(b"world");
        assert_eq!(s0, 0, "first push must return seq 0");
        assert_eq!(s1, 1, "second push must return seq 1");
        assert_eq!(buf.next_seq(), 2, "next_seq must be 2 after two pushes");
    }

    #[test]
    fn stays_under_64kib_and_drops_oldest() {
        let mut buf = SequencedOutputBuffer::new(64 * 1024);
        let chunk = vec![0u8; 8 * 1024]; // 8 KiB
        // Push 10 x 8 KiB = 80 KiB — must overflow.
        for _ in 0..10 {
            buf.push(&chunk);
        }
        assert!(
            buf.len_bytes() <= 64 * 1024,
            "buffer must stay at or under 64 KiB, got {}",
            buf.len_bytes()
        );
        assert!(buf.truncated(), "overflow must set truncated");
    }

    #[test]
    fn newest_chunk_always_survives() {
        // Tiny max (4 bytes) to force overflow even with small chunks.
        let mut buf = SequencedOutputBuffer::new(4);
        buf.push(b"abc"); // fits
        let last_seq = buf.push(b"xyz"); // causes overflow of first chunk
        // The ring should contain only the newest chunk.
        let retained: Vec<_> = buf.ring.iter().map(|(s, _)| *s).collect();
        assert!(
            retained.contains(&last_seq),
            "newest chunk (seq {last_seq}) must survive; ring has seqs: {retained:?}"
        );
    }

    #[test]
    fn small_buffer_never_truncates() {
        let mut buf = SequencedOutputBuffer::new(256);
        buf.push(b"a");
        buf.push(b"bb");
        buf.push(b"ccc");
        assert!(!buf.truncated(), "no overflow → truncated must be false");
        assert_eq!(
            buf.lowest_retained_seq(),
            0,
            "lowest_retained_seq must remain 0 when nothing was dropped"
        );
    }

    #[test]
    fn truncation_marker_tracks_lowest_seq() {
        // Create a tiny buffer so we can precisely control overflow.
        let mut buf = SequencedOutputBuffer::new(10);
        // Push chunks until overflow.  Each chunk is 6 bytes; two fit (12>10 → overflow after 2nd).
        // Actually: 6 bytes fits. Push 6: total=6, ok. Push 6 again: total=12 > 10, pop oldest.
        let s0 = buf.push(b"aaaaaa"); // seq 0, 6 bytes
        let _s1 = buf.push(b"bbbbbb"); // seq 1, 6 bytes → overflow, drops seq 0
        assert!(buf.truncated(), "should have truncated after overflow");
        assert_ne!(
            buf.lowest_retained_seq(),
            s0,
            "seq 0 should have been evicted; lowest_retained should be > 0, got {}",
            buf.lowest_retained_seq()
        );
        // The lowest retained should be seq 1 (the only remaining chunk).
        let expected_lowest = _s1;
        assert_eq!(
            buf.lowest_retained_seq(),
            expected_lowest,
            "lowest_retained_seq should equal seq 1 ({expected_lowest})"
        );
    }

    // ── Helper: create a NoshPublicKey from a seed byte ──────────────────────
    fn test_key(seed: u8) -> NoshPublicKey {
        NoshPublicKey::from_raw([seed; 32])
    }

    /// Returns true if `/bin/sh` is available (guards shell-spawning tests).
    fn have_sh() -> bool {
        std::path::Path::new("/bin/sh").exists()
    }

    /// Open a real /bin/sh session for use in cap/reaper tests.
    fn open_sh_session(key: NoshPublicKey) -> crate::session::Session {
        use crate::session;
        let passwd = session::lookup_self(Some("/bin/sh"));
        let (sess, _reader, _writer) =
            session::open(&passwd, "xterm", 80, 24, &[], key).expect("open /bin/sh");
        sess
    }

    // ── SessionRegistry / SessionSlot cap+LRU tests ──────────────────────────

    #[test]
    fn cap_evicts_least_recently_active_orphan() {
        if !have_sh() {
            eprintln!("skipping cap_evicts_least_recently_active_orphan: /bin/sh unavailable");
            return;
        }

        let identity = test_key(0x01);
        let raw = *identity.key32();
        let registry = SessionRegistry::new(5, Duration::ZERO);

        // Create 6 slots and orphan them in order. The first one orphaned will
        // be the oldest (LRU candidate).
        let mut slots = Vec::new();
        for i in 0..6u8 {
            let sess = open_sh_session(test_key(0x01));
            let slot = SessionSlot::new(sess);
            registry.register_active(slot.clone());
            slots.push((i, slot));
        }

        // Orphan them in sequence, each a tiny bit "later" by manipulating
        // last_active via mark_orphaned (which sets last_active = Instant::now()).
        // We need the first orphaned to have a clearly older last_active than the
        // last one. Add brief sleeps via std::thread::sleep to spread timestamps.
        let mut orphaned_ids = Vec::new();
        for (_, slot) in &slots {
            std::thread::sleep(Duration::from_millis(5));
            registry.orphan(slot);
            orphaned_ids.push(slot.session_id);
        }

        // After orphaning 6 with cap=5, exactly 5 should remain.
        let count = registry.orphan_count(&raw);
        assert_eq!(count, 5, "orphan count must be exactly 5 (cap), got {count}");

        // The first-orphaned slot (oldest last_active) must be the one evicted.
        let evicted_id = orphaned_ids[0];
        let guard = registry.inner.lock().unwrap();
        let remaining: Vec<Uuid> = guard
            .get(&raw)
            .unwrap()
            .iter()
            .map(|s| s.session_id)
            .collect();
        assert!(
            !remaining.contains(&evicted_id),
            "the first-orphaned (oldest last_active) slot must have been evicted"
        );
        // The newest orphan (last in the list) must be retained.
        let newest_id = orphaned_ids[5];
        assert!(
            remaining.contains(&newest_id),
            "the newest orphan must be retained"
        );
        drop(guard);

        // Cleanup: sighup all remaining slots.
        for (_, slot) in &slots {
            slot.sighup();
        }
    }

    #[test]
    fn different_identities_are_independent() {
        if !have_sh() {
            eprintln!("skipping different_identities_are_independent: /bin/sh unavailable");
            return;
        }

        let registry = SessionRegistry::new(2, Duration::ZERO);

        // Create 3 slots for identity A and orphan them → 1 should be evicted.
        let key_a = test_key(0xAA);
        let raw_a = *key_a.key32();
        let key_b = test_key(0xBB);
        let raw_b = *key_b.key32();

        // Identity B: 1 orphan (under cap).
        let sess_b = open_sh_session(test_key(0xBB));
        let slot_b = SessionSlot::new(sess_b);
        registry.register_active(slot_b.clone());
        registry.orphan(&slot_b);
        assert_eq!(registry.orphan_count(&raw_b), 1);

        // Identity A: 3 orphans with cap=2 → should evict 1.
        let mut a_slots = Vec::new();
        for _ in 0..3 {
            let sess = open_sh_session(test_key(0xAA));
            let slot = SessionSlot::new(sess);
            registry.register_active(slot.clone());
            std::thread::sleep(Duration::from_millis(5));
            registry.orphan(&slot);
            a_slots.push(slot);
        }

        // Identity A's orphan count should be 2 (cap enforced).
        assert_eq!(
            registry.orphan_count(&raw_a),
            2,
            "identity A should have exactly 2 orphans"
        );
        // Identity B's orphan count must be UNCHANGED.
        assert_eq!(
            registry.orphan_count(&raw_b),
            1,
            "identity B's orphan count must not be affected by identity A's eviction"
        );

        // Cleanup.
        slot_b.sighup();
        for s in &a_slots {
            s.sighup();
        }
    }

    #[test]
    fn active_slot_never_evicted() {
        if !have_sh() {
            eprintln!("skipping active_slot_never_evicted: /bin/sh unavailable");
            return;
        }

        let registry = SessionRegistry::new(1, Duration::ZERO);
        let key = test_key(0x55);
        let raw = *key.key32();

        // One Active slot + two Orphaned → cap=1, so one orphan must be evicted.
        // The Active slot must NEVER be evicted (D-07).
        let sess_active = open_sh_session(test_key(0x55));
        let slot_active = SessionSlot::new(sess_active);
        registry.register_active(slot_active.clone());
        // slot_active remains Active.

        let sess_o1 = open_sh_session(test_key(0x55));
        let slot_o1 = SessionSlot::new(sess_o1);
        registry.register_active(slot_o1.clone());
        std::thread::sleep(Duration::from_millis(5));
        registry.orphan(&slot_o1);

        let sess_o2 = open_sh_session(test_key(0x55));
        let slot_o2 = SessionSlot::new(sess_o2);
        registry.register_active(slot_o2.clone());
        std::thread::sleep(Duration::from_millis(5));
        registry.orphan(&slot_o2);

        // After orphaning 2 with cap=1, exactly 1 orphan should remain.
        assert_eq!(registry.orphan_count(&raw), 1);

        // The Active slot must still be present.
        let guard = registry.inner.lock().unwrap();
        let slots = guard.get(&raw).unwrap();
        let active_present = slots
            .iter()
            .any(|s| s.session_id == slot_active.session_id && s.state() == SlotState::Active);
        assert!(active_present, "Active slot must never be evicted (D-07)");
        drop(guard);

        // Cleanup.
        slot_active.sighup();
        slot_o1.sighup();
        slot_o2.sighup();
    }

    // ── Reaper tests ─────────────────────────────────────────────────────────

    #[test]
    fn idle_timeout_zero_never_reaps_on_idle() {
        if !have_sh() {
            eprintln!("skipping idle_timeout_zero_never_reaps_on_idle: /bin/sh unavailable");
            return;
        }

        // idle_timeout = 0 → idle reaping disabled (D-08).
        let registry = SessionRegistry::new(5, Duration::ZERO);
        let key = test_key(0x10);
        let raw = *key.key32();

        let sess = open_sh_session(test_key(0x10));
        let slot = SessionSlot::new(sess);
        registry.register_active(slot.clone());
        registry.orphan(&slot);

        // Simulate "time passed" by setting last_active far in the past.
        *slot.last_active.lock().unwrap() =
            Instant::now() - Duration::from_secs(3600);

        // Reap pass — idle reaping must be a no-op (idle_timeout = 0).
        registry.reap_once();
        assert_eq!(
            registry.orphan_count(&raw),
            1,
            "idle_timeout=0 must not reap on idle; orphan should still be present"
        );

        slot.sighup();
    }

    #[test]
    fn finite_idle_timeout_reaps_old_orphan() {
        if !have_sh() {
            eprintln!("skipping finite_idle_timeout_reaps_old_orphan: /bin/sh unavailable");
            return;
        }

        // Very short timeout; we'll fake last_active to be way in the past.
        let registry = SessionRegistry::new(5, Duration::from_millis(10));
        let key = test_key(0x20);
        let raw = *key.key32();

        let sess = open_sh_session(test_key(0x20));
        let slot = SessionSlot::new(sess);
        registry.register_active(slot.clone());
        registry.orphan(&slot);

        // Force last_active well past the timeout.
        *slot.last_active.lock().unwrap() =
            Instant::now() - Duration::from_secs(10);

        registry.reap_once();
        assert_eq!(
            registry.orphan_count(&raw),
            0,
            "finite idle timeout: orphan idle > timeout must be reaped"
        );
    }

    /// Exercise the PRODUCTION ownership path: the shell child is TAKEN out of
    /// the Session (into a wait task) before the slot is orphaned — exactly as
    /// `server.rs::run_session` does. The event-driven exit watcher (mirroring
    /// the `TransportLost` arm) must remove the slot once the shell exits,
    /// releasing the MasterPty and freeing the cap slot.
    ///
    /// This test FAILS against the pre-fix code (which `drop(wait_task)`s and
    /// relies on the reaper's `try_wait`, permanently None for a taken child →
    /// orphan_count stays 1) and PASSES after the fix.
    #[tokio::test]
    async fn exited_orphan_removed_via_real_taken_child_path() {
        if !have_sh() {
            eprintln!("skipping exited_orphan_removed_via_real_taken_child_path: /bin/sh unavailable");
            return;
        }

        use crate::session;

        let registry = SessionRegistry::new(5, Duration::ZERO);
        let key = test_key(0x31);
        let raw = *key.key32();

        // Open a real /bin/sh and feed it a long `sleep` so the shell stays
        // alive (running orphan) until we explicitly make it exit. We keep the
        // writer alive so the shell's stdin does not hit EOF.
        let passwd = session::lookup_self(Some("/bin/sh"));
        let (mut sess, _reader, mut writer) =
            session::open(&passwd, "xterm", 80, 24, &[], test_key(0x31)).expect("open /bin/sh");
        use std::io::Write as _;
        writer.write_all(b"sleep 60\n").expect("write sleep");
        writer.flush().expect("flush");
        // PRODUCTION PATH: take the child into a wait task BEFORE orphaning, so
        // the slot's `try_wait()` is permanently None (child gone).
        let child = sess.take_child().expect("session has a child");
        let pid = sess.child_pid().expect("child pid");
        let slot = SessionSlot::new(sess);
        registry.register_active(slot.clone());
        registry.orphan(&slot);
        assert_eq!(registry.orphan_count(&raw), 1, "slot must be orphaned");

        // Spawn the exit watcher exactly as the server's TransportLost arm does.
        let wait_task = tokio::spawn(crate::session::wait_child(child));
        let watcher_registry = registry.clone();
        let watcher_slot = slot.clone();
        let watcher = tokio::spawn(async move {
            let _ = wait_task.await;
            watcher_registry.remove_slot(&watcher_slot);
        });

        // The orphaned shell is still running: it must NOT be removed yet.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            registry.orphan_count(&raw),
            1,
            "a still-RUNNING orphan must not be removed"
        );

        // Now make the orphaned shell exit (SIGHUP by pid — the child was taken
        // but the pid is still recorded on the Session, just like ClientClosed).
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGHUP,
        );

        // The watcher should observe the exit and remove the slot.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if registry.orphan_count(&raw) == 0 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "exited orphan was not removed within 5s; orphan_count={}",
                    registry.orphan_count(&raw)
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        watcher.await.unwrap();
        assert_eq!(
            registry.orphan_count(&raw),
            0,
            "exit watcher must remove the orphan whose shell has exited (no slot/MasterPty leak)"
        );
    }

    /// Backstop coverage: a slot that STILL OWNS its child (child not taken)
    /// is reaped by `reap_once()` via `try_wait` once its shell exits. This
    /// keeps the reaper's exit branch honest for any path that does not hand the
    /// child to a watcher.
    #[test]
    fn reaper_backstop_removes_exited_orphan_with_owned_child() {
        if !have_sh() {
            eprintln!("skipping reaper_backstop_removes_exited_orphan_with_owned_child: /bin/sh unavailable");
            return;
        }

        let registry = SessionRegistry::new(5, Duration::ZERO);
        let key = test_key(0x30);
        let raw = *key.key32();

        // Child is NOT taken — the slot retains it, so try_wait works.
        let slot = SessionSlot::new(open_sh_session(test_key(0x30)));
        registry.register_active(slot.clone());
        registry.orphan(&slot);

        // SIGHUP the shell so it exits.
        slot.sighup();

        // Poll until the shell actually exits (try_wait).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if slot.try_wait().is_some() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("shell did not exit within 5s after SIGHUP");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        registry.reap_once();
        assert_eq!(
            registry.orphan_count(&raw),
            0,
            "reaper backstop must remove orphan (owning its child) whose shell has exited"
        );
    }
}
