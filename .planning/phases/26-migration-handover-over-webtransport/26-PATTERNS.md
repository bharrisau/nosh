# Phase 26: Migration Handover over WebTransport - Pattern Map

**Mapped:** 2026-06-14
**Files analyzed:** 8 new/modified files
**Analogs found:** 6 / 8

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `crates/nosh-client/src/main.rs` (reconnect loop) | controller | event-driven | `crates/nosh-client/src/main.rs` (lines 1354-1450, WebTransport reconnect path) | exact |
| `crates/nosh-client/src/inner_auth.rs` | service | request-response | `crates/nosh-client/src/inner_auth.rs` (lines 136-282, run_inner_auth_client) | exact |
| `crates/nosh-server/src/server.rs` (run_reattach_session) | service | request-response | `crates/nosh-server/src/server.rs` (lines 1583-1679, run_reattach_session) | exact |
| `crates/nosh-server/src/registry.rs` (reattach method) | service | request-response | `crates/nosh-server/src/registry.rs` (lines 676-728, reattach method) | exact |
| `crates/nosh-client/tests/webtransport.rs` (new test) | test | event-driven | `crates/nosh-client/tests/webtransport.rs` (lines 51-100, wt01_live_shell_over_webtransport) | role-match |
| `crates/nosh-client/tests/reattach.rs` (new concurrent test) | test | event-driven | `crates/nosh-client/tests/reattach.rs` (lines 399-443, reattach_rejected_while_session_active) | role-match |
| `crates/nosh-client/src/screen.rs` (reconnecting banner) | component | state-transform | `crates/nosh-client/src/screen.rs` (lines 121-150, ConnectionLossOverlay) | exact |
| `crates/nosh-client/src/main.rs` (~. abort UX) | controller | request-response | `crates/nosh-client/src/main.rs` (lines 119-126, stdin quit detection) | exact |

## Pattern Assignments

### `crates/nosh-client/src/main.rs` (WebTransport reconnect loop)

**Analog:** `crates/nosh-client/src/main.rs` (lines 1354-1450)

**Context:** The existing WebTransport reconnect path in the main supervisor loop provides the exact pattern for Phase 26. This loop handles connection failures, runs the inner handshake (Phase 25), and manages reattach with persistent backoff.

**Reconnect loop pattern** (lines 1354-1450):
```rust
// ── WebTransport connect path ──────────────────────────────────────
let url = format!("https://{}:{}{}", args.host, args.port, args.wt_path);
let wt_config = nosh_client::wt_transport::build_wt_client_config();
let conn = match nosh_client::wt_transport::connect_wt(wt_config, &url).await {
    Ok(c) => c,
    Err(e) => {
        tracing::warn!("webtransport connect failed: {e}");
        eprintln!("\r\nnosh: reconnecting…\r");
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = quit_during_backoff(&mut stdin_quit) => {
                eprintln!("\r\nnosh: quit\r");
                break;
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
        continue;
    }
};

// ── Inner SSH-key auth on the control stream (Phase 25, D-02) ─────
let (ctrl_send, ctrl_recv) = match nosh_client::inner_auth::run_inner_auth_client(
    &*conn,
    &known_hosts,
    &args.host,
    identity.signer(),
).await
{
    Ok(pair) => pair,
    Err(e) => {
        let fatal = is_fatal_connect_error(&e) || {
            let msg = format!("{e:#}").to_ascii_lowercase();
            msg.contains("host key mismatch")
                || msg.contains("inner auth: host key")
                || msg.contains("inner auth: ekm mismatch")
                || msg.contains("not accepted")
        };
        if fatal {
            tracing::error!("fatal inner auth error (not retrying): {e:#}");
            eprintln!("\r\nnosh: connection aborted — {e:#}\r");
            conn.close(1, b"inner-auth-failed");
            exit_code = 1;
            break;
        }
        tracing::warn!("webtransport inner auth failed (transient): {e}");
        eprintln!("\r\nnosh: reconnecting…\r");
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = quit_during_backoff(&mut stdin_quit) => {
                eprintln!("\r\nnosh: quit\r");
                break;
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
        continue;
    }
};
```

**Backoff constants** (lines 54-56):
```rust
/// Reconnect backoff: start 250ms, double on each retry up to 10s cap (D-10).
const BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
```

**Reattach dispatch pattern** (lines 1429-1444):
```rust
// Inner auth succeeded — ctrl_send/ctrl_recv are the authenticated
// control stream. Use them for session dispatch (no second open_bi).
let pump_outcome = if let Some(tok) = token {
    let reattach_result = reattach_session_on_stream(
        &*conn,
        ctrl_send,
        ctrl_recv,
        tok,
        highest_applied,
        &mut highest_applied,
        &mut resize,
        &mut token,
        args.predict,
        args.status,
    )
    .await;
    reattach_result.unwrap_or(PumpOutcome::TransportDrop)
} else {
    let fresh_result = fresh_session_on_stream(
        &*conn,
        ctrl_send,
        ctrl_recv,
        term.clone(),
        // ... fresh session parameters
    )
    .await;
    fresh_result.unwrap_or(PumpOutcome::TransportDrop)
};
```

**PumpOutcome handling** (lines 1466-1472):
```rust
match pump_outcome {
    PumpOutcome::CleanExit(code) => { exit_code = code; break; }
    PumpOutcome::UserQuit => break,
    PumpOutcome::TransportDrop => {
        // Reconnect with backoff
        eprintln!("\r\nnosh: reconnecting…\r");
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = quit_during_backoff(&mut stdin_quit) => {
                eprintln!("\r\nnosh: quit\r");
                break;
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}
```

---

### `crates/nosh-client/src/inner_auth.rs` (inner handshake for reconnect)

**Analog:** `crates/nosh-client/src/inner_auth.rs` (lines 136-282)

**Context:** The Phase 25 inner handshake function that the reconnect loop must call on each fresh WebTransport session before sending Reattach (MH-1 ordering).

**Function signature** (lines 136-141):
```rust
pub async fn run_inner_auth_client(
    conn: &dyn nosh_proto::transport_trait::NoshTransport,
    known_hosts: &Path,
    host: &str,
    client_signer: Arc<dyn RawEd25519Signer>,
) -> anyhow::Result<(Box<dyn NoshSendStream>, Box<dyn NoshRecvStream>)>
```

**Core handshake pattern** (lines 142-282):
```rust
// Step 1: open the control bidi stream.
let (mut send, mut recv) = conn
    .open_bi()
    .await
    .context("open inner-auth control stream")?;

// Step 2: derive the local RFC 9266 EKM binding from the outer TLS session.
let mut ekm = [0u8; 32];
conn.export_keying_material(&mut ekm, INNER_AUTH_EKM_LABEL, INNER_AUTH_EKM_CONTEXT)
    .context("derive RFC 9266 EKM for inner-auth binding")?;

// Step 3: receive the server's challenge.
let (server_nonce, server_spki_vec, ekm_from_server) =
    match read_message_ns(&mut *recv).await? {
        Message::InnerAuthChallenge {
            server_nonce,
            server_spki,
            ekm: ekm_from_server,
        } => (server_nonce, server_spki, ekm_from_server),
        other => {
            anyhow::bail!(
                "inner auth: expected InnerAuthChallenge, got {}",
                other.variant_name()
            );
        }
    };

// Step 4: D-01 binding check — client verifies the server's EKM matches
if ekm_from_server != ekm {
    anyhow::bail!(
        "inner auth: EKM mismatch (D-01 binding check failed) — \
         possible transparent proxy on a different TLS session"
    );
}

// Step 5: parse the server's SPKI.
let server_key = nosh_key_from_spki(&server_spki_vec)
    .ok_or_else(|| anyhow::anyhow!("inner auth: server SPKI is malformed or not Ed25519"))?;

// Step 6: TOFU / known_hosts check.
match lookup_known_host(known_hosts, host)
    .with_context(|| format!("known_hosts lookup for {host}"))?
{
    Some(pinned) => {
        if pinned != server_key {
            anyhow::bail!(
                "inner auth: host key mismatch for {host} — \
                 known_hosts pins a different key (aborting; possible MITM)"
            );
        }
    }
    None => {
        let accepted = prompt_tofu_or_fail(host, &server_key.fingerprint())
            .context("TOFU prompt for inner auth")?;
        if !accepted {
            anyhow::bail!(
                "inner auth: host key for {host} not accepted; connection declined"
            );
        }
        record_known_host(known_hosts, host, &server_key)
            .with_context(|| format!("record known host {host}"))?;
    }
}

// Steps 7-10: generate client nonce, sign, send response, verify server completion
// ... (see full implementation)
```

---

### `crates/nosh-server/src/server.rs` (run_reattach_session)

**Analog:** `crates/nosh-server/src/server.rs` (lines 1583-1679)

**Context:** The server-side reattach handler is unchanged from v1.1 (already generic over NoshTransport after Phase 23). This is the target of the MH-2 double-attach guard.

**Function signature** (lines 1583-1591):
```rust
pub(crate) async fn run_reattach_session(
    conn: Box<dyn NoshTransport>,
    peer: SocketAddr,
    identity: nosh_auth::NoshPublicKey,
    mut send: Box<dyn NoshSendStream>,
    mut recv: Box<dyn NoshRecvStream>,
    reattach_params: ([u8; 16], u64), // (token, last_acked_seq)
    registry: Arc<crate::registry::SessionRegistry>,
) -> anyhow::Result<()>
```

**Two-factor reattach authorization** (lines 1596-1611):
```rust
// ── Step 1: Two-factor reattach authorization ─────────────────────────────
let slot = match registry.reattach(&token, &identity) {
    Ok(s) => s,
    Err(_) => {
        // ALL rejection causes take this identical path (D-07 no-oracle).
        // Log identity fingerprint only; never the token.
        tracing::info!(identity = %identity.fingerprint(), "reattach rejected");
        let _ = nosh_proto::write_message_ns(&mut *send, &Message::ReattachErr).await;
        // Finish the send stream so the client can read the ReattachErr frame
        // before the connection is closed.
        let _ = send.finish().await;
        let _ = tokio::time::timeout(Duration::from_millis(200), send.stopped()).await;
        conn.close(CLOSE_PROTOCOL, b"reattach rejected");
        return Ok(());
    }
};
```

---

### `crates/nosh-server/src/registry.rs` (reattach method with MH-2 guard)

**Analog:** `crates/nosh-server/src/registry.rs` (lines 676-728)

**Context:** The registry reattach method implements the MH-2 double-attach guard via atomic `HashMap::remove` and `Orphaned → Reconnecting` state transition.

**MH-2 guard: state transition** (lines 708-718):
```rust
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
```

**Token lookup scoped by identity** (lines 683-700):
```rust
// Find the first slot in this identity's Vec whose token matches.
// Because we scoped the lookup to this identity's Vec, a valid token
// presented under a DIFFERENT identity simply won't be found here →
// NotFound (same path as a bad token — no oracle, D-07).
let slot = match slots.iter().find(|s| s.token() == *token) {
    Some(s) => s.clone(),
    None => return Err(ReattachReject::NotFound),
};
```

---

### `crates/nosh-client/tests/webtransport.rs` (network-change simulation test)

**Analog:** `crates/nosh-client/tests/webtransport.rs` (lines 51-100)

**Context:** Existing WebTransport test structure provides the pattern for spawning a WT server, connecting a client, and running a session script. Phase 26 extends this with connection-drop simulation.

**Test fixture pattern** (lines 57-76):
```rust
let server = common::spawn_wt_server(Some(SH.to_string()))
    .await
    .expect("spawn_wt_server returned None — /bin/sh missing");

// Build a WT client config that trusts the server's self-signed cert by
// its SHA-256 hash (WebTransport certificate-hashes W3C API equivalent).
let config = wtransport::ClientConfig::builder()
    .with_bind_default()
    .with_server_certificate_hashes([server.cert_hash.clone()])
    .build();

let url = format!("https://127.0.0.1:{}/nosh", server.addr.port());
let transport = tokio::time::timeout(
    Duration::from_secs(10),
    nosh_client::wt_transport::connect_wt(config, &url),
)
.await
.expect("connect_wt timed out after 10s")
.expect("connect_wt failed");
```

---

### `crates/nosh-client/tests/reattach.rs` (concurrent same-token test)

**Analog:** `crates/nosh-client/tests/reattach.rs` (lines 399-443)

**Context:** The existing reattach test for mutual exclusion (SC#4) provides the pattern for the concurrent same-token MH-2 test.

**Concurrent reattach pattern** (lines 402-443):
```rust
/// SC#4 / D-12: a Reattach for a session that is still Active (client still
/// attached) must be rejected — prevents two-clients-one-session race.
#[tokio::test]
async fn reattach_rejected_while_session_active() {
    if !have_sh() {
        eprintln!("skipping reattach_rejected_while_session_active: /bin/sh unavailable");
        return;
    }

    let registry = SessionRegistry::new(5, Duration::ZERO);
    let client_key = TestKey::generate();
    let server = server_with_key(registry.clone(), &client_key).await;

    // Connect with key A and KEEP the connection active (do NOT drop).
    let (ep1, _dir1) = client_endpoint_for(&client_key);
    let conn1 = client::connect(&ep1, server.addr, HOST, Duration::from_secs(30)).await.expect("connect 1");
    let qt1 = QuinnTransport(conn1.clone());
    let (_send1, _recv1, token) =
        client::open_session_with_token(&qt1, "xterm".to_string(), 80, 24, vec![])
            .await
            .expect("open session 1");

    // Give the server a moment to register the slot as Active.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // From a second endpoint with the SAME key, attempt to reattach the Active session.
    let (ep2, _dir2) = client_endpoint_for(&client_key);
    let conn2 = client::connect(&ep2, server.addr, HOST, Duration::from_secs(30)).await.expect("connect 2");
    let qt2 = QuinnTransport(conn2.clone());
    let (outcome2, _, _) = client::reattach_collect(&qt2, token, 0)
        .await
        .expect("reattach_collect for active slot");
    conn2.close(0u32.into(), b"done");
    ep2.close(0u32.into(), b"done");

    assert_eq!(
        outcome2,
        ReattachOutcome::Err,
        "Reattach for Active session must be rejected (D-12 mutual exclusion)"
    );
}
```

---

### `crates/nosh-client/src/screen.rs` (ConnectionLossOverlay banner)

**Analog:** `crates/nosh-client/src/screen.rs` (lines 121-150)

**Context:** The v1.2 reconnection overlay provides the exact pattern for the Phase 26 reconnecting banner.

**Reconnecting banner pattern** (lines 121-150):
```rust
impl Overlay for ConnectionLossOverlay {
    /// Return `None` unless `active && row == 0`.
    ///
    /// When active, builds a banner like:
    /// `nosh: reconnecting — last contact 7s ago. Press ~. to disconnect.`
    /// padded with spaces to `cols` width, rendered in reverse-video (SGR 7).
    fn cell_at(&self, row: u16, col: u16) -> Option<Cell> {
        if !self.active || row != 0 {
            return None;
        }
        let elapsed = self.last_contact.elapsed().as_secs();
        let banner = format!(
            "nosh: reconnecting \u{2014} last contact {elapsed}s ago. Press ~. to disconnect."
        );
        // Space-pad to terminal width.
        let padded: Vec<char> = banner
            .chars()
            .chain(std::iter::repeat(' '))
            .take(self.cols as usize)
            .collect();
        let ch = padded.get(col as usize).copied().unwrap_or(' ');
        Some(Cell {
            ch,
            style: CellStyle(CellStyle::REVERSE),
            fg: None,
            bg: None,
            wide: false,
        })
    }
}
```

---

### `crates/nosh-client/src/main.rs` (~. abort UX)

**Analog:** `crates/nosh-client/src/main.rs` (lines 119-126, 473-570)

**Context:** The SSH-style `~.` escape detection provides the pattern for local quit during reconnection.

**~. escape detection** (lines 119-126):
```rust
/// Read stdin; treat EOF, Ctrl-C (0x03), or a `~.` sequence as a quit request.
///
/// `~.` is matched as a simple two-byte tail anywhere in a read batch — suff...
pub async fn read stdin_quit_or Quit(bytes: &mut [u8]) -> anyhow::Result<(Vec<u8>, bool)> {
    let n = stdin.read(bytes).await?;
    let bytes = &bytes[..n];
    let quit = bytes.is_empty()
        || bytes == &[0x03]
        || bytes.windows(2).any(|w| w == b"~."); // SSH-style ~. escape
    // ...
}
```

**Quit-during-backoff pattern** (lines 1365-1370):
```rust
tokio::select! {
    _ = tokio::time::sleep(backoff) => {}
    _ = quit_during_backoff(&mut stdin_quit) => {
        eprintln!("\r\nnosh: quit\r");
        break;
    }
}
```

---

## Shared Patterns

### Bounded exponential backoff
**Source:** `crates/nosh-client/src/main.rs` (lines 54-56, 1371, 1422)
**Apply to:** All reconnection attempts
```rust
const BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

// In reconnect loop:
backoff = (backoff * 2).min(BACKOFF_MAX);
```

### Fatal vs transient error classification
**Source:** `crates/nosh-client/src/main.rs` (lines 58-106, 1395-1401)
**Apply to:** All connect/handshake error handling
```rust
/// Classify a `client::connect()` failure as a PERMANENT (fatal) error that must
/// abort immediately, vs. a transient one that should be retried with backoff.
fn is_fatal_connect_error(e: &anyhow::Error) -> bool {
    let msg = format!("{e:#}").to_ascii_lowercase();
    // Fatal: host key mismatch, TOFU decline, EKM binding failure
    msg.contains("host key mismatch")
        || msg.contains("known_hosts")
        || msg.contains("tofu")
        || msg.contains("ekm mismatch")
        || msg.contains("not accepted")
}

// Usage:
let fatal = is_fatal_connect_error(&e) || {
    let msg = format!("{e:#}").to_ascii_lowercase();
    msg.contains("host key mismatch")
        || msg.contains("inner auth: host key")
        || msg.contains("inner auth: ekm mismatch")
        || msg.contains("not accepted")
};
if fatal {
    tracing::error!("fatal error (not retrying): {e:#}");
    eprintln!("\r\nnosh: connection aborted — {e:#}\r");
    conn.close(1, b"fatal-error");
    exit_code = 1;
    break;
}
```

### Reconnecting banner with ~. escape
**Source:** `crates/nosh-client/src/screen.rs` (lines 121-150), `main.rs` (lines 119-126)
**Apply to:** All reconnection UX
```rust
// Banner text:
let banner = format!(
    "nosh: reconnecting \u{2014} last contact {elapsed}s ago. Press ~. to disconnect."
);

// ~. escape detection:
let quit = bytes.is_empty()
    || bytes == &[0x03]
    || bytes.windows(2).any(|w| w == b"~.");
```

### Token rotation on successful reattach
**Source:** `crates/nosh-client/src/client.rs` (lines 542-560), `main.rs` (lines 1774-1783)
**Apply to:** All reattach success paths
```rust
pub enum ReattachOutcome {
    Ok {
        new_token: [u8; 16],
        replaying_from_seq: u64,
        truncated: bool,
    },
    Err,
}

// Usage:
match outcome {
    ReattachOutcome::Ok { new_token, replaying_from_seq, truncated } => {
        *token_out = Some(new_token);  // Rotate token immediately
        if truncated {
            eprintln!("\r\nnosh: output truncated\r");
        }
        *highest_applied = replaying_from_seq;
        // ... run pump
    }
    ReattachOutcome::Err => {
        *token_out = None;
        eprintln!("\r\nnosh: session ended\r");
        return Ok(PumpOutcome::CleanExit(1));
    }
}
```

---

## No Analog Found

Files with no close match in the codebase (planner should use RESEARCH.md patterns instead):

| File | Role | Data Flow | Reason |
|------|------|-----------|--------|
| None | — | — | All files have strong analogs in the existing v1.1/v1.2 codebase |

---

## Metadata

**Analog search scope:** `crates/nosh-client/src/`, `crates/nosh-server/src/`, `crates/nosh-client/tests/`
**Files scanned:** 12 Rust files (client main.rs, inner_auth.rs, screen.rs, server registry.rs, server.rs, transport tests)
**Pattern extraction date:** 2026-06-14

---

## Key Patterns Summary

1. **Reconnect loop structure:** WebTransport path (lines 1354-1450 in main.rs) — connect with backoff, run inner auth, dispatch fresh or reattach
2. **Inner auth as reconnect gatekeeper:** `run_inner_auth_client` must complete BEFORE `Reattach` is sent (MH-1 ordering)
3. **MH-2 guard:** Server-side `Orphaned → Reconnecting` atomic transition under registry lock (registry.rs:712-718)
4. **Bounded exponential backoff:** 250ms initial, 2x multiplier, 10s cap (constants + loop pattern)
5. **Fatal vs transient errors:** Host-key/TOFU/EKM failures are fatal (no retry); transport errors are transient (backoff retry)
6. **Reconnecting banner:** v1.2 ConnectionLossOverlay with "last contact Xs ago. Press ~. to disconnect."
7. **Token rotation:** Immediate token swap on `ReattachOutcome::Ok` (single-use, D-05)
8. **Test patterns:** WebTransport fixture + concurrent reattach pattern for MH-2 validation
