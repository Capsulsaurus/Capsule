//! Shared read/write access to the key-free sync feed (slice `S-C2`).
//!
//! The **write** side ([`Mutation::record_finalization`]) is called from the upload
//! finalization transaction (slice `S-C1`): it mints the next per-album `sync_seq` under
//! the album counter's row lock and appends one feed row, atomically with the asset's
//! `uploaded` flip. The **read** side ([`Query::feed_page`] / [`Query::accessible_album_ids`])
//! backs the gRPC `SyncService`.
//!
//! The feed row shapes (`FeedBlobManifest`, `FeedBlobRef`) are defined here once so the
//! writer (upload) and the reader (sync) reason over a single representation.

mod mutation;
mod query;

pub use mutation::Mutation;
pub use query::Query;
use serde::{Deserialize, Serialize};

/// What changed, mirroring `capsule.sync.v1.ChangeKind` as a stable on-disk small-int.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A new asset/blob became visible in the album.
    Created = 1,
    /// An existing asset's metadata (or `original_held`) advanced.
    MetadataUpdated = 2,
    /// The asset was tombstoned.
    Deleted = 3,
}

impl ChangeKind {
    /// The stored discriminant.
    #[must_use]
    pub fn as_i16(self) -> i16 {
        self as i16
    }

    /// Recover the kind from its stored discriminant, if valid.
    #[must_use]
    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            1 => Some(Self::Created),
            2 => Some(Self::MetadataUpdated),
            3 => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// A single blob's content address and role, carried on a feed entry (never blob bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedBlobRef {
    /// Ciphertext content address (lowercase hex).
    pub ciphertext_hash: String,
    /// `original | metadata | derivative | provenance | backup` (closed enum).
    pub role: String,
    /// MIME/format string, for derivatives.
    pub format: String,
    /// Ciphertext size in bytes.
    pub size: u64,
}

/// The asset's blobs by role — the JSON payload of `sync_entries.blobs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedBlobManifest {
    /// The original ciphertext blob, if this entry carries one.
    pub original: Option<FeedBlobRef>,
    /// Derivative / metadata / provenance blobs.
    #[serde(default)]
    pub derivatives: Vec<FeedBlobRef>,
}

/// The serve-time reverse lookup result for slice `S-C10`: the newest committed feed
/// reference that names a queried ciphertext content address.
///
/// A key-free media serve resolves a bare content hash to *which* asset/album committed it,
/// its role, and the derived `original_held` completeness fact — the facts the status
/// taxonomy (`awaiting-original` vs gone) is decided from. The absence of any such reference
/// is the "unknown hash" case the serve endpoint answers `404` for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobServeReference {
    /// The album the referencing entry belongs to.
    pub album_id: String,
    /// The asset the blob is a part of.
    pub asset_id: String,
    /// The blob's role on the asset (`original | metadata | derivative | provenance | backup`).
    pub role: String,
    /// Whether the asset's **original** blob is finalized on the server. A missing original
    /// on an `original_held = false` asset is expected `awaiting-original` staged state, not a
    /// dangling reference.
    pub original_held: bool,
}

/// The prepared payload for one feed entry, handed to [`Mutation::record_finalization`].
#[derive(Debug, Clone)]
pub struct FeedEntryInput {
    /// The album whose per-album `sync_seq` is minted.
    pub album_id: String,
    /// The album protocol pin (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// What changed.
    pub kind: ChangeKind,
    /// The asset id.
    pub asset_id: String,
    /// The signed manifest as opaque canonical CBOR.
    pub manifest_cbor: Vec<u8>,
    /// The encrypted metadata blob, when this row's blob is the metadata blob.
    pub metadata_blob: Option<Vec<u8>>,
    /// Per-role blob content addresses.
    pub blobs: FeedBlobManifest,
    /// The derived `original_held` completeness fact (S-C1 definition).
    pub original_held: bool,
}
