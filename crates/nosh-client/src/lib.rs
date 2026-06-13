//! `nosh-client` library surface — connection setup and round-trip helpers
//! exposed so integration tests can drive a client in-process.

pub mod channel; // Phase 21: per-channel drain task + even-id allocator
pub mod client;
#[cfg(feature = "webtransport")]
pub mod inner_auth; // Phase 25 Plan 03: WebTransport inner SSH-key auth + TOFU prompt
pub mod platform;
pub mod predictor; // NEW: PredictionOverlay, PendingPrediction, Validity, InputAction
pub mod quinn_transport; // Phase 24: Quinn pass-through wrappers over NoshTransport
pub mod screen; // NEW: ClientScreen, Overlay, ConnectionLossOverlay
#[cfg(feature = "webtransport")]
pub mod wt_transport; // Phase 24 Plan 04: WebTransport client wrapper + connect_wt

pub use client::{
    build_client_config, connect, make_endpoint,
};
