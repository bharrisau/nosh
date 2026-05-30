# Roadmap: nosh

## Milestones

- ✅ **v1.0 M0–M2 Architecture-Validation Spike** — Phases 1-3 (shipped 2026-05-29)
- **v1.1 M3 Roaming + Windows Client** — Phases 4-8 (in progress)

## Phases

<details>
<summary>✅ v1.0 M0–M2 Architecture-Validation Spike (Phases 1-3) — SHIPPED 2026-05-29</summary>

- [x] Phase 1: QUIC Transport Skeleton (4/4 plans) — completed 2026-05-29
- [x] Phase 2: SSH-Key Mutual Auth (4/4 plans) — completed 2026-05-29
- [x] Phase 3: PTY Session Core (3/3 plans) — completed 2026-05-29

Full detail archived at `.planning/milestones/v1.0-ROADMAP.md`.

</details>

### v1.1 M3 Roaming + Windows Client

- [x] **Phase 4: Identity Threading** — Wire `Session.identity` from the authenticated TLS handshake (fills the deliberate v1.0 seam) (completed 2026-05-30)
- [x] **Phase 5: Session Persistence** — Orphaned sessions survive client disconnect; per-identity cap and idle timeout in place before first orphan (completed 2026-05-30)
- [x] **Phase 6: Cold Reattach Protocol** — 1-RTT reconnect to an orphaned session with two-factor authorization (SSH handshake + token) (completed 2026-05-30)
- [x] **Phase 7: Connection Migration Validation** — Explicit migration config plus headless and live test coverage confirming zero-RTT roaming (completed 2026-05-30)
- [ ] **Phase 8: Windows Client** — Native Windows client connects to a Linux server with on-disk key signing, raw mode, resize, and locale propagation

## Phase Details

### Phase 4: Identity Threading
**Goal**: Every server-side session carries the authenticated peer's SSH identity as a non-optional field, enforced by the type system
**Depends on**: Nothing (v1.1 first phase; v1.0 Phase 3 is the foundation)
**Requirements**: IDENT-01
**Success Criteria** (what must be TRUE):
  1. `Session.identity` is a non-optional `NoshPublicKey` field — the compiler rejects constructing a `Session` without supplying a verified identity
  2. After a successful TLS handshake, `conn.peer_identity()` is downcast and `extract_spki_from_cert` called before any session message is read; the connection is rejected (not silently defaulted) if peer identity extraction fails
  3. All existing handshake tests still pass with no changes to their assertions
**Plans**: TBD

### Phase 5: Session Persistence
**Goal**: An orphaned session (PTY + shell + output buffer) survives QUIC disconnect and waits for a client to reattach, within the per-identity session cap
**Depends on**: Phase 4 (Session.identity is the persistence key and cap-enforcement key)
**Requirements**: PERSIST-01, PERSIST-02, PERSIST-03
**Success Criteria** (what must be TRUE):
  1. When the QUIC connection drops, the server's `MasterPty` stays open (not closed) so the shell is not SIGHUP'd; a live shell in an orphaned session remains interactive to a reattaching client
  2. Orphaned sessions accumulate outgoing PTY chunks with monotonic u64 sequence numbers from the moment of session open, held in a `SequencedOutputBuffer` ring bounded at 64 KiB
  3. A configurable idle timeout (default `0` = disabled, Mosh behavior) governs orphaned-session lifetime; the setting is tested at both `0` and a finite duration
  4. A per-identity cap (default 5) is enforced before the first orphaned session is stored; attempting to exceed the cap produces a deterministic error, not a silent drop
  5. A background zombie-reaper task calls `child.try_wait()` on all orphaned sessions; no shell-process zombies accumulate after normal shell exit
**Plans**: 3 plans
Plans:
- [ ] 05-01-PLAN.md — registry.rs foundation: SequencedOutputBuffer (64 KiB sequenced ring), SessionRegistry + SessionSlot, per-identity cap + LRU eviction, zombie/idle reaper (Wave 1)
- [ ] 05-02-PLAN.md — wire registry into server.rs: thread Arc<SessionRegistry>, feed output buffer, subdivide disconnect outcome (orphan on transport loss, no SIGHUP; teardown on clean close/exit) + integration tests (Wave 2)
- [ ] 05-03-PLAN.md — server CLI: --idle-timeout-secs (+ NOSH_IDLE_TIMEOUT_SECS env, CLI precedence) and --max-sessions-per-identity, construct registry from config (Wave 3)

### Phase 6: Cold Reattach Protocol
**Goal**: A client that disconnected and reconnected can resume its orphaned session in 1 RTT, with output continuity and no possibility of session hijacking
**Depends on**: Phase 5 (orphaned sessions and the SequencedOutputBuffer must exist before reattach can be tested)
**Requirements**: ROAM-02, IDENT-02
**Success Criteria** (what must be TRUE):
  1. A client that sends `Reattach{token, last_acked_seq}` on a new QUIC connection's first stream (after completing the full TLS mutual handshake) receives its orphaned session back: buffered output since `last_acked_seq` is replayed with no duplicated or dropped bytes
  2. Reattach authorization is two-factor — the SSH/TLS mutual handshake re-runs on every reconnection (same `authorized_keys` path as a fresh connect) AND the reattach token must match a session bound to that same SSH identity; either factor alone is insufficient
  3. A headless negative test confirms that a valid token presented with a different SSH key is rejected with the same error as a bad token (no oracle enumeration of session existence)
  4. The session state machine enforces mutual exclusion: a second `Reattach` while a session is already in Reconnecting state is rejected, preventing the two-clients-one-session race
**Plans**: TBD

### Phase 7: Connection Migration Validation
**Goal**: A live nosh session survives a client IP/path change with no re-handshake and no application-visible interruption, confirmed by headless CI and a human live check
**Depends on**: Phase 6 (migration tests are more meaningful against a server with full session registry in place)
**Requirements**: ROAM-01
**Success Criteria** (what must be TRUE):
  1. `ServerConfig::migration(true)` is set explicitly in the server's transport configuration (not left as an implicit default); the code comment documents intent
  2. A headless integration test performs `Endpoint::rebind()` mid-session and asserts that the active reliable stream continues without `ConnectionError` and produces no message loss
  3. qlog inspection from the headless test confirms CID rotation on path change (RFC 9000 §9.5 privacy requirement met)
  4. A human-verified Wi-Fi→cellular live check is recorded as PASSED in the phase completion notes, confirming the zero-RTT roaming experience against a real network change
**Plans**: TBD

### Phase 8: Windows Client
**Goal**: A native Windows client (no WSL) connects to and authenticates against a Linux nosh server, with a working interactive session including resize and correct locale
**Depends on**: Phase 4 (FileSigner shares the RawEd25519Signer trait boundary stabilized in Phase 4; Phase 5/6/7 server changes are independent)
**Requirements**: WIN-01, WIN-02, WIN-03, WIN-04
**Success Criteria** (what must be TRUE):
  1. `cargo check --target x86_64-pc-windows-gnu` (or msvc) passes with no errors; the Windows client crate cross-compiles cleanly without WSL or a C toolchain
  2. The Windows client authenticates against a Linux server using an on-disk unencrypted OpenSSH Ed25519 private key (selected via `--identity-file`); the key material is held in the narrowest possible scope with `ZeroizeOnDrop` and the private key is never written to logs or error messages
  3. The Windows client operates in raw VT input/output mode via `crossterm::terminal::enable_raw_mode()`; `Event::Resize` events from `EventStream` are converted to server PTY resize messages (using Windows console resize events, not SIGWINCH)
  4. The Windows client propagates `TERM` (defaulting to `xterm-256color`) and `LANG` (defaulting to `en_US.UTF-8` if unset) so the remote shell renders correctly; a best-effort file-permission warning is emitted on startup with the Windows ACL limitation documented
**Plans**: TBD
**UI hint**: yes

## Progress

| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|----------------|--------|-----------|
| 1. QUIC Transport Skeleton | v1.0 | 4/4 | Complete | 2026-05-29 |
| 2. SSH-Key Mutual Auth | v1.0 | 4/4 | Complete | 2026-05-29 |
| 3. PTY Session Core | v1.0 | 3/3 | Complete | 2026-05-29 |
| 4. Identity Threading | v1.1 | 2/2 | Complete | 2026-05-30 |
| 5. Session Persistence | v1.1 | 3/3 | Complete | 2026-05-30 |
| 6. Cold Reattach Protocol | v1.1 | 4/4 | Complete | 2026-05-30 |
| 7. Connection Migration Validation | v1.1 | 2/2 | Complete   | 2026-05-30 |
| 8. Windows Client | v1.1 | 0/? | Not started | - |
