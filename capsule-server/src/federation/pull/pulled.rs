//! [`PulledAlbums`]: what a pull writes, and the state the next pull re-validates against.
//!
//! # Only what verified
//!
//! A pull writes through [`PulledAlbums::commit`] and nowhere else, and it commits a page only
//! once every entry of it has passed [`validate`](super::validate). So the port never holds an
//! entry that failed, and it holds the facts invariant 20 is re-applied against: the feed
//! position (`cursor`, the `sync_seq` high-water mark), each asset's chain head and the heads it
//! superseded (17), and the album's AMK high-water mark (18).
//!
//! # A commit is conditional
//!
//! The worker's lease keeps two replicas off one job, but a lease can lapse under a slow pull.
//! A commit therefore names the high-water mark it was validated from and is refused when the
//! album has moved since, so a page validated against stale state is never written over a
//! fresher one.
//!
//! # What lives where
//!
//! This file holds the port, its records and [`InMemoryPulledAlbums`], the deterministic double
//! the puller's tests drive. The durable adapter — rows beside the subscriptions, the metadata
//! blobs in the blob store charged to the receiving members' quota — is follow-up work, as is
//! the breadcrumb index that reads these entries.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use super::JobKey;
use crate::store::StoreFuture;

/// How many superseded heads are kept per asset for the stale-revival check.
///
/// A manifest the album has moved past is refused when it reappears ([`AssetHead::superseded`]).
/// Sixty-four is far more history than a revival attack needs to be caught on the heads a pull
/// actually saw, and bounds what a peer that edits one asset forever can make this server keep.
pub const MAX_SUPERSEDED_HEADS: usize = 64;

/// One asset's chain position, as the last accepted pull left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetHead {
    /// The record hash of the last accepted provenance record: the value its successor names as
    /// `prior_provenance_hash`.
    pub head: Hash32,
    /// Heads this asset has moved past, newest last, at most [`MAX_SUPERSEDED_HEADS`].
    pub superseded: Vec<Hash32>,
    /// Whether the last accepted record is a `delete`.
    pub deleted: bool,
}

impl AssetHead {
    /// The asset after `next` is accepted on top of this head.
    pub fn advanced(&self, next: Hash32, deleted: bool) -> Self {
        if next == self.head {
            return Self {
                deleted,
                ..self.clone()
            };
        }
        let mut superseded = self.superseded.clone();
        superseded.push(self.head);
        let excess = superseded.len().saturating_sub(MAX_SUPERSEDED_HEADS);
        superseded.drain(..excess);
        Self {
            head: next,
            superseded,
            deleted,
        }
    }
}

/// One album's pull state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PulledAlbum {
    /// The cursor the next page is asked for after. `None` pulls from the start.
    pub cursor: Option<String>,
    /// The highest `sync_seq` accepted. Every later entry must be above it.
    pub high_water: Option<u64>,
    /// The highest `amk_version` an accepted manifest carried.
    pub amk_version: Option<u32>,
    /// Every asset an accepted entry named, by its id.
    pub assets: BTreeMap<String, AssetHead>,
}

/// What an accepted entry changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The asset appeared.
    Created,
    /// The asset changed.
    Updated,
    /// The asset was deleted.
    Deleted,
}

/// One blob an accepted entry names: its role on the wire, address and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledBlob {
    /// `original`, `metadata`, `derivative` or `provenance`.
    pub role: &'static str,
    /// Its content address.
    pub hash: Hash32,
    /// Its size in bytes.
    pub size: u64,
}

/// One entry that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedEntry {
    /// The asset.
    pub asset_id: String,
    /// Its position in the album's feed.
    pub sync_seq: u64,
    /// What changed.
    pub change: Change,
    /// When the home server says it changed. Audit only.
    pub changed_at: Timestamp,
    /// The provenance record, verbatim. `None` only on a purged tombstone.
    pub manifest: Option<Vec<u8>>,
    /// The record's hash, the asset's chain head after this entry.
    pub head: Option<Hash32>,
    /// The metadata blob the manifest commits to, whose bytes the page carries.
    pub metadata_blob: Option<Hash32>,
    /// The asset's blobs, by address. Their bytes are fetched when a member views them.
    pub blobs: Vec<PulledBlob>,
    /// The signed derivative manifests, verbatim.
    pub derivative_manifests: Vec<Vec<u8>>,
    /// Whether the original has landed on the home server.
    pub original_held: bool,
    /// Whether the asset has been purged.
    pub purged: bool,
}

/// One validated page, ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPage {
    /// The entries, in feed order.
    pub entries: Vec<AcceptedEntry>,
    /// Every metadata blob the entries commit to, verified against its address.
    pub metadata_blobs: BTreeMap<Hash32, Vec<u8>>,
    /// The album's state once this page is applied, the cursor included.
    pub state: PulledAlbum,
}

/// Where pulled albums are kept.
pub trait PulledAlbums: std::fmt::Debug + Send + Sync {
    /// `job`'s state; the default (nothing pulled yet) when there is none.
    fn read<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, PulledAlbum>;

    /// Write `page` for `job`, if the album's high-water mark is still `from`.
    ///
    /// Atomic: the entries, the blobs and the new state land together or not at all. Answers
    /// whether it was written; `false` means the album moved since `from` was read.
    fn commit<'a>(
        &'a self,
        job: &'a JobKey,
        from: Option<u64>,
        page: ValidatedPage,
    ) -> StoreFuture<'a, bool>;

    /// Forget `job`'s cursor and high-water mark, keeping every asset head it holds.
    ///
    /// What a `409 error.sync.cursor_stale` asks for: the home server's index was rebuilt, so the
    /// feed restarts from zero, and what this server holds is re-validated against the heads it
    /// already accepted rather than discarded.
    fn restart<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, ()>;
}

/// The deterministic double of [`PulledAlbums`].
#[derive(Debug, Default)]
pub struct InMemoryPulledAlbums {
    albums: Mutex<BTreeMap<JobKey, Held>>,
}

#[derive(Debug, Default, Clone)]
struct Held {
    state: PulledAlbum,
    entries: Vec<AcceptedEntry>,
    metadata_blobs: BTreeMap<Hash32, Vec<u8>>,
}

impl InMemoryPulledAlbums {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every entry written for `job`, in the order written.
    pub fn entries(&self, job: &JobKey) -> Vec<AcceptedEntry> {
        self.lock()
            .get(job)
            .map(|held| held.entries.clone())
            .unwrap_or_default()
    }

    /// The metadata blob at `hash` written for `job`.
    pub fn metadata_blob(&self, job: &JobKey, hash: &Hash32) -> Option<Vec<u8>> {
        self.lock()
            .get(job)
            .and_then(|held| held.metadata_blobs.get(hash).cloned())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<JobKey, Held>> {
        self.albums.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PulledAlbums for InMemoryPulledAlbums {
    fn read<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, PulledAlbum> {
        Box::pin(async move {
            Ok(self
                .lock()
                .get(job)
                .map(|held| held.state.clone())
                .unwrap_or_default())
        })
    }

    fn commit<'a>(
        &'a self,
        job: &'a JobKey,
        from: Option<u64>,
        page: ValidatedPage,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut albums = self.lock();
            let held = albums.entry(job.clone()).or_default();
            if held.state.high_water != from {
                tracing::debug!(%job, ?from, current = ?held.state.high_water, "a pulled page was validated against a stale position");
                return Ok(false);
            }
            held.state = page.state;
            held.entries.extend(page.entries);
            held.metadata_blobs.extend(page.metadata_blobs);
            Ok(true)
        })
    }

    fn restart<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            if let Some(held) = self.lock().get_mut(job) {
                held.state.cursor = None;
                held.state.high_water = None;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
