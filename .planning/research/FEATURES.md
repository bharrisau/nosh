# Feature Research

**Domain:** Roaming remote shell — M4 Predictive Echo + Daily-Driver QoL (nosh v1.2)
**Researched:** 2026-06-01
**Confidence:** HIGH (Mosh source-level research, Mosh paper, upstream issue tracker, ET docs, SSH man pages, OSC 52 ecosystem research; all claims attributed)

---

## Framing

This document is scoped to **v1.2 (M4)**: adding predictive local echo (the headline
differentiator), connection-loss UX, and a research-selected set of QoL wins to the already-working
v1.1 QUIC shell with roaming and Windows client. All v1.0/v1.1 table-stakes (PTY, auth, I/O,
resize, env sanitization, migration, cold reattach, Windows client) are shipped and are not
re-listed unless they have direct v1.2 interactions.

tmux integration is **explicitly excluded** from scope per PROJECT.md. Do not recommend it.

---

## Feature Landscape

### Table Stakes (Users Expect These)

Features that define whether the M4 milestone is complete. Missing any one means the claimed
feature does not work.

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| **Speculative local echo: normal typing** | Any tool claiming to be a "Mosh successor" must predict typed characters before server confirmation. Without this, nosh provides no UX advantage over SSH on latency. Mosh has trained users to expect instant feedback when typing. | HIGH | The client runs a copy of the terminal state model (vte) locally. For each printable keystroke, it predicts the character will appear at the cursor position and renders it immediately without waiting for the server. Confirmed on the next server frame update; wrong predictions corrected within one RTT. |
| **Speculative local echo: backspace** | Backspace is the second most common "editing" key after printable characters. If it does not speculate, the UX is visibly uneven — typed chars appear instantly, deletions lag. | HIGH | The client predicts that the character before the cursor will be deleted and the cursor will move left one column. Requires the local terminal model to track cursor position. |
| **Speculative local echo: left/right arrow cursor motion** | Mosh explicitly supports left/right arrow keys for cursor repositioning. This makes line editing (moving within a typed command) feel instant, not laggy. | HIGH | The client predicts cursor column movement. The prediction is cell-based — the cursor moves left or right one cell. The server confirms or corrects on the next frame. |
| **Unconfirmed prediction rendering: underline** | On high-latency or glitchy links (RTT > 80 ms), Mosh underlines predicted-but-unconfirmed characters so the user knows these are speculative. This is the established visual convention; users who know Mosh expect it. Without it, wrong predictions are silent failures — the screen just jumps when correction arrives. | MEDIUM | Underline is applied to cells in the predicted overlay that have not yet been confirmed by the server. The underline flag is set on the cell attribute in the local terminal model. Removal of the underline happens atomically when the server frame confirming that epoch arrives. |
| **Prediction epochs: reset on control characters** | When the user presses ESC, Enter, Ctrl-C, or up/down arrows, the current prediction epoch ends. The client stops displaying new predictions until at least one prediction from the *new* epoch has been confirmed by the server (a "confirmed correct" signal). This is the mechanism by which Mosh avoids echoing passwords and wrong command completions. | HIGH | An epoch is a monotonically increasing counter tied to the `local_frame_sent` value. Each control character or line-ending key increments the epoch and sets predictions for that epoch to "tentative." Tentative predictions are still tracked internally but are not rendered until the server confirms one from the new epoch. |
| **Conservative fallback: no display in non-echo contexts** | Mosh's model requires "a previous prediction on the same row of the terminal has been confirmed by the server, without any intervening control character keystrokes." On the first keystroke of a new line, the client does not display a prediction — it waits for the server to confirm the first character, then enables prediction for subsequent characters on that row. This prevents echoing passwords (sudo, ssh passphrase) since `stty -echo` suppresses the server echo, so no confirmation ever arrives and predictions stay hidden. | HIGH | The per-row confirmed-prediction requirement is the primary safety mechanism. It means: (a) prediction never activates on a fresh terminal line until the server has confirmed the row is echo-enabled; (b) full-screen apps like vim where the cursor is constantly repositioned rarely satisfy the "confirmed on this row without intervening control chars" condition, so prediction naturally suppresses in those contexts. |
| **Adaptive mode: engage only when RTT exceeds threshold** | Mosh's default `--predict adaptive` shows predictions only when the link is slow (RTT > ~30 ms, disable below ~20 ms). On a LAN where round trips are sub-10 ms, predictive echo adds visual noise without measurable benefit. | MEDIUM | RTT thresholds: prediction display activates above `SRTT_TRIGGER_HIGH` (~30 ms), deactivates below `SRTT_TRIGGER_LOW` (~20 ms). Underline (flagging) activates above `FLAG_TRIGGER_HIGH` (~80 ms), deactivates below `FLAG_TRIGGER_LOW` (~50 ms). Hysteresis between thresholds prevents toggling. Adaptive is the right default; `--predict always` and `--predict never` are user overrides. |
| **Server confirms frame via datagram** | The datagram path (RFC 9221) carries server terminal state diffs back to the client. This is what triggers prediction confirmation or correction. The reliable stream carries control; datagrams carry the terminal state so loss is tolerated (latest-state-wins). Without the datagram feedback loop, there is no confirmation mechanism and predictions stay underlined indefinitely. | HIGH | This is the foundational architectural integration: datagrams carry a compact diff of the server's terminal state (the `vte`-based model on the server, serialised). The client receives a frame, applies it to the local authoritative model, and compares it against the predicted overlay. Cells that match are confirmed; cells that differ cause the overlay to be discarded for that region and the authoritative state rendered instead. |
| **Connection-loss notice: on-screen overlay** | When the server has not been heard from for several seconds, the user must see a visible, non-intrusive notice that the link is down and nosh is waiting to reconnect. Mosh shows this as a blue banner at the top of the screen ("mosh: Last contact X seconds ago"). Without any indicator, the user cannot distinguish "server is thinking" from "network is down" and will likely start smashing keys in confusion. | MEDIUM | Mosh's banner is a single line at the top of the terminal content area (not an OS dialog). It reads e.g. "nosh: reconnecting — last contact 12 s ago". It must not displace terminal content permanently — it overlays row 0 while the session is orphaned and is removed when the connection resumes. The exact text should include the elapsed time, which increments. |
| **Connection-loss notice: abort instructions** | When the connection has been lost for long enough that the user may want to give up, the banner must tell them how to forcibly disconnect. Mosh shows "Press Ctrl-^ . to disconnect." Without this, users who do not know the escape sequence are stuck — they cannot `exit` because the shell is unreachable. | LOW | Displayed in the same banner after a configurable threshold (e.g. 5+ seconds disconnected). The escape sequence for nosh should follow the established convention nosh v1.1 already implemented: `~.` (tilde-dot, matching SSH and the already-shipped nosh v1.1 `~.` quit). |
| **Prediction never engages during the initial epoch** | On the very first keystroke of a session, there are no prior confirmed predictions. The client must not display a prediction on row/column zero immediately — it must wait for at least one server confirmation. | MEDIUM | Startup state: epoch 0, no confirmed cells. Until the first confirmation arrives from the server, all predictions are tentative and hidden. Only after the server's first frame lands and confirms at least one cell does the engine start displaying predictions (subject to the RTT threshold). |

### Differentiators (Competitive Advantage)

Features that make nosh meaningfully better than Mosh/ET/SSH for this milestone's goals.

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| **Predictive echo on QUIC datagrams (not custom UDP)** | Mosh's SSP uses a bespoke UDP protocol with its own encryption layer. nosh's datagram path is QUIC RFC 9221 — the same authenticated, encrypted QUIC connection already used for the control stream. No second credential, no second port. This is architecturally cleaner and NAT-friendlier than SSP. | HIGH (frame format design) | The main new design work vs Mosh is the frame format for terminal state diffs over QUIC datagrams, and the client-side overlay engine. The QUIC layer itself is already working. |
| **`--predict always/adaptive/never` user override** | Mosh users know these flags and expect them. Shipping the same three modes removes a migration friction. `always` is useful for developers who want zero-latency feedback even on LAN. `never` is useful for debugging weird rendering artifacts. | LOW | These map directly to the SRTT-threshold logic: `always` bypasses the RTT check; `never` disables the overlay engine entirely; `adaptive` is the default with hysteresis. |
| **Underline only when flagging, not always** | Mosh only underlines predictions when RTT > ~80 ms. Below that threshold, predictions are shown without underline (no visual noise). This is a key UX subtlety: on a moderately fast link, the user gets instant echo with no visual distraction; only on a genuinely slow or lossy link does the underline appear as a signal. | LOW | Implemented by the FLAG_TRIGGER hysteresis logic. Predictions render as normal text when RTT < FLAG_TRIGGER_LOW; they render with an underline attribute when RTT > FLAG_TRIGGER_HIGH. |
| **Connection-loss timer ticking in the banner** | Showing elapsed seconds since last contact ("12 s ago", "1m 34s ago") tells the user how bad the situation is. Mosh does this. A static "disconnected" message is less useful — the ticking timer provides urgency and context. | LOW | The client increments a counter since the last received server datagram and renders it in the banner overlay each redraw cycle (e.g. every 1 second). Uses wall-clock time, not a tokio timer. |
| **OSC 52 clipboard passthrough** | On a daily-driver basis, copying text from a remote vim/neovim session to the local clipboard without X11 forwarding is one of the most frequent friction points for remote shell users. OSC 52 lets the remote application set the local clipboard via a terminal escape sequence. nosh forwards the sequence to the local terminal. Users of neovim, helix, and lazygit all benefit. | MEDIUM | The server receives the OSC 52 escape sequence from the PTY output stream (as part of the shell's output). nosh must detect and forward it over the reliable stream to the client, which re-emits it to the local terminal. The local terminal (Windows Terminal, WezTerm, iTerm2) then handles the clipboard write. Mosh added OSC 52 support in v1.4.0; nosh should not ship without it if it claims "daily-driver" UX. Note: Mosh's OSC 52 is limited to one UDP packet's worth of data; nosh's reliable-stream forwarding has no such limitation. |
| **Terminal title propagation (OSC 0/2 passthrough)** | Shell prompts commonly emit `ESC]0;user@host:~/path BEL` to set the terminal title. Over SSH this works automatically. Remote shell tools that strip OSC sequences break tab titles in Windows Terminal, iTerm2, WezTerm, etc. — a daily friction for users with multiple sessions. | LOW | nosh already passes terminal output as bytes from the PTY through the reliable stream. OSC 0/2 sequences need to be passed through rather than stripped. The implementation is a policy choice (don't strip) rather than new code, unless the server side is actively filtering. Verify current behavior and un-filter if needed. |
| **Reconnect automatically on cold reattach (already shipped in v1.1)** | nosh's cold reattach (already shipped) means the user never needs to type a command to resume a session after a network outage. The UX comparison: SSH requires the user to kill the hung session and run `ssh` again; ET reconnects but shows no UI during the process; Mosh has no reattach at all. nosh's existing 1-RTT reattach is a differentiator that v1.2 should not regress on. | N/A (already shipped) | Verify that the v1.2 reconnect path still works correctly when the client comes back up after a longer outage (cold reattach through the new session machinery). |

### Anti-Features (Commonly Requested, Often Problematic)

| Feature | Why Requested | Why Problematic | Alternative |
|---------|---------------|-----------------|-------------|
| **Predictive echo for all control sequences** | "Predict vim commands, terminal escape sequences, etc." — reduces all latency. | Mosh's design lesson is explicit: the model must "prove itself anew on each row of the terminal and after each control character." Predicting control sequences (ESC, CSI) requires a full terminal emulator in the client and still fails in complex cases (multiline inserts, cursor addressing in fullscreen apps, readline's right-prompt). The prediction error rate becomes high enough to cause persistent screen corruption, not just occasional glitches. | Predict only the narrow set Mosh proved safe: printable characters, backspace, left/right arrows. Trust the epoch reset to suppress predictions in complex contexts. |
| **Persistent status bar at bottom of screen** | "Show connection quality, RTT, session info always." | Requires stealing a terminal row permanently, which breaks programs that assume fixed terminal height (vim, htop, any ncurses app). The row-stealing approach is exactly why Mosh's status bar has been controversial. Mosh only shows the banner *when disconnected*, not always. | Show the connection-loss banner only when the link is actually down (overlaying row 0 temporarily, restoring when link resumes). A `--status` flag could show RTT in the title bar (OSC 0/2) as a non-intrusive alternative. |
| **OSC 52 clipboard *read* (paste from local to remote)** | "I want the remote vim to paste from my local clipboard." | OSC 52's "get" operation (asking the terminal to return clipboard contents) is a security hole — any rogue remote process can silently exfiltrate the user's clipboard. Most terminal emulators disable it by default. Implementing it in nosh would require an explicit user opt-in and is not a daily-driver win. | OSC 52 write-only (setting remote clipboard from remote app → local terminal). This is the safe, common operation. "Get" is out of scope. |
| **Predictive up/down arrow (command history preview)** | "Show the command that up-arrow will recall before the server responds." | Up/down arrows in bash/zsh/fish invoke readline's history recall, which depends entirely on the server's history state. The client cannot predict this without syncing the history state, which is a different and much more complex protocol. Mosh explicitly *resets the epoch* on up/down arrow (they are excluded from the prediction set precisely because they are not predictable). | Reset epoch on up/down arrow (epoch reset behavior). Do not attempt history-recall prediction. |
| **Named session listing and selection** | "List my sessions and pick which one to reattach to — like tmux." | Requires a session enumeration protocol (leaks session metadata to any client that can reach the server), a selection UI in the client, and complicates the cold reattach handshake. The v1.1 reattach model is one-session-per-identity; extending it to named sessions is an M5 concern. | Auto-reattach to the most recent orphaned session for the authenticated identity. Named sessions deferred to M5. |
| **tmux integration** | "Integrate with tmux for multiplexing." | Explicitly excluded per PROJECT.md scope. tmux integration conflicts with the "native scrollback" story (tmux has its own scrollback, not the terminal's), creates complex passthrough requirements for OSC sequences, and adds dependency on a third tool. | nosh handles multiplexing natively in M5 (control-first channel multiplexing from the INIT.md design). Use nosh's own channels, not tmux. |
| **Predictive echo on the Windows client for the v1.2 milestone** | "Since we're adding predictive echo, add it to the Windows client too." | The datagram path (RFC 9221) and the client-side prediction engine both need to work on Linux first. The Windows client (`nosh-client`) will need updates to the local terminal model, but the initial predictive echo implementation should be Linux-client-first to contain scope. | Add Windows client predictive echo as a v1.2 stretch goal or defer to a point release once Linux predictive echo is validated. |

---

## Feature Dependencies

```
[v1.1 validated stack: QUIC, auth, PTY, I/O, resize, env sanitization, migration,
 cold reattach, Windows client, session persistence, output ring-buffer]
    └──prerequisite for──> [all v1.2 features]

[QUIC RFC 9221 datagrams (already working from v1.0 TRANS-02)]
    └──required by──> [Datagram state sync: server → client terminal diffs]
    └──required by──> [Server confirmation for prediction epochs]

[Server-side terminal state model (vte)]
    └──required by──> [Datagram frame: compact terminal diff serialisation]
    └──required by──> [Server sends diffs when PTY output changes the terminal state]

[Client-side terminal state model (local vte copy)]
    └──required by──> [Speculative overlay engine (predicting typed chars)]
    └──required by──> [Confirmation logic: compare received frame against overlay]

[Speculative overlay engine]
    └──required by──> [Printable char prediction]
    └──required by──> [Backspace prediction]
    └──required by──> [Left/right arrow prediction]
    └──required by──> [Epoch tracking and tentative prediction hiding]
    └──required by──> [Underline rendering for unconfirmed predictions]
    └──required by──> [Adaptive RTT threshold logic]

[Datagram feedback loop (server diffs → client)]
    └──required by──> [Prediction confirmation / correction]
    └──enables──> [Adaptive mode RTT measurement (SRTT from datagram round-trips)]

[Connection-loss detection (no datagram received in N seconds)]
    └──required by──> [On-screen reconnecting banner]
    └──required by──> [Elapsed-time counter in banner]
    └──required by──> [Abort instructions in banner]

[Existing ~. quit escape (already shipped in v1.1)]
    └──reused by──> [Abort instructions in the connection-loss banner]

[OSC 52 detection in PTY output stream (server side)]
    └──required by──> [OSC 52 clipboard passthrough to client]

[OSC 0/2 passthrough (server side — policy, not new code)]
    └──required by──> [Terminal title propagation]
```

### Dependency Notes

- **Datagram loop is the critical path for predictive echo**: The vte-based server terminal model
  must serialize diffs; the client must deserialize and apply them. This is all new code in v1.2
  and the highest complexity area.
- **Client-side vte model is independent of server-side**: The client needs its own terminal model
  for the prediction overlay. The server and client terminal models converge via the datagram
  frames, not by sharing state.
- **Connection-loss banner is independent of predictive echo**: It depends only on the datagram
  receive timer (no datagram in > ~5 s = connection lost). It can be implemented and tested before
  the full prediction engine is working.
- **OSC 52 and title propagation are nearly independent**: Both are server-side PTY-output
  passthrough policies. They share the "detect special escape sequence in PTY output stream"
  mechanism but are independent features. Both can be added in a single pass.
- **Abort instructions reuse v1.1's `~.` escape**: The banner just needs to tell the user to type
  `~.` — the handler is already implemented. No new escape-sequence handling needed.

---

## Predictive Echo Behavior Reference

This section is user-centric (not implementation) — for requirements writers and UX reviewers.

### What Good Predictive Echo Feels Like

On a link with 80–150 ms RTT (typical mobile, VPN, cross-continental SSH): typing feels **instant
and local**. Typed characters appear on screen the moment the key is pressed, not 80 ms later.
Backspace deletes immediately. Left/right arrow repositions the cursor immediately. The terminal
feels like a local shell.

The only visual indication that you are on a remote session is a subtle **underline** on characters
that have not yet been confirmed by the server. On a 100 ms link this means characters are
underlined for roughly one round-trip, then the underline disappears as the server frame arrives.
On a 200+ ms link the underline may be visible for longer but characters still appear instantly.

When predictions are wrong (estimated at ~0.9% of keystrokes in Mosh's published data), the
correction is **silent and fast**: the server frame replaces the incorrect prediction within one
RTT. There is no flash, no beep, no modal. The user may not notice it happened.

### What "Conservative" Means in Practice

Prediction goes conservative (stops displaying unconfirmed output) in these situations:

1. **After a control character** (ESC, Enter, Ctrl-C, Ctrl-D, up/down arrows, any non-printing
   key): the epoch resets. No predictions are displayed until the server confirms at least one
   keystroke from the new epoch. This naturally suppresses prediction inside vim normal mode
   (ESC takes you there), during password entry (Ctrl-C or Enter starts the non-echo context,
   and since the server never echoes, no confirmation ever arrives), and after command execution
   (Enter resets the epoch; prediction only starts again once the shell prompt is shown and the
   server confirms the first character after the prompt).

2. **On the first keystroke of a new terminal row**: No prior confirmed predictions on that row
   means no display. The user types the first character, the server echoes it (within one RTT),
   confirmation arrives, and subsequent characters on the same row are predicted. This is
   transparent on fast links.

3. **In full-screen apps** (vim, htop, less, etc.): These apps use cursor-addressed output (CSI
   sequences), constantly reposition the cursor, and emit non-printable control sequences. The
   epoch reset on every control character means predictions almost never pass the "confirmed on
   this row without intervening control chars" test. In practice, predictive echo is effectively
   disabled inside vim — the user types keys and waits for the server response, same as with plain
   SSH. This is correct behavior, not a bug.

4. **During `adaptive` mode on a fast LAN** (RTT < ~20 ms): The prediction overlay is hidden
   entirely. No underlines, no speculative characters. The terminal looks and feels identical to
   a plain SSH session. Prediction only activates as the link degrades.

### What the Connection-Loss Banner Looks Like

Mosh's established UX — which nosh should follow:

- **Location**: A single line overlaid at the top of the terminal content (row 0). Does not
  permanently consume a row — it appears on top of the existing content while the link is down
  and is removed when the link resumes.
- **Color**: Mosh uses reverse-video (e.g. white text on blue background) for high visibility
  without requiring a separate UI element.
- **Content**: "nosh: reconnecting — last contact Xs ago. Press ~. to disconnect."
  The counter increments each second. After a threshold (e.g. 5 s), the abort instruction
  appears. At shorter intervals, just the reconnecting notice is shown without the abort hint
  (to avoid alarming users on a brief glitch).
- **Removal**: When the next server datagram arrives, the banner disappears and the full
  terminal content is re-rendered. No confirmation required from the user.

---

## MVP Definition

### v1.2 Launch Requirements (M4 done when these pass)

- [ ] **Datagram terminal state sync**: server sends compact diffs of terminal state over QUIC
  datagrams; client applies them to a local authoritative terminal model; diffs are idempotent and
  latest-state-wins (loss-tolerant)
- [ ] **Predictive echo: printable characters**: typed printable character appears at cursor
  position immediately, without waiting for server; incorrect predictions corrected silently within
  one RTT
- [ ] **Predictive echo: backspace**: backspace deletes the character before the cursor immediately
- [ ] **Predictive echo: left/right arrow**: cursor moves left/right immediately (cursor motion
  prediction)
- [ ] **Unconfirmed rendering: underline**: cells in the prediction overlay that have not been
  confirmed by the server render with underline attribute; underline is only applied when RTT
  exceeds ~80 ms (FLAG_TRIGGER_HIGH); removed when confirmed
- [ ] **Prediction epochs**: control characters (ESC, Enter, Ctrl-C, up/down arrows) reset the
  prediction epoch; no new predictions are displayed until the server confirms a keystroke from the
  new epoch
- [ ] **Conservative fallback**: prediction does not engage on a fresh terminal row until the
  server has confirmed at least one character on that row without intervening control chars; this
  naturally suppresses prediction during password entry and in full-screen apps
- [ ] **Adaptive mode (default)**: prediction display activates only when SRTT > ~30 ms; disabled
  below ~20 ms; `--predict always` and `--predict never` CLI flags override
- [ ] **Connection-loss banner**: when no server datagram received for > ~5 s, overlay a single
  line at terminal row 0 with "nosh: reconnecting — last contact Xs ago"; counter increments each
  second; banner removed when server contact resumes
- [ ] **Abort instructions**: banner includes "Press ~. to disconnect" after threshold (5+ s)
- [ ] **OSC 52 clipboard passthrough**: server detects OSC 52 sequences in PTY output; forwards
  them over the reliable stream to the client; client re-emits to the local terminal; write-only
  (no OSC 52 get)
- [ ] **Terminal title propagation**: OSC 0/2 sequences from PTY output are passed through to the
  client and re-emitted to the local terminal (no stripping); verify and un-filter if currently
  stripped

### Add After Validation (Post-v1.2 / Point Release)

- [ ] **Windows client predictive echo**: extend the client-side prediction engine to the Windows
  client; defer until Linux predictive echo is validated
- [ ] **`--predict experimental` mode**: aggressive prediction with documented higher error rate;
  useful for research/debugging; not appropriate as a default
- [ ] **RTT/latency in terminal title** (`--status`): show current RTT in OSC 0/2 window title as
  a non-intrusive connection-quality indicator; low complexity once SRTT measurement is in place
  from the prediction engine
- [ ] **Host identification in connection-loss banner**: Mosh bug #1049 notes that the banner
  doesn't identify *which* connection is down when running multiple sessions. Add hostname to the
  banner text.

### Future Consideration (M5+)

- [ ] **Native scrollback sync**: full scrollback buffer mirrored to the client (full M5 version);
  PROJECT.md explicitly scopes only "lightweight scrollback" as a candidate for v1.2 research —
  the lightweight read: OSC 52/terminal title are the v1.2 candidates; full scrollback is M5
- [ ] **Named/numbered session listing**: pick a specific orphaned session by name; M5
- [ ] **Agent and port forwarding**: dedicated channels per INIT.md design; M5
- [ ] **Bell/system notification passthrough (OSC 9)**: forward terminal bell to a local desktop
  notification; useful for long-running remote commands; M5+ or a point release (low complexity
  once the OSC sequence passthrough mechanism is in place from OSC 52)

---

## Feature Prioritization Matrix

| Feature | User Value | Implementation Cost | Priority |
|---------|------------|---------------------|----------|
| Datagram terminal state sync (server → client diffs) | HIGH (foundational for prediction) | HIGH (new subsystem: diff format, serialization, loss handling) | P1 |
| Client-side prediction overlay engine | HIGH (the headline feature) | HIGH (local vte model + speculative overlay + epoch logic) | P1 |
| Predictive echo: printable chars + backspace + arrows | HIGH (headline UX) | MEDIUM (once overlay engine exists, specific predictors are straightforward) | P1 |
| Unconfirmed rendering (underline) | HIGH (table stakes per Mosh paper) | LOW (cell attribute on the overlay) | P1 |
| Prediction epochs (control char reset) | HIGH (safety mechanism, prevents password echoing) | MEDIUM (epoch counter + tentative state) | P1 |
| Conservative fallback (fresh row, full-screen apps) | HIGH (correctness, no false echoes) | MEDIUM (per-row confirmed state tracking) | P1 |
| Adaptive mode + `--predict` CLI flag | MEDIUM (ergonomic but not blocking) | LOW (RTT threshold logic; flag parsing) | P1 |
| Connection-loss banner with counter | HIGH (daily driver table stakes — users must know link is down) | LOW (overlay row 0 from a timer) | P1 |
| Abort instructions in banner (`~.`) | HIGH (escape valve when reconnect stalls) | LOW (reuses existing `~.` handler) | P1 |
| OSC 52 clipboard passthrough | HIGH (daily driver, eliminates a major friction point) | MEDIUM (detect in PTY stream, forward on reliable stream, emit on client) | P1 |
| Terminal title propagation | MEDIUM (polish, multi-session clarity) | LOW (policy: don't strip OSC 0/2) | P2 |
| Windows client predictive echo | MEDIUM (parity with Linux client) | HIGH (separate from Linux client scope) | P3 |
| RTT indicator in terminal title | LOW (nice-to-have visibility) | LOW (OSC 0/2 + SRTT already measured) | P3 |
| Host ID in connection-loss banner | LOW (multi-session use case) | LOW (add hostname string to banner text) | P3 |
| Bell/system notification (OSC 9) | LOW (rarely needed; most users use `wait` in a shell script) | LOW (OSC passthrough pattern) | P3 |

**Priority key:**
- P1: Must have for v1.2 launch (M4 done)
- P2: Should have; add when main prediction engine is validated
- P3: Stretch / future point release

---

## Competitor Feature Analysis

| Feature | Mosh 1.4.x | Eternal Terminal | Plain SSH + autossh | nosh v1.2 |
|---------|------------|-----------------|---------------------|-----------|
| Predictive local echo | Yes — printable chars, backspace, left/right arrows; underline when flagged; adaptive/always/never modes; epoch-based conservative fallback | No | No | Yes — same model; QUIC datagrams instead of custom UDP SSP |
| Echo: full-screen apps (vim) | Conservative — epoch resets suppress prediction, effectively disabled in vim normal mode | N/A | N/A | Same conservative behavior |
| Connection-loss indicator | Yes — blue banner top of screen, "last contact X seconds ago", "Press Ctrl-^ . to disconnect" | Silent — ET caches keystrokes silently, no user-visible indicator during outage | No (SSH hangs silently) | Yes — overlay banner, "last contact Xs ago", "Press ~. to disconnect" |
| Clean disconnect when link is down | Yes — Ctrl-^ . | No documented escape | Yes — SSH `~.` (but requires a new TCP connection, which also hangs) | Yes — `~.` (already shipped v1.1) |
| OSC 52 clipboard passthrough | Yes (since v1.4.0) — write-only; limited to ~1 UDP packet size | Unknown | No (blocked by SSH protocol) | Yes — write-only; no size limit (reliable stream) |
| Terminal title propagation | Partial — OSC sequences have historically been inconsistent in Mosh | Passes through | Yes (via pty) | Yes — pass-through (verify not stripped) |
| Session persistence | No reattach | Yes — TCP reconnect; silent during outage | No (autossh starts new session) | Yes — 1-RTT cold reattach (v1.1); migration (v1.1) |
| Roaming / IP change | Yes — detects new source IP; brief stall | Yes — TCP reconnect | autossh: starts new session | Yes — QUIC migration, zero latency, invisible |
| Scrollback | No | No | Via local terminal | Deferred to M5 |

---

## QoL Wins Value Ranking (Solo Daily-Driver)

Ranked by value-per-effort for a single developer using nosh as their primary remote shell from
a laptop or desktop. Higher rank = more daily friction removed. Complexity noted relative to the
existing v1.1 codebase.

| Rank | Feature | Daily-Driver Value | Complexity | Why This Rank |
|------|---------|-------------------|------------|---------------|
| 1 | **Predictive local echo (adaptive)** | Eliminates the perception of network latency for typing | HIGH | The headline M4 feature. Without it, nosh's UX is SSH on QUIC — functional but not meaningfully better for interactive use. |
| 2 | **Connection-loss banner + abort instructions** | Removes "is it hung or just slow?" confusion; escape valve when network is dead | LOW | High frequency event (coffee shop WiFi, VPN fluctuation); low implementation cost. Immediately visible payoff. |
| 3 | **OSC 52 clipboard passthrough** | Eliminates the "how do I copy from remote vim to local clipboard?" problem that every Mosh/SSH user encounters | MEDIUM | Very high daily friction if missing. Neovim, helix, lazygit all benefit. nosh's reliable-stream implementation is cleaner than Mosh's UDP-size-limited version. |
| 4 | **Terminal title propagation** | Multi-session clarity in Windows Terminal, iTerm2, WezTerm tab titles | LOW | Near-zero implementation cost (a policy choice). Removes a subtle daily annoyance for multi-session users. |
| 5 | **`--predict always/adaptive/never` flags** | Power-user control. `always` for high-latency links; `never` for debugging rendering glitches | LOW | Implementation is trivially tied to the existing prediction engine. High value for users who hit edge cases. |

**Explicitly deferred per PROJECT.md** (do not scope into v1.2):

- Full native scrollback sync (M5) — lightweight OSC 52 passthrough covers the most common
  "copy from terminal" case; full scrollback is a larger protocol change
- Bell/notification passthrough (M5+ point release) — low daily-driver value; most users
  use `wait` in a script or observe the shell prompt directly
- Named session listing (M5) — v1.1 auto-reattach covers the solo daily-driver case

---

## Sources

- Mosh paper: https://mosh.org/mosh-paper.pdf (SSP, prediction accuracy, epoch model)
- Mosh predictive overlay system deep wiki: https://deepwiki.com/mobile-shell/mosh/4.2-predictive-overlay-system (SRTT thresholds: SRTT_TRIGGER_HIGH ~30 ms, FLAG_TRIGGER_HIGH ~80 ms; `Validity` enum; `cull()` confirmation)
- Mosh man page (Ubuntu 22.04): https://manpages.ubuntu.com/manpages/jammy/man1/mosh.1.html (--predict modes: adaptive/always/never; escape sequences: Esc . to disconnect, Esc Ctrl-Z to suspend; MOSH_ESCAPE_KEY)
- Mosh man page (Arch): https://man.archlinux.org/man/mosh.1.en (same; Ctrl-^ . = Ctrl-Shift-6 then period)
- Mosh issue #1049 (banner does not show hostname): https://github.com/mobile-shell/mosh/issues/1049
- Mosh issue #932 (fish autosuggestions + prediction interaction): https://github.com/mobile-shell/mosh/issues/932
- Mosh issue #275 (timeout counter display): https://github.com/mobile-shell/mosh/issues/275
- Mosh OSC 52 PR #1054: https://github.com/mobile-shell/mosh/pull/1054
- Mosh OSC 52 PR #1104 (additional clipboard types): https://github.com/mobile-shell/mosh/pull/1104
- Blink discussion OSC 52 over Mosh/SSH: https://github.com/blinksh/blink/discussions/1948 (Mosh 1.4.0+ supports OSC 52 write; limited to one UDP packet)
- OSC 52 overview: https://oppi.li/posts/OSC-52/ (format, payload limits, write vs read)
- OSC 52 copy/paste clipboard journey: https://miek.nl/2024/january/31/osc52-my-cut-paste-journey/
- Eternal Terminal how it works: https://eternalterminal.dev/howitworks/ (BackedReader/BackedWriter silent reconnect; no user-visible disconnection indicator)
- SSH escape sequences: https://redgreenrepeat.com/2018/01/12/til-ssh-disconnect-sequence-and-escape-characters/ (~. disconnect, ~B BREAK, ~# forwarded connections list)
- Mosh homepage prediction description: https://mosh.org/ (underline outstanding predictions; warns on connection loss; Ctrl-^ . to quit)
- CS244 Mosh reproduction: https://reproducingnetworkresearch.wordpress.com/2017/06/05/cs244-17-mosh-an-interactive-remote-shell-for-mobile-clients/ (70% keystroke prediction accuracy; 0.9% error rate)
- Hacker News Mosh prediction discussion: https://news.ycombinator.com/item?id=23026968

---
*Feature research for: nosh v1.2 — M4 Predictive Echo + Daily-Driver QoL*
*Researched: 2026-06-01*
