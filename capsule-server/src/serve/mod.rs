//! Key-free ranged blob serving (`S-C10`) — the read side of the store.
//!
//! Resolves a bare **content address** to a serve decision. The server holds no key, so it
//! serves opaque ciphertext octets and never plaintext; what it decides is not *what* the bytes
//! are but whether they may be handed over at all, composed from the same facts storage
//! verification tracks: `indexed ∧ live ∧ present`.
//!
//! # The status taxonomy is the contract
//!
//! | Resolution | Status | Meaning to a client |
//! | --- | --- | --- |
//! | [`ServeResolution::Serve`] | `200`, or `206` for a range | present ∧ indexed ∧ retrievable |
//! | [`ServeResolution::NotFound`] | `404` | no live reference names the address, or it is malformed |
//! | [`ServeResolution::Gone`] | `410` | referenced but not retrievable per policy — **permanent**, so the client degrades to a lower representation |
//!
//! # The `409 error.blob.pending_upload` arm is absent, and that is a finding
//!
//! The Salvo surface rendered a fourth status: an original that is legitimately still uploading
//! is `409` and **transient**, explicitly not the permanent `410`, so a client waits instead of
//! degrading. It is not here, because in this port it is **unreachable** — and a declared status
//! nothing can reach is exactly the `S-C28` defect the rebuild exists to make impossible, so it
//! is deleted rather than declared and left dark.
//!
//! It is unreachable because the index learns a blob's address at **finalization**, which is
//! after the bytes have committed. An original whose reference exists therefore always has its
//! bytes, and one whose bytes are missing has no reference at all — so the two answers this
//! surface can give are "served" and "unknown", never "still coming". The Salvo schema differed:
//! it created a pending asset row at *session creation*, so the reference outlived the absence.
//!
//! Restoring it is not a line of code. The index would have to record a **declared** original at
//! reservation, which raises the question that decides the shape: an abandoned session would
//! leave a reference promising an original forever, turning the transient `409` into a permanent
//! one — the precise failure the `409`/`410` split exists to prevent. Filed as `S-C40` rather
//! than guessed at. Note that nothing can reach the state today anyway: a second device learns
//! an original's address from the signed manifest, and the only party holding an unfinalized
//! original's address is the device uploading it.
//!
//! # Resolution order, and why it is an order
//!
//! A malformed address is unknown **before any read**, so this path is not an oracle over
//! arbitrary strings. A reference is looked up **before** the store is touched, so a fetch for
//! an address nothing references never reaches the bytes. Only then is presence asked about.
//!
//! # What is disclosed, stated plainly
//!
//! Any authenticated account may fetch any *live* address it can name. That is a capability
//! model, not an authorization one: a content address is the hash of ciphertext, so producing
//! one without already holding the bytes is producing a preimage. The alternative — scoping the
//! fetch to the caller's own albums — cannot be written yet and would be wrong if it were:
//! shared albums, drops and federated peers all fetch blobs they do not own, and the read
//! authority that would decide those cases is `S-C4`/`S-C5` work that has no port here.
//!
//! **The `403` in the contract has no implementation, on either side.** Download & Sync says a
//! `403` on this path signals an authorization change, distinct from a durability loss, and
//! makes the client re-sync its membership before degrading. Neither this surface nor the
//! Salvo one it replaces ever renders one, because neither has a per-album read authority to
//! render it from. Recorded rather than approximated — see `S-C39`.
//!
//! # What is missing, and owned elsewhere
//!
//! - **Moderation takedown** (`served = false` → `410` before any byte is touched) is `S-C17`;
//!   there is no `served` flag on an asset row yet.
//! - **GC state** landed with `S-C11`: a blob the collector has marked is refused `410` while
//!   its bytes are still on disk, because those bytes are on their way out and a client that
//!   fetched them would be caching something about to vanish. Checked *before* the store is
//!   touched, so a marked blob's bytes are never read.
//!
//! A **quarantined** blob needs no check here and gets none: [`crate::blob::BlobStore::quarantine`]
//! moves the bytes out of the store, so a quarantined address presents as a reference with no
//! bytes and resolves to `Gone` through the dangling-reference arm. One less thing to keep in
//! step, and it is a property of the port rather than a coincidence.

use std::sync::Arc;

use bytes::Bytes;

use crate::blob::{BlobError, BlobStore, ContentAddress};
use crate::index::{AssetIndex, AssetState};

/// The media serving module's collaborators, as one injectable value.
///
/// The **same** index the feed reads and the upload path writes: a feed entry names an address
/// and this is what turns that name into bytes, so two indexes would mean a feed that points at
/// blobs this surface calls unknown.
#[derive(Debug, Clone)]
pub struct ServeContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    marks: Arc<dyn crate::gc::CollectionStore>,
}

impl ServeContext {
    /// Assembles the module from its collaborators.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        marks: Arc<dyn crate::gc::CollectionStore>,
    ) -> Self {
        Self {
            index,
            blobs,
            marks,
        }
    }

    /// The asset index the reference lookup reads.
    pub fn index(&self) -> &dyn AssetIndex {
        self.index.as_ref()
    }

    /// The blob store the bytes come from.
    pub fn blobs(&self) -> &dyn BlobStore {
        self.blobs.as_ref()
    }

    /// The collector's marks (`S-C11`).
    pub fn marks(&self) -> &dyn crate::gc::CollectionStore {
        self.marks.as_ref()
    }

    /// A handle to the store, for a [`BlobSource`] that outlives the request borrow.
    pub fn blob_handle(&self) -> Arc<dyn BlobStore> {
        Arc::clone(&self.blobs)
    }
}

/// What the server decided about one content address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeResolution {
    /// Present ∧ indexed ∧ retrievable — serve these bytes, ranged.
    Serve {
        /// The address to read from.
        address: ContentAddress,
        /// Its complete length, which every `Range` offset is relative to.
        size: u64,
    },
    /// No live reference names the address, or it is not an address at all.
    NotFound,
    /// Referenced but not retrievable per policy: a deleted asset, or a dangling reference.
    Gone,
}

/// A collaborator could not answer, so nothing was decided.
#[derive(Debug, thiserror::Error)]
#[error("the blob could not be resolved: {0}")]
pub struct ServeUnavailable(String);

/// Resolve `hash` to a serve decision.
///
/// # Errors
///
/// Returns [`ServeUnavailable`] when the index or the blob store could not answer — never for a
/// blob that is simply absent, which is a decision rather than a failure.
#[tracing::instrument(skip(context), fields(hash = %hash))]
pub async fn resolve(
    context: &ServeContext,
    hash: &str,
) -> Result<ServeResolution, ServeUnavailable> {
    // A string that is not a content address can address no committed blob. Answered as
    // unknown before any lookup, so this is not an oracle over arbitrary input.
    let Ok(address) = ContentAddress::parse(hash) else {
        tracing::trace!("a malformed content address is unknown, not a bad request");
        return Ok(ServeResolution::NotFound);
    };

    let reference = context
        .index()
        .find_reference(&address)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the asset index could not resolve a content address");
            ServeUnavailable("the index could not answer".to_owned())
        })?;

    let Some(reference) = reference else {
        tracing::debug!("no live reference names the address");
        return Ok(ServeResolution::NotFound);
    };

    // A deleted asset's blobs are gone, not unknown: the client that already has the asset must
    // learn to stop asking, and `410` is what tells it to.
    if reference.state == AssetState::Tombstoned {
        tracing::info!(asset = %reference.asset_id, "a deleted asset's blob was refused");
        return Ok(ServeResolution::Gone);
    }

    // Mid-collection (`S-C11`). Decided before the store is touched, so a marked blob's bytes
    // are never read — the bytes are still there, and that is exactly why the check has to come
    // from the mark rather than from their presence.
    if context
        .marks()
        .marked_since(&address)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the collector's marks could not be read");
            ServeUnavailable("the collection marks could not be read".to_owned())
        })?
        .is_some()
    {
        tracing::info!(asset = %reference.asset_id, "a blob awaiting collection was refused");
        return Ok(ServeResolution::Gone);
    }

    let stat = context.blobs().stat(&address).await.map_err(|error| {
        tracing::error!(%error, "the blob store could not stat an address");
        ServeUnavailable("the blob store could not answer".to_owned())
    })?;

    if let Some(stat) = stat {
        tracing::trace!(
            role = reference.role.as_str(),
            size = stat.size,
            "serving ciphertext"
        );
        return Ok(ServeResolution::Serve {
            address,
            size: stat.size,
        });
    }

    // No bytes behind a live reference. Every arm of this is a dangling reference in this port
    // — see the `S-C40` note above for why the `awaiting-original` case cannot land here, and
    // why `original_held` is carried on the reference against the slice that will need it.
    tracing::warn!(
        asset = %reference.asset_id,
        role = reference.role.as_str(),
        original_held = reference.original_held,
        "a referenced blob is missing from the store: dangling reference"
    );
    Ok(ServeResolution::Gone)
}

/// One blob, as a source Kynos can range over.
///
/// A [`ByteSource`](kynos::response::range::source::ByteSource) rather than a path, which is
/// what keeps ranged serving on the blob **port** instead of on the filesystem: an object-store
/// adapter serves resumable ranges with nothing above it changing, and a test serves them out
/// of a `BTreeMap`. Nothing here reads a whole blob into memory — the span asked for is the
/// span read.
#[derive(Debug)]
pub struct BlobSource {
    blobs: Arc<dyn BlobStore>,
    address: ContentAddress,
    size: u64,
}

impl BlobSource {
    /// A source over `address`, whose complete length has already been established.
    ///
    /// The length is passed in rather than re-`stat`ed because the resolution above already
    /// asked, and asking twice would let the two answers disagree.
    pub fn new(blobs: Arc<dyn BlobStore>, address: ContentAddress, size: u64) -> Self {
        Self {
            blobs,
            address,
            size,
        }
    }
}

impl kynos::response::range::source::ByteSource for BlobSource {
    type Error = BlobError;

    async fn complete_length(&self) -> Result<u64, Self::Error> {
        Ok(self.size)
    }

    async fn read_span(&self, first: u64, last: u64) -> Result<Bytes, Self::Error> {
        // Both offsets are within the length just reported, so the span is the store's window
        // clamp away from being safe — `read_at` clamps rather than refusing.
        let span =
            usize::try_from(last.saturating_sub(first).saturating_add(1)).unwrap_or(usize::MAX);
        let bytes = self
            .blobs
            .read_at(&self.address, first, span)
            .await?
            // The blob was there when it was stat'd and is not there now — a delete or a GC
            // sweep raced this read. Reported as the store failing to read, which is what it
            // is; the response is already committed to a status by then.
            .ok_or_else(|| BlobError::Backend {
                operation: "read a span of a blob that vanished mid-response",
                detail: format!("{} is no longer present", self.address),
            })?;
        Ok(Bytes::from(bytes))
    }
}
