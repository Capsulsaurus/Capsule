//! The lifecycle op's client-side shape, shared by every client that sends one: the
//! server-visible projection of a signed lifecycle manifest ([`op_envelope`]) and which records
//! of an asset's chain go to the server when it holds a given head ([`chain_after`],
//! [`stretch_after`]).
//!
//! One copy, so the CLI and the FFI lane send the same envelope and the same stretch for the
//! same chain; a second copy could drift and send a purge from one client that the other would
//! have held or ordered differently.

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::provenance::{ManifestCore, ProvenanceRecord};

use crate::rest::types::ManifestEnvelope;

/// The server-visible projection of a lifecycle manifest: every field verbatim, naming the
/// original's ciphertext, and no derivative (the server refuses the pair on a lifecycle write).
#[must_use]
pub fn op_envelope(core: &ManifestCore) -> ManifestEnvelope {
    let wire = |value: serde_json::Result<serde_json::Value>| {
        value
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    ManifestEnvelope {
        crypto_suite_id: i64::from(core.crypto_suite_id),
        protocol_version: core.protocol_version.clone(),
        album_id: Some(core.album_id.to_string()),
        file_id: core.file_id.to_string(),
        amk_version: i64::from(core.amk_version.0),
        ciphertext_hash: core.ciphertext_hash.to_hex(),
        plaintext_size: i64::try_from(core.plaintext_size).unwrap_or(i64::MAX),
        chunk_size: i64::from(core.chunk_size),
        key_mode: wire(serde_json::to_value(core.key_mode)),
        metadata_blob_hash: core.metadata_blob_hash.map(|h| h.to_hex()),
        original_blob_hash: None,
        derivative_role: None,
        derivative_format: None,
        created_by_user: core.created_by_user.to_string(),
        created_by_device: core.created_by_device.to_string(),
        client_version: core.client_version.clone(),
        timestamp: core.timestamp.clone(),
        action: wire(serde_json::to_value(core.action)),
        prior_provenance_hash: core.prior_provenance_hash.map(|h| h.to_hex()),
        retention_until: core.retention_until.clone(),
    }
}

/// The records of `records` (a chain, oldest first) the server lacks when it holds `head`: those
/// after it, which is none when `head` already is the last; or the last record alone when
/// `head` is unknown or not in the chain, which the server judges (a stale chain is refused,
/// never applied).
#[must_use]
pub fn chain_after(records: &[ProvenanceRecord], head: Option<Hash32>) -> &[ProvenanceRecord] {
    let start = head
        .and_then(|head| records.iter().position(|r| r.record_hash() == head))
        .map_or_else(|| records.len().saturating_sub(1), |at| at + 1);
    records.get(start..).unwrap_or_default()
}

/// The stretch a delivery sends when the server holds `head`: [`chain_after`], except that a
/// server level with the chain is sent the last record alone, which it answers as a replay (a
/// send whose answer was lost). Empty only for an empty chain.
#[must_use]
pub fn stretch_after(records: &[ProvenanceRecord], head: Option<Hash32>) -> &[ProvenanceRecord] {
    let after = chain_after(records, head);
    if after.is_empty() {
        records
            .get(records.len().saturating_sub(1)..)
            .unwrap_or_default()
    } else {
        after
    }
}

#[cfg(test)]
mod tests;
