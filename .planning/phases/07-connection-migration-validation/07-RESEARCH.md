# Phase 7: Connection Migration Validation — Research

**Researched:** 2026-05-30
**Phase goal:** A live nosh session survives a client IP/path change with no re-handshake and no application-visible interruption, confirmed by headless CI and a human live check.
**Requirement:** ROAM-01

This research answers "what do I need to know to PLAN this phase well?" It is grounded in the **actual installed crate source** (`quinn 0.11.9`, `quinn-proto 0.11.14` in `~/.cargo/registry`), the existing nosh codebase, and the locked decisions D-01..D-06 in `07-CONTEXT.md`.

---

## 1. The single most important finding (drives the whole plan)

**quinn 0.11.9's built-in qlog cannot, on its own, prove CID rotation or PATH_CHALLENGE the way D-05 literally states.** This was verified by reading the installed source, not docs:

- `quinn-proto-0.11.14/src/connection/qlog.rs` emits **only four** `EventData` variants: `MetricsUpdated`, `PacketLost`, `PacketSent`, `PacketReceived`.
- The `PacketHeader` it writes populates only `packet_number`, `packet_type`, and (on sent) `length`. **`scid`/`dcid` are left at `Default` — connection IDs are NOT recorded in the qlog.**
- **No frame-level events** are emitted (no `PathChallenge`, no `NewConnectionId`/`connection_id_updated`). The qlog therefore contains no PATH_CHALLENGE record and no CID field to diff.
- `emit_event` uses the *original* remote CID as the qlog `group_id` — it is stable for the connection lifetime and does not change on migration.

PITFALLS.md #3 ("inspect `connection_id_updated` events") assumed a richer qlog than quinn 0.11.9 ships. That event does not exist in this version.

### Resolution (honors D-05 mechanism + RFC 9000 §9.5 intent without silently dropping the decision)

quinn DOES expose the needed evidence through a different, more reliable surface: **`Connection::stats()` → `ConnectionStats.frame_tx` / `frame_rx` (`FrameStats`)**. Verified fields in `quinn-proto-0.11.14/src/connection/stats.rs`:

```
FrameStats { ..., new_connection_id: u64, retire_connection_id: u64,
             path_challenge: u64, path_response: u64, ping: u64, ... }
PathStats  { rtt: Duration, cwnd, lost_packets, sent_packets, current_mtu, ... }
ConnectionStats { frame_tx: FrameStats, frame_rx: FrameStats, path: PathStats, ... }
```

So the plan should do BOTH, mapping cleanly onto D-05's letter and intent:

1. **Wire qlog (D-05 literal):** enable the `qlog` cargo feature on quinn, attach a `QlogConfig`/`QlogStream` to the client (and/or server) `TransportConfig` via `TransportConfig::qlog_stream(Some(..))`, write the trace to a file, and have the test **assert the qlog file exists, is non-empty, and parses as valid JSON-seq** (qlog `TraceSeq`). This produces the human-inspectable artifact the decision calls for.
2. **Assert CID rotation / path validation programmatically (D-05 intent, RFC 9000 §9.5 + §9.3):** capture `conn.stats()` immediately before the rebind and again after continuity is re-established, and **hard-assert** that across the rebind:
   - `frame_tx.path_challenge` (or `frame_rx.path_challenge`) increased — path validation ran, AND
   - `new_connection_id` and/or `retire_connection_id` counters increased — CIDs rotated.

   These counters are the authoritative, version-stable proof. The qlog is the supplementary artifact.

The planner MUST encode this split explicitly so the executor doesn't waste time trying to grep PATH_CHALLENGE out of a qlog that never contains it. (Plan-check note: this is a deliberate, documented interpretation of D-05, not a dropped decision.)

---

## 2. quinn 0.11.9 migration API surface (verified against installed source)

| Need | API (exists in 0.11.9) | Source ref |
|------|------------------------|------------|
| Enable server-side migration | `ServerConfig::migration(bool)` — **default `true`** (D-01) | `quinn-0.11.9` ServerConfig |
| Force a client path change | `Endpoint::rebind(socket: std::net::UdpSocket) -> io::Result<()>` | `endpoint.rs:240` |
| (alt) abstract socket rebind | `Endpoint::rebind_abstract(Arc<dyn AsyncUdpSocket>)` | `endpoint.rs:250` |
| Observe new path | `Connection::remote_address() -> SocketAddr` (updates after migration) | `connection.rs:511` |
| RTT / stall measurement | `Connection::rtt() -> Duration`, `Connection::stats() -> ConnectionStats` (`path.rtt`, `frame_*`) | `connection.rs:529/534` |
| qlog | feature `qlog = ["proto/qlog"]`; `quinn::{QlogConfig, QlogStream}` re-exported; `TransportConfig::qlog_stream(Some(stream))` | `lib.rs:71`, `config/transport.rs:347` |

**Methods that do NOT exist (do not plan around them):** `Connection::migrate()`, `Connection::set_path()`, `Connection::network_path()`, `Connection::path()`. Migration is driven entirely by `Endpoint::rebind` on the client + the server's `migration(true)` gate; quinn-proto runs PATH_CHALLENGE/PATH_RESPONSE automatically.

**`rebind` semantics (from quinn docs + endpoint.rs):** replaces the underlying UDP socket live for ALL connections on that endpoint and broadcasts `ConnectionEvent::Rebind` to each driver, which probes the new path. Docs warn "connections to servers unreachable from the new address will be lost" — irrelevant here since loopback `127.0.0.1` is reachable from any fresh `127.0.0.1:0` socket. D-02 (rebind to a fresh local UDP socket, same host, new port) is the correct, CI-portable trigger.

### qlog wiring shape (verified API)
```text
QlogConfig::default().writer(Box<dyn Write+Send+Sync>).title(Some(..)).into_stream() -> Option<QlogStream>
transport_config.qlog_stream(Some(stream));   // on TransportConfig before building endpoint config
```
`into_stream()` returns `None` if the streamer fails to init (logged via tracing::warn) — the test should treat a `None` as a soft setup failure, not a migration failure.

---

## 3. Existing code touchpoints (verified line anchors — executor re-reads at exec time)

- **`crates/nosh-proto/src/transport.rs`** — `transport_config(enable_keep_alive: bool) -> TransportConfig`. Already sets datagram buffers, `max_idle_timeout(300s)`, and (client only) `keep_alive_interval(15s)`. **Pitfall #4 check:** 300s idle ≫ any loopback path-validation window (sub-ms RTT, a few ms even under CI jitter); keep-alive 15s keeps the new path warm. These settings do NOT fight migration — confirm and document, do not change them. The qlog stream must be attachable per-endpoint, so the qlog wiring should be additive (e.g. a new helper or an optional param) rather than baked into the shared `transport_config`, because qlog needs a distinct writer per endpoint.
- **`crates/nosh-server/src/server.rs`** — `build_server_config(host_key_path, authorized_keys_path)` builds `quinn::ServerConfig::with_crypto(..)` then calls `server_config.transport_config(Arc::new(nosh_proto::transport_config(false)))`. **D-01 lands here:** add `server_config.migration(true);` with an intent comment immediately after the config is built. (Concurrency note: Phase 6 also edits server.rs — `handle_connection`/`run_session`; the migration line is in `build_server_config` near the top, structurally independent. Executor must re-read.)
- **`crates/nosh-client/src/client.rs`** — `make_endpoint(&identity, known_hosts, host)`, `build_client_config`, `open_session`/`open_session_with_token`, `send_input`, `collect_until_close`, `run_session_collect`. The client endpoint is what the test will `rebind`. Client may need a qlog-enabled endpoint constructor variant for the test.
- **`crates/nosh-client/tests/common/mod.rs`** — in-process harness: `TestKey`, `spawn_server*`, `client_endpoint`, `session_marker_usable`. Tests bind loopback `127.0.0.1:0`, run the server in-process via `run_accept_loop`. Extend with a qlog-enabled client endpoint builder + a rebind helper.
- **`crates/nosh-client/tests/session.rs`** — canonical session-test patterns (open_session → send_input → collect; `read_pid`/`proc_state` helpers; 15s timeouts; `/bin/sh` skip guard). The migration test belongs in a new `tests/migration.rs` mirroring these patterns.
- **`Cargo.toml` (workspace root)** — `quinn = { version = "0.11.9", default-features = false, features = ["runtime-tokio", "rustls-ring"] }`. **`qlog` is NOT in the feature list** — Plan must add it (workspace-level, or as a dev-dependency feature for the test crate). Adding `qlog` pulls the `qlog` crate as a transitive dep (verified `qlog = ["dep:qlog"]` in quinn-proto). No `serde_json` in the tree yet — the qlog file validation can use the `qlog` crate's own types or a minimal line-is-JSON check; avoid adding `serde_json` unless the planner judges it cleaner (it is already a transitive dep of several crates, low cost).

---

## 4. Test design (headless, D-02/D-03/D-04/D-05)

A single integration test (in `tests/migration.rs`, normal CI test — NOT `#[ignore]`; the live check is the only manual piece) should:

1. Spawn the in-process server (`/bin/sh`, skip if absent — reuse `connect_session_server` pattern) with `migration(true)` (now explicit).
2. Build a **qlog-enabled client endpoint** writing to a temp file; connect; open a session.
3. Drive a **long-running, monotonically-numbered output stream** so continuity is checkable across the rebind. Robust generator: `for i in $(seq 1 N); do echo "LINE:$i"; sleep <small>; done` (or a steady `yes | nl`-style stream throttled so it spans the rebind). The test reads `PtyData` frames and parses the `LINE:<n>` sequence.
4. Mid-stream (after observing some lines), snapshot `conn.stats()` + `conn.remote_address()` + `conn.rtt()`, then `endpoint.rebind(UdpStdSocket::bind("127.0.0.1:0"))` onto a FRESH socket (D-02).
5. Continue reading. **Hard-fail (D-03)** on: any `ConnectionError` on the stream, any gap/duplication/out-of-order in the `LINE:<n>` sequence, or `close_reason()` becoming a transport error. Assert the SAME connection continues — `conn.stable_id()` unchanged and NO new TLS handshake (there is only ever one handshake on a quinn `Connection`; assert continuity via uninterrupted stream + unchanged `stable_id`, and that `remote_address()` may differ).
6. **Measure the stall (D-04):** record wall-clock time between the last pre-rebind frame and the first post-rebind frame; compare to `conn.rtt()` (or `stats().path.rtt`). **Log** the measured stall and `~Nx RTT` ratio; **soft-warn** (eprintln/tracing::warn) if > ~3× RTT; **do NOT hard-fail** on it (CI jitter, D-04).
7. **CID/path assertion (D-05 intent):** snapshot `conn.stats()` again after continuity resumes; hard-assert `frame_tx.path_challenge` (or frame_rx) increased AND `new_connection_id`/`retire_connection_id` increased vs the pre-rebind snapshot.
8. **qlog assertion (D-05 literal):** flush/close the connection so the qlog streamer finalizes, then assert the qlog file exists, is non-empty, and parses (valid qlog `TraceSeq`/JSON-seq). Document in a comment that frame-level PATH_CHALLENGE is not in quinn 0.11.9 qlog, so the FrameStats assertion is the binding CID-rotation check.

**Timing robustness:** the output generator must out-live the rebind+path-validation window with margin. On loopback RTT is sub-millisecond, so a per-line `sleep 0.05` over ~40-100 lines (2-5s of stream) gives a comfortable window. Wrap the whole test in a generous `tokio::time::timeout` (e.g. 30s) so a hang fails loudly instead of stalling CI.

**Pitfall #2 mitigation in-test (optional but recommended):** immediately after `rebind`, send a tiny client `PtyData`/control frame (or rely on keep-alive) to advance the server's anti-amplification budget on the new path, reducing the measured stall. The CONTEXT specifics call the stall "expected behavior, not a bug" — measure it; the client PING is a nice-to-have, not required by D-04.

---

## 5. Human live check (D-06)

A short markdown doc (e.g. `docs/migration-live-check.md` or under the phase dir — Claude's discretion) describing the Wi-Fi→cellular procedure:
- Start a `nosh` session over Wi-Fi against a reachable server.
- Run a visible continuous output (`ping`, `top`, `tail -f`, or a `seq` loop) so a stall/break is obvious.
- Disable Wi-Fi / enable cellular on the client device (forces a real IP change).
- **PASS criteria:** session continues, no re-auth prompt, no visible data loss, output resumes within ~1-2s. A short stall is acceptable (the anti-amplification stall).
- A checklist + a place to record the operator's PASS/date.
- **Non-blocking:** Phase 7 is marked `human_needed`; the autonomous run completes without it; the operator records PASSED later in the phase completion notes (D-06).

---

## 6. Pitfall cross-check (PITFALLS.md #1-#4)

| Pitfall | How this phase addresses it |
|---------|------------------------------|
| #1 migration flag not set | D-01: explicit `ServerConfig::migration(true)` + intent comment in `build_server_config`. |
| #2 anti-amplification stall | D-04: measured + logged + soft-warn > 3× RTT; not a hard gate. Optional post-rebind client PING to advance budget. |
| #3 CID linkability | D-05: assert `new_connection_id`/`retire_connection_id`/`path_challenge` FrameStats deltas (qlog lacks the events). |
| #4 keep-alive/idle vs migration | transport.rs: keep client keep-alive 15s + idle 300s UNCHANGED; loopback path-validation window is ms-scale, far inside 300s. Confirm + comment; do NOT shorten idle timeout for "faster failure". |

---

## 7. Open risks / notes for the planner

- **Concurrency with Phase 6:** server.rs and client.rs are being edited concurrently. Keep Phase 7 edits surgical and structurally anchored: D-01 goes in `build_server_config` (top of server.rs, independent of `handle_connection`/`run_session`); client qlog endpoint is an additive constructor. Executor re-reads at exec time.
- **qlog feature scope:** prefer enabling `qlog` narrowly (test/dev path) if it can be done without forcing it into the production default-features set; but a workspace-level `qlog` feature on quinn is acceptable and simplest. Planner decides; either way `cargo test --workspace` must stay green and `cargo build` must not regress.
- **`cargo test --workspace` must stay green** (hard requirement). The new test must skip cleanly when `/bin/sh` is absent (mirror existing guard).
- No `serde_json`/`tempfile` gap for the test crate: `tempfile` is already a dev-dep of nosh-client; reuse it for the qlog temp file.

---

## RESEARCH COMPLETE

- D-05 reconciled with quinn 0.11.9 reality: qlog artifact (literal) + FrameStats CID/path assertion (intent). Documented as a deliberate interpretation, not a dropped decision.
- All migration API anchors verified against installed `quinn 0.11.9` / `quinn-proto 0.11.14` source.
- Test design, transport-config interaction (Pitfall #4), and human-check doc scoped.
- Ready for planning.
