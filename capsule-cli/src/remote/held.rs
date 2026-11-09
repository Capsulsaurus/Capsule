//! Held lifecycle records: what of an asset's signed chain goes to the server, and when
//! (Organization — Deletion State Machine, "An upload in flight is redacted").
//!
//! The server never sees a `delete` for an asset it has not indexed. So a lifecycle record is
//! sent only once the server's feed lists the asset, whichever device uploaded its create, and
//! then as the stretch of the chain after the head the server holds, in chain order, so each op
//! chains onto the one before (invariant 17). Until then the record is **held**.
//!
//! What is sent is never read from anywhere but the asset's own provenance chain in the
//! library ([`local_chain`]): a live or trashed asset's, or the chain a purge keeps as the
//! tombstone, so a held delete and the shorter-retention re-issue an early purge appended
//! outlive the purge and are sent once another device's upload of the create is indexed. The
//! delegation ledger keeps a copy of a delegated asset's stretch only to know which
//! assets wait on a hand-off's custody; its delivery, like `capsule trash` and `capsule push`,
//! re-derives what to send by comparing the chain with the head a fresh feed drain names
//! ([`ServerChains`]), so a lost upload queue or ledger, a reinstall that keeps the library, or a
//! crash between signing and sending loses nothing.
//!
//! One stretch is never sent: a `trash-restore` that undoes a held `delete` whose window has
//! closed. The server judges a restore by when it arrives, so it would apply the delete, refuse
//! the restore and purge an asset the user restored in time. The pair is withheld instead
//! ([`HoldReason::RestoreTooLate`]): the server keeps the asset live at the head it holds, and
//! the library keeps every record. The chain is hash-linked, so nothing after the pair can be
//! sent either: a later delete of the asset never reaches the server, which keeps it live, and
//! only the library's own trash holds it.
//!
//! Both rules are the SDK's, defined once for every client that sends a lifecycle record
//! ([`capsule_sdk::albums::plan_delivery`]): the CLI and the FFI lane hold and withhold the
//! same stretches. What this module adds is the CLI's reading of the library and the feed.

use std::collections::HashMap;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::ProvenanceRecord;
use capsule_library::lifecycle::Workspace;
pub use capsule_sdk::albums::HoldReason;
// What a run does with one asset's chain, and the rule that decides it.
pub(crate) use capsule_sdk::albums::{HeldDelivery as Delivery, plan_delivery as plan};
use capsule_sdk::sync::FeedEntry;
use uuid::Uuid;

/// An asset's provenance chain as a delivery reads it from the library.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalChain<'a> {
    /// The chain, oldest first.
    pub(crate) records: &'a [ProvenanceRecord],
    /// The sealed metadata blob the library holds for the asset, which a record that binds it
    /// carries; `None` for a purged asset, whose blob the purge removed. A `delete` binds none.
    pub(crate) metadata_blob: Option<&'a [u8]>,
}

/// `asset_id`'s chain in `workspace`: the asset's own while the library holds it (live or in
/// the trash), otherwise the chain its purge kept ([`Workspace::purged_chain`]). `None` for an
/// asset the library never held.
pub(crate) fn local_chain<'a>(workspace: &'a Workspace, asset_id: &Uuid) -> Option<LocalChain<'a>> {
    if let Some(asset) = workspace.asset(asset_id) {
        return Some(LocalChain {
            records: asset.chain.records(),
            metadata_blob: Some(&asset.metadata_blob),
        });
    }
    workspace.purged_chain(asset_id).map(|records| LocalChain {
        records,
        metadata_blob: None,
    })
}

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

    /// Whether the server already holds the head of `records`, `asset_id`'s chain: nothing of it
    /// is left to send.
    pub(crate) fn is_level(&self, asset_id: &Uuid, records: &[ProvenanceRecord]) -> bool {
        records
            .last()
            .is_some_and(|head| self.head(asset_id) == Some(Some(head.record_hash())))
    }

    /// The clock a held stretch is judged by: this device's, floored by the latest server
    /// `changed_at` the drain saw ([`capsule_sdk::albums::judging_clock`]).
    pub(crate) fn judging_clock(&self, now: jiff::Timestamp) -> jiff::Timestamp {
        capsule_sdk::albums::judging_clock(now, self.latest_changed_at)
    }
}

/// The records of `records` (a chain, oldest first) the server lacks when it holds `head`, by
/// the SDK's one rule for every client ([`capsule_sdk::albums::chain_after`]): [`plan`] sends
/// it, and the delegation ledger holds it until a hand-off's custody
/// ([`super::lifecycle_ops_after`]).
pub(crate) use capsule_sdk::albums::chain_after as after_head;

#[cfg(test)]
mod tests;
