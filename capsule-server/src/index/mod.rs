//! The asset index — the durable half of the upload path, and the sync feed's only source
//! (slice `S-C37`).
//!
//! # Why this is not in [`crate::store`]
//!
//! Everything in `store` is **volatile**: an upload session, a login session, a ceremony. Each
//! has a TTL that is a property of its store, each is reconstructible from a retry, and the
//! adapter that will serve them in production is Valkey. Nothing here is any of those things.
//! An asset row is the library — losing one loses a photo's existence, not a transfer's
//! progress — so it has no TTL, no expiry semantics, and exactly one production adapter shape:
//! PostgreSQL. Folding it into `AuthStateStore`'s neighbourhood would put a permanent record
//! behind a port whose whole contract is "this disappears on schedule", which is how the Salvo
//! grab-bag started.
//!
//! # One sequence, not two — which is `S-C21`'s fix rather than its patch
//!
//! The retired feed had **two** ordering keys: a per-album `sync_seq` the client checked for
//! rewinds, and a global `bigserial feed_seq` the cursor paged over. The race lived in the
//! second one and only because it was a second one. A PostgreSQL sequence is deliberately
//! **non-transactional**: `nextval` hands out 5 and 6 to two concurrent finalizations and does
//! not roll back, so if 6 commits first a reader can page past it and 5 becomes permanently
//! invisible. No amount of care at the reader fixes that, because the gap is indistinguishable
//! from a rolled-back write.
//!
//! This port has one sequence per **owner**, and its numbers are allocated from a row the
//! allocating transaction locks until it commits (`UPDATE … SET next_seq = next_seq + 1 …
//! RETURNING`). Allocation order is therefore commit order, and the skip window is not
//! expressible. It also removes the need for the second key: an album's entries are a
//! *subsequence* of its owner's, and the restriction of a strictly increasing sequence is
//! strictly increasing, so one number satisfies both the cursor's ordering and the client's
//! per-album anti-rewind check. The numbers an album sees have gaps; the design contract asks
//! for monotonicity, never contiguity, and a client that required contiguity would break the
//! moment a sibling album wrote.
//!
//! The cost is honest and worth stating: finalizations **within one library** serialize on that
//! library's counter row for the length of one `UPDATE`. Across libraries there is no
//! contention at all. That is the price of a feed with no skip window, and it is the right
//! trade for a per-user photo library.
//!
//! # Visibility is a predicate over the bundle, not over one blob
//!
//! [`crate::upload::visibility`] owns the definition and this port consumes it. Note what the
//! definition became: the upload protocol says an asset becomes visible "once its **manifest
//! and metadata blob** are finalized", and since `S-C30` the manifest is a blob of its own — so
//! the gate is two finalized sessions, and a per-role `did this one flip visibility` function
//! cannot express it. See [`crate::upload::visibility::bundle_is_publishable`].
//!
//! # What this port never does
//!
//! - **It never touches bytes.** It holds content *addresses*; the provenance blob's bytes are
//!   read through [`crate::blob::BlobStore`] by whoever serves them. That is what keeps
//!   `S-C30`'s verbatim rule enforceable: there is no path from the index to a byte, so the
//!   index cannot re-serialize a manifest even by accident.
//! - **It never decides what a change means to a reader.** [`ChangeKind`] is computed per page
//!   against the reader's own cursor, because "created" and "updated" are facts about the
//!   *reader's* history, not about the asset. A row knows when it was first published; only the
//!   cursor knows whether that was before or after this client last looked.
//! - **It never hands a row to a caller who did not reserve it.** See [`Reservation::Conflict`].

pub mod conformance;
pub mod memory;

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use crate::blob::ContentAddress;
use crate::store::{AlbumId, AssetId, BlobRole, OwnerId, StoreFuture};

/// The future every index operation returns.
///
/// The same [`StoreFuture`] the state ports use, and deliberately the same [`crate::store::StoreError`]
/// behind it: "the backend is unreachable / refused / holds something undecodable" is the same
/// three-way distinction whichever durable thing failed, and a second taxonomy saying the same
/// three things would only give routes two shapes to map onto one set of `error.*` codes.
pub type IndexFuture<'a, T> = StoreFuture<'a, T>;

/// Where an asset row is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssetState {
    /// Reserved by an upload session; its index tier is incomplete, so it is invisible to every
    /// device including the one uploading it.
    Pending,
    /// Publishable: the manifest and metadata blobs are both held.
    Visible,
    /// Deleted. The row is retained deliberately — a device that has been offline learns of the
    /// deletion from the tombstone, and a dropped row would read as "never existed", which is
    /// exactly the stale-revival shape the provenance chain exists to refuse.
    Tombstoned,
}

/// Why the server refuses to serve an asset whose bytes it still holds (`S-C17`).
///
/// Orthogonal to [`AssetState`] rather than a fourth variant of it, and deliberately: a hold is
/// **reversible**, and lifting one has to restore whatever state the asset was already in. A
/// state machine that swallowed `Visible` on takedown would have to remember what to put back,
/// which is a second copy of a fact the row already holds.
///
/// A hold never deletes bytes. design/moderation.md is explicit that a takedown is a serving
/// constraint and not a destruction — the user owns the data and can still restore from their
/// own backup — so this changes what the server hands out and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServingHold {
    /// An admin takedown. Reversible at admin discretion.
    Takedown,
    /// A legal hold: unservable while the law requires it.
    ///
    /// Distinguished from [`ServingHold::Takedown`] because who may lift it differs — a legal
    /// hold ends when the obligation does, not when an admin decides. The distinction is
    /// carried here rather than in an admin surface's policy code so that a store, a log line
    /// and an audit record all say the same thing about why an asset stopped serving.
    LegalHold,
}

impl ServingHold {
    /// The name this hold travels under, for a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Takedown => "takedown",
            Self::LegalHold => "legal_hold",
        }
    }
}

/// What applying a hold did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldOutcome {
    /// The row's hold is now what was asked for, and was not before.
    Applied,
    /// The row already carried exactly that hold, or already carried none. Nothing was written.
    ///
    /// Distinguished from [`HoldOutcome::Applied`] so a moderation action that is a no-op is
    /// visible as one: re-applying a takedown must not append a second provenance record
    /// claiming the asset was taken down twice.
    Unchanged,
    /// No such asset.
    NotFound,
}

/// One blob the asset holds, as the index knows it: a role and an address, never bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobRef {
    /// The blob's role in its bundle.
    pub role: BlobRole,
    /// Its ciphertext content address.
    pub address: ContentAddress,
    /// Its size in bytes, as finalization measured it — what lets a client budget a fetch
    /// before issuing one.
    pub size: u64,
}

/// The durable row an upload session reserves before any byte lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAsset {
    /// The asset the bundle belongs to.
    pub asset_id: AssetId,
    /// The billing and namespace entity it is filed under.
    pub owner_id: OwnerId,
    /// The album it is filed into.
    pub album_id: AlbumId,
    /// The album's pinned protocol date (`YYYY-MM-DD`) at reservation.
    pub protocol_version: String,
    /// The crypto suite the bundle was sealed under.
    pub crypto_suite_id: u16,
    /// When the first session of this bundle reserved the row.
    pub created_at: Timestamp,
}

/// An asset as the index holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRow {
    /// The asset's identifier.
    pub asset_id: AssetId,
    /// The owner the row is filed under.
    pub owner_id: OwnerId,
    /// The album it belongs to.
    pub album_id: AlbumId,
    /// The album's pinned protocol date at reservation.
    pub protocol_version: String,
    /// The crypto suite the bundle was sealed under.
    pub crypto_suite_id: u16,
    /// Where the row is in its life.
    pub state: AssetState,
    /// Every blob finalized against this asset, ordered by role then address.
    pub blobs: Vec<BlobRef>,
    /// The sequence number the row was **first** published at, if it ever was.
    ///
    /// Never moves once set. It is what makes [`ChangeKind`] answerable: a reader whose cursor
    /// is below this number has never seen the asset, whatever has happened to it since.
    pub first_seq: Option<u64>,
    /// The sequence number of the row's most recent publishable change, if any.
    ///
    /// `None` while the row is [`AssetState::Pending`] — a row nothing can see yet has no place
    /// in the feed, which is what keeps an abandoned half-bundle from consuming a number.
    pub sync_seq: Option<u64>,
    /// The content hash of the last manifest accepted for this asset — invariant 17's
    /// *stored chain head*.
    ///
    /// `None` until a lifecycle manifest has been accepted. Stored explicitly rather than
    /// derived from the provenance blob's content address: with `crypto_suite_id = 1` the two
    /// are digests of the same byte string and therefore equal, but they are **not** the same
    /// identifier — a content address is whatever digest the suite selects, while
    /// `prior_provenance_hash` is fixed at SHA-256. `S-C31` records that trap; relying on the
    /// coincidence here would set it.
    pub chain_head: Option<Hash32>,
    /// The album-key epoch the last accepted manifest was written under — invariant 18's
    /// subject.
    ///
    /// Per **row**, with the album's high-water mark taken as the maximum over its rows. A
    /// separate album table would be a second home for the same fact, and the two would
    /// eventually disagree about an album whose newest asset was rolled back.
    pub amk_version: u64,
    /// The serving hold moderation has placed on the asset, if any (`S-C17`).
    ///
    /// `None` is the ordinary case. A held row keeps every one of its blobs and its place in
    /// the feed; what changes is that the serving path and storage verification both refuse to
    /// hand the bytes over or to promise they can be fetched.
    pub hold: Option<ServingHold>,
    /// The instant the asset's signed `delete` manifest fixed as its retention floor.
    ///
    /// `None` until a delete carries one. The purge worker reads it here rather than from a
    /// server policy, which is the whole point: the floor is *signed*, so a hostile server
    /// cannot accelerate a purge by editing a config and a buggy one cannot retain past the
    /// window the user chose.
    pub retention_until: Option<Timestamp>,
    /// When the row was reserved.
    pub created_at: Timestamp,
    /// When it last changed.
    pub updated_at: Timestamp,
}

impl AssetRow {
    /// The address of the single blob holding `role`, if the asset holds one.
    ///
    /// Meaningful only for the singular roles — [`BlobRole::Original`], [`BlobRole::Metadata`],
    /// [`BlobRole::Provenance`]. A bundle may hold many derivatives, so
    /// [`Self::derivatives`] is the accessor for those.
    pub fn address_for(&self, role: BlobRole) -> Option<&ContentAddress> {
        self.blobs
            .iter()
            .find(|blob| blob.role == role)
            .map(|blob| &blob.address)
    }

    /// Every derivative the asset holds, in address order.
    pub fn derivatives(&self) -> impl Iterator<Item = &BlobRef> {
        self.blobs
            .iter()
            .filter(|blob| blob.role == BlobRole::Derivative)
    }

    /// Whether the original blob has landed — the `original_held` completeness fact, derived
    /// here rather than stored so it cannot disagree with the blob rows it summarises.
    pub fn original_held(&self) -> bool {
        crate::upload::visibility::derive_original_held(
            self.address_for(BlobRole::Original).is_some(),
        )
    }

    /// Whether the bundle's index tier is complete.
    pub fn is_publishable(&self) -> bool {
        crate::upload::visibility::bundle_is_publishable(self.blobs.iter().map(|blob| blob.role))
    }
}

/// What reserving a pending row did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// The row is new.
    Created(Box<AssetRow>),
    /// A row already existed and agrees with the declaration: this is a second session of the
    /// same bundle, which is the normal case — a bundle is several uploads.
    Joined(Box<AssetRow>),
    /// A row exists under a different owner, album or protocol pin.
    ///
    /// Deliberately carries **nothing**. A caller that hits this is by definition not the party
    /// the existing row belongs to, so handing back the row would answer a guessed asset id
    /// with another account's album — the same disclosure argument that blocks `S-C22`'s
    /// `409 duplicate_blob`, in the one place where the guess costs the attacker nothing.
    Conflict,
}

/// One blob finalization, as reported to the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRecord {
    /// The blob's role.
    pub role: BlobRole,
    /// Its content address.
    pub address: ContentAddress,
    /// Its size in bytes.
    pub size: u64,
    /// When finalization committed it.
    pub finalized_at: Timestamp,
}

/// What recording a finalized blob did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobOutcome {
    /// The blob is now held.
    Recorded {
        /// The asset as it now stands.
        row: Box<AssetRow>,
        /// The sequence number this minted, or `None` when the asset is still pending — a blob
        /// landing on an incomplete bundle changes nothing any client can observe, so it takes
        /// no number and produces no feed entry.
        minted: Option<u64>,
    },
    /// The blob was already held at this exact address: a retried finalization.
    ///
    /// Idempotent by address rather than by role, so a genuine retry is free and a *different*
    /// address for a singular role is a [`BlobOutcome::Conflict`] rather than a silent
    /// re-point.
    AlreadyHeld(Box<AssetRow>),
    /// The role is singular and the asset already holds a different address for it.
    ///
    /// Refused rather than overwritten: the original and the metadata blob are named by the
    /// signed manifest, so letting a later session re-point them would let an authorized device
    /// swap the bytes under a signature that still verifies against the old ones.
    Conflict,
    /// There is no row for this asset — it was never reserved, or it was purged.
    NotFound,
}

/// What a feed entry is, relative to the reader asking for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChangeKind {
    /// The reader has never seen this asset: its first publication is above the reader's
    /// cursor.
    Created,
    /// The reader has seen it before and something about it has changed since.
    Updated,
    /// It is deleted. Emitted even to a reader that never saw the asset, which no-ops on it;
    /// the alternative — suppressing tombstones for readers below `first_seq` — would make a
    /// page's size depend on who is reading it for no benefit either side can observe.
    Deleted,
}

impl ChangeKind {
    /// The stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Deleted => "deleted",
        }
    }
}

/// What the index knows about one content address, for the serving path.
///
/// Deliberately **not** owner-scoped, unlike [`AssetIndex::find_by_address`], and the asymmetry
/// is the point rather than an oversight. That lookup answers "which asset of *yours* holds
/// these bytes" and hands the client an asset id, so scoping it is what stops one account
/// learning another's holdings. This one answers "is this address live", and its answer reaches
/// the client only as a status — served, gone, or unknown. A caller must already hold the
/// content address to ask, and a content address is the hash of ciphertext nobody can produce
/// without the key, so it is a capability rather than a guessable name.
///
/// See [`crate::serve`] for what the reachable disclosure actually is, and for the
/// album-scoped `403` the download-sync contract describes and neither implementation renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobReference {
    /// The asset the reference belongs to.
    pub asset_id: AssetId,
    /// The role the address holds in that asset's bundle.
    pub role: BlobRole,
    /// The asset's lifecycle state — `Tombstoned` is what turns a reference into a `410`.
    pub state: AssetState,
    /// Whether that asset's original has landed, which is what tells a missing original apart
    /// from a dangling reference.
    pub original_held: bool,
    /// The serving hold on the asset the reference belongs to (`S-C17`), if any.
    ///
    /// Carried on the reference rather than looked up separately, so the serving path decides
    /// a takedown from the *same read* that found the reference — one round trip, and no window
    /// in which a hold applied between the two reads is missed.
    pub hold: Option<ServingHold>,
}

/// A lifecycle write that does not move blob bytes (`S-C16`).
///
/// The closed set is the authorization doc's: everything a `POST /albums/{id}/ops` may carry.
/// `create` and `replace` are absent because they move bytes and are therefore uploads —
/// `create` is `S-C1`'s and `replace` is `S-C43`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpAction {
    /// Soft-delete: the asset is tombstoned and recoverable until its retention window closes.
    Delete,
    /// The inverse of a delete, before that window closes.
    TrashRestore,
    /// A new encrypted metadata blob for an asset that keeps its bytes.
    MetadataUpdate,
    /// A derivative whose bytes the server already holds is attached, or re-pointed.
    Derivative,
}

impl OpAction {
    /// The stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::TrashRestore => "trash-restore",
            Self::MetadataUpdate => "metadata-update",
            Self::Derivative => "derivative",
        }
    }
}

/// One accepted lifecycle write, as the index applies it.
///
/// Every field is a fact the gate has already checked. The index decides the two *stateful*
/// invariants it alone can answer — 17's chain and 18's epoch — and applies the change with its
/// sequence number in one critical section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleOp {
    /// The asset the manifest names.
    pub asset_id: AssetId,
    /// The caller, so the index checks ownership rather than trusting it.
    pub owner_id: OwnerId,
    /// The album the op was addressed to.
    pub album_id: AlbumId,
    /// What is being done.
    pub action: OpAction,
    /// The content hash of the manifest being applied. Becomes the new chain head, and is the
    /// idempotency key — a byte-identical resubmission has the same hash.
    pub manifest_hash: Hash32,
    /// What the manifest claims its predecessor's hash was — invariant 17's subject.
    pub prior_provenance_hash: Option<Hash32>,
    /// The album epoch it was written under — invariant 18's subject.
    pub amk_version: u64,
    /// The stored provenance blob holding the signed manifest, so the feed serves those exact
    /// bytes (`S-C30`) for a lifecycle write as it does for an upload.
    pub provenance: ContentAddress,
    /// The stored metadata blob, when the action carries one.
    pub metadata: Option<ContentAddress>,
    /// The retention floor the manifest signed, when it carries one.
    ///
    /// Meaningful on a `delete`. A `trash-restore` clears it, because an asset back in the live
    /// set has no window to run out.
    pub retention_until: Option<Timestamp>,
    /// When the server accepted it.
    pub at: Timestamp,
}

/// What applying a lifecycle write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpOutcome {
    /// Applied, with the feed position it minted.
    Applied {
        /// The row as it now stands.
        row: Box<AssetRow>,
        /// The sequence number this change occupies.
        sync_seq: u64,
    },
    /// This exact manifest has already been applied. Nothing was written, and the number is the
    /// one the original application minted.
    ///
    /// Which is what makes the response byte-identical without remembering any bytes: the body
    /// is a pure function of `(asset_id, action, sync_seq)` and all three are stored facts. The
    /// retired implementation kept the serialized response in a table — a second copy of
    /// something derivable, and therefore a second thing that can be wrong.
    Replayed {
        /// The sequence number the first application minted.
        sync_seq: u64,
    },
    /// Invariant 17: `prior_provenance_hash` is not this asset's chain head.
    StaleChain {
        /// What the head actually is, so the owner can re-read and rebase rather than retry a
        /// losing manifest forever. Reached only by the asset's owner, so it discloses nothing.
        head: Option<Hash32>,
    },
    /// Invariant 18: the epoch regresses against the album's high-water mark.
    AmkRegressed {
        /// The album's highest accepted epoch.
        stored: u64,
    },
    /// No such asset, not this caller's, or not in that album.
    ///
    /// One value for all three, as [`Reservation::Conflict`] is: the asset id is client-chosen,
    /// so a guess must buy nothing.
    NotFound,
}

/// One entry of the sync feed.
///
/// Carries addresses, never bytes. The route that serves a page reads the provenance blob
/// through [`crate::blob::BlobStore`] and emits those bytes verbatim (`S-C30`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEntry {
    /// The asset that changed.
    pub asset_id: AssetId,
    /// The album it belongs to. The client's anti-rewind high-water mark is kept per album.
    pub album_id: AlbumId,
    /// The album's pinned protocol date, which the client checks against its own maximum
    /// before applying anything.
    pub protocol_version: String,
    /// The entry's position in the owner's sequence — strictly increasing within a page, and
    /// therefore strictly increasing within any album the page touches.
    pub sync_seq: u64,
    /// What this is to the reader who asked.
    pub change: ChangeKind,
    /// The address of the provenance blob holding the signed manifest, when the asset has one.
    pub provenance: Option<ContentAddress>,
    /// The address of the encrypted metadata blob, when the asset has one.
    pub metadata: Option<ContentAddress>,
    /// The original and derivative blobs the asset holds.
    pub blobs: Vec<BlobRef>,
    /// Whether the original has landed — `false` is the derived `awaiting-original` state.
    pub original_held: bool,
    /// When the change happened.
    pub at: Timestamp,
}

/// The durable asset index.
pub trait AssetIndex: std::fmt::Debug + Send + Sync {
    /// Reserve `asset`'s row, or join the one a sibling session already reserved.
    ///
    /// Idempotent on the asset id, which is what lets every session of a bundle call it
    /// unconditionally at creation without the client having to elect one to go first.
    fn reserve(&self, asset: PendingAsset) -> IndexFuture<'_, Reservation>;

    /// The row for `asset`, whatever its state.
    fn read<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>>;

    /// Record that `blob` finalized against `asset`, minting the owner's next sequence number
    /// in the same operation when the change is one a client can observe.
    ///
    /// One call, because the blob row, the state transition and the sequence number describe
    /// one event — the same reason [`crate::store::UploadSessionStore::record_progress`] is one
    /// call. Splitting them is how the retired server could publish an asset whose sequence
    /// number had not been minted yet.
    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> IndexFuture<'a, BlobOutcome>;

    /// Tombstone `asset`, minting a sequence number so the deletion reaches every device.
    ///
    /// Tombstoning a still-pending row publishes nothing: nobody ever saw it, so there is
    /// nothing to retract. The row still becomes [`AssetState::Tombstoned`] so its id cannot be
    /// reserved back into life.
    fn tombstone<'a>(
        &'a self,
        asset: &'a AssetId,
        at: Timestamp,
    ) -> IndexFuture<'a, Option<AssetRow>>;

    /// The asset already holding `address` under `(owner, album)`, if there is one.
    ///
    /// The signature **is** the idempotency key the validation doc fixes for session creation —
    /// `(owner_id, hash, album_id)` — rather than a convention a caller is trusted to apply.
    /// Both scopes are load-bearing and for different reasons:
    ///
    /// - **Owner** is the disclosure boundary. The blob store could say whether *anyone* holds
    ///   a ciphertext, and answering `S-C22`'s `existing_asset` from that would tell one account
    ///   what another holds — which content addressing makes a real cross-tenant leak.
    /// - **Album** is the merge contract. A `409 duplicate_blob` is the *client's merge
    ///   trigger*: it means "you already have these bytes here, reconcile the two assets
    ///   locally". Across two albums there is nothing to merge — the same thumbnail
    ///   legitimately belongs to assets in both — so the second upload proceeds and the blob
    ///   store deduplicates it onto the occupied address instead.
    fn find_by_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<AssetId>>;

    /// What the index knows about `address`, for the serving path — see [`BlobReference`].
    ///
    /// **Pending rows do not count as references.** An asset whose index tier has not landed is
    /// in nobody's feed, so nothing can legitimately have learned its addresses; treating it as
    /// unknown keeps a half-finished upload from being fetchable before it is published.
    ///
    /// **A visible reference outranks a tombstoned one.** Content addressing means two assets
    /// share a thumbnail, so deleting one of them must not take the other's bytes with it. The
    /// retired implementation took the *newest* reference and would have answered `410` for a
    /// blob a live asset still holds; this asks whether any live asset holds it.
    fn find_reference<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<BlobReference>>;

    /// Apply one lifecycle write, with the two stateful invariants and the sequence mint in one
    /// critical section.
    ///
    /// They are decided **here** rather than by the caller for the reason
    /// [`crate::directory`]'s monotonic guard is: a caller that reads the chain head, compares
    /// and then writes has a window in which a concurrent op lands, and two ops racing through
    /// it can both chain onto the same head — which is the double-apply invariant 17 exists to
    /// catch. An adapter owes atomicity across the whole sequence.
    fn apply_op(&self, op: LifecycleOp) -> IndexFuture<'_, OpOutcome>;

    /// Place, replace or lift `asset`'s serving hold (`S-C17`).
    ///
    /// `None` lifts. A hold is a property of the asset rather than of its blobs, because
    /// content addressing means two assets can share a thumbnail and taking one down must not
    /// take the other's bytes with it — the same reason
    /// [`AssetIndex::find_reference`] prefers a live reference over a tombstoned one.
    ///
    /// The bytes are never touched. design/moderation.md: a takedown is a serving constraint,
    /// not a destruction, and is reversible by default.
    fn set_hold<'a>(
        &'a self,
        asset: &'a AssetId,
        hold: Option<ServingHold>,
    ) -> IndexFuture<'a, HoldOutcome>;

    /// How many asset rows reference `address`.
    ///
    /// A **query**, which is the contract: a blob's reference count is derived from the rows
    /// that name it and is never a separately-maintained number. A counter is a second copy of
    /// a derivable fact, and a counter that drifts low deletes a live blob.
    ///
    /// Tombstoned rows count. Deleting is not purging — a deleted asset's bytes stay until the
    /// retention window it signed has passed, which is what makes trash recoverable.
    fn reference_count<'a>(&'a self, address: &'a ContentAddress) -> IndexFuture<'a, u64>;

    /// Up to `limit` rows in asset-id order, after `after`.
    ///
    /// The integrity scrub's walk. Paged and ordered so a store too large to hold in memory can
    /// still be checked, and so an interrupted pass resumes where it stopped rather than
    /// starting over — which for a store worth scrubbing is the difference between a check that
    /// finishes and one that never does.
    ///
    /// Every row, whatever its state. A scrub that skipped pending or tombstoned rows would
    /// skip exactly the rows a half-finished write leaves behind.
    fn rows<'a>(
        &'a self,
        after: Option<&'a AssetId>,
        limit: usize,
    ) -> IndexFuture<'a, Vec<AssetRow>>;

    /// Up to `limit` tombstoned rows, oldest change first.
    ///
    /// The purge worker's input. Ordered so a bounded pass makes progress on the oldest
    /// deletions rather than revisiting the same page.
    fn tombstoned(&self, limit: usize) -> IndexFuture<'_, Vec<AssetRow>>;

    /// Drop `asset`'s blob references, keeping the tombstone.
    ///
    /// The row **stays**. A client that has not synced since the delete still has to learn
    /// about it, so removing the row would make the deletion invisible rather than final —
    /// and the feed entry a tombstone produces already carries no byte references, so the
    /// purge changes nothing a reader can see.
    fn purge<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>>;

    /// Up to `limit` feed entries for `owner` after sequence number `after`, in sequence order.
    ///
    /// `after` is exclusive and `0` is "from the beginning", which is why sequence numbers start
    /// at 1: a fresh client's cursor and "I have seen nothing" are the same value, so a client
    /// needs no special first-call shape.
    fn feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>>;

    /// The highest sequence number `owner`'s feed has minted, or `0` for an empty library.
    ///
    /// What lets a page report whether the client is caught up without asking for another page
    /// that would come back empty.
    fn head_seq<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64>;
}

/// Build the feed entry a row presents to a reader sitting at `after`.
///
/// Free function rather than a method so every adapter renders an entry identically: the
/// [`ChangeKind`] rule in particular is the kind of thing two adapters would drift on.
pub(crate) fn entry_for(row: &AssetRow, after: u64) -> Option<FeedEntry> {
    let sync_seq = row.sync_seq?;
    let change = match row.state {
        AssetState::Tombstoned => ChangeKind::Deleted,
        // A pending row has no `sync_seq`, so it never reaches here.
        AssetState::Pending | AssetState::Visible => {
            if row.first_seq.is_some_and(|first| first > after) {
                ChangeKind::Created
            } else {
                ChangeKind::Updated
            }
        }
    };

    Some(FeedEntry {
        asset_id: row.asset_id.clone(),
        album_id: row.album_id.clone(),
        protocol_version: row.protocol_version.clone(),
        sync_seq,
        change,
        provenance: row.address_for(BlobRole::Provenance).cloned(),
        metadata: row.address_for(BlobRole::Metadata).cloned(),
        blobs: row
            .blobs
            .iter()
            .filter(|blob| matches!(blob.role, BlobRole::Original | BlobRole::Derivative))
            .cloned()
            .collect(),
        original_held: row.original_held(),
        at: row.updated_at,
    })
}
