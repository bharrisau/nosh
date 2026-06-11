
## Pre-existing test failure discovered during 20-02 execution

- **Test:** `sync03_acked_epoch_advances_baseline` in `crates/nosh-client/tests/sync.rs`
- **Failure:** `epoch must advance after epoch-ack: e1=1, e2=1`
- **Status:** Pre-existing (confirmed fails without 20-02 changes)
- **Cause:** Likely timing or epoch-ack handling issue, possibly related to burst server behavior (plan 20-01)
- **Action:** Deferred. Investigate in a future phase or as a separate debug task.
