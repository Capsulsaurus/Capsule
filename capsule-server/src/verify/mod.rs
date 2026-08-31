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
//! - **Moderation holds** landed with `S-C17`: an asset under a
//!   [`ServingHold`](crate::index::ServingHold) reports every blob `stored` and **not**
//!   `retrievable`. The bytes are present and staying present; the server simply will not hand
//!   them over. Reporting `retrievable` would tell a client it may release its only local copy
//!   of something it can never fetch back, which is the exact decision this surface exists to
//!   inform.
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

/// How hard to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Ask the index and the store whether the bytes are there. Cheap, and the default.
    Structural,
    /// Also re-read and re-hash them (`S-C41`).
    ///
    /// Rate-limited per account, because a deep scan reads and hashes every declared blob: an
    /// unbounded one is an I/O-amplification attack costing the attacker one small JSON body.
    /// The contract calls the limiter *half of the feature*, and this port agrees — the flag and
    /// the budget landed together.
    Deep,
    /// A deep scan was asked for and the account's budget is spent.
    ///
    /// A third state rather than falling back to [`Depth::Structural`], because the *verdict*
    /// has to say so. A client that asked to look at the bytes and silently got a structural
    /// answer would read `deep` as absent and conclude nobody had looked — which is true, but
    /// indistinguishable from never having asked, and it is the difference between retrying
    /// later and giving up.
    RateLimited,
}

impl DeepVerdict {
    /// The name this verdict travels under.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intact => "intact",
            Self::Corrupt => "corrupt",
            Self::RateLimited => "rate_limited",
        }
    }
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
    /// What a **deep** scan found, when one was asked for and admitted (`S-C41`).
    ///
    /// `None` on a structural check, and that absence is load-bearing: it is the difference
    /// between *"we did not look at the bytes"* and *"we looked and they were fine"*, and a
    /// client deciding whether to delete its only copy must be able to tell those apart.
    pub deep: Option<DeepVerdict>,
}

/// What re-reading and re-hashing a blob's bytes found (`S-C41`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepVerdict {
    /// The bytes on disk hash to the address they are filed under.
    Intact,
    /// They do not. The blob is corrupt, whatever the structural check said.
    ///
    /// The contract's own validation bullet: *"corrupt a stored blob's bytes on disk; assert the
    /// structural check still reports `stored = true` but `deep = true` reports a hash
    /// mismatch."* `stored` is a question about the filesystem and this is a question about the
    /// bytes, and silent corruption is exactly where the two diverge.
    Corrupt,
    /// The deep scan did not run, because this account has spent its budget.
    ///
    /// Reported rather than refused for the whole request: a deep scan is an *addition* to a
    /// structural verdict, and throwing away a perfectly good structural answer because the
    /// optional half was throttled would make the limiter cost more than it saves.
    RateLimited,
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
            // A blob this asset does not hold is not re-hashed even on a deep scan: the store
            // is deliberately not asked about it at all, because answering would be a
            // cross-account existence oracle.
            deep: None,
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
    depth: Depth,
) -> Result<Vec<AssetVerdict>, VerifyUnavailable> {
    let checked_at = context.clock().now();
    let mut verdicts = Vec::with_capacity(queries.len());
    for query in queries {
        verdicts.push(verify_one(context, owner, query, checked_at, depth).await?);
    }
    Ok(verdicts)
}

/// One asset's verdict.
async fn verify_one(
    context: &VerifyContext,
    owner: &OwnerId,
    query: &AssetQuery,
    checked_at: Timestamp,
    depth: Depth,
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

        // The second place `retrievable` is not `stored`, and the one that matters most for a
        // client about to release its only local copy: a marked blob is on disk and on its way
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

        // The deep scan, when one was asked for. Only for a blob that is actually there —
        // re-hashing an absent blob has nothing to compare, and `stored = false` already says
        // everything a client needs.
        let deep = match (depth, stored) {
            (Depth::Structural, _) | (Depth::Deep | Depth::RateLimited, false) => None,
            (Depth::RateLimited, true) => Some(DeepVerdict::RateLimited),
            (Depth::Deep, true) => Some(rehash(context, hash).await?),
        };

        blobs.push(BlobVerdict {
            hash: hash.clone(),
            role: Some(held.role),
            stored,
            indexed: true,
            deep,
            // A held asset's bytes are present and will stay present — but the server will not
            // hand them over, so promising `retrievable` would be telling a client it can drop
            // its only copy of something it can never fetch back. `stored` stays true, which is
            // the honest pair: *we have your bytes, and we will not serve them.* (`S-C17`)
            retrievable: stored && !collectable && row.hold.is_none(),
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

/// How much of a blob is read into memory at once during a deep scan.
///
/// A megabyte. The point of streaming here is not speed, it is that a deep verify of a
/// multi-gigabyte original must not be a multi-gigabyte allocation — which would turn the
/// I/O-amplification attack this feature is rate-limited against into a memory one that no
/// budget bounds.
const REHASH_CHUNK_BYTES: usize = 1024 * 1024;

/// Re-read `address` and compare what it hashes to against the address it is filed under.
///
/// Streams through [`BlobStore::read_at`](crate::blob::BlobStore::read_at) a megabyte at a time
/// and never holds the whole blob. A short read ends the walk: the store is append-only and
/// committed, so fewer bytes than the address implies is itself corruption, and the digest will
/// say so.
async fn rehash(
    context: &VerifyContext,
    address: &ContentAddress,
) -> Result<DeepVerdict, VerifyUnavailable> {
    use capsule_core::crypto::hash::Sha256Hasher;

    let mut hasher = Sha256Hasher::new();
    let mut offset = 0_u64;
    loop {
        let chunk = context
            .blobs()
            .read_at(address, offset, REHASH_CHUNK_BYTES)
            .await
            .map_err(|error| {
                tracing::error!(%error, %address, "a deep verify could not read a blob");
                VerifyUnavailable("the blob store could not be read".to_owned())
            })?;
        let Some(chunk) = chunk.filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        offset = offset.saturating_add(chunk.len() as u64);
        hasher.update(&chunk);
        if chunk.len() < REHASH_CHUNK_BYTES {
            break;
        }
    }

    let actual = hasher.finalize().to_hex();
    if actual == address.as_str() {
        Ok(DeepVerdict::Intact)
    } else {
        // The finding the whole feature exists for: `stored` is a question about the filesystem
        // and this is a question about the bytes. Silent corruption is exactly where they part.
        tracing::error!(
            %address,
            found = %actual,
            "a deep verify found a blob whose bytes are not its address"
        );
        Ok(DeepVerdict::Corrupt)
    }
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
