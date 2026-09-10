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
//! | [`ServeResolution::Serve`] | `200`, or `206` for a range | present ∧ indexed ∧ retrievable ∧ the caller's |
//! | [`ServeResolution::AwaitingUpload`] | `409` | nothing references the address **yet** — the caller's own device has an upload of exactly these bytes in flight. **Transient**, so the client waits |
//! | [`ServeResolution::NotFound`] | `404` | no live reference names the address, or it is malformed |
//! | [`ServeResolution::Gone`] | `410` | referenced but not retrievable per policy — **permanent**, so the client degrades to a lower representation |
//!
//! Three distinct facts collapse into that one `410` — a deleted asset, a blob awaiting
//! collection, and a moderation hold — and they collapse deliberately. The client's action is
//! identical in all three (stop asking, degrade), and a status that told an anonymous fetcher
//! *which* it was would turn this path into a moderation oracle: whether a given asset was
//! taken down is exactly what a takedown does not owe a peer. The distinction is in the log
//! line and, for the owner, in the audit record — not on the wire.
//!
//! # Where the transient `409` comes from, and where it deliberately does not (`S-C40`)
//!
//! An original that is legitimately still uploading is `409` and **transient**, explicitly not
//! the permanent `410`, so a client waits instead of degrading to a thumbnail forever. The port
//! could not render it at first, because the index learns a blob's address at **finalization**:
//! a reference that exists always has its bytes, and bytes that are missing have no reference —
//! "served" and "unknown", never "still coming". It was deleted rather than declared and left
//! dark, which is the `S-C28` rule applied to a status this surface wanted.
//!
//! **The declaration turned out to be somewhere it already was.** The obvious fix — and the one
//! `S-C40` was filed predicting — was to record a *declared* original in the asset index at
//! reservation. That is the Salvo schema, and it is worse than it looks:
//!
//! - an abandoned session would leave a reference promising an original forever, turning the
//!   transient `409` into a permanent one — the precise failure the `409`/`410` split exists to
//!   prevent — so it would need a lifetime and a reconciliation of its own;
//! - and every in-flight upload would become a **reference with no bytes**, which is exactly
//!   what the integrity scrub (`S-C14`) is built to report as a dangling reference. The fix
//!   would have made a normal Tuesday afternoon look like corruption.
//!
//! An active upload session declaring that hash already *is* the promise. It already carries a
//! bounded lifetime (24 hours, `LIFETIME_CAP`), is already reconciled by the discard worker, and
//! is already removed when the upload is abandoned. So the resolution asks
//! [`UploadSessionStore::pending_for_address`](crate::store::UploadSessionStore::pending_for_address)
//! when nothing references an address, and the promise cannot outlive the thing that made it.
//!
//! **It is scoped to the caller's own account.** Unscoped, the `409` would tell any
//! authenticated caller who can name a hash that *somebody, somewhere* is uploading those exact
//! bytes. That is a small cross-account signal for no gain: the case the transient answer exists
//! for is a second device of the **same account** fetching an original the first one is still
//! sending, having learned its address from the signed manifest. A sharee or a federated peer
//! gets the `404` they got before, and waits for the feed's `original_held` to flip instead.
//!
//! # Resolution order, and why it is an order
//!
//! A malformed address is unknown **before any read**, so this path is not an oracle over
//! arbitrary strings. A reference is looked up **before** the store is touched, so a fetch for
//! an address nothing references never reaches the bytes. Only then is presence asked about.
//!
//! # What is disclosed, stated plainly (`S-C39`, `S-C51`)
//!
//! **An account fetches the blobs of its own assets and of the albums it is currently a member
//! of, and nothing else.** Until `S-C39` any authenticated account could fetch any live address
//! it could name, defended as a capability model — a content address is the hash of ciphertext,
//! so producing one without holding the bytes is producing a preimage. The defence is not wrong
//! and it is not the contract, and it stacks badly besides: an address that leaks once is a
//! permanent capability, because the address never changes.
//!
//! The decision is [`ReadAuthority`]'s, asked from the reference the index returned and asked
//! **first**. A former member of the album — an account the owner's roster once named and no
//! longer does — is answered [`ServeResolution::Forbidden`], the `403` the download contract
//! describes as an authorization change; everyone else with no relationship is told exactly
//! what a caller naming an unknown address is told. See [`crate::serve::authority`] for why the
//! `403`/`404` boundary is drawn there and not one step further out, and [`crate::membership`]
//! for where the fact behind the `403` comes from.
//!
//! Non-accounts are not locked out of shared content either: `/s/{id}/blob/{hash}` serves
//! exactly the addresses a share link enumerates, and the drop surface serves its own. Neither
//! routes through here.
//!
//! # What is missing, and owned elsewhere
//!
//! - **Moderation takedown** landed with `S-C17`: an asset under a
//!   [`ServingHold`](crate::index::ServingHold) is refused `410` before any byte is touched.
//!   The bytes stay exactly where they are — a takedown is a serving constraint and not a
//!   destruction — so this answer can only come from the index, never from the store.
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

pub mod authority;

pub use self::authority::{
    BlobReadAccess, MembershipAuthority, ReadAuthority, ReadAuthorityError, ReadAuthorityFuture,
    membership_reads,
};
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
    uploads: Arc<dyn crate::store::UploadSessionStore>,
    authority: Arc<dyn ReadAuthority>,
}

impl ServeContext {
    /// Assembles the module from its collaborators.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        marks: Arc<dyn crate::gc::CollectionStore>,
        uploads: Arc<dyn crate::store::UploadSessionStore>,
        authority: Arc<dyn ReadAuthority>,
    ) -> Self {
        Self {
            index,
            blobs,
            marks,
            uploads,
            authority,
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

    /// The upload sessions, which are where an unfinalized blob's promise lives (`S-C40`).
    ///
    /// The **same** store the upload routes open sessions in, for the same reason the index is
    /// the same one the feed reads: two stores would mean a transient answer about an upload
    /// nobody is making.
    pub fn uploads(&self) -> &dyn crate::store::UploadSessionStore {
        self.uploads.as_ref()
    }

    /// Who may read a blob (`S-C39`).
    pub fn authority(&self) -> &dyn ReadAuthority {
        self.authority.as_ref()
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
    /// Nothing references the address **yet**, and the caller has an upload of exactly these
    /// bytes in flight (`S-C40`).
    ///
    /// Transient. The client waits and retries rather than degrading, which is the whole reason
    /// this is not folded into [`Self::NotFound`].
    AwaitingUpload {
        /// The session that promised the bytes, for the log line. Never put on the wire: it is
        /// another device's upload identifier and the fetcher has no use for it.
        upload: crate::store::UploadId,
    },
    /// No live reference names the address, it is not an address at all, or the caller has no
    /// relationship to the asset that holds it.
    NotFound,
    /// The caller once had access to the album that holds it and does not now (`S-C51`).
    ///
    /// The one refusal that discloses the address is live, and it discloses it only to an
    /// account the server holds a revoked membership row for. Decided **before** every policy
    /// refusal below, so a former member learns nothing about holds or deletions either.
    Forbidden,
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
#[tracing::instrument(skip(context), fields(hash = %hash, owner = %owner))]
pub async fn resolve(
    context: &ServeContext,
    owner: &crate::store::OwnerId,
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
        // Nothing references it. Before answering "unknown", ask whether the caller's own
        // account is in the middle of putting it there (`S-C40`) — the difference between
        // "never heard of it" and "your other device is still sending it" is the difference
        // between a client degrading permanently and a client waiting.
        if let Some(upload) = context
            .uploads()
            .pending_for_address(owner, hash)
            .await
            .map_err(|error| {
                tracing::error!(%error, "the upload sessions could not be asked about an address");
                ServeUnavailable("the upload sessions could not answer".to_owned())
            })?
        {
            tracing::debug!(%upload, "the address is not referenced yet: an upload is in flight");
            return Ok(ServeResolution::AwaitingUpload { upload });
        }
        tracing::debug!("no live reference names the address");
        return Ok(ServeResolution::NotFound);
    };

    // Who is asking (`S-C39`). **First among the refusals**, and the order is the security
    // property: every answer below this line — the takedown `410`, the tombstone `410`, the
    // collection `410`, the dangling `410` — is a fact about somebody's asset, and a caller with
    // no relationship to it must not be able to read any of them off the status line. Deciding
    // ownership last would have turned this path into an oracle that reports on other accounts'
    // deletions.
    match context
        .authority()
        .blob_read_access(owner, &reference)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the read authority could not decide a blob fetch");
            ServeUnavailable("the read authority could not decide".to_owned())
        })? {
        BlobReadAccess::Granted => {}
        BlobReadAccess::Revoked => return Ok(ServeResolution::Forbidden),
        BlobReadAccess::Unrelated => return Ok(ServeResolution::NotFound),
    }

    // Moderation takedown (`S-C17`). First among the refusals and **before any read**: a held
    // asset's bytes are on disk and completely intact — the hold is a serving constraint, not a
    // destruction — so nothing about the store can produce this answer and it has to come from
    // the index. `410` rather than `404` is the moderation doc's explicit per-surface rule:
    // capability-URL serving answers `404` because it must not confirm a URL ever existed,
    // while a takedown signals removal of content the fetcher already knows about.
    if let Some(hold) = reference.hold {
        tracing::info!(
            asset = %reference.asset_id,
            hold = hold.as_str(),
            "a blob under a serving hold was refused; its bytes are untouched"
        );
        return Ok(ServeResolution::Gone);
    }

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

    // No bytes behind a live reference, and this stays `Gone` unconditionally. A reference is
    // written at finalization, *after* the bytes commit, so a reference without bytes means the
    // bytes were removed — never that they have not arrived. That is why `S-C40`'s transient
    // answer is decided in the no-reference arm above and not here: putting it here would
    // require the index to hold references to bytes that do not exist yet, which is the shape
    // the integrity scrub reports as corruption.
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
