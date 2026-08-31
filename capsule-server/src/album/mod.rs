//! Album provisioning (`S-C25`), and the write authority it grounds (`S-C20`).
//!
//! # The gap this closes
//!
//! Invariant 6 asks two things of every write: *the album exists*, and *the caller has write
//! capability on it*. Until this module there was no way for either to become true. A container
//! album's id is [derived from the account master
//! key](capsule-docs/src/content/docs/design/organization.md), so the client already knows it —
//! but the server had never heard of it, and nothing anywhere created an album. A real
//! `capsule push` therefore had nowhere to land.
//!
//! # Idempotent by contract, not by luck
//!
//! The same id arrives from every device the user owns, and again after a passphrase recovery.
//! So re-provisioning is a success that writes nothing, not a conflict. `201` on the first,
//! `200` afterwards — a client never has to branch on which.
//!
//! # The pin is the server's, and it is fixed at provisioning
//!
//! [`AlbumRecord::protocol_version`] is set from the protocol the *server* speaks when the album
//! is created, never from anything the request carries. That is `S-C19` in one line: invariant 6
//! compares a write against the album's own pin, and an album whose pin came from a request
//! would be comparing a request against itself. Moving a pin forward is an album upgrade
//! (`S-C24`), which is a ceremony rather than a field.
//!
//! # No name, and the refusal is deliberate rather than an omission
//!
//! Provisioning accepts an id and nothing else. `albums.name` and `albums.description` are
//! plaintext columns from before the key-free model and the server is not entitled to album
//! titles; a body carrying one is refused rather than silently ignored, so a client is told the
//! server will not hold it. `S-C26` retires the columns themselves.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use uuid::Uuid;

use crate::store::{AlbumId, OwnerId, StoreFuture};

/// One provisioned album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRecord {
    /// The client-derived identifier.
    pub album_id: AlbumId,
    /// The account it is bound to.
    pub owner_id: OwnerId,
    /// The protocol date the album is pinned to — the server's own at provisioning.
    pub protocol_version: String,
    /// When it was provisioned.
    pub created_at: Timestamp,
}

/// What a provisioning attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// The album was created and bound to the caller.
    Created(AlbumRecord),
    /// The album is already the caller's. Nothing was written.
    AlreadyProvisioned(AlbumRecord),
    /// The id cannot be bound to this account.
    ///
    /// Carries nothing. A derived album id is unguessable before creation, and an answer that
    /// distinguished "somebody else holds this" from any other refusal would make the endpoint
    /// an existence oracle over other accounts' derived ids.
    NotAvailable,
}

/// Where provisioned albums live.
pub trait AlbumStore: std::fmt::Debug + Send + Sync {
    /// Bind `record`'s album to its owner, if it is free or already theirs.
    ///
    /// The check and the write are one operation for the reason
    /// [`crate::directory::DeviceDirectoryStore::publish`]'s comparison is: a caller that reads,
    /// compares and then writes has a window in which a concurrent provision lands, and two
    /// accounts racing through it could both believe they hold the album.
    fn provision(&self, record: AlbumRecord) -> StoreFuture<'_, ProvisionOutcome>;

    /// The album, if it has been provisioned.
    fn read<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<AlbumRecord>>;
}

/// A deterministic in-memory adapter.
#[derive(Debug, Default)]
pub struct InMemoryAlbums {
    albums: Mutex<BTreeMap<AlbumId, AlbumRecord>>,
}

impl InMemoryAlbums {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl AlbumStore for InMemoryAlbums {
    fn provision(&self, record: AlbumRecord) -> StoreFuture<'_, ProvisionOutcome> {
        Box::pin(async move {
            let mut albums = lock(&self.albums);
            if let Some(existing) = albums.get(&record.album_id) {
                return Ok(if existing.owner_id == record.owner_id {
                    ProvisionOutcome::AlreadyProvisioned(existing.clone())
                } else {
                    tracing::info!(
                        album = %record.album_id,
                        "an album provision was refused: the id is bound elsewhere"
                    );
                    ProvisionOutcome::NotAvailable
                });
            }
            tracing::info!(
                album = %record.album_id,
                owner = %record.owner_id,
                pin = %record.protocol_version,
                "an album was provisioned"
            );
            albums.insert(record.album_id.clone(), record.clone());
            Ok(ProvisionOutcome::Created(record))
        })
    }

    fn read<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<AlbumRecord>> {
        Box::pin(async move { Ok(lock(&self.albums).get(album).cloned()) })
    }
}

/// Whether `id` is the canonical spelling of a UUID.
///
/// Canonical, not merely parseable: `Uuid::parse_str` accepts braced and urn forms and mixed
/// case, and an album whose id round-trips to a different string is an album two devices would
/// disagree about. The derived id is the *same value* on every device, so its spelling has to
/// be too.
pub fn is_canonical_album_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id)
}

/// The album-provisioning module's collaborators.
#[derive(Debug, Clone)]
pub struct AlbumContext {
    albums: Arc<dyn AlbumStore>,
    clock: Arc<dyn crate::store::Clock>,
}

impl AlbumContext {
    /// Assembles the module from its collaborators.
    pub fn new(albums: Arc<dyn AlbumStore>, clock: Arc<dyn crate::store::Clock>) -> Self {
        Self { albums, clock }
    }

    /// Where provisioned albums live.
    pub fn albums(&self) -> &dyn AlbumStore {
        self.albums.as_ref()
    }

    /// The clock a provisioning is stamped from.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }
}

pub mod authority;

#[cfg(test)]
mod tests;
