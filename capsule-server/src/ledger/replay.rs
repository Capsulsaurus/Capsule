//! Reading a verified ledger back as the custody facts it records.
//!
//! A finalization writes a [`LedgerEvent::Custody`] entry and then resolves it with an
//! [`LedgerEvent::Indexed`] or a [`LedgerEvent::Refused`] entry for the same upload, or with
//! nothing when the process died between the two ([the module docs](super)). Rebuild and the
//! scrub's ledger check both need the pairing, so it is made here once: two readers pairing
//! entries two ways would disagree about which writes a rebuild replays.

use std::collections::BTreeMap;

use capsule_core::crypto::hash::Hash32;

use super::{ChainReport, LedgerEntry, LedgerEvent, LedgerScan, verify_chain, verify_links};
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

    /// Verify `owner`'s scanned ledger as far as these keys allow.
    ///
    /// No anchor: no store records a ledger head outside the ledger yet (#757), so a tail cut
    /// exactly at an entry boundary is not detected here.
    pub fn verify(&self, owner: &OwnerId, scan: &LedgerScan) -> ChainReport {
        if self.0.is_empty() {
            return verify_links(owner, scan);
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
            None,
        )
    }
}

#[cfg(test)]
mod tests;
