//! The ledger every producer appends to, recording each owner's head in the asset index.
//!
//! An owner ledger cannot see a tail cut at an entry boundary on its own: what is left is a
//! valid shorter chain. The head recorded here is the anchor [`super::verify_chain`] needs to
//! see it ([the module docs](super#where-the-anchor-lives)).
//!
//! A decorator rather than a step in each producer, so no producer — finalization, a drop
//! adoption, a lifecycle op, a directory publish — can append without the head moving, and the
//! adapters stay ignorant of the index.
//!
//! # Never extend a cut chain
//!
//! An adapter extends the tail it finds on disk, and it trusts that tail rather than the anchor.
//! (The filesystem adapter re-reads it whenever another process, such as `purge --apply`, has
//! appended since, which keeps a second writer on the chain but cannot see a cut one.) After
//! a restart over a ledger that lost its tail at an entry boundary, it would append at a position
//! the anchor already names, and the rewrite would bury the loss. So the first append per owner
//! in a process first checks the ledger's links against the anchor, and refuses the write with
//! [`LedgerError::Damaged`] while the ledger is behind it or disagrees with it. The refusal
//! holds until a rebuild under `--apply` reports the loss and re-anchors; nothing is lost by
//! refusing, and a write accepted onto the cut chain could not be told apart from a forgery.
//! Once an owner passes, the adapter's tail is authoritative for the life of the process. Every
//! process that appends checks on its own first append, the server's and an operator's purge
//! alike. The anchor is raise-only, so their records may interleave.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use super::{
    ChainFault, LedgerEntry, LedgerError, LedgerEvent, LedgerFuture, LedgerHead, LedgerScan,
    OwnerLedger, verify_links,
};
use crate::attestation::ReceiptSigner;
use crate::index::AssetIndex;
use crate::store::OwnerId;

/// An [`OwnerLedger`] that records each appended entry's head in the asset index.
#[derive(Debug)]
pub struct AnchoredLedger {
    inner: Arc<dyn OwnerLedger>,
    index: Arc<dyn AssetIndex>,
    /// The owners whose ledger this process has checked against its anchor.
    checked: Mutex<BTreeSet<OwnerId>>,
}

impl AnchoredLedger {
    /// Append to `inner`, anchoring each head in `index`.
    pub fn new(inner: Arc<dyn OwnerLedger>, index: Arc<dyn AssetIndex>) -> Self {
        Self {
            inner,
            index,
            checked: Mutex::new(BTreeSet::new()),
        }
    }

    fn is_checked(&self, owner: &OwnerId) -> bool {
        self.checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(owner)
    }

    /// Refuse to extend `owner`'s ledger if it ends below its anchor or disagrees with it.
    ///
    /// An anchor the index cannot answer for is not a verdict: the check is skipped, left
    /// unmarked so the next append tries again, and the write proceeds as it would have before
    /// any anchor existed.
    async fn check_once(&self, owner: &OwnerId) -> Result<(), LedgerError> {
        if self.is_checked(owner) {
            return Ok(());
        }
        let anchor = match self.index.ledger_head(owner).await {
            Ok(anchor) => anchor,
            Err(error) => {
                tracing::warn!(%owner, %error, "could not read an owner ledger's anchor; appending unchecked");
                return Ok(());
            }
        };
        if let Some(anchor) = anchor {
            let scan = self.inner.scan(owner).await?;
            let report = verify_links(owner, &scan, Some(anchor));
            if let Some(
                fault @ (ChainFault::BehindAnchor { .. } | ChainFault::AnchorMismatch { .. }),
            ) = report.fault
            {
                tracing::error!(
                    %owner,
                    anchored = anchor.seq,
                    %fault,
                    "an owner ledger disagrees with its anchor; refusing appends until a rebuild re-anchors it"
                );
                return Err(LedgerError::Damaged {
                    owner: owner.clone(),
                    fault,
                });
            }
        }
        self.checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(owner.clone());
        Ok(())
    }
}

impl OwnerLedger for AnchoredLedger {
    /// Append through the wrapped ledger, then record the entry as the owner's head.
    ///
    /// The first append per owner checks the ledger against its anchor first (the module
    /// docs). The record follows the append, so the anchor never names an entry the ledger did
    /// not durably hold. It is best-effort: the entry is already durable and the caller's write
    /// proceeds on it, so a record that fails is logged and leaves the anchor behind the ledger,
    /// which a later append's record overtakes. Refusing the write instead would turn an index
    /// hiccup into a failed upload whose ledger entry is already written.
    fn append<'a>(
        &'a self,
        owner: &'a OwnerId,
        event: LedgerEvent,
        recorded_at: Timestamp,
        signer: &'a dyn ReceiptSigner,
    ) -> LedgerFuture<'a, LedgerEntry> {
        Box::pin(async move {
            self.check_once(owner).await?;
            let entry = self.inner.append(owner, event, recorded_at, signer).await?;
            let head = LedgerHead {
                seq: entry.core.seq,
                hash: entry.chain_hash(),
            };
            match self.index.record_ledger_head(owner, head).await {
                Ok(()) => {
                    tracing::trace!(%owner, ledger_seq = head.seq, "anchored an owner ledger's head");
                }
                Err(error) => tracing::warn!(
                    %owner,
                    ledger_seq = head.seq,
                    %error,
                    "could not anchor an owner ledger's head; the anchor stays behind the ledger"
                ),
            }
            Ok(entry)
        })
    }

    fn scan<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, LedgerScan> {
        self.inner.scan(owner)
    }

    fn owners(&self) -> LedgerFuture<'_, Vec<OwnerId>> {
        self.inner.owners()
    }

    /// Attach through the wrapped ledger. No head moves: an attachment is not an entry.
    fn attach<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: Hash32,
        bytes: Vec<u8>,
    ) -> LedgerFuture<'a, ()> {
        self.inner.attach(owner, key, bytes)
    }

    fn attachment<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: &'a Hash32,
    ) -> LedgerFuture<'a, Option<Vec<u8>>> {
        self.inner.attachment(owner, key)
    }
}

#[cfg(test)]
mod tests;
