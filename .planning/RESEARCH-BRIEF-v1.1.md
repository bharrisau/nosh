# v1.1 Research Brief — M3 Roaming + Windows Client

**Status:** PENDING — research not yet run. Milestone setup (PROJECT.md, STATE.md, config) is done and committed; this is the deferred research step of `/gsd:new-milestone`.

**To resume:** start a session in this repo and say:

> Run the v1.1 research from `.planning/RESEARCH-BRIEF-v1.1.md`

Claude spawns the 4 `gsd:gsd-project-researcher` agents below in parallel (note the **`gsd:` namespace prefix** — the bare name fails after a plugin reload), waits for all 4, then spawns `gsd:gsd-research-synthesizer` to write `SUMMARY.md`. After research completes, continue the milestone with requirements → roadmap (or just run `/gsd:new-milestone` is NOT needed — resume requirements inline).

**Models:** researchers/synthesizer = `sonnet`. **Output dir:** `.planning/research/` (v1.0 research already archived to `.planning/milestones/v1.0-research/`).

**Templates:** `/home/bharris/.claude/plugins/marketplaces/gsd-plugin/templates/research-project/{STACK,FEATURES,ARCHITECTURE,PITFALLS,SUMMARY}.md`

---

## Shared milestone context (all researchers)

SUBSEQUENT MILESTONE — adding roaming + Mosh-style session persistence + 1-RTT cold reattach + a bounded Windows-client slice to an existing, working Rust QUIC remote shell (nosh).

Existing validated stack (v1.0, DO NOT re-research): quinn 0.11.9, rustls 0.23.40 (custom SPKI-pinning verifiers), tokio 1.52.x, portable-pty 0.9.0, ssh-key 0.6.7, ssh-agent-client-rs 1.1.2 (Linux ssh-agent signing), ed25519-dalek 2.2.0, vte 0.15.0, rcgen 0.14.8. Workspace crates: nosh-proto, nosh-auth, nosh-server, nosh-client. Linux-only so far.

Known-by-design v1.0 seams: `Session.identity` NOT threaded from the authenticated peer cert; datagrams enabled but carry no session traffic; cold reattach NOT implemented; single-account server (no privilege drop); Ed25519-only.

Scope decisions locked for v1.1:
- Roaming validated headless via a forced path change / dual-interface switch; real Wi-Fi→cellular as a human-verified live check.
- Session persistence is Mosh-style (survive until shell exits) with a **configurable idle timeout defaulting to 0 (disabled)**; a per-identity cap bounds memory.
- Cold reattach is **1-RTT** (0-RTT stays deferred), authorization **bound to the SSH identity**.
- Windows = **client only** (→ Linux server); signs from an **on-disk OpenSSH key file** (agent integration deferred). Native Windows server (ConPTY) stays M6.

---

## Researcher 1 — STACK

<question>
What stack additions/changes/configuration are needed for v1.1? HIGH PRECISION on current crate APIs (verify with Context7 / docs.rs):

1. quinn 0.11.x connection migration — what does quinn support for client address/path migration? Automatic on server when client source addr changes, or must the client call something? Document real APIs: `Endpoint::rebind`, `Connection::set_path`/path mgmt, `migrate`, `TransportConfig` knobs affecting migration, connection-ID management, server-side config to ACCEPT a migrated path. Cite exact method names/signatures that exist in 0.11.9. Explicitly note what does NOT exist.
2. Windows client — crate(s) for Windows terminal raw-mode / VT input+output (crossterm vs windows-rs console). Does the existing Linux raw-mode client need a portability shim? Does quinn/tokio build for `x86_64-pc-windows-msvc` with current features? ring vs aws-lc-rs on Windows MSVC (does ring build on Windows?).
3. On-disk OpenSSH key signing (Windows) — ssh-key 0.6.7 surface for loading an OpenSSH private key from disk and producing an Ed25519 signature directly (no agent). Parsing `~/.ssh/id_ed25519`, passphrase-encrypted keys, signing bytes. Confirm this avoids ssh-agent-client-rs on Windows.
4. Session persistence / reattach — crate help for a sequence-numbered reliable-resume buffer (ET BackedReader) and reattach tokens, or hand-rolled on existing quinn streams + CSPRNG token (which crate — rand/getrandom — already in tree)?

Specific versions, confirm current, flag what NOT to add.
</question>
<files_to_read>
- .planning/PROJECT.md
- INIT.md (§10 milestone path; migration/reattach detail)
- .planning/milestones/v1.0-research/STACK.md
- Cargo.toml + per-crate Cargo.toml
</files_to_read>
<output>Write to .planning/research/STACK.md using the STACK.md template.</output>

---

## Researcher 2 — FEATURES

<question>
How do these features work in roaming remote shells (Mosh, Eternal Terminal), and what behavior will users rely on?
1. Connection migration / roaming UX — user expectation on IP change (Wi-Fi→cellular, sleep/resume, NAT rebind); Mosh's model; what "just works" looks like on QUIC; what feedback the user sees during a roam.
2. Session persistence (Mosh model) — how mosh-server persists a detached session; lifetime (runs until shell exits, no idle timeout by default); one server per session; how the client re-finds it; what state survives a disconnect. How ET differs (idle timeout, session IDs, daemon).
3. Cold reattach — ET's BackedReader/sequence-number reliable-resume; client reconnects with token + last-acked seq; server replays unacked bytes; 1-RTT handshake shape; reattach authorization bound to SSH identity (anti-hijack).
4. Windows client behavior — table-stakes vs nice-to-have for a Windows client talking to a Linux server (raw VT mode, resize, etc.).

Categorize table stakes / differentiators / anti-features. Note complexity and dependencies on existing system (esp. requires Session.identity threading first). Reattach identity-binding is table stakes, not optional.
</question>
<files_to_read>
- .planning/PROJECT.md
- INIT.md (roaming, session persistence, reattach, topology)
- .planning/milestones/v1.0-research/FEATURES.md
- .planning/MILESTONES.md
</files_to_read>
<output>Write to .planning/research/FEATURES.md using the FEATURES.md template.</output>

---

## Researcher 3 — ARCHITECTURE

<question>
How do roaming, session persistence, cold reattach, identity threading, and a Windows client integrate with the existing nosh architecture? Ground every claim in the REAL code (cite file paths under crates/*/src).
1. Session lifecycle & ownership — PTY+shell+terminal state must outlive the QUIC connection. Right ownership model (SessionManager/registry keyed by SSH identity fingerprint; connection task borrows a session). How it changes the server task structure. Where the per-identity persisted-session cap lives.
2. Migration vs reattach — two distinct paths. Migration = same QUIC connection, new path, handled inside quinn, no session re-lookup. Cold reattach = new connection after old died, find orphaned Session by token+identity, rebind I/O. Map both through the accept loop, connection handler, nosh-proto messages.
3. Identity threading — extract authenticated peer SSH fingerprint from rustls/quinn (peer cert/SPKI) after handshake, bind into Session.identity. Where in handshake→session-setup it happens.
4. Reattach protocol on nosh-proto — sequence-numbered reliable-resume buffer; Reattach{token, last_acked_seq}; server validation + replay. New frames/messages; interaction with reliable control stream vs datagrams.
5. Windows client integration — platform abstraction for terminal raw-mode + key-signing (ssh-agent Linux vs on-disk key Windows). Clean trait/cfg boundary keeping nosh-client cross-platform without forking.

Integration points, new vs modified components, data-flow changes, suggested build order respecting dependencies.
</question>
<files_to_read>
- .planning/PROJECT.md
- INIT.md (architecture, topology, reattach/migration rationale)
- .planning/milestones/v1.0-research/ARCHITECTURE.md
- .planning/MILESTONES.md
- crates/*/src (Session struct, nosh-proto messages, nosh-client raw-mode + signing, nosh-auth verifiers)
</files_to_read>
<output>Write to .planning/research/ARCHITECTURE.md using the ARCHITECTURE.md template.</output>

---

## Researcher 4 — PITFALLS

<question>
Common mistakes when adding roaming / session persistence / cold reattach / a Windows client to a QUIC remote shell? Be specific and actionable; map each to a prevention strategy and the phase that should address it.
1. QUIC migration pitfalls — path validation/anti-amplification limits during migration; spoofed-path / connection-ID linkability and privacy; NAT rebinding vs deliberate migration; server `migration` config; how migration interacts with keep-alive and idle timeout; what breaks if both endpoints migrate.
2. Session persistence pitfalls — orphaned-session memory growth (why the per-identity cap + the disabled-by-default idle timeout matter); zombie shell processes; reattach race when the old connection isn't fully dead (two clients on one session); SIGHUP/controlling-terminal semantics when the PTY outlives the connection.
3. Cold-reattach security — session hijacking if reattach isn't bound to the SSH identity; reattach-token entropy/lifetime; replay; sequence-number resync correctness (lost/duplicated output on replay).
4. Identity threading — getting the wrong fingerprint, cert vs SPKI confusion, timing (before vs after handshake completes).
5. Windows client — on-disk key handling (the temporary exception to "never handle the private key"): file perms, passphrase prompts, never logging key material; CRLF/VT/codepage and resize-event quirks; cross-compile/CI gaps.

For each: warning signs, prevention, which phase should own it.
</question>
<files_to_read>
- .planning/PROJECT.md
- INIT.md (risk register, security invariants)
- .planning/milestones/v1.0-research/PITFALLS.md
- .planning/MILESTONES.md
</files_to_read>
<output>Write to .planning/research/PITFALLS.md using the PITFALLS.md template.</output>

---

## Synthesizer (after all 4 complete)

Spawn `gsd:gsd-research-synthesizer` (model sonnet): read STACK.md, FEATURES.md, ARCHITECTURE.md, PITFALLS.md; write `.planning/research/SUMMARY.md` using the SUMMARY.md template; commit after writing.
