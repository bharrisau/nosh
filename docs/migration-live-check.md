# nosh Connection Migration — Wi-Fi → Cellular Live Check (D-06)

## Purpose

This procedure confirms that a live `nosh` session survives a **real network
change** (Wi-Fi to cellular) with:

- No re-authentication prompt — the SAME QUIC connection continues;
- No data loss — output before the switch is intact and output resumes after;
- No `nosh` reconnect/error messages — migration is invisible to the user.

This is distinct from **cold reattach** (Phase 6): migration keeps the existing
QUIC connection alive via RFC 9000 connection migration; it does not create a new
connection and does not replay buffered output. There is no `Reattach` message
and no new TLS handshake.

A brief output pause of ~1–2 seconds after the network switch is **expected and
acceptable** — this is the RFC 9000 §9.4 anti-amplification stall while the
server validates the new path (see PITFALLS.md Pitfall #2). Any longer pause
should be investigated but is not a hard failure for this checklist.

The automated counterpart (CI-safe) is `crates/nosh-client/tests/migration.rs`
(`migration_survives_path_change`), which validates the same properties
headlessly via `Endpoint::rebind()`.

---

## Prerequisites

Before running this check, ensure:

- **Server:** a `nosh` server running on a public IP (or any host reachable from
  both Wi-Fi and cellular networks), with your SSH public key in
  `~/.ssh/authorized_keys` on the server.
- **Client device:** a device that can switch from Wi-Fi to cellular — a
  smartphone, or a laptop with a mobile hotspot / USB-tether available.
- **SSH key:** the client's SSH key must already be known to the server
  (`~authorized_keys`).
- **Client binary:** a recent `nosh-client` binary on the client device.

---

## Step-by-Step Procedure

### 1 — Connect over Wi-Fi

```sh
nosh-client --host <server-host> --port 443
```

Establish a working session over your Wi-Fi connection.

### 2 — Start a visible continuous output

Run a command in the session whose output will make any stall or break
immediately obvious:

```sh
# Option A: simple numbered output (easiest to spot gaps)
i=0; while true; do echo "LINE:$i  $(date)"; i=$((i+1)); sleep 1; done

# Option B: ping-style
ping <some-host>

# Option C: tail a log
tail -f /var/log/syslog
```

Note the current line number or timestamp before the network switch.

### 3 — Switch from Wi-Fi to cellular

Force a real source-IP change by one of:

- **On a phone (iOS / Android):** turn OFF Wi-Fi while staying connected to
  cellular data.
- **On a laptop with a hotspot:** disconnect from Wi-Fi (disable the Wi-Fi
  adapter in system preferences / `nmcli radio wifi off`), and ensure the laptop
  is already tethered to cellular via USB or Wi-Fi hotspot.
- **Brief toggle:** enabling Airplane Mode for 2–3 seconds and then turning it
  off (so cellular comes back first) also works if you cannot disable Wi-Fi
  independently.

### 4 — Observe the session

Watch for:

- A brief pause (~1–2 seconds) — **expected** (anti-amplification stall).
- Continued output after the pause — **required for PASS**.
- Any error, reconnection prompt, or session loss — **indicates a FAIL**.

---

## PASS Checklist

Mark each box when confirmed:

- [ ] The session did **NOT** prompt for re-authentication after the network
      switch.
- [ ] No `nosh` reconnect message or error appeared in the session.
- [ ] The continuous output **resumed** after at most a brief pause
      (~1–2 seconds is acceptable; see Purpose above).
- [ ] **No lines were lost or duplicated** across the switch (if using a
      numbered output like `LINE:$i`, verify the sequence is contiguous before
      and after).
- [ ] The session remained the **same session** — same shell state, same
      scrollback, no new prompt/banner from a fresh login.

---

## RESULT

Complete this block after running the check and attach it to the Phase 7
completion notes.

```
Operator: ___________________________
Date:     ___________________________

Server OS:  ___________________________   (e.g. Ubuntu 24.04)
Client OS:  ___________________________   (e.g. macOS 14 / iOS 18 / Android 15)
Networks:   Wi-Fi provider/SSID: _______________________
            Cellular provider:   _______________________

Measured stall (rough, seconds): _____ s

Result: [ ] PASS   [ ] FAIL

Notes / observations:
___________________________________________________________________
___________________________________________________________________
```

---

## Non-blocking Note

This live check is **non-blocking** for autonomous Phase 7 completion. Phase 7
is marked `human_needed` specifically for this check — the autonomous CI run
(including the headless `migration_survives_path_change` test) completes without
it. Record a PASS in the phase completion notes when you have run the check.

If the result is **FAIL**, open a bug referencing the stall duration, network
details, and `nosh` version, and compare against the headless test output
(`cargo test -p nosh-client --test migration -- --nocapture`) to isolate whether
the failure is loopback-only or real-network specific.

---

*Reference: 07-CONTEXT.md D-06, 07-RESEARCH.md §5, PITFALLS.md Pitfall #2*
