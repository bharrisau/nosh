# Milestones

## v1.2 M4 Predictive Echo + Daily-Driver Readiness (Shipped: 2026-06-07)

**Phases completed:** 12 phases, 29 plans, 24 tasks

**Key accomplishments:**

- Site 1 — run_session (formerly lines ~356-373):
- Delivers `nosh-proto/src/datagram.rs` — the shared sparse terminal-diff contract (SYNC-01): StateDiff type, tag-byte encode/decode pair with provable STRICT cap, cursor-priority fill with continue-past-rejection, and 16 inline tests.
- crates/nosh-server/src/registry.rs:
- 1. [Rule 1 - Bug] Fixed pre-existing clippy::unnecessary_lazy_evaluations in decode_datagram
- New imports added to server.rs:
- Test 1 — `sync03_server_emits_datagram_after_pty_output`
- One-liner:
- One-liner:
- One-liner:
- Mosh PredictionEngine translated to Rust: full epoch/Validity state machine in predictor.rs with 49 unit tests proving noecho suppression, CJK width, dropped-datagram tolerance, and RTT hysteresis
- Mosh PredictionOverlay wired into the live client: speculative echo renders immediately on keypress through the single display path, predictions cull against StateDiffs using quinn RTT, `--predict always|adaptive|never` selects the mode (adaptive default), keystroke bytes to server unchanged, and Phase 17 latency instrumentation hook in place
- Full D-15-04 validation matrix in tests/predict.rs: 8 unit cases (vim/CJK/less/paste/Ctrl-C/simulated-loss/Home-End/adaptive-RTT) + 2 live-server integration cases (read -s noecho security gate + e2e echo confirm), all passing
- One-liner:
- One-liner:
- One-liner:
- Task 1 (doc skeleton):
- Task 1 — fuzz/ crate scaffold:
- Task 1 — read_message + decode_datagram + decode_epoch_ack (D-01):
- Task 1 — quic_packet fuzzer + seed corpus:
- D-03/D-04. All three tasks complete.
- One-liner:
- One-liner:
- D-03a verification (Task 1):
- Task 1 — D-03b connect-clear wiring:
- Path taken: `datagram_send_buffer_space()` (the preferred path).
- Task 1 — D-03 instrumentation (91bb14b):

---

## v1.1 M3 Roaming + Windows Client (Shipped: 2026-05-30)

**Phases completed:** 6 phases (4-9)

**Delivered:** Roaming-tolerant session persistence plus a native Windows client — a live session survives real network changes via QUIC migration and reconnects to an orphaned server-side session, all reachable from a no-WSL Windows client using on-disk SSH keys. Validated end-to-end against a Linux server.

**Key accomplishments:**

- Identity threading (P4): the authenticated peer's SSH identity (SPKI from the TLS handshake) is a non-optional `Session.identity` — the single key that flows into persistence, the per-identity cap, and reattach authorization (IDENT-01).
- Session persistence (P5): orphaned sessions survive disconnect Mosh-style — `MasterPty` held open (no SIGHUP), 64 KiB `SequencedOutputBuffer`, idle timeout (default 0), per-identity cap before first store, zombie reaper (PERSIST-01..03).
- Cold reattach (P6): 1-RTT resume over a fresh connection, two-factor (full TLS re-run + identity-scoped token selector, no oracle), byte-exact replay from last-acked sequence (IDENT-02, ROAM-02).
- Connection migration (P7): `ServerConfig::migration(true)` + headless rebind test (stable connection id, no loss, CID rotation), plus a real network-change live check passed from the Windows client (ROAM-01).
- Native Windows client (P8/P9): cross-compiles (no WSL/C toolchain), on-disk Ed25519 signing via the shared `RawEd25519Signer` trait, raw VT console I/O, resize, TERM/locale; hardened in P9 — VT console-input mode, ssh-style `~.` local-quit escape, `authorized_keys` warn+skip, client connect timeout, server migration logging (WIN-01..04).
- Live Windows→Linux validation: auth, shell, UTF-8/locale, resize, vim/arrows/`less`, `~.` quit, Ctrl-C→remote, and **network roaming** all confirmed on a real Windows host (sign-off in `docs/windows-client-test.md`).

**Audit:** 11/11 requirements, 6/6 phases, 4/4 cross-phase integration invariants WIRED, no blockers. 3 tracked tech-debt items (PTY reader-zombie race [latent], Windows cross-compile CI gate never run, WSAEMSGSIZE warning). Tag: v1.1.

---

## v1.0 M0-M2 Architecture-Validation Spike (Shipped: 2026-05-29)

**Phases completed:** 3 phases, 11 plans

**Delivered:** A single QUIC connection on UDP/443 carries a live interactive shell, authenticated entirely from the user's existing SSH-key identity, on Linux — proving the architecture's three foundational bets end-to-end.

**Key accomplishments:**

- QUIC transport skeleton: quinn endpoints over UDP/443 with TLS 1.3, shared `nosh/0` ALPN, a reliable bidirectional stream and RFC 9221 datagrams coexisting on one connection, plus keep-alive against the idle timeout (TRANS-01..05).
- SSH-key mutual auth wired into the TLS handshake: custom rustls SPKI-pinning verifiers (client TOFU `known_hosts`, server `authorized_keys`), real `CertificateVerify` signature verification (never stubbed), and pre-auth DoS hardening (half-open cap + auth-completion timeout) (AUTH-01..05).
- ssh-agent signing: the TLS `CertificateVerify` is produced via the ssh-agent socket so the private key is never loaded directly; live Ed25519 agent handshake passes (AUTH-04).
- PTY session core: real `portable-pty` login shell over the authenticated connection — bidirectional I/O, raw-mode RAII restore, ~40 ms coalesced resize, signal forwarding, deny-by-default env sanitization, `SessionClose{exit_code}` propagation, clean QUIC close, and a structured server-side `Session` struct as the M3 reattach seam (SESS-01..11).
- Milestone audit passed 21/21 requirements, 3/3 phases, 3/3 integration seams, 1/1 E2E flow; build/test/clippy reproduced clean (27 tests pass + 3 `#[ignore]`-gated live tests).

**Stats:** ~3,460 LOC Rust across crates `nosh-proto`, `nosh-auth`, `nosh-server`, `nosh-client`. Timeline: 2026-05-29 (single-day spike). Tag: v1.0.

**Known-by-design limitations (M3+ seams, not blockers):** `Session.identity` not yet threaded from the authenticated peer cert; datagrams carry no session traffic (enablement only); cold reattach not implemented; single-account server (no privilege drop); Ed25519-only (RFC 7250 RPK deferred). Two items (SESS-03 SIGKILL restore, SESS-06 Ctrl-C) are human-verified live (not headless-automatable) and recorded PASSED.

---
