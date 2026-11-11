//! The lifecycle op's client-side shape, shared by every client that sends one: the
//! server-visible projection of a signed lifecycle manifest ([`op_envelope`]), which records
//! of an asset's chain go to the server when it holds a given head ([`chain_after`],
//! [`stretch_after`]), and whether they may go now ([`plan_delivery`]).
//!
//! # Held records
//!
//! The server never sees a lifecycle record of an asset it has not indexed (Organization —
//! Deletion State Machine), so a stretch is sent only once the server's feed lists the asset,
//! and until then it is **held** ([`HoldReason::NotIndexed`]).
//!
//! One stretch is never sent: a `trash-restore` that undoes a held `delete` whose window has
//! closed. The server judges a restore by when it arrives, so it would apply the delete, refuse
//! the restore and purge an asset the user restored in time. The pair is withheld instead
//! ([`HoldReason::RestoreTooLate`]): the server keeps the asset live at the head it holds, and
//! the library keeps every record. The chain is hash-linked, so nothing after the pair can be
//! sent either.
//!
//! One copy, so the CLI and the FFI lane send the same envelope and the same stretch for the
//! same chain; a second copy could drift and send a purge from one client that the other would
//! have held or ordered differently.

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::provenance::{Action, ManifestCore, ProvenanceRecord};
use serde::Serialize;

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
        crypto_suite_id: core.crypto_suite_id,
        protocol_version: core.protocol_version.clone(),
        album_id: Some(core.album_id.to_string()),
        file_id: core.file_id.to_string(),
        amk_version: core.amk_version.0,
        ciphertext_hash: core.ciphertext_hash.to_hex(),
        plaintext_size: core.plaintext_size,
        chunk_size: core.chunk_size,
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

/// Why an asset's records were not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// The server's feed does not list the asset: its create is not indexed there, so no
    /// lifecycle record of it may be sent yet. Sent by the first delivery that finds it indexed.
    NotIndexed,
    /// A restore undoes a delete the server has not seen, and that delete's window has closed:
    /// sent, the server would apply the delete and refuse the restore. Withheld for good; the
    /// server keeps the asset live at the head it holds.
    RestoreTooLate,
}

/// What a delivery does with one asset's chain ([`plan_delivery`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldDelivery<'a> {
    /// Nothing is sent, for this reason; `retention_until` is the closed window (RFC 3339) of a
    /// [`HoldReason::RestoreTooLate`].
    Held {
        /// Why nothing is sent.
        reason: HoldReason,
        /// The withheld `delete`'s signed window, for a [`HoldReason::RestoreTooLate`].
        retention_until: Option<String>,
    },
    /// Send these records, in order, each once the one before it was applied.
    Send(&'a [ProvenanceRecord]),
}

/// Decide what of `records` (an asset's chain, oldest first) goes to the server, given what its
/// feed lists for the asset (`server`: `None` when it does not list it, otherwise the hash of
/// the head it holds, `None` for an entry that carries no record) and the judging clock
/// ([`judging_clock`]).
///
/// - Not listed: held ([`HoldReason::NotIndexed`]).
/// - Listed: the [`stretch_after`] the server's head, unless a `trash-restore` in it undoes a
///   `delete` of the same stretch whose `retention_until` is at or before `judge`, which holds
///   it ([`HoldReason::RestoreTooLate`]).
#[must_use]
pub fn plan_delivery(
    records: &[ProvenanceRecord],
    server: Option<Option<Hash32>>,
    judge: jiff::Timestamp,
) -> HeldDelivery<'_> {
    let Some(server_head) = server else {
        return HeldDelivery::Held {
            reason: HoldReason::NotIndexed,
            retention_until: None,
        };
    };
    let stretch = stretch_after(records, server_head);
    if let Some(until) = restore_too_late(stretch, judge) {
        return HeldDelivery::Held {
            reason: HoldReason::RestoreTooLate,
            retention_until: Some(until),
        };
    }
    HeldDelivery::Send(stretch)
}

/// The clock a held stretch is judged by: this device's (`now`), floored by the latest
/// `changed_at` a feed drain saw on the server's clock, so a restore is withheld whenever either
/// clock says the server would refuse it. Only [`plan_delivery`] uses it: a retention sweep
/// judges expiry by the server's `changed_at` alone, since a device clock running ahead would
/// purge early.
#[must_use]
pub fn judging_clock(
    now: jiff::Timestamp,
    latest_changed_at: Option<jiff::Timestamp>,
) -> jiff::Timestamp {
    latest_changed_at.map_or(now, |seen| now.max(seen))
}

/// The closed window of a `delete` in `stretch` that a later `trash-restore` of the same
/// stretch undoes, when there is one.
fn restore_too_late(stretch: &[ProvenanceRecord], judge: jiff::Timestamp) -> Option<String> {
    let mut undone: Option<&ProvenanceRecord> = None;
    for record in stretch {
        match record.manifest.core.action {
            Action::Delete => undone = Some(record),
            Action::TrashRestore => {
                let closed = undone
                    .and_then(|delete| delete.manifest.core.retention_until.clone())
                    .filter(|until| {
                        until
                            .parse::<jiff::Timestamp>()
                            .is_ok_and(|until| judge >= until)
                    });
                if closed.is_some() {
                    return closed;
                }
                undone = None;
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests;
