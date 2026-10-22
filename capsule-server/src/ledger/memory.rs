//! The deterministic in-memory owner ledger.
//!
//! It keeps each owner's ledger as the same byte stream the filesystem adapter writes — frames
//! of canonical CBOR — rather than as a list of entries, so the conformance suite's tampering
//! cases (a flipped byte, a cut tail) mean exactly what they mean on disk, and the decoder the
//! two adapters share is exercised by both.

use std::collections::BTreeMap;
use std::sync::Mutex;

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use super::{
    ChainFault, LedgerEntry, LedgerError, LedgerEvent, LedgerFuture, LedgerHead, LedgerScan,
    OwnerLedger, check_attachment_size, check_owner_id, decode_frames, encode_frame, link_check,
    sign_entry,
};
use crate::attestation::ReceiptSigner;
use crate::store::OwnerId;

/// A deterministic in-memory ledger.
#[derive(Debug, Default)]
pub struct InMemoryLedger {
    owners: Mutex<BTreeMap<OwnerId, Log>>,
    attachments: Mutex<BTreeMap<(OwnerId, Hash32), Vec<u8>>>,
}

/// One owner's bytes, and the head an append extends.
#[derive(Debug, Default)]
struct Log {
    bytes: Vec<u8>,
    /// `None` until an append has checked the bytes; reset whenever a test rewrites them.
    head: Option<Option<LedgerHead>>,
}

impl InMemoryLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// `owner`'s raw bytes, for a test that tampers with them.
    pub fn raw(&self, owner: &OwnerId) -> Vec<u8> {
        lock(&self.owners)
            .get(owner)
            .map(|log| log.bytes.clone())
            .unwrap_or_default()
    }

    /// Replace `owner`'s raw bytes, as tampering or a crash would.
    ///
    /// The cached head is dropped, so the next append re-reads what is there, exactly as a
    /// restarted filesystem adapter would.
    pub fn replace_raw(&self, owner: &OwnerId, bytes: Vec<u8>) {
        lock(&self.owners).insert(owner.clone(), Log { bytes, head: None });
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Read `log`'s head, cutting a torn tail off it first, as a restarted adapter would.
fn load_head(owner: &OwnerId, log: &mut Log) -> Result<Option<LedgerHead>, LedgerError> {
    let decoded = decode_frames(&log.bytes);
    match decoded.damage {
        None => {}
        Some(ChainFault::TornTail { .. }) => {
            tracing::warn!(
                %owner,
                cut = log.bytes.len() - decoded.whole_len,
                "cut a torn tail off an owner ledger before appending"
            );
            log.bytes.truncate(decoded.whole_len);
        }
        Some(fault) => {
            return Err(LedgerError::Damaged {
                owner: owner.clone(),
                fault,
            });
        }
    }
    link_check(owner, &decoded.entries).map_err(|fault| LedgerError::Damaged {
        owner: owner.clone(),
        fault,
    })
}

impl OwnerLedger for InMemoryLedger {
    fn append<'a>(
        &'a self,
        owner: &'a OwnerId,
        event: LedgerEvent,
        recorded_at: Timestamp,
        signer: &'a dyn ReceiptSigner,
    ) -> LedgerFuture<'a, LedgerEntry> {
        Box::pin(async move {
            check_owner_id(owner)?;
            let mut owners = lock(&self.owners);
            let log = owners.entry(owner.clone()).or_default();

            let head = if let Some(head) = log.head {
                head
            } else {
                load_head(owner, log)?
            };

            let entry = sign_entry(owner, head, event, recorded_at, signer);
            log.bytes.extend_from_slice(&encode_frame(&entry));
            log.head = Some(Some(LedgerHead {
                seq: entry.core.seq,
                hash: entry.chain_hash(),
            }));
            tracing::debug!(
                %owner,
                seq = entry.core.seq,
                kind = entry.core.event.kind(),
                "appended to an in-memory owner ledger"
            );
            Ok(entry)
        })
    }

    fn scan<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, LedgerScan> {
        Box::pin(async move {
            check_owner_id(owner)?;
            let decoded = decode_frames(&self.raw(owner));
            Ok(LedgerScan {
                entries: decoded.entries,
                damage: decoded.damage,
            })
        })
    }

    fn owners(&self) -> LedgerFuture<'_, Vec<OwnerId>> {
        Box::pin(async move {
            Ok(lock(&self.owners)
                .iter()
                .filter(|(_, log)| !log.bytes.is_empty())
                .map(|(owner, _)| owner.clone())
                .collect())
        })
    }

    fn attach<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: Hash32,
        bytes: Vec<u8>,
    ) -> LedgerFuture<'a, ()> {
        Box::pin(async move {
            check_owner_id(owner)?;
            check_attachment_size(bytes.len() as u64)?;
            tracing::debug!(%owner, %key, bytes = bytes.len(), "attached a document to an in-memory owner ledger");
            lock(&self.attachments).insert((owner.clone(), key), bytes);
            Ok(())
        })
    }

    fn attachment<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: &'a Hash32,
    ) -> LedgerFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            check_owner_id(owner)?;
            Ok(lock(&self.attachments).get(&(owner.clone(), *key)).cloned())
        })
    }
}
