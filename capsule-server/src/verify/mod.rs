//! Storage verification (`S-C3`) — the key-free durability verdict.
//!
//! A client is about to delete its only local copy of a photo. Before it does, it asks the
//! server whether the server really holds what it thinks it holds. That question is this
//! module, and the answer gates a destructive action, so every part of it is written to fail
//! *closed*: an unknown asset is not durable, an unassociated hash is not stored, and a
//! collaborator that cannot answer is an error rather than an optimistic verdict.
//!
//! # The three facts
//!
//! | Fact | Source | Meaning |
//! | --- | --- | --- |
//! | `indexed` | [`crate::index::AssetIndex`] | a live row *of the caller's* names this address |
//! | `stored` | [`crate::blob::BlobStore`] | the bytes are at that content address |
//! | `retrievable` | both | indexed ∧ stored ∧ nothing is withholding it |
//!
//! `durable` is their conjunction over every hash the client declared. Over the *declared* set,
//! not the server's: the client is asking about the copies it is relying on, and a server that
//! answered about its own idea of the asset would answer a different question.
//!
//! # `indexed` is checked first, and that ordering is a disclosure boundary
//!
//! A hash this asset does not hold reports `stored = false` **even when the store holds those
//! bytes**, because the store is asked only about hashes the asset already references. Content
//! addressing means one blob serves many assets, so answering "yes, those bytes exist" for an
//! arbitrary hash would turn a durability query into a cross-account existence oracle. The
//! storage-verification contract fixes this shape — *"a hash the server does not associate with
//! the asset comes back `stored = false, indexed = false`, never silently omitted"* — and this
//! is the reason it does.
//!
//! # Owner-scoped, which the retired surface was not
//!
//! The Salvo endpoint took any `asset_id` and answered about it. This one answers only about
//! the caller's own assets; somebody else's asset is indistinguishable from one that does not
//! exist. An asset id is a UUID and hard to guess, but "hard to guess" is not an access
//! control, and a verdict about another account's asset is disclosure about another account.
//!
//! # What is missing, and owned elsewhere
//!
//! - **`deep`** — the opt-in re-hash that catches silent bit-rot — is **not here**, and not
//!   because it is hard. Its contract is *"rate-limited per user and coalesced, so a client
//!   cannot turn it into an I/O-amplification attack"*, and the per-user counter that would
//!   enforce that has no port (`S-C32`). Shipping the re-hash without the limiter would ship
//!   the amplification, so the two halves land together — see `S-C41`.
//! - **GC state** landed with `S-C11`: a blob the collector has marked is `stored` and **not**
//!   `retrievable`, which is the combination that matters most here. Its bytes are on disk
//!   right now and on their way out, so a client that read `stored` alone and released its copy
//!   would be releasing it into a window that closes. A **quarantined** blob already reports
//!   correctly with no extra check, because quarantining moves the bytes out of the store.
//! - The **signed** `StorageAttestation` form of this verdict is `S-C15`. It wraps this engine
//!   rather than replacing it, which is why the verdict type is public.

use std::sync::Arc;

use jiff::Timestamp;

use crate::blob::{BlobStore, ContentAddress};
use crate::index::{AssetIndex, AssetState};
use crate::store::{AssetId, BlobRole, Clock, OwnerId};

/// The most assets one request may ask about.
///
/// A bound, not a rate limit: it caps the work a single request can buy, which is a different
/// property from capping how many requests an account may make (`S-C32`). A client releasing
/// local originals verifies a page at a time, so this is well above what any real caller sends.
pub const MAX_ASSETS_PER_REQUEST: usize = 100;

/// The most blob hashes one asset may declare.
///
/// An asset's bundle is an original, a metadata blob, a provenance blob and a handful of
/// derivatives. A declaration an order of magnitude past that is not a client this server has.
pub const MAX_BLOBS_PER_ASSET: usize = 32;

/// The media verification module's collaborators.
#[derive(Debug, Clone)]
pub struct VerifyContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    marks: Arc<dyn crate::gc::CollectionStore>,
    clock: Arc<dyn Clock>,
}

impl VerifyContext {
    /// Assembles the module from its collaborators.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        marks: Arc<dyn crate::gc::CollectionStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            index,
            blobs,
            marks,
            clock,
        }
    }

    /// The asset index the `indexed` fact comes from.
    pub fn index(&self) -> &dyn AssetIndex {
        self.index.as_ref()
    }

    /// The blob store the `stored` fact comes from.
    pub fn blobs(&self) -> &dyn BlobStore {
        self.blobs.as_ref()
    }

    /// The collector's marks, which is where `retrievable` diverges from `stored`.
    pub fn marks(&self) -> &dyn crate::gc::CollectionStore {
        self.marks.as_ref()
    }

    /// The trusted clock a verdict is stamped from.
    ///
    /// The server's, never the client's: a verdict says when *the server* looked, and a
    /// client-supplied instant would let a stale verdict be presented as a fresh one.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }
}

/// One asset a client is asking about, with the exact copies it is relying on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetQuery {
    /// The asset.
    pub asset_id: AssetId,
    /// Every content address the client would be trusting the server with.
    pub blob_hashes: Vec<ContentAddress>,
}

/// One declared blob's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobVerdict {
    /// The address as the client declared it.
    pub hash: ContentAddress,
    /// The role the asset holds it under, or `None` when the asset does not hold it at all.
    pub role: Option<BlobRole>,
    /// The bytes are at that address. Only ever asked about an address the asset holds.
    pub stored: bool,
    /// A live row of the caller's references the address.
    pub indexed: bool,
    /// Nothing is withholding it.
    pub retrievable: bool,
}

impl BlobVerdict {
    /// A declared hash this asset does not hold — the shape the contract fixes for it.
    fn unassociated(hash: ContentAddress) -> Self {
        Self {
            hash,
            role: None,
            stored: false,
            indexed: false,
            retrievable: false,
        }
    }

    /// Whether this blob contributes a `true` to the asset's durability.
    fn is_durable(&self) -> bool {
        self.stored && self.indexed && self.retrievable
    }
}

/// One asset's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetVerdict {
    /// The asset the client asked about.
    pub asset_id: AssetId,
    /// Every declared blob is stored ∧ indexed ∧ retrievable.
    pub durable: bool,
    /// One entry per declared hash, in the order they were declared. Never silently shortened:
    /// a client matching by position would otherwise mis-attribute a verdict.
    pub blobs: Vec<BlobVerdict>,
    /// When the server looked.
    pub checked_at: Timestamp,
}

/// A collaborator could not answer, so no verdict was reached.
///
/// Never conflated with "not durable". A client told `durable = false` keeps its local copy,
/// which is safe; a client told `durable = true` deletes it. Only the second is catastrophic,
/// which is why this exists rather than a pessimistic default: an outage that silently reads as
/// a real non-durability would train users to ignore the state that matters.
#[derive(Debug, thiserror::Error)]
#[error("the verdict could not be computed: {0}")]
pub struct VerifyUnavailable(String);

/// Compute the verdict for every query, in order.
///
/// # Errors
///
/// Returns [`VerifyUnavailable`] when the index or the blob store could not answer.
#[tracing::instrument(skip(context, queries), fields(owner = %owner, assets = queries.len()))]
pub async fn verify(
    context: &VerifyContext,
    owner: &OwnerId,
    queries: &[AssetQuery],
) -> Result<Vec<AssetVerdict>, VerifyUnavailable> {
    let checked_at = context.clock().now();
    let mut verdicts = Vec::with_capacity(queries.len());
    for query in queries {
        verdicts.push(verify_one(context, owner, query, checked_at).await?);
    }
    Ok(verdicts)
}

/// One asset's verdict.
async fn verify_one(
    context: &VerifyContext,
    owner: &OwnerId,
    query: &AssetQuery,
    checked_at: Timestamp,
) -> Result<AssetVerdict, VerifyUnavailable> {
    let row = context
        .index()
        .read(&query.asset_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, asset = %query.asset_id, "the index could not answer a verdict");
            VerifyUnavailable("the index could not answer".to_owned())
        })?;

    // Not the caller's asset, never reserved, or deleted. All three answer identically: every
    // declared hash is unassociated, and the asset is not durable. A caller learns nothing
    // about which of the three it was, which is the point.
    let live = row.filter(|row| &row.owner_id == owner && row.state != AssetState::Tombstoned);
    let Some(row) = live else {
        tracing::debug!(asset = %query.asset_id, "no live asset of this owner: not durable");
        return Ok(AssetVerdict {
            asset_id: query.asset_id.clone(),
            durable: false,
            blobs: query
                .blob_hashes
                .iter()
                .cloned()
                .map(BlobVerdict::unassociated)
                .collect(),
            checked_at,
        });
    };

    let mut blobs = Vec::with_capacity(query.blob_hashes.len());
    for hash in &query.blob_hashes {
        let Some(held) = row.blobs.iter().find(|blob| &blob.address == hash) else {
            // Not this asset's blob. The store is deliberately **not** asked — see the module
            // docs on why answering would be a cross-account existence oracle.
            blobs.push(BlobVerdict::unassociated(hash.clone()));
            continue;
        };

        let stored = context
            .blobs()
            .stat(hash)
            .await
            .map_err(|error| {
                tracing::error!(%error, %hash, "the blob store could not answer a verdict");
                VerifyUnavailable("the blob store could not answer".to_owned())
            })?
            .is_some();

        // The one place `retrievable` is not `stored`: a marked blob is on disk and on its way
        // out (`S-C11`).
        let collectable = context
            .marks()
            .marked_since(hash)
            .await
            .map_err(|error| {
                tracing::error!(%error, %hash, "the collector's marks could not be read");
                VerifyUnavailable("the collection marks could not be read".to_owned())
            })?
            .is_some();

        blobs.push(BlobVerdict {
            hash: hash.clone(),
            role: Some(held.role),
            stored,
            indexed: true,
            retrievable: stored && !collectable,
        });
    }

    let durable = blobs.iter().all(BlobVerdict::is_durable);
    if !durable {
        tracing::info!(asset = %row.asset_id, "an asset was reported not durable");
    }
    Ok(AssetVerdict {
        asset_id: query.asset_id.clone(),
        durable,
        blobs,
        checked_at,
    })
}

#[cfg(test)]
mod tests {
    use capsule_core::crypto::hash::Hash32;
    use capsule_core::crypto::keys::HybridSigningKey;
    use capsule_core::crypto::receipts::{
        BlobRole as ReceiptRole, CustodyReceipt, CustodyReceiptCore, role_str,
    };

    /// This crate can construct, sign and verify a custody receipt from **core's own type**
    /// while linking `capsule-core` with `default-features = false` (`S-C46`).
    ///
    /// The guard, not a feature. Nothing here uses receipts yet — `S-C15` will — and the point
    /// is that the day it does, it must not have to define its own copy. A signed structure
    /// defined at both ends is one added field away from a signature that stops verifying, and
    /// the failure would present as the server withholding receipts, which is exactly the
    /// accusation custody receipts exist to make checkable. If somebody moves the type back
    /// behind `native`, this stops compiling.
    #[test]
    fn the_server_can_sign_a_receipt_from_cores_own_type() {
        let key = HybridSigningKey::generate();
        let core = CustodyReceiptCore {
            version: "custody-receipt/v1".to_owned(),
            crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
            protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
            server_id: "capsule.example".to_owned(),
            server_key_id: Hash32([0x11; 32]),
            receipt_seq: 1,
            prior_receipt_hash: None,
            upload_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f".to_owned(),
            asset_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61".to_owned(),
            blob_role: role_str(ReceiptRole::Original).to_owned(),
            ciphertext_hash: Hash32([0x22; 32]),
            size: 4096,
            envelope_hash: None,
            uploaded_by_user: "01937b7c-0000-7000-8000-000000000001".to_owned(),
            uploaded_by_device: None,
            received_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let receipt = CustodyReceipt {
            server_sig: key.sign(&core.signing_bytes()),
            core,
        };

        assert!(receipt.verify_under(&key.verifying_key()));
        // And it survives the wire form the client decodes.
        let bytes = receipt.to_canonical_cbor();
        assert_eq!(
            CustodyReceipt::from_canonical_cbor(&bytes).expect("a receipt round-trips"),
            receipt,
        );
    }
}
