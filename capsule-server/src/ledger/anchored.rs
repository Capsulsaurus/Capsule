//! The ledger every producer appends to, recording each owner's head in the asset index.
//!
//! An owner ledger cannot see a tail cut at an entry boundary on its own: what is left is a
//! valid shorter chain. The head recorded here is the anchor [`super::verify_chain`] needs to
//! see it ([the module docs](super#where-the-anchor-lives)).
//!
//! A decorator rather than a step in each producer, so no producer — finalization, a drop
//! adoption, a lifecycle op, a directory publish — can append without the head moving, and the
//! adapters stay ignorant of the index.

use std::sync::Arc;

use jiff::Timestamp;

use super::{LedgerEntry, LedgerEvent, LedgerFuture, LedgerHead, LedgerScan, OwnerLedger};
use crate::attestation::ReceiptSigner;
use crate::index::AssetIndex;
use crate::store::OwnerId;

/// An [`OwnerLedger`] that records each appended entry's head in the asset index.
#[derive(Debug, Clone)]
pub struct AnchoredLedger {
    inner: Arc<dyn OwnerLedger>,
    index: Arc<dyn AssetIndex>,
}

impl AnchoredLedger {
    /// Append to `inner`, anchoring each head in `index`.
    pub fn new(inner: Arc<dyn OwnerLedger>, index: Arc<dyn AssetIndex>) -> Self {
        Self { inner, index }
    }
}

impl OwnerLedger for AnchoredLedger {
    /// Append through the wrapped ledger, then record the entry as the owner's head.
    ///
    /// The record follows the append, so the anchor never names an entry the ledger did not
    /// durably hold. It is best-effort: the entry is already durable and the caller's write
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
            let entry = self.inner.append(owner, event, recorded_at, signer).await?;
            let head = LedgerHead {
                seq: entry.core.seq,
                hash: entry.chain_hash(),
            };
            match self.index.record_ledger_head(owner, head).await {
                Ok(()) => {
                    tracing::trace!(%owner, ledger_seq = head.seq, "anchored an owner ledger's head")
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
}

#[cfg(test)]
mod tests;
