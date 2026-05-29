# Plan 02-03 Summary: Endpoint wiring + pre-auth DoS cap

**Status:** Complete

## What was built
- `nosh-server/src/server.rs` — `build_server_config(host_key, authorized_keys)`
  loads the Ed25519 host key from a file, mints a SPKI=host-key cert, serves it
  via `NoshServerCertResolver`, and enforces `AuthorizedKeysVerifier` (replaces
  `with_no_client_auth`). `run_accept_loop(endpoint, AuthLimits)` bounds
  concurrent half-open handshakes with a `Semaphore` (refuse over cap) and wraps
  the handshake in `tokio::time::timeout` (drop on elapse). Echo handlers kept.
- `nosh-server/src/main.rs` — flags `--host-key`, `--authorized-keys`,
  `--max-concurrent-handshakes` (64), `--auth-timeout-secs` (5).
- `nosh-client/src/client.rs` — `ClientIdentity` (`from_agent` resolves
  `SSH_AUTH_SOCK` / `--identity`; `from_signer` for tests),
  `build_client_config(identity, known_hosts, host)` pins via `HostKeyVerifier`
  and presents the agent-signed cert via `NoshClientCertResolver`.
- `nosh-client/src/main.rs` — flags `--identity`, `--known-hosts`, `--host`.
- `NoshServerCertResolver`/`NoshClientCertResolver` added to `signer.rs`.

## Verification
- `cargo build --workspace --all-targets` clean; `cargo clippy` clean.
- Transport tests (`tests/transport.rs`, 5) now pass over a mutually
  authenticated link, proving the wiring end-to-end.
- Both binaries' `--help` list the new flags.

## Decisions honored
D-04, D-06, D-07, D-08, D-10, D-13; AUTH-01/02/04/05.
