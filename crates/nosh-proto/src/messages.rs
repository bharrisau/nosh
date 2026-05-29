//! Wire message types for the nosh control protocol.
//!
//! Phase 1 defines the `Message` enum with room for future control frames.
//! Future control frames (e.g. `Resize { cols, rows }`, `Signal { signum }`,
//! `ShellOpen { .. }`) plug in here as additional variants without touching
//! the transport or codec layers — see decision D-05.

use serde::{Deserialize, Serialize};

/// A control-protocol message exchanged over a reliable QUIC stream.
///
/// `SessionClose` is the first real control frame (cheap-now item D-05): the
/// remote shell's exit code and a human-readable reason are delivered to the
/// client when a session ends (the client then exits with `exit_code`). More
/// variants are added in later phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    /// Session terminated; carries the shell exit code and a reason string.
    SessionClose { exit_code: i32, reason: String },
}
