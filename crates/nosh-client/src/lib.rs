//! `nosh-client` library surface — connection setup and round-trip helpers
//! exposed so integration tests can drive a client in-process.

pub mod client;
pub mod platform;
pub mod predictor; // NEW: PredictionOverlay, PendingPrediction, Validity, InputAction
pub mod screen; // NEW: ClientScreen, Overlay, ConnectionLossOverlay

pub use client::{
    build_client_config, concurrent_roundtrip, connect, datagram_roundtrip, make_endpoint,
    stream_echo_roundtrip,
};
