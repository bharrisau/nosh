# Plan 02-04 Summary: AUTH-01..05 integration tests

**Status:** Complete

## What was built
- `nosh-client/tests/common/mod.rs` — shared harness: throwaway Ed25519
  `TestKey` (signer + pinned pubkey + OpenSSH host-key file), in-process
  mutually-authenticated `spawn_server`, and `client_endpoint`.
- `nosh-client/tests/auth.rs` — the AUTH success-criteria tests.

## Test → requirement mapping (all passing)
| Test | Requirement |
|------|-------------|
| `mutual_auth_inprocess_happy_path` | AUTH-03/04 (in-process) |
| `unknown_client_key_rejected` | AUTH-01 |
| `host_key_mismatch_aborts` | AUTH-02 (mismatch) |
| `tofu_first_contact_records` | AUTH-02 (TOFU) |
| `forged_certificate_verify_rejected` | AUTH-03 (signature not stubbed) |
| `agent_ed25519_handshake_live` (`--ignored`) | AUTH-04 (LIVE ssh-agent) |
| `preauth_flood_bounded` | AUTH-05 |

## Notes
- TLS 1.3 lets the client `connect()` resolve before server-side client-cert
  verification, so negative tests assert on **session usability** (connect AND
  a stream echo), not just `connect()` erroring.
- The live ssh-agent test was run and **passes** here (ssh-agent/ssh-keygen are
  present); it is `#[ignore]`-gated so CI without an agent skips cleanly.

## Verification
- `cargo test --workspace`: 6 auth + 5 transport + 6 nosh-auth unit + 3 proto =
  all pass. `agent_ed25519_handshake_live` and `idle_survival_60s` pass when run
  with `--ignored`.
