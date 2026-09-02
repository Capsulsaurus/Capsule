//! Refuse-by-default validation invariants — the operational core of the threat model
//! (SSoT: [Threat Model — Validation Invariants]).
//!
//! These are **pure, key-less** structural checks: the protocol/capability handshake
//! ([`protocol`]) and the server-side manifest envelope ([`structural`]). They mirror the
//! client-side checks in [`verify_asset`](crate::crypto::verify_asset), and the server
//! write paths consume them today — the envelope gate, the ops surface, the feed,
//! federation pull, and the drop routes all validate through here. This module used to describe those consumers as
//! deferred; they are six live call sites, and the checks are the only thing standing
//! between a key-free server and a malformed write.
//!
//! Upload-transport-specific invariants (chunk offset/4 KiB alignment, cumulative size)
//! live with the deferred upload protocol.
//!
//! [Threat Model — Validation Invariants]: https://docs/design/threat-model/validation/

pub mod protocol;
pub mod structural;

pub use protocol::{HandshakeReject, protocol_gate};
pub use structural::{
    EnvelopeContext, EnvelopeReject, check_manifest_envelope, check_metadata_blob_envelope,
    metadata_blob_hash_matches,
};
