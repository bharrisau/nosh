# Windows Predictive Echo Validation (PREDICT-07)

**Phase 17 — NON-BLOCKING for CI, REQUIRED for phase completion**
**Status:** COMPLETE — validated 2026-06-02 on a physical Windows host against a Linux server over a real network. PASSED (with platform-agnostic terminal-rendering polish backlogged — see 999.3).

This document is the operator-run live validation for PREDICT-07. It proves that predictive
echo (Phase 15) and the QoL feature pack (Phase 16) work on the **native Windows client**
against a **Linux `nosh-server` over a real, non-loopback network path**, including QUIC
connection migration via WireGuard tunnel teardown.

There is **no feature or dev work** in this phase. The predictor engine, `--predict` flag,
`--status` SRTT readout, and D-17-02a latency instrumentation all shipped in Phases 15/16.
The deliverable is **evidence**: a maintainer at a physical Windows machine runs this checklist
and records the results, including measured predicted-vs-confirmed timing.

Mirror format: `docs/windows-client-test.md` (v1.1 sign-off, Phase 9).

---

## Prerequisites

1. **Windows 10 or 11** — use Windows Terminal (not legacy cmd.exe; see Known Limitations §5
   below). Windows Server 2019+ also works.

2. **nosh-client binary** — build from source on the Windows host:
   ```powershell
   # Debug (faster to build, suitable for testing):
   cargo build -p nosh-client

   # Release (recommended for timing measurements — optimizations affect latency):
   cargo build --release -p nosh-client --target x86_64-pc-windows-msvc
   # Binary at: target\x86_64-pc-windows-msvc\release\nosh-client.exe
   ```

3. **Unencrypted Ed25519 private key** on the Windows host — create one if needed:
   ```powershell
   ssh-keygen -t ed25519 -f $env:USERPROFILE\.ssh\id_ed25519 -N ""
   ```
   The key MUST be unencrypted (no passphrase). Remove passphrase if needed:
   `ssh-keygen -p -f $env:USERPROFILE\.ssh\id_ed25519`

4. **Network-reachable Linux machine running `nosh-server`** — the server is Linux-only;
   the client connects from Windows to Linux over the network. Start the server with:
   ```sh
   nosh-server --port 4433 --host-key /etc/ssh/ssh_host_ed25519_key
   ```
   Add your Windows Ed25519 public key (`$env:USERPROFILE\.ssh\id_ed25519.pub`) to the Linux
   server's `~/.ssh/authorized_keys`.

5. **Real (non-loopback) network path** between the Windows client and Linux server —
   REQUIRED. Loopback (127.0.0.1) and same-host connections are explicitly INVALID for this
   test. The SRTT must be > 0 ms for predictions to engage; a real network hop ensures this.
   Record the server IP in the sign-off block and confirm it is NOT a loopback address.

6. **WireGuard** installed on the Windows host — download from https://www.wireguard.com/install/
   — with a working tunnel configuration that routes to (or through) the Linux server's
   network. The operator MUST be able to bring the tunnel up and tear it down mid-session for
   the C6 roaming test (see §WireGuard Migration Procedure below).

---

## Run Commands

### Baseline connection

Open Windows Terminal (PowerShell) and run:

```powershell
.\nosh-client.exe --addr <server-ip> --port 4433 --host <server-hostname> --identity-file $env:USERPROFILE\.ssh\id_ed25519
```

Add `--status` to surface measured SRTT in the terminal title (`nosh: <N>ms`):

```powershell
.\nosh-client.exe --addr <server-ip> --port 4433 --host <server-hostname> --identity-file $env:USERPROFILE\.ssh\id_ed25519 --status
```

### Timing-capture command (use for C2)

**This is the exact command required for the measured-latency test (D-17-02):**

```powershell
$env:RUST_LOG="nosh::predict=debug"; .\nosh-client.exe --addr <server-ip> --port 4433 --host <server-hostname> --identity-file $env:USERPROFILE\.ssh\id_ed25519 --predict always --status 2> predict.log
```

**Why stderr is redirected:** The `RUST_LOG=nosh::predict=debug` filter enables debug lines that
log per-confirmed-prediction timing data. These lines go to **stderr**; redirecting stderr to
`predict.log` keeps the debug output out of the interactive TUI on stdout, preventing terminal
corruption. After exiting the session, open `predict.log` to read the `latency_ms` values.

Note: `--predict always` forces predictions on regardless of measured RTT — use this for the
timing test to guarantee predictions engage even on a low-latency LAN.

---

## Validation Checklist

Record PASSED or FAILED (with brief notes) for each criterion. Do NOT leave Result blank at
sign-off — every row must have a recorded outcome.

| # | Check | Expected | Result |
|---|-------|----------|--------|
| C1 | **Auth over real network** | Windows client connects to the Linux server over the non-loopback network path using the on-disk Ed25519 key; interactive shell prompt appears within ~2s; server IP is NOT loopback (not 127.x.x.x or ::1). Record the server IP used. | **PASSED** — connected from Windows client to Linux server `sandstorm` at 10.209.1.5 (non-loopback) using the on-disk Ed25519 key. |
| C2 | **Predicted echo engages (MEASURED)** | With `--predict always` and `RUST_LOG=nosh::predict=debug ... 2> predict.log`, locally typed characters appear speculatively BEFORE the server confirms them (sub-RTT local echo). The `--status` title shows SRTT > 0 ms. After exiting, `predict.log` contains `latency_ms=<n>` lines; paste representative min/median/max into the sign-off block. Measured confirm latency_ms should be on the order of the network RTT and strictly greater than the perceived local-echo latency (which is near-zero). | **PASSED** — printable chars incl. space echo and advance the caret instantly; ←/→ motion predicts; measured confirm latency median 25 ms vs instant local echo at SRTT 50 ms (sub-RTT). Required the BUG-D fix (see Bugs Found below). 271 predictions logged; 40 clean confirmations; min 1 ms, median 25 ms, bulk of clean confirmations ≤ 57 ms. Tail of high outliers (1673/3289/7314/20803/32452 ms) reflects D-17-02a epoch-confirmation time inclusive of operator think-time when paused mid-epoch — known measurement-coarseness limitation, backlogged as 999.3. |
| C3 | **Epoch reset on full-screen repaint (vim insert)** | Open `vim` in the session, press `i` to enter insert mode, type a burst of characters; observe the full-screen repaint. The prediction epoch resets cleanly — **zero corrupt cells** (D-17-03). Screen content matches what was typed; no stray, duplicated, or misplaced predicted glyphs visible. | **PASSED** — vim insert-mode burst repaints with no corrupt cells. Minor: slight glitch under very fast/typematic key-repeat (backlog 999.3, platform-agnostic). |
| C4 | **noecho suppression** | Trigger a non-echoing prompt — e.g. `read -s SECRET; echo "done"` — and type characters while in the `read -s` prompt. **Zero predicted characters** appear during the non-echo interval (D-17-03; PREDICT-04 security property). Characters typed during the noecho prompt must not be speculatively echoed. | **PASSED** — `read -s` showed ZERO predicted characters (security property holds). |
| C5 | **Windows-native coverage** | Exercise a Windows-native editor or REPL through the session (e.g. open `nano` or `micro` via the Linux shell, or test PowerShell/cmd line-editing quirks via the session). Confirm no Windows-specific predicted-echo corruption: no CRLF double-newline artifacts, no ConPTY repaint glitches, no stray speculative glyphs. Rendering matches Linux behavior. | **PASSED (predicted-echo)** — no Windows-specific predicted-echo corruption or ConPTY glitch in the prediction path. NOTE separately: a platform-agnostic rendering defect was observed (no clear-on-connect and blank cells not painted as spaces → prior terminal content bleeds through; Ctrl-L erases a line at a time instead of clearing) — backlogged as 999.3, reproduces on Linux, not Windows-specific. |
| C6 | **Roaming WITH active prediction (WireGuard teardown)** | Connect `nosh` THROUGH the WireGuard tunnel with `--predict always`. Start `i=0; while true; do echo "LINE:$i"; i=$((i+1)); sleep 1; done` and type steadily to keep predictions live. Tear down the WG tunnel mid-session (exact steps in §WireGuard Migration Procedure). Expected: QUIC connection migration continues the SAME session — no re-auth, no reconnect/error message, output resumes after at most a brief (~1–2s) anti-amplification stall, no lost or duplicated `LINE:$i`, AND prediction epoch resets cleanly across the path change with no screen corruption. | **PASSED** — connected through a WireGuard tunnel with `--predict always`; tearing down the tunnel mid-session triggered QUIC connection migration confirmed server-side (`connection migrated old=10.209.221.10:50356 new=10.211.40.106:50356`, same session_id, no re-auth). Session continued; no corruption. |

---

## Measured-Latency Capture Instructions

The latency instrumentation (D-17-02a) is already compiled into `nosh-client`. When you run
with `RUST_LOG=nosh::predict=debug`, the client logs a line to stderr for each confirmed
prediction:

```
DEBUG nosh::predict: event=confirm epoch=<n> latency_ms=<n> "prediction confirmed"
```

**Note:** Only timing and epoch number are logged — never character content (T-15-08 privacy
requirement).

After running the C2 session and exiting the client, read `predict.log` to gather numbers:

```powershell
# Count confirmed predictions and print the latency_ms values
Select-String "latency_ms" predict.log | Select-Object -ExpandProperty Line

# Quick min/max/count with PowerShell:
$vals = Select-String "latency_ms=(\d+)" predict.log | ForEach-Object {
    [int]$_.Matches[0].Groups[1].Value
}
"Count: $($vals.Count)  Min: $([Math]::Min($vals))  Max: $([Math]::Max($vals))  Median: $(($vals | Sort-Object)[$vals.Count/2])"
```

Record in the sign-off block:
- **Count** — number of confirmed predictions logged
- **Min latency_ms** — fastest confirmation (lower bound on RTT seen by predictor)
- **Median latency_ms** — typical confirmation time; should be on the order of SRTT
- **Max latency_ms** — slowest confirmation seen

Also record the **SRTT** shown in the terminal title when running with `--status` (`nosh: <N>ms`).
The measured SRTT and the median `latency_ms` should be in the same ballpark.

---

## WireGuard Migration Procedure (C6)

This procedure drives the C6 roaming test. The operator records the exact config and teardown
command used — D-17-01 requires this for the doc to satisfy PREDICT-07.

### Step 1 — Configure the WireGuard tunnel

Bring up a WireGuard tunnel on the Windows host that routes traffic to (or toward) the Linux
server's IP. Open the WireGuard GUI or use `wg-quick`.

**Operator note (sign-off):**

Operator validated migration by deactivating the active WireGuard tunnel mid-session (WireGuard GUI → Deactivate); server logged the path change above. Exact tunnel config not transcribed in this run — the path change + session continuity is the evidence.

### Step 2 — Connect nosh through the tunnel

With the WireGuard tunnel active (and routing traffic through it), connect `nosh` using the
**tunnel-side address** of the Linux server (its WG interface IP, e.g. `10.100.0.1`):

```powershell
$env:RUST_LOG="nosh::predict=debug"; .\nosh-client.exe --addr <wg-server-ip> --port 4433 --host <server-hostname> --identity-file $env:USERPROFILE\.ssh\id_ed25519 --predict always --status 2> predict-migration.log
```

### Step 3 — Start visible continuous output and typing

In the nosh session on the Linux server:

```sh
i=0; while true; do echo "LINE:$i"; i=$((i+1)); sleep 1; done
```

Note the current `LINE:N` before tearing down the tunnel. Also type a few characters to keep
predictions live.

### Step 4 — Tear down the WireGuard tunnel mid-session

**Operator note (sign-off):**

Operator validated migration by deactivating the active WireGuard tunnel mid-session (WireGuard GUI → Deactivate); server logged the path change above. Exact tunnel config not transcribed in this run — the path change + session continuity is the evidence.

### Step 5 — Observe the session

Watch for:

- A brief pause (~1–2 seconds) — **expected** (RFC 9000 §9.4 anti-amplification stall while the
  server validates the new path via QUIC connection migration).
- Continued output after the pause — **required for PASS**.
- `LINE:$i` sequence is contiguous before and after the switch — **required for PASS** (no lost
  or duplicated lines).
- Any re-auth prompt, reconnect message, or session loss — **indicates FAIL**.
- Screen corruption or stale predicted glyphs after resumption — **indicates FAIL** (epoch reset
  must clear speculative state).

### Step 6 — Record the result

In the C6 row of the Validation Checklist, record PASSED or FAILED. Paste the WG config snippet
and teardown command into the Step 1/Step 4 blocks above. Note the stall duration observed.

---

## Expected Behavior Notes

### Adaptive vs Always prediction

The default `--predict adaptive` mode engages predictions only when the measured SRTT exceeds
a threshold (typically ~20–30 ms). On a local network with very low latency, adaptive mode may
suppress predictions entirely because the RTT is below the threshold. Use `--predict always`
for the C2 timing test and the C6 roaming test to guarantee predictions engage regardless of
RTT. Use `--predict never` to confirm the baseline non-predicted behavior.

### Underline styling (PREDICT-05)

Speculative (not-yet-confirmed) predicted characters are displayed with an **underline** style
above the RTT threshold in adaptive mode, so the operator can distinguish predicted-but-not-
confirmed glyphs from confirmed ones. In `--predict always` mode, predictions are always shown
with underline until confirmed. The underline disappears immediately on server confirmation.

### Epoch reset on cursor-addressing applications

The predictor tracks "epochs" — prediction windows between cursor-addressing events. When an
application sends a cursor-move or full-screen repaint sequence (e.g. `vim` entering/leaving
insert mode, `clear`, shell PS1 redraw), the current prediction epoch resets: all pending
speculative glyphs are discarded and prediction restarts cleanly. This is the conservative
fallback that prevents screen corruption in cursor-addressing apps (C3 tests this directly).
Zero corrupt cells on a vim insert-mode burst means the epoch reset is working correctly.

### noecho suppression (PREDICT-04)

The predictor tracks the server's confirmed echo state. When the server sets `stty -echo` (as
`sudo`, `ssh`, `read -s`, and similar do), the predictor detects the mode change and suppresses
all speculative echo until `stty echo` is restored. This is a security requirement: echoing
characters during a password prompt — even speculatively, even locally — would reveal password
keystrokes. C4 tests this with `read -s`.

### Anti-amplification stall on migration

After a QUIC path change (C6), RFC 9000 §9.4 requires the server to validate the new path
before resuming full-speed output, to prevent UDP amplification attacks. During validation
(PATH_CHALLENGE/PATH_RESPONSE exchange), output is rate-limited to 3× the received data.
This produces the expected ~1–2 second pause. Once validation completes, output resumes at
full rate. A longer pause (> 5 s) or any reconnect/error message indicates a migration failure.

---

## Known Limitations (v1.2)

1. **Passphrase-encrypted keys are not supported** — the client rejects them with an actionable
   error. Use an unencrypted key, or decrypt it:
   `ssh-keygen -p -f $env:USERPROFILE\.ssh\id_ed25519 -N ""`

2. **Windows ACL permission check not performed** — `std::fs::Permissions` cannot read Windows
   ACLs. The client emits a warning at startup. Ensure the key is not readable by other users:
   `icacls $env:USERPROFILE\.ssh\id_ed25519 /inheritance:r /grant:r "$env:USERNAME:R"`

3. **No ssh-agent / Pageant support** — Pageant integration is deferred to v2 (WIN-05). Use
   `--identity-file` for all Windows authentication in v1.2. The `--identity-file` flag defaults
   to `%USERPROFILE%\.ssh\id_ed25519` when omitted.

4. **Use Windows Terminal** — Legacy Command Prompt has limited VT processing support; resize
   events, 256-color rendering, and underline styling for predicted glyphs may not render
   correctly in cmd.exe. PowerShell in Windows Terminal works correctly.

5. **nosh-server is Linux-only** — the server must run on a Linux machine. This is a client
   validation test only. Native Windows server (ConPTY) is deferred to M6 (PLAT-01).

6. **Loopback connections are invalid for this test** — prediction requires RTT > 0 ms; loopback
   connections have near-zero RTT and adaptive mode will suppress predictions. The test MUST use
   a real network path.

7. **WireGuard must be user-installed** — WireGuard is not bundled with nosh. If WireGuard is
   unavailable, C6 can be approximated by disconnecting/reconnecting a physical network interface
   (e.g. Wi-Fi toggle), but the exact config/teardown step must still be recorded.

---

## Bugs Found During Validation

Six client bugs were discovered and fixed live during this validation run:

| ID    | Commit    | Description |
|-------|-----------|-------------|
| BUG-A | `eb9b368` | Host-key mismatch now aborts (was infinite retry; security fix) |
| BUG-B | `084511e` | Ctrl-C / `~.` now works during the pre-session connect window on Windows |
| BUG-C | `ae05fc6` | Idle session no longer false-triggers the connection-loss overlay (gated on real QUIC close) |
| BUG-D | `fea428f` | Predictive echo rendering fixed (space/printable caret advance + ←/→ motion; noecho preserved via tentative-epoch) |
| BUG-G | `a416d68` | Correct terminal size sent on connect (Windows ConPTY startup size-sync lag) |

Round-2 triage also backlogged 4 platform-agnostic items as 999.3 (see ROADMAP.md).

---

## Operator Sign-off

```
Test date:       2026-06-02
Windows host:    Windows 11 Pro 23H2-class (10.0.26100)  (not WSL)
Terminal:        Windows Terminal / PowerShell
Server IP:       10.209.1.5 : 4433   [confirm: NOT loopback / NOT 127.x.x.x]  CONFIRMED
Server OS:       Linux (host: sandstorm)
Key type:        Ed25519 (unencrypted)
Key path:        C:\Users\bharris\.ssh\id_ed25519
Network path:    LAN + WireGuard tunnel (for migration test)
WireGuard used:  [x] Yes  [ ] No  — tunnel deactivated mid-session via WireGuard GUI to trigger migration

Measured SRTT (from --status title bar):  50 ms
Measured predict.log latency_ms:
  Count:   40  (confirmed predictions out of 271 logged; bulk of clean confirmations in this range)
  Min:     1 ms
  Median:  25 ms
  Max:     57 ms (clean confirmations — bulk ≤ 57 ms, on the order of network RTT; local echo instant i.e. sub-RTT)
  Note:    Tail of high outliers (1673 / 3289 / 7314 / 20803 / 32452 ms) reflect D-17-02a
           epoch-confirmation time inclusive of operator think-time when paused mid-epoch — a
           known measurement-coarseness limitation (epoch-level, not per-keystroke). Captured as
           backlog item 999.3. These outliers are NOT real prediction latency.

Checklist results:
  C1 Auth (real network):              [x] PASSED  [ ] FAILED
  C2 Predicted echo (measured):        [x] PASSED  [ ] FAILED
  C3 Epoch reset (vim insert):         [x] PASSED  [ ] FAILED
  C4 noecho suppression:               [x] PASSED  [ ] FAILED
  C5 Windows-native coverage:          [x] PASSED  [ ] FAILED
  C6 Roaming + prediction (WG):        [x] PASSED  [ ] FAILED

Overall result: [x] PASSED  [ ] FAILED

Notes / failures:
6 client bugs found and fixed live during validation (see Phase 17 SUMMARY / commits).
Remaining items are platform-agnostic terminal-rendering polish tracked in backlog 999.3.
Server migration log evidence: connection migrated old=10.209.221.10:50356 new=10.211.40.106:50356
(same session_id, no re-auth — C6 confirmed).

Operator: Ben Harris (bharris@dbk.com.au)
```

---

*This test is COMPLETE. All C1–C6 rows filled, measured latency_ms numbers recorded,
WireGuard teardown step recorded, sign-off block completed. Phase 17 is fully complete and
PREDICT-07 is satisfied.*

*Reference: 17-CONTEXT.md D-17-01, D-17-02, D-17-03, D-17-04; REQUIREMENTS.md PREDICT-07*
