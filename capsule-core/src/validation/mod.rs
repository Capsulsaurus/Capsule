//! Refuse-by-default validation invariants — the operational core of the threat model
//! (SSoT: [Threat Model — Validation Invariants]).
//!
//! These are **pure, key-less** structural checks: the protocol/capability handshake
//! ([`protocol`]), the server-side manifest envelope ([`structural`]), and idempotency
//! keys ([`idempotency`]). They are reusable by the (deferred) server write paths and
//! mirror the client-side checks in [`verify_asset`](crate::crypto::verify_asset).
//!
//! Upload-transport-specific invariants (chunk offset/4 KiB alignment, cumulative size)
//! live with the deferred upload protocol; the chunk idempotency key is provided here.
//!
//! [Threat Model — Validation Invariants]: https://docs/design/threat-model/validation/

pub mod idempotency;
pub mod protocol;
pub mod structural;

pub use idempotency::IdempotencyKey;
pub use protocol::{HandshakeReject, protocol_gate};
pub use structural::{EnvelopeContext, EnvelopeReject, check_manifest_envelope};
