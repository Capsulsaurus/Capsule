//! Reading a verified ledger back as the custody facts it records.
//!
//! A finalization writes a [`LedgerEvent::Custody`] entry and then resolves it with an
//! [`LedgerEvent::Indexed`] or a [`LedgerEvent::Refused`] entry for the same upload, or with
//! nothing when the process died between the two ([the module docs](super)). Rebuild and the
//! scrub's ledger check both need the pairing, so it is made here once: two readers pairing
//! entries two ways would disagree about which writes a rebuild replays.

use std::collections::BTreeMap;

use capsule_core::crypto::hash::Hash32;

use super::rebuild::{PURGE, PURGED};
use super::{
    ChainReport, LedgerEntry, LedgerEvent, LedgerHead, LedgerScan, verify_chain, verify_links,
};
use crate::attestation::PublishedKey;
use crate::store::OwnerId;

/// How a custody entry was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The index committed the write. `sync_seq` is the asset's position after it, when the
    /// write minted or found one.
    Indexed {
        /// The asset's `sync_seq` after the write.
        sync_seq: Option<u64>,
    },
    /// The index refused the write. Never replayed.
    Refused,
    /// Nothing resolved it: the process stopped between the custody entry and the next, or the
    /// append after the index write failed. Whether the row committed is unknown, so a rebuild
    /// replays it and cannot prove its sequence number.
    Unresolved,
}

/// One custody entry, paired with what resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Custody {
    /// The custody entry's own position in the ledger.
    pub ledger_seq: u64,
    /// The session that produced custody.
    pub upload_id: String,
    /// The asset the blob belongs to.
    pub asset_id: String,
    /// The blob's role, as the wire spells it.
    pub blob_role: String,
    /// The content address the server recomputed.
    pub content_hash: Hash32,
    /// The blob's length.
    pub size: u64,
    /// The SHA-256 of the signed manifest, when the blob is one.
    pub manifest_sha256: Option<Hash32>,
    /// The account the storage is attributed to.
    pub upload_user_id: String,
    /// The server's clock at the commit.
    pub received_at: String,
    /// What resolved it.
    pub resolution: Resolution,
}

/// Every custody entry in `entries`, in ledger order, paired with its resolution.
///
/// A resolving entry names the upload; the first one after the custody entry for the same
/// upload resolves it. A retried finalization of the same upload writes a second custody entry,
/// and each is resolved by the first resolving entry that follows it.
pub fn custody(entries: &[LedgerEntry]) -> Vec<Custody> {
    let mut out: Vec<Custody> = Vec::new();
    // The open custody entries, by upload, as indexes into `out`.
    let mut open: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for entry in entries {
        match &entry.core.event {
            LedgerEvent::Custody {
                upload_id,
                asset_id,
                blob_role,
                content_hash,
                size,
                manifest_sha256,
                upload_user_id,
                received_at,
                ..
            } => {
                open.entry(upload_id.as_str()).or_default().push(out.len());
                out.push(Custody {
                    ledger_seq: entry.core.seq,
                    upload_id: upload_id.clone(),
                    asset_id: asset_id.clone(),
                    blob_role: blob_role.clone(),
                    content_hash: *content_hash,
                    size: *size,
                    manifest_sha256: *manifest_sha256,
                    upload_user_id: upload_user_id.clone(),
                    received_at: received_at.clone(),
                    resolution: Resolution::Unresolved,
                });
            }
            LedgerEvent::Indexed {
                upload_id,
                sync_seq,
                ..
            } => resolve(
                &mut out,
                &mut open,
                upload_id,
                Resolution::Indexed {
                    sync_seq: *sync_seq,
                },
            ),
            LedgerEvent::Refused { upload_id, .. } => {
                resolve(&mut out, &mut open, upload_id, Resolution::Refused);
            }
            LedgerEvent::Directory { .. } | LedgerEvent::Lifecycle { .. } => {}
        }
    }
    out
}

/// One lifecycle op, paired with the entry that recorded its sequence number.
///
/// A lifecycle op appends a [`LedgerEvent::Lifecycle`] with no `sync_seq` before the index call
/// and a second one carrying the position once the index has applied it (#753). The pair shares
/// the asset and the manifest hash, which is the index's replay key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifecycle {
    /// The first entry's position in the ledger.
    pub ledger_seq: u64,
    /// The asset the op applies to.
    pub asset_id: String,
    /// The op, as the wire spells it.
    pub action: String,
    /// The SHA-256 of the manifest that authorised it, which is also its blob's address.
    pub manifest_hash: Option<Hash32>,
    /// `Some(position)` once the index applied it; `None` when nothing recorded a position —
    /// the index refused it, or the process stopped, or the second append was lost.
    pub sync_seq: Option<u64>,
    /// The server's clock when the first entry was written: when the op was accepted.
    pub recorded_at: String,
    /// For a retention purge ([`PURGE`]), whether a [`PURGED`] entry confirmed the index applied
    /// it (#811). Always `false` for every other action.
    pub confirmed: bool,
}

/// Every lifecycle op in `entries`, in ledger order, paired with the entry that recorded its
/// sequence number.
///
/// An entry that already carries a `sync_seq` with no open op before it (a server that wrote
/// one entry per op) stands on its own. A [`PURGED`] entry is never an op of its own: it
/// confirms the latest unconfirmed [`PURGE`] of the same asset before it, and one with no such
/// purge confirms nothing.
pub fn lifecycle(entries: &[LedgerEntry]) -> Vec<Lifecycle> {
    let mut out: Vec<Lifecycle> = Vec::new();
    for entry in entries {
        let LedgerEvent::Lifecycle {
            asset_id,
            action,
            manifest_hash,
            sync_seq,
        } = &entry.core.event
        else {
            continue;
        };
        if action == PURGED {
            if let Some(open) = out
                .iter_mut()
                .rev()
                .find(|op| op.action == PURGE && !op.confirmed && op.asset_id == *asset_id)
            {
                open.confirmed = true;
            }
            continue;
        }
        if let Some(seq) = sync_seq
            && let Some(open) = out.iter_mut().rev().find(|op| {
                op.sync_seq.is_none()
                    && op.asset_id == *asset_id
                    && op.manifest_hash == *manifest_hash
            })
        {
            open.sync_seq = Some(*seq);
            continue;
        }
        out.push(Lifecycle {
            ledger_seq: entry.core.seq,
            asset_id: asset_id.clone(),
            action: action.clone(),
            manifest_hash: *manifest_hash,
            sync_seq: *sync_seq,
            recorded_at: entry.core.recorded_at.clone(),
            confirmed: false,
        });
    }
    out
}

/// One device-directory publish the ledger recorded: an offer, which the store may have
/// refused after the entry was written ([`crate::routes::directory`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryOffer {
    /// The entry's position in the ledger.
    pub ledger_seq: u64,
    /// The account whose directory it is.
    pub user_id: String,
    /// The version it offered.
    pub directory_version: u64,
    /// The hash of the signed document, which is also its attachment's key.
    pub document_hash: Hash32,
    /// The server's clock when the entry was written.
    pub recorded_at: String,
}

/// Every device-directory offer in `entries`, in ledger order.
pub fn directory(entries: &[LedgerEntry]) -> Vec<DirectoryOffer> {
    entries
        .iter()
        .filter_map(|entry| match &entry.core.event {
            LedgerEvent::Directory {
                user_id,
                directory_version,
                document_hash,
            } => Some(DirectoryOffer {
                ledger_seq: entry.core.seq,
                user_id: user_id.clone(),
                directory_version: *directory_version,
                document_hash: *document_hash,
                recorded_at: entry.core.recorded_at.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Resolve every open custody entry for `upload` with `resolution`.
fn resolve(
    out: &mut [Custody],
    open: &mut BTreeMap<&str, Vec<usize>>,
    upload: &str,
    resolution: Resolution,
) {
    if let Some(pending) = open.remove(upload) {
        for at in pending {
            out[at].resolution = resolution;
        }
    }
}

/// The published attestation keys a ledger is verified under, when the caller holds them.
///
/// Empty on a host given no key: verification then checks links and positions only
/// ([`verify_links`]), and the report a caller prints says so.
#[derive(Debug, Clone, Default)]
pub struct LedgerKeys(Vec<PublishedKey>);

impl LedgerKeys {
    /// Verify under `keys`.
    pub fn new(keys: Vec<PublishedKey>) -> Self {
        Self(keys)
    }

    /// Whether signatures are checked.
    pub fn checks_signatures(&self) -> bool {
        !self.0.is_empty()
    }

    /// Verify `owner`'s scanned ledger as far as these keys allow, against `anchor`.
    ///
    /// `anchor` is the head the index recorded ([`crate::index::AssetIndex::ledger_head`]); with
    /// it, a tail cut exactly at an entry boundary is [`super::ChainFault::BehindAnchor`] on a
    /// key-less host as on a keyed one. `None` checks the chain alone.
    pub fn verify(
        &self,
        owner: &OwnerId,
        scan: &LedgerScan,
        anchor: Option<LedgerHead>,
    ) -> ChainReport {
        if self.0.is_empty() {
            return verify_links(owner, scan, anchor);
        }
        verify_chain(
            owner,
            scan,
            &|key_id| {
                self.0
                    .iter()
                    .find(|key| &key.key_id == key_id)
                    .map(|key| key.public.clone())
            },
            anchor,
        )
    }
}

#[cfg(test)]
mod tests;
