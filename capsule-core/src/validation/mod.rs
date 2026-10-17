//! Refuse-by-default validation invariants — the operational core of the threat model
//! (SSoT: [Threat Model — Validation Invariants]).
//!
//! These are **pure, key-less** structural checks: the protocol/capability handshake
//! (`protocol`) and the server-side manifest envelope (`structural`). They mirror the
//! client-side checks in [`verify_asset`](crate::crypto::verify_asset::verify_asset), and the server
//! write paths consume them today — the envelope gate, the ops surface, the feed,
//! federation pull, and the drop routes all validate through here. This module used to describe those consumers as
//! deferred; they are six live call sites, and the checks are the only thing standing
//! between a key-free server and a malformed write.
//!
//! Upload-transport-specific invariants (chunk offset/4 KiB alignment, cumulative size)
//! live with the deferred upload protocol.
//!
//! [Threat Model — Validation Invariants]: https://docs/design/threat-model/validation/

mod protocol;
mod structural;

// Every public item of both modules, because this barrel is the module's only public path
// (#480) and a predicate that exists but cannot be named is not a contract. The server's
// envelope gate and its protocol negotiator between them reach eight of these.
pub use protocol::{HandshakeReject, check_sidecar_schema, check_suite, protocol_gate};
pub use structural::{
    EnvelopeContext, EnvelopeReject, album_pin_matches, amk_version_monotonic,
    check_manifest_envelope, check_metadata_blob_envelope, content_type_allowed,
    device_added_before, hash_length_ok, metadata_blob_hash_matches, prior_hash_matches,
    size_in_bounds, timestamp_within_drift,
};
