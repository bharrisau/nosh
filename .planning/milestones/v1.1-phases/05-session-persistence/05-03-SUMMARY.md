# Plan 05-03 Summary: CLI Config for Session Persistence

**Status:** Complete
**Completed:** 2026-05-30
**Commit:** 8a477d1 (bundled with 05-02)

## What Was Built

### Task 1: CLI flags in Args struct

- `#[arg(long, env = "NOSH_IDLE_TIMEOUT_SECS", default_value_t = 0)] idle_timeout_secs: u64`
  - Default 0 = disabled (Mosh behavior, D-08)
  - `env = "NOSH_IDLE_TIMEOUT_SECS"` provides automatic CLI > env > default precedence (D-09)
  - Doc comment references D-08/D-09
- `#[arg(long, default_value_t = 5)] max_sessions_per_identity: usize`
  - Doc comment references D-05
- clap `"env"` feature added to workspace `Cargo.toml`

### Task 2: SessionRegistry construction in main()

- `let registry = SessionRegistry::new(args.max_sessions_per_identity, Duration::from_secs(args.idle_timeout_secs));`
- `tracing::info!(idle_timeout_secs, max_sessions_per_identity, "session persistence config");`
- `server::run_accept_loop(endpoint, registry, limits, args.shell).await`

### Task 3: CLI/env precedence tests

- `tests::cli_env_precedence_idle_timeout` in main.rs verifies:
  1. Default: no flag, no env → `idle_timeout_secs == 0` (D-08)
  2. CLI flag `--idle-timeout-secs 30` → yields 30
  3. `NOSH_IDLE_TIMEOUT_SECS=45`, no CLI → yields 45 (D-09)
  4. `NOSH_IDLE_TIMEOUT_SECS=45` + `--idle-timeout-secs 7` → yields 7 (CLI wins, D-09)
  5. `max_sessions_per_identity` default == 5 (D-05)

## Tests
- `cargo test -p nosh-server --bin nosh-server`: 1/1 passed
- `cargo build -p nosh-server`: clean
- `cargo clippy -p nosh-server -- -D warnings`: clean
- `nosh-server --help` lists `--idle-timeout-secs` and `--max-sessions-per-identity`
