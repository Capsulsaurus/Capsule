//! Held lifecycle records: what of an asset's signed chain goes to the server, and when
//! (Organization — Deletion State Machine, "An upload in flight is redacted").
//!
//! The server never sees a `delete` for an asset it has not indexed. So a lifecycle record is
//! sent only once the server's feed lists the asset, whichever device uploaded its create, and
//! then as the stretch of the chain after the head the server holds, in chain order, so each op
//! chains onto the one before (invariant 17). Until then the record is **held**.
//!
//! A held record is kept nowhere but the asset's own provenance chain in the library. Nothing
//! else records what is held: every run re-derives it by comparing that chain with the head a
//! fresh feed drain names ([`ServerChains`]), so a lost upload queue, a reinstall that keeps the
//! library, or a crash between signing and sending loses nothing.
//!
//! One stretch is never sent: a `trash-restore` that undoes a held `delete` whose window has
//! closed. The server judges a restore by when it arrives, so it would apply the delete, refuse
//! the restore and purge an asset the user restored in time. The pair is withheld instead
//! ([`HoldReason::RestoreTooLate`]): the server keeps the asset live at the head it holds, and
//! the library keeps every record.

use std::collections::HashMap;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::{Action, ProvenanceRecord};
use capsule_sdk::sync::FeedEntry;
use serde::Serialize;
use uuid::Uuid;

/// The server's chain head for every asset its feed lists, read off one fresh drain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ServerChains {
    /// Every asset the feed lists — the assets the server has indexed — with the hash of the
    /// head record its latest entry carries, or `None` for an entry that carries none (a purged
    /// asset, or a server that could not produce the record).
    heads: HashMap<Uuid, Option<Hash32>>,
    /// The latest `changed_at` the drain saw, on the server's clock.
    latest_changed_at: Option<jiff::Timestamp>,
}

impl ServerChains {
    /// Fold a drain's entries, in feed order: an asset's latest entry names its head.
    pub(crate) fn from_feed(entries: &[FeedEntry]) -> Self {
        let mut chains = Self::default();
        for entry in entries {
            let Some(asset_id) = std::str::from_utf8(&entry.asset_id)
                .ok()
                .and_then(|id| Uuid::parse_str(id).ok())
            else {
                tracing::debug!("held records: a feed entry names no asset id this library uses");
                continue;
            };
            // The feed serves the head record's canonical CBOR unchanged, and a record's hash
            // is the digest of exactly that encoding.
            let head = (!entry.manifest_cbor.is_empty()).then(|| hash_bytes(&entry.manifest_cbor));
            chains.heads.insert(asset_id, head);
            if let Ok(at) = entry.changed_at.parse::<jiff::Timestamp>() {
                chains.latest_changed_at = Some(chains.latest_changed_at.map_or(at, |t| t.max(at)));
            }
        }
        tracing::debug!(
            indexed = chains.heads.len(),
            latest_changed_at = ?chains.latest_changed_at,
            "held records: the server's chain heads read off the feed"
        );
        chains
    }

    /// Whether the server has indexed `asset_id`, and if so the head it holds.
    pub(crate) fn head(&self, asset_id: &Uuid) -> Option<Option<Hash32>> {
        self.heads.get(asset_id).copied()
    }

    /// The clock a held stretch is judged by: this device's, floored by the latest server
    /// `changed_at` the drain saw, as every expiry decision is.
    pub(crate) fn judging_clock(&self, now: jiff::Timestamp) -> jiff::Timestamp {
        self.latest_changed_at.map_or(now, |seen| now.max(seen))
    }
}

/// Why an asset's records were not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// The server's feed does not list the asset: its create is not indexed there, so no
    /// lifecycle record of it may be sent yet. Sent by the first run that finds it indexed.
    NotIndexed,
    /// A restore undoes a delete the server has not seen, and that delete's window has closed:
    /// sent, the server would apply the delete and refuse the restore. Withheld for good; the
    /// server keeps the asset live at the head it holds.
    RestoreTooLate,
}

/// What a run does with one asset's chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delivery<'a> {
    /// Nothing is sent, for this reason; `retention_until` is the closed window of a
    /// [`HoldReason::RestoreTooLate`].
    Held {
        reason: HoldReason,
        retention_until: Option<String>,
    },
    /// Send these records, in order, each once the one before it was applied.
    Send(&'a [ProvenanceRecord]),
}

/// Decide what of `records` (an asset's chain, oldest first) goes to the server, given the
/// head the server holds for it (`server`: `None` when it has not indexed the asset) and the
/// judging clock.
///
/// - Not indexed: held ([`HoldReason::NotIndexed`]).
/// - The server's head is in the chain: the records after it. Where it already is the local
///   head, the head alone, which the server answers as a replay: a send whose answer was lost.
/// - The server's head is unknown or not in the chain: the head alone, which the server judges
///   (a stale chain is refused, never applied).
///
/// A stretch in which a `trash-restore` undoes a `delete` of the same stretch whose
/// `retention_until` is at or before `judge` is held ([`HoldReason::RestoreTooLate`]).
pub(crate) fn plan(
    records: &[ProvenanceRecord],
    server: Option<Option<Hash32>>,
    judge: jiff::Timestamp,
) -> Delivery<'_> {
    let Some(server_head) = server else {
        return Delivery::Held {
            reason: HoldReason::NotIndexed,
            retention_until: None,
        };
    };
    let last = records.len().saturating_sub(1);
    let start = server_head
        .and_then(|head| records.iter().position(|r| r.record_hash() == head))
        .map_or(last, |at| (at + 1).min(last));
    let stretch = records.get(start..).unwrap_or_default();
    if let Some(until) = restore_too_late(stretch, judge) {
        return Delivery::Held {
            reason: HoldReason::RestoreTooLate,
            retention_until: Some(until),
        };
    }
    Delivery::Send(stretch)
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
