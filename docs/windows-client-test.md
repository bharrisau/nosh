# Windows Client Interactive Test (D-02)

**Phase 8 - NON-BLOCKING human validation test**
**Status:** Pending operator execution on a real Windows host

This document is the documented human Windows interactive test (D-02). The
phase is marked `human_needed` — autonomous completion does not block on this
test. An operator records PASSED/FAILED below after running on a real Windows
box with a network-reachable Linux nosh server.

---

## Prerequisites

1. **Windows 10 or 11** — use Windows Terminal (not legacy cmd.exe; see
   Limitations §4 below). Windows Server 2019+ also works.

2. **nosh-client binary** — either:
   - Build from source: install Rust for Windows (https://rustup.rs/) +
     `cargo build -p nosh-client --release`, binary at
     `target\release\nosh-client.exe`; OR
   - Use a pre-built release binary (when published on GitHub Releases).

3. **Unencrypted Ed25519 private key** — create one if needed:
   ```
   ssh-keygen -t ed25519 -f %USERPROFILE%\.ssh\id_ed25519 -N ""
   ```
   The key MUST be unencrypted (no passphrase). If you have a passphrase-protected
   key, remove it first: `ssh-keygen -p -f %USERPROFILE%\.ssh\id_ed25519`

4. **Reachable Linux nosh server** with `authorized_keys` containing your
   Ed25519 public key (`%USERPROFILE%\.ssh\id_ed25519.pub`). Start with:
   ```
   nosh-server --port 4433 --host-key /etc/ssh/ssh_host_ed25519_key
   ```

---

## Run Command

Open Windows Terminal (PowerShell or Command Prompt) and run:

```
nosh-client.exe --addr <server-ip> --port 4433 --host <server-hostname> --identity-file %USERPROFILE%\.ssh\id_ed25519
```

Alternatively, if the key is at the default location (`%USERPROFILE%\.ssh\id_ed25519`),
you can omit `--identity-file`:

```
nosh-client.exe --addr <server-ip> --port 4433 --host <server-hostname>
```

---

## Validation Checklist

Record PASSED or FAILED for each item:

| # | Check | Expected | Result |
|---|-------|----------|--------|
| 1 | **Connection + auth** | Client connects using on-disk Ed25519 key; interactive shell prompt appears within ~2s | |
| 2 | **Raw mode** | Interactive shell works: characters appear immediately without line-buffering; `cat` echoes; `vim` / `less` render correctly | |
| 3 | **Ctrl-C forwarding** | Pressing Ctrl-C in an interactive session (e.g. during `sleep 100`) sends 0x03 to the remote shell (interrupts the command), NOT terminating the client | |
| 4 | **Terminal resize** | While in `vim` or `less`, drag the Windows Terminal window to resize it; within ~100ms the remote PTY reflows content to match the new size | |
| 5 | **Locale** | Run `echo $TERM` → shows `xterm-256color`; run `echo $LANG` → shows `en_US.UTF-8`; UTF-8 characters (e.g. `echo -e '\xc3\xa9'` → `é`) render correctly | |
| 6 | **Encrypted key rejected** | Run with an encrypted key file: error message mentions "passphrase-encrypted" and says to decrypt it; does NOT attempt to connect | |

---

## Expected Behavior Notes

- **Authentication**: The client signs the TLS CertificateVerify using the on-disk
  Ed25519 private key via `FileSigner`. No ssh-agent is used on Windows in v1.1.
- **Resize (coalescing)**: Rapid drag-resize events are debounced to ~40ms;
  only one `Resize` message is sent per coalesced burst.
- **Ctrl-C behavior**: During active session, Ctrl-C is forwarded as byte 0x03
  to the remote shell. Ctrl-C during a *reconnect wait* terminates the client.

---

## Known Limitations (v1.1)

1. **Passphrase-encrypted keys are not supported** — the client rejects them with
   an actionable error. Use an unencrypted key, or decrypt it with
   `ssh-keygen -p -f key -N ""`. Interactive passphrase prompt is deferred to v1.2
   (WIN-06).

2. **Windows ACL permission check not performed** — `std::fs::Permissions` cannot
   read Windows ACLs. The client emits a warning at startup noting this limitation.
   Ensure the key file is not readable by other users (right-click → Properties →
   Security tab, or `icacls %USERPROFILE%\.ssh\id_ed25519 /inheritance:r
   /grant:r "%USERNAME%:R"`). This is Pitfall 13 / D-10.

3. **No ssh-agent / Pageant support** — Pageant integration is deferred to v1.2
   (WIN-05). Use `--identity-file` for all Windows authentication in v1.1.

4. **Use Windows Terminal** — Legacy Command Prompt has limited VT processing
   support; resize events and 256-color rendering may not work in cmd.exe.
   PowerShell in Windows Terminal works correctly. (Pitfall 15.)

5. **Native Windows server not available** — v1.1 is client-only. The nosh server
   runs on Linux only (Phase 8 goal: Windows CLIENT connecting to Linux server).

---

## Operator Sign-off

```
Test date:    _______________
Windows host: Windows _____ (version)
Terminal:     _______________
Server IP:    _______________
Key type:     Ed25519 (unencrypted)

Overall result: [ ] PASSED  [ ] FAILED (see notes below)

Notes / failures:
_______________________________________________
_______________________________________________

Operator: _______________
```

---

*This test is NON-BLOCKING for autonomous phase completion. The phase is marked
`human_needed`. The operator's recorded result is required before the phase can
be marked fully complete.*
