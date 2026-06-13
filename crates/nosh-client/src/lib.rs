//! `nosh-client` library surface — connection setup and round-trip helpers
//! exposed so integration tests can drive a client in-process.

pub mod channel; // Phase 21: per-channel drain task + even-id allocator
pub mod client;
pub mod platform;
pub mod predictor; // NEW: PredictionOverlay, PendingPrediction, Validity, InputAction
pub mod quinn_transport; // Phase 24: Quinn pass-through wrappers over NoshTransport
pub mod screen; // NEW: ClientScreen, Overlay, ConnectionLossOverlay

pub use client::{
    build_client_config, connect, make_endpoint,
};
