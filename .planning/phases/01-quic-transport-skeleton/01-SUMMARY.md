---
plan_id: "01"
phase: 1
title: "Workspace scaffold + nosh-proto codec + nosh-auth placeholder verifier + shared transport config"
status: complete
requirements: [TRANS-01, TRANS-03, TRANS-05]
---

# Summary — Plan 01: Foundation

## What was built
- **Cargo workspace** (`Cargo.toml`, resolver 2) with shared `[workspace.dependencies]` and four members under `crates/`: `nosh-proto`, `nosh-auth`, `nosh-server`, `nosh-client` (D-01). `.gitignore` excludes `/target`.
- **nosh-proto** (lib):
  - `ALPN: &[u8] = b"nosh/0"` — single canonical ALPN constant (PITFALL 4).
  - `messages::Message` enum with `SessionClose { exit_code, reason }` (D-05), room for future control frames.
  - `codec` — the single isolated codec module (D-03): `encode`/`decode` (postcard body + u32-BE length prefix), async `write_message`/`read_message`, `ProtoError`, 16 MiB frame cap. Migration path postcard→prost is a one-file swap here (D-04).
  - `transport::transport_config(enable_keep_alive)` — datagram receive/send buffers (1 MiB, enables RFC 9221, PITFALL 1), finite 300s `max_idle_timeout` (PITFALL 3), 15s `keep_alive_interval` when enabled (client side, TRANS-05).
- **nosh-auth** (lib): `PlaceholderServerVerifier` implementing rustls `ServerCertVerifier` — accepts any cert (no pinning yet) BUT delegates `verify_tls12_signature`/`verify_tls13_signature` to `CryptoProvider` (REAL, not stubbed — PITFALL 5). `TODO(phase-2)` marks the pinning seam.
- **nosh-server / nosh-client**: stub `main.rs` + complete `Cargo.toml` dependency lists (real entrypoints land in plans 02/03).

## Verification
- `cargo build` (whole workspace, 4 crates) — exits 0.
- `cargo test -p nosh-proto` — 3 codec tests pass (sync round-trip, big-endian length prefix, async write/read round-trip).

## Key files created
- Cargo.toml, .gitignore
- crates/nosh-proto/{Cargo.toml, src/lib.rs, src/messages.rs, src/codec.rs, src/transport.rs}
- crates/nosh-auth/{Cargo.toml, src/lib.rs, src/verifier.rs}
- crates/nosh-server/{Cargo.toml, src/main.rs}, crates/nosh-client/{Cargo.toml, src/main.rs}

## Notes / deviations
- None. rustls 0.23 `ServerCertVerifier` API matched the planned signatures exactly; resolved to rustls 0.23, quinn 0.11.9, rcgen 0.14.8, tokio 1.52.3.

## Self-Check: PASSED
