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
use crate::terminal::TerminalState;
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

    // ── Phase 6: replay and continuous ack trim ──────────────────────────────

    /// Return all buffered chunks with seq GREATER THAN OR EQUAL TO
    /// `last_acked_seq`, for replaying to a reconnecting client (D-09 / ROAM-02).
    ///
    /// CONVENTION (next-expected-seq, LOCKED — see `Message::Reattach`):
    /// `last_acked_seq` is the COUNT of chunks the client has applied, which —
    /// since seq is 0-based — equals the seq of the next chunk it expects (the
    /// lowest seq it has NOT yet applied). Replay is therefore INCLUSIVE: every
    /// chunk with `seq >= last_acked_seq` is replayed. A value of `0` ("applied
    /// nothing") replays everything from seq 0 (or from `lowest_retained_seq`
    /// if the ring was truncated and the requested resume point predates it).
    ///
    /// Returns `(chunks, replaying_from_seq, truncated_below_request)` where:
    /// - `chunks`: owned `(seq, Bytes)` in ascending seq order, no dup/gap
    ///   within the retained range.
    /// - `replaying_from_seq`: the sequence number of the first replayed chunk
    ///   (or `last_acked_seq` if the ring is empty / nothing to replay).
    /// - `truncated_below_request`: `true` when the client's requested resume
    ///   point fell below the buffer's lowest retained seq (cap dropped it).
    pub fn replay_from(&self, last_acked_seq: u64) -> (Vec<(u64, Bytes)>, u64, bool) {
        // Next-expected-seq: the client wants chunks starting AT last_acked_seq.
        let want_from = last_acked_seq;

        let (ring_front_seq, truncated_below_request) = if let Some(&(front_seq, _)) = self.ring.front() {
            let truncated = front_seq > want_from;
            (front_seq, truncated)
        } else {
            // Ring is empty — nothing to replay.
            return (Vec::new(), want_from, false);
        };

        let replaying_from_seq = if truncated_below_request {
            ring_front_seq
        } else {
            want_from
        };

        // Collect all chunks with seq >= last_acked_seq (ascending, no dup/gap
        // within the retained range — Pitfall #9). Inclusive boundary is what
        // makes the next-expected-seq convention exactly-once.
        let chunks: Vec<(u64, Bytes)> = self
            .ring
            .iter()
            .filter(|(seq, _)| *seq >= want_from)
            .map(|(seq, b)| (*seq, b.clone()))
            .collect();

        (chunks, replaying_from_seq, truncated_below_request)
    }

    /// Drop ring entries the client has already applied, freeing buffer space
    /// (D-08 continuous acking).
    ///
    /// CONVENTION (next-expected-seq, LOCKED — see `Message::Ack`): `acked_seq`
    /// is the COUNT of chunks the client has applied == the seq of the next
    /// chunk it expects. The chunks it has ALREADY applied are seqs
    /// `0..acked_seq`, so we drop every entry with `seq < acked_seq`. We MUST
    /// NEVER drop `seq >= acked_seq` — those are chunks the client has not yet
    /// applied and may still need on replay (the inflated-Ack silent-loss bug).
    ///
    /// This is NOT a data-loss truncation — the client already has the dropped
    /// bytes — so it MUST NOT set `self.truncated` and must NOT advance
    /// `lowest_retained_seq` in a way that would signal data loss to a future
    /// `replay_from` call. Only the cap-eviction path in `push()` sets
    /// `truncated`.
    pub fn trim_acked(&mut self, acked_seq: u64) {
        while let Some(&(seq, _)) = self.ring.front() {
            if seq < acked_seq {
                if let Some((_, chunk)) = self.ring.pop_front() {
                    self.total_bytes -= chunk.len();
                }
            } else {
                break;
            }
        }
        // DO NOT set self.truncated here — trim_acked removes bytes the client
        // has already consumed; it is not a data-loss event.
        // lowest_retained_seq is only meaningful when truncated == true (it
        // signals a gap to replay_from). We do not update it here to avoid
        // falsely signalling a data gap that was not caused by cap overflow.
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
/// - `Reconnecting`: a `Reattach` request has been accepted (the token matched
///   and the two-factor check passed) and the reattach session is in the process
///   of replaying output and rebinding the I/O pump. The slot is NOT eligible for
///   idle-timeout reaping or LRU eviction while in this state (it is mid-rebind).
///   A second `Reattach` for a `Reconnecting` slot is rejected (D-12 / IDENT-02).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotState {
    Active,
    Orphaned,
    /// Mid-reattach: token validated, I/O rebind in progress. Not reaped,
    /// not cap-counted (Reconnecting is intentionally excluded from orphan
    /// counts — it is neither idle nor eligible for eviction).
    Reconnecting,
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
    /// Server-side authoritative terminal state (SYNC-02).
    /// Lock order: always acquire `output_buf` lock before `terminal_state` lock.
    /// Never hold either lock across `.await` (Anti-Pattern #2).
    terminal_state: Mutex<TerminalState>,
    state: Mutex<SlotState>,
    last_active: Mutex<Instant>,
    /// Single-use CSPRNG reattach token (D-05). Rotated on every successful
    /// reattach. MUST NOT be logged — log only `identity.fingerprint()` (D-07).
    token: Mutex<[u8; 16]>,
    /// The PTY master writer. Taken by the pump on each attach; returned to the
    /// slot by the TransportLost path so a reattach pump can reclaim it.
    /// `None` while a pump is running (writer is in the blocking task).
    pub pty_writer: Mutex<Option<crate::session::PtyWriter>>,
}

impl SessionSlot {
    /// Wrap a `Session` in a new Active slot.
    ///
    /// Initializes the reattach token to a fresh CSPRNG uuid v4 (D-05).
    /// The PTY writer is stored as `None` initially; the caller (server) stores
    /// it via `pty_writer` after taking it from the session.
    /// The terminal state is initialized at 80×24 (conventional default); the
    /// resize path corrects dimensions when the client reports its actual size.
    pub fn new(session: Session) -> Arc<SessionSlot> {
        let identity = session.identity.clone();
        let session_id = session.session_id;
        Arc::new(SessionSlot {
            identity,
            session_id,
            session: Mutex::new(session),
            output_buf: Mutex::new(SequencedOutputBuffer::default()),
            terminal_state: Mutex::new(TerminalState::new(80, 24)),
            state: Mutex::new(SlotState::Active),
            last_active: Mutex::new(Instant::now()),
            token: Mutex::new(Uuid::new_v4().into_bytes()),
            pty_writer: Mutex::new(None),
        })
    }

    /// Take the PTY writer out of the slot for use by the I/O pump.
    /// Returns `None` if the writer is currently held by a pump (or never stored).
    pub fn take_pty_writer(&self) -> Option<crate::session::PtyWriter> {
        self.pty_writer.lock().unwrap().take()
    }

    /// Return the PTY writer to the slot (called by the TransportLost path so
    /// a future reattach pump can reclaim it).
    pub fn return_pty_writer(&self, w: crate::session::PtyWriter) {
        *self.pty_writer.lock().unwrap() = Some(w);
    }

    /// Clone a new PTY reader from the master (for the reattach pump).
    /// `portable_pty::MasterPty::try_clone_reader` is used; may fail if the PTY
    /// has already been closed (shell exited).
    pub fn clone_pty_reader(&self) -> anyhow::Result<crate::session::PtyReader> {
        self.session
            .lock()
            .unwrap()
            .try_clone_reader()
    }

    /// The PTY master fd as a raw integer (delegation to `Session::master_raw_fd`).
    /// Acquires the session lock briefly, copies the `i32` value, and releases the
    /// lock. The caller MUST NOT hold the lock across any `spawn_blocking` or async
    /// context (Pitfall 2). Used by the server pump to extract the fd for the
    /// interruptible reader in `pty_io`.
    #[cfg(unix)]
    pub fn master_raw_fd(&self) -> Option<i32> {
        self.session.lock().unwrap().master_raw_fd()
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

    /// Transition `Orphaned → Reconnecting` (D-12). Called inside `registry.reattach`
    /// while holding the registry lock, to make the transition atomic.
    /// A reconnecting slot is NOT reaped and NOT counted toward the orphan cap.
    pub fn mark_reconnecting(&self) {
        *self.state.lock().unwrap() = SlotState::Reconnecting;
    }

    /// Transition `Reconnecting → Active`. Called after replay completes and the
    /// I/O pump is rebound. Also refreshes `last_active` (D-03).
    pub fn mark_active(&self) {
        self.touch();
        *self.state.lock().unwrap() = SlotState::Active;
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

    /// Feed PTY output into BOTH the sequenced replay buffer AND the terminal state model.
    /// Returns the assigned sequence number from the replay buffer (identical to what
    /// `push_output` would have returned for the same chunk sequence — SYNC-02).
    ///
    /// # Lock order
    ///
    /// Acquires `output_buf` lock FIRST (seq assignment, replay path — never fails),
    /// then acquires `terminal_state` lock (advance — no error path; CSI/OSC parse
    /// errors are silently ignored). This ordering is critical for replay integrity:
    /// `SequencedOutputBuffer::push` MUST run before `TerminalState::advance` so that
    /// a hypothetical panic in `advance` (impossible today but guarded anyway) can
    /// never skip seq assignment. Both locks are held only for brief field mutations —
    /// NEVER across `.await` (Anti-Pattern #2, D-12-05 / Pitfall 8).
    pub fn push_output_and_parse(&self, chunk: &[u8]) -> u64 {
        let seq = self.output_buf.lock().unwrap().push(chunk);
        self.terminal_state.lock().unwrap().advance(chunk);
        seq
    }

    // ── Phase 6: token management ────────────────────────────────────────────

    /// Return the current reattach token (copy).
    ///
    /// CALLER CONTRACT: the returned bytes MUST NOT be logged. Log only
    /// `identity.fingerprint()` (D-07).
    pub fn token(&self) -> [u8; 16] {
        *self.token.lock().unwrap()
    }

    /// Generate a fresh CSPRNG token (uuid v4), store it, and return the new
    /// value. The previous token is immediately invalidated (single-use, D-05).
    ///
    /// CALLER CONTRACT: the returned bytes MUST NOT be logged (D-07).
    pub fn rotate_token(&self) -> [u8; 16] {
        let new_token = Uuid::new_v4().into_bytes();
        self.commit_token(new_token);
        new_token
    }

    /// Mint a fresh CSPRNG token candidate WITHOUT storing it (the prior token
    /// stays valid). Pair with [`Self::commit_token`] to rotate only AFTER the
    /// `ReattachOk` carrying this candidate is confirmed delivered (W1 fix):
    /// if the send fails, the slot keeps the prior token so the client's
    /// indefinite retry (D-10) can still succeed with the token it already
    /// holds. Otherwise a failed send would leave the slot holding a token the
    /// client never received, making the session permanently un-reattachable.
    ///
    /// CALLER CONTRACT: the returned bytes MUST NOT be logged (D-07).
    pub fn mint_token_candidate(&self) -> [u8; 16] {
        Uuid::new_v4().into_bytes()
    }

    /// Commit a previously-minted token candidate as the slot's live token,
    /// invalidating the prior one (single-use, D-05). Call this only after the
    /// `ReattachOk` carrying `new_token` has been successfully written/flushed.
    ///
    /// CALLER CONTRACT: `new_token` MUST NOT be logged (D-07).
    pub fn commit_token(&self, new_token: [u8; 16]) {
        *self.token.lock().unwrap() = new_token;
    }

    // ── Phase 6: replay and continuous-ack trim delegates ───────────────────

    /// Replay output starting just after `last_acked_seq`. Delegates to
    /// `SequencedOutputBuffer::replay_from`. Locks briefly; no `.await` under
    /// the lock (Anti-Pattern #2).
    ///
    /// Returns `(chunks, replaying_from_seq, truncated_below_request)`.
    pub fn replay_from(&self, last_acked_seq: u64) -> (Vec<(u64, Bytes)>, u64, bool) {
        self.output_buf.lock().unwrap().replay_from(last_acked_seq)
    }

    /// Trim buffered output the client has already applied (seq <= acked_seq).
    /// Does NOT set the truncation flag — only cap-overflow does that (D-08).
    /// Locks briefly; no `.await` under the lock (Anti-Pattern #2).
    pub fn trim_acked(&self, acked_seq: u64) {
        self.output_buf.lock().unwrap().trim_acked(acked_seq);
    }

    /// Resize the PTY and update the terminal state model dimensions (D-12-03: no reflow).
    ///
    /// Calls `Session::resize` (sends SIGWINCH to the shell) and also resizes the
    /// `TerminalState` grid to track the new dimensions. Returns the `Session::resize`
    /// result as the primary outcome; the terminal state resize is infallible.
    ///
    /// Lock discipline: acquires `session` lock then `terminal_state` lock sequentially
    /// (never held simultaneously). Neither lock is held across `.await` (Anti-Pattern #2).
    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        let result = self.session.lock().unwrap().resize(cols, rows);
        self.terminal_state.lock().unwrap().resize(cols, rows);
        result
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

// ─── ReattachReject ──────────────────────────────────────────────────────────

/// Internal-only granular rejection reason for `SessionRegistry::reattach`.
///
/// The SERVER collapses every variant to the single opaque `Message::ReattachErr`
/// wire frame (D-07 no-oracle). This type is `pub(crate)` so tests can assert
/// specific paths, but it is NEVER sent on the wire and NEVER exposed to clients.
///
/// All variants map to exactly the same wire response — the granularity here is
/// for operator-level tracing only, not for distinguishing responses to the client.
#[derive(Debug, PartialEq, Eq)]
pub enum ReattachReject {
    /// No matching slot for this (identity, token) pair. Covers both "unknown
    /// token" and "valid token presented under the wrong identity" — intentionally
    /// the same variant so there is no per-variant wire difference.
    NotFound,
    /// Belt-and-suspenders: token found in this identity's Vec but the slot's
    /// bound identity disagrees. Should never occur in correct code.
    IdentityMismatch,
    /// The matching slot is not Orphaned (it is Active or Reconnecting, D-12).
    NotOrphaned,
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

    // ── Phase 6: two-factor reattach ─────────────────────────────────────────

    /// Two-factor reattach: atomically validate the token + identity and
    /// transition the matching orphaned slot to `Reconnecting` (D-12 / IDENT-02).
    ///
    /// Both factors are required:
    /// 1. **Token**: the presented `token` must match a slot in the registry.
    /// 2. **Identity**: the slot must live in the `identity`'s per-identity Vec
    ///    (i.e. the TLS-authenticated identity must equal the slot's bound
    ///    identity). A valid token presented under the WRONG identity simply will
    ///    not be found in that identity's Vec — same `NotFound` result, no oracle.
    ///
    /// Errors:
    /// - `NotFound` — no matching slot (covers wrong token AND wrong identity,
    ///   intentionally indistinguishable for the no-oracle invariant D-07).
    /// - `IdentityMismatch` — belt-and-suspenders: token was found in the
    ///   identity's Vec but the slot identity disagrees (should never happen).
    /// - `NotOrphaned` — the session is Active or Reconnecting (D-12 mutual
    ///   exclusion: prevents two-clients-one-session race, Pitfall #10).
    ///
    /// On success, the slot transitions `Orphaned → Reconnecting` ATOMICALLY
    /// under the registry lock, and the same `Arc<SessionSlot>` instance is
    /// returned (never a new slot — the orphan-exit watcher's `Arc::ptr_eq`
    /// check remains valid, plan 06-03 critical safety point).
    ///
    /// The caller (server) is responsible for:
    /// - Calling `slot.rotate_token()` to mint the `ReattachOk.new_token`.
    /// - Calling `slot.mark_active()` after replay + I/O rebind.
    ///
    /// LOGGING: log only `identity.fingerprint()` and an outcome; NEVER log
    /// the token (D-07).
    pub fn reattach(
        &self,
        token: &[u8; 16],
        identity: &NoshPublicKey,
    ) -> Result<Arc<SessionSlot>, ReattachReject> {
        let key = *identity.key32();

        let slot = {
            let mut guard = self.inner.lock().unwrap();
            let slots = match guard.get_mut(&key) {
                Some(v) => v,
                None => {
                    // This identity has no sessions at all.
                    return Err(ReattachReject::NotFound);
                }
            };

            // Find the first slot in this identity's Vec whose token matches.
            // Because we scoped the lookup to this identity's Vec, a valid token
            // presented under a DIFFERENT identity simply won't be found here →
            // NotFound (same path as a bad token — no oracle, D-07).
            let slot = match slots.iter().find(|s| s.token() == *token) {
                Some(s) => s.clone(),
                None => return Err(ReattachReject::NotFound),
            };

            // Belt-and-suspenders: the slot lives in this identity's Vec so its
            // identity MUST equal `identity`. Verify explicitly.
            if slot.identity != *identity {
                return Err(ReattachReject::IdentityMismatch);
            }

            // D-12 mutual exclusion: only Orphaned slots may be reattached.
            // Active → the old client is still there.
            // Reconnecting → another reattach attempt is in progress (race).
            let state = slot.state();
            if state != SlotState::Orphaned {
                return Err(ReattachReject::NotOrphaned);
            }

            // Transition Orphaned → Reconnecting atomically while still under
            // the registry lock (D-12 atomicity requirement).
            slot.mark_reconnecting();

            slot
        }; // registry lock released here

        tracing::info!(
            identity = %identity.fingerprint(),
            "reattach accepted"
        );
        Ok(slot)
    }

    // ── End Phase 6 reattach ─────────────────────────────────────────────────

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
                        // Keep Active and Reconnecting slots (D-07 / D-12).
                        // Reconnecting is intentionally not reaped: it is mid-rebind,
                        // not idle.
                        return true;
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

    // ── Phase 6: replay, trim, token, reattach, mutual-exclusion tests ───────

    #[test]
    fn replay_from_returns_only_unacked_in_order() {
        let mut buf = SequencedOutputBuffer::new(64 * 1024);
        // Push 5 chunks → seqs 0..4
        for i in 0u8..5 {
            buf.push(&[i; 10]);
        }
        // Next-expected-seq convention: replay_from(3) means "client applied 3
        // chunks (seqs 0,1,2), next expected is seq 3" → replay seqs 3 and 4.
        let (chunks, replaying_from_seq, truncated) = buf.replay_from(3);
        let seqs: Vec<u64> = chunks.iter().map(|(s, _)| *s).collect();
        assert_eq!(seqs, vec![3, 4], "should replay seqs >= 3 (inclusive)");
        assert_eq!(replaying_from_seq, 3, "replaying_from_seq should equal next-expected");
        assert!(!truncated, "no truncation in a non-overflowed buffer");

        // replay_from(0) ("applied nothing") replays EVERYTHING from seq 0.
        let (all, from0, _) = buf.replay_from(0);
        let all_seqs: Vec<u64> = all.iter().map(|(s, _)| *s).collect();
        assert_eq!(all_seqs, vec![0, 1, 2, 3, 4], "replay_from(0) replays all");
        assert_eq!(from0, 0, "replaying_from_seq for applied-nothing is 0");

        // replay_from(5) (applied all 5) replays nothing — next expected past end.
        let (none, from5, _) = buf.replay_from(5);
        assert!(none.is_empty(), "applied-all replays nothing");
        assert_eq!(from5, 5);
    }

    #[test]
    fn replay_from_signals_truncation_when_request_predates_ring() {
        // Tiny buffer → overflow early.
        // We need the ring's front to be STRICTLY GREATER than want_from = last_acked_seq + 1.
        // Use a very tiny buffer (6 bytes) so every new push evicts the previous.
        let mut buf = SequencedOutputBuffer::new(6);
        // Push 4 x 6-byte chunks. After each push only 1 fits.
        let _s0 = buf.push(b"aaaaaa"); // seq 0
        let _s1 = buf.push(b"bbbbbb"); // seq 1 → overflow drops seq 0; ring=[1]
        let _s2 = buf.push(b"cccccc"); // seq 2 → overflow drops seq 1; ring=[2]
        let _s3 = buf.push(b"dddddd"); // seq 3 → overflow drops seq 2; ring=[3]

        assert!(buf.truncated(), "buffer must be truncated after overflow");
        let lowest = buf.lowest_retained_seq();
        assert_eq!(lowest, 3, "only seq 3 should be in the ring");

        // Client applied nothing (last_acked_seq=0, equivalent to "client applied seq 0
        // but wants seq 1+"). Ring starts at 3, so want_from=1 < ring_front=3 → truncation.
        let (chunks, replaying_from_seq, truncated_below_request) = buf.replay_from(0);
        assert!(truncated_below_request, "must signal truncation when request predates ring front");
        assert_eq!(
            replaying_from_seq, lowest,
            "replaying_from_seq must be the lowest retained seq on truncation"
        );
        // All remaining chunks (just seq 3) should be returned.
        assert!(!chunks.is_empty(), "must return remaining chunks");
        // All returned seqs must be > 0 (the acked seq).
        assert!(chunks.iter().all(|(s, _)| *s > 0), "all replayed seqs must be > last_acked");
    }

    #[test]
    fn trim_acked_drops_acked_and_keeps_unacked_and_does_not_truncate() {
        let mut buf = SequencedOutputBuffer::new(64 * 1024);
        // Push 6 chunks: seqs 0..5.
        for i in 0u8..6 {
            buf.push(&[i; 20]);
        }
        let bytes_before = buf.len_bytes();
        assert_eq!(bytes_before, 6 * 20, "should have 6*20 bytes before trim");

        // Next-expected-seq convention: trim_acked(3) means "client applied 3
        // chunks (seqs 0,1,2), next expected is seq 3" → drop seqs < 3.
        buf.trim_acked(3);

        // Should have dropped 3 chunks (seqs 0, 1, 2) = 60 bytes.
        assert_eq!(buf.len_bytes(), 3 * 20, "should have 3 chunks left after trim_acked(3)");
        // truncated must remain false (trim_acked is NOT a data-loss event).
        assert!(!buf.truncated(), "trim_acked must NOT set truncated flag");

        // CRITICAL (silent-loss guard): trim_acked must NEVER drop a chunk the
        // client has not applied. The next-expected chunk (seq 3) must survive.
        let (chunks, replaying_from, trunc) = buf.replay_from(3);
        let seqs: Vec<u64> = chunks.iter().map(|(s, _)| *s).collect();
        assert_eq!(seqs, vec![3, 4, 5], "replay after trim must return remaining seqs (no drop)");
        assert_eq!(replaying_from, 3, "next-expected chunk must still be replayable, not trimmed");
        assert!(!trunc, "no truncation after trim (trim is not overflow)");
    }

    #[test]
    fn rotate_token_changes_token() {
        if !have_sh() {
            eprintln!("skipping rotate_token_changes_token: /bin/sh unavailable");
            return;
        }
        let sess = open_sh_session(test_key(0xF1));
        let slot = SessionSlot::new(sess);
        let original = slot.token();
        let rotated = slot.rotate_token();
        assert_ne!(original, rotated, "rotate_token must produce a different token");
        let stored = slot.token();
        assert_eq!(rotated, stored, "stored token must equal the rotated token");
        slot.sighup();
    }

    #[test]
    fn reattach_matches_token_within_identity() {
        if !have_sh() {
            eprintln!("skipping reattach_matches_token_within_identity: /bin/sh unavailable");
            return;
        }
        let identity = test_key(0xA1);
        let raw = *identity.key32();
        let registry = SessionRegistry::new(5, Duration::ZERO);

        let sess = open_sh_session(test_key(0xA1));
        let slot = SessionSlot::new(sess);
        registry.register_active(slot.clone());
        // Orphan the slot so it's eligible for reattach.
        registry.orphan(&slot);
        assert_eq!(registry.orphan_count(&raw), 1);

        let token = slot.token();

        // reattach should succeed and return the SAME Arc instance.
        let result = registry.reattach(&token, &identity);
        assert!(result.is_ok(), "reattach must succeed with correct token and identity");
        let reattached = result.unwrap();

        // CRITICAL: must be the SAME Arc instance (Arc::ptr_eq check).
        assert!(
            Arc::ptr_eq(&reattached, &slot),
            "reattach must return the SAME Arc instance (not a new slot)"
        );
        // Slot must now be Reconnecting.
        assert_eq!(reattached.state(), SlotState::Reconnecting, "slot must be Reconnecting after reattach");

        // Cleanup.
        slot.sighup();
    }

    #[test]
    fn reattach_wrong_identity_is_notfound() {
        if !have_sh() {
            eprintln!("skipping reattach_wrong_identity_is_notfound: /bin/sh unavailable");
            return;
        }
        let identity_a = test_key(0xA2);
        let identity_b = test_key(0xB2);
        let registry = SessionRegistry::new(5, Duration::ZERO);

        // Register and orphan a slot for identity A.
        let sess = open_sh_session(test_key(0xA2));
        let slot = SessionSlot::new(sess);
        registry.register_active(slot.clone());
        registry.orphan(&slot);
        let token_a = slot.token();

        // Case 1: valid token for A, presented under identity B → NotFound (no oracle).
        let r1 = registry.reattach(&token_a, &identity_b);
        assert!(r1.is_err(), "valid token + wrong identity must be rejected");

        // Case 2: bogus token, correct identity A → NotFound.
        let bogus_token = [0xFFu8; 16];
        let r2 = registry.reattach(&bogus_token, &identity_a);
        assert!(r2.is_err(), "bogus token + correct identity must be rejected");

        // NO-ORACLE ASSERTION: both rejections are Err (indistinguishable at the
        // wire level — both map to the same opaque ReattachErr variant).
        // We assert both are Err without inspecting the variant value
        // (though for internal completeness, both are NotFound).
        let is_err_1 = r1.is_err();
        let is_err_2 = r2.is_err();
        assert_eq!(
            is_err_1, is_err_2,
            "both rejection types must produce the same Err result (no oracle)"
        );

        slot.sighup();
    }

    #[test]
    fn reattach_active_or_reconnecting_is_rejected() {
        if !have_sh() {
            eprintln!("skipping reattach_active_or_reconnecting_is_rejected: /bin/sh unavailable");
            return;
        }
        let identity = test_key(0xC3);
        let registry = SessionRegistry::new(5, Duration::ZERO);

        // --- Case 1: Active slot rejected ---
        let sess_active = open_sh_session(test_key(0xC3));
        let slot_active = SessionSlot::new(sess_active);
        registry.register_active(slot_active.clone());
        // Do NOT orphan — slot remains Active.
        let token_active = slot_active.token();

        let r_active = registry.reattach(&token_active, &identity);
        assert!(r_active.is_err(), "reattach of Active slot must be rejected (D-12)");
        match r_active {
            Err(ReattachReject::NotOrphaned) => {}
            Err(other) => panic!("Active slot must return NotOrphaned, got {other:?}"),
            Ok(_) => panic!("expected Err but got Ok"),
        }

        // --- Case 2: Reconnecting slot rejected (second reattach attempt) ---
        let sess_o = open_sh_session(test_key(0xC3));
        let slot_o = SessionSlot::new(sess_o);
        registry.register_active(slot_o.clone());
        registry.orphan(&slot_o);
        let token_o = slot_o.token();

        // First reattach succeeds → slot becomes Reconnecting.
        let r_first = registry.reattach(&token_o, &identity);
        assert!(r_first.is_ok(), "first reattach must succeed");
        assert_eq!(slot_o.state(), SlotState::Reconnecting);

        // Second reattach with the SAME token (slot is now Reconnecting) → rejected.
        // Note: after first reattach, the token IS still the same (rotate_token is
        // called by the SERVER, not by registry.reattach). The slot is Reconnecting.
        let r_second = registry.reattach(&token_o, &identity);
        assert!(r_second.is_err(), "second reattach of Reconnecting slot must be rejected (D-12)");
        match r_second {
            Err(ReattachReject::NotOrphaned) => {}
            Err(other) => panic!("Reconnecting slot must return NotOrphaned on second attempt, got {other:?}"),
            Ok(_) => panic!("expected Err but got Ok"),
        }

        // Cleanup.
        slot_active.sighup();
        slot_o.sighup();
    }

    /// W2 regression (timing-independent): after an orphan, the slot ALWAYS
    /// holds a usable PTY writer, so the input path works once the slot is
    /// reattached — keystrokes reach the PTY.
    ///
    /// This models the server's writer hand-back deterministically: the input
    /// task stores the writer back into the slot on session-end (no racy 200 ms
    /// oneshot). We assert the writer is present after orphan, take it (as the
    /// reattach pump does), write a command, and confirm the bytes reached the
    /// PTY by reading the line discipline's echo via a cloned reader.
    ///
    /// Before the fix, a timed-out oneshot left `pty_writer == None`, so this
    /// take would fail and a real reattach would be accepted-then-re-orphaned
    /// forever. The reliable hand-back makes the writer's presence invariant.
    #[test]
    fn orphaned_slot_always_has_usable_writer() {
        if !have_sh() {
            eprintln!("skipping orphaned_slot_always_has_usable_writer: /bin/sh unavailable");
            return;
        }

        use crate::session;
        use std::io::{Read as _, Write as _};

        let registry = SessionRegistry::new(5, Duration::ZERO);

        // Open a real /bin/sh and KEEP the writer (the server stores it in the
        // slot; on orphan the input task hands it back — modelled here by
        // storing it directly into the slot).
        let passwd = session::lookup_self(Some("/bin/sh"));
        let (sess, _reader, writer) =
            session::open(&passwd, "xterm", 80, 24, &[], test_key(0x77)).expect("open /bin/sh");
        let slot = SessionSlot::new(sess);
        registry.register_active(slot.clone());

        // The server's input task stores the writer back into the slot on exit.
        slot.return_pty_writer(writer);

        // Orphan the slot.
        registry.orphan(&slot);

        // INVARIANT: an orphaned slot ALWAYS has a usable writer (W2 fix).
        let mut w = slot
            .take_pty_writer()
            .expect("orphaned slot must hold a usable PTY writer (W2)");

        // Reattach would clone a fresh reader; do the same here BEFORE writing so
        // we capture the echo.
        let mut reader = slot.clone_pty_reader().expect("clone reader for reattach");

        // INPUT PATH PROOF: write a command; the PTY line discipline echoes the
        // bytes, proving the keystrokes reached the PTY through the handed-back
        // writer.
        w.write_all(b"echo W2_PATH_OK\n").expect("write to PTY");
        w.flush().expect("flush PTY writer");

        // Read until we see the echoed marker (bounded by a wall-clock deadline,
        // not a sleep-based assumption).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut acc = Vec::new();
        let mut buf = [0u8; 4096];
        let seen = loop {
            if std::time::Instant::now() > deadline {
                break false;
            }
            match reader.read(&mut buf) {
                Ok(0) => break false,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    if String::from_utf8_lossy(&acc).contains("W2_PATH_OK") {
                        break true;
                    }
                }
                Err(_) => break false,
            }
        };
        assert!(
            seen,
            "keystrokes must reach the PTY after orphan (writer usable); got: {:?}",
            String::from_utf8_lossy(&acc)
        );

        // Cleanup.
        slot.sighup();
    }

    // ── Phase 12: push_output_and_parse regression tests ─────────────────────

    /// Replay-integrity regression test (SYNC-02 / Pitfall 8).
    ///
    /// Proves that `push_output_and_parse` is byte-identical to `push_output` for
    /// the `SequencedOutputBuffer` path — seq numbers, replay_from results, and
    /// trim_acked behavior are all identical. This guards the cold-reattach path:
    /// converting the three server.rs callsites from `push_output` to
    /// `push_output_and_parse` must not change replay semantics.
    ///
    /// The test operates at the `SequencedOutputBuffer` level directly (no real
    /// PTY/Session needed) to keep it fast and dependency-free. A paired
    /// `TerminalState` is advanced in the same order as `push_output_and_parse`
    /// would do, verifying that both buffers receive the data.
    #[test]
    fn push_output_and_parse_seq_replay_trim_byte_identical_to_push_output() {
        use crate::terminal::TerminalState;

        // Build a "control" run using push_output (the old primitive).
        let mut control_buf = SequencedOutputBuffer::new(64 * 1024);
        let chunks: &[&[u8]] = &[b"hello", b"\x1b[1mworld\x1b[0m", b"\r\nfoo"];
        let mut control_seqs = Vec::new();
        for chunk in chunks {
            control_seqs.push(control_buf.push(chunk));
        }
        // Get replay_from(0) and trim_acked result from the control run.
        let (control_replay, control_replay_from, control_truncated) = control_buf.replay_from(0);
        control_buf.trim_acked(2);
        let (control_after_trim, _, _) = control_buf.replay_from(0);

        // Build a "test" run using the push_output_and_parse ordering manually
        // (push buf first, then advance terminal — same as push_output_and_parse).
        let mut test_buf = SequencedOutputBuffer::new(64 * 1024);
        let mut test_ts = TerminalState::new(80, 24);
        let mut test_seqs = Vec::new();
        for chunk in chunks {
            let seq = test_buf.push(chunk);
            test_ts.advance(chunk);
            test_seqs.push(seq);
        }
        let (test_replay, test_replay_from, test_truncated) = test_buf.replay_from(0);
        test_buf.trim_acked(2);
        let (test_after_trim, _, _) = test_buf.replay_from(0);

        // (a) Seq numbers must be identical.
        assert_eq!(
            control_seqs, test_seqs,
            "seq numbers from push_output_and_parse must be identical to push_output"
        );

        // (b) replay_from must be identical.
        assert_eq!(
            control_replay_from, test_replay_from,
            "replay_from_seq must be identical"
        );
        assert_eq!(
            control_truncated, test_truncated,
            "truncated_below_request must be identical"
        );
        let control_replay_seqs: Vec<u64> = control_replay.iter().map(|(s, _)| *s).collect();
        let test_replay_seqs: Vec<u64> = test_replay.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            control_replay_seqs, test_replay_seqs,
            "replay_from chunk seqs must be identical"
        );
        let control_replay_data: Vec<&[u8]> = control_replay.iter().map(|(_, d)| d.as_ref()).collect();
        let test_replay_data: Vec<&[u8]> = test_replay.iter().map(|(_, d)| d.as_ref()).collect();
        assert_eq!(
            control_replay_data, test_replay_data,
            "replay_from chunk data must be byte-identical"
        );

        // (c) trim_acked results must be identical.
        let control_after_seqs: Vec<u64> = control_after_trim.iter().map(|(s, _)| *s).collect();
        let test_after_seqs: Vec<u64> = test_after_trim.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            control_after_seqs, test_after_seqs,
            "trim_acked must produce identical remaining chunk seqs"
        );

        // (d) TerminalState must have observed the chunks (both buffers genuinely fed).
        // After pushing "hello\x1b[1mworld\x1b[0m\r\nfoo", the terminal should have
        // text at the expected positions.
        let h_cell = test_ts.cell(0, 0);
        assert_eq!(h_cell.ch, 'h', "terminal must have advanced: cell(0,0) should be 'h'");
        // "foo" is on row 1 (after \r\n)
        let f_cell = test_ts.cell(1, 0);
        assert_eq!(f_cell.ch, 'f', "terminal must have advanced: cell(1,0) should be 'f'");
    }

    /// Slot-level test: `push_output_and_parse` on a real `SessionSlot` feeds
    /// both the `SequencedOutputBuffer` AND the `TerminalState`.
    ///
    /// Requires /bin/sh (guarded). Verifies the integration path through SessionSlot.
    #[test]
    fn slot_push_output_and_parse_feeds_both_buffers() {
        if !have_sh() {
            eprintln!("skipping slot_push_output_and_parse_feeds_both_buffers: /bin/sh unavailable");
            return;
        }

        let sess = open_sh_session(test_key(0xD0));
        let slot = SessionSlot::new(sess);

        // Push text that the TerminalState will parse.
        let seq0 = slot.push_output_and_parse(b"hello");
        let seq1 = slot.push_output_and_parse(b" world");

        // (a) Seq numbers must be 0 and 1 (SequencedOutputBuffer unchanged).
        assert_eq!(seq0, 0, "first push_output_and_parse must return seq 0");
        assert_eq!(seq1, 1, "second push_output_and_parse must return seq 1");

        // (b) replay_from(0) must return both chunks (replay path unaffected).
        let (chunks, replay_from_seq, truncated) = slot.replay_from(0);
        assert_eq!(chunks.len(), 2, "replay_from(0) must return both chunks");
        assert_eq!(replay_from_seq, 0);
        assert!(!truncated);
        assert_eq!(chunks[0].1.as_ref(), b"hello");
        assert_eq!(chunks[1].1.as_ref(), b" world");

        // (c) trim_acked must work (trim seq 0, keep seq 1).
        slot.trim_acked(1);
        let (after_trim, _, _) = slot.replay_from(0);
        assert_eq!(after_trim.len(), 1, "after trim_acked(1), only seq 1 remains");
        assert_eq!(after_trim[0].0, 1);

        // (d) TerminalState must have observed the bytes.
        // Lock the terminal_state field and inspect it.
        let ts = slot.terminal_state.lock().unwrap();
        let cell_h = ts.cell(0, 0);
        assert_eq!(cell_h.ch, 'h', "terminal must have advanced: cell(0,0)='h'");
        drop(ts);

        slot.sighup();
    }
}
