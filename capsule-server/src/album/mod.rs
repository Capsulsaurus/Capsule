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
    /// The upgrade ceremony in flight against this album, if any (`S-C24`).
    ///
    /// `None` is the ordinary state. While it is `Some` and unexpired the album is **quiescing**:
    /// the members have stopped writing and are draining, and the server refuses any upload that
    /// does not name the ceremony's `intent_id`.
    pub upgrade: Option<UpgradeQuiescence>,
}

/// An album-upgrade ceremony the server has accepted (`S-C24`).
///
/// # Why the whole intent is stored rather than its fields
///
/// The deadline is a **duration**, and the expiry is `received_at + deadline` on the *server's*
/// clock — that is the entire point of the design: a skewed member clock can neither extend nor
/// shorten the window. Keeping the signed intent intact means the predicate that decides expiry is
/// [`capsule_core::crypto::upgrade::UpgradeIntent::is_expired`], the same function the client
/// reasons with, rather than a second arithmetic here that could round differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeQuiescence {
    /// The intent exactly as the proposing admin device signed it.
    ///
    /// The signature is verified **before** this is written, against the account's published
    /// device directory (`S-C42`'s anchor), so a recorded quiescence is one an admin really asked
    /// for. It is not re-verified on every read: a directory that later revokes the proposing
    /// device does not retroactively un-propose an upgrade, exactly as a revoked device's earlier
    /// manifests stay verifiable.
    pub intent: capsule_core::crypto::upgrade::UpgradeIntent,
    /// The server's own clock when it accepted the intent.
    ///
    /// The **only** anchor the deadline is measured from, and the reason this field exists at all.
    pub received_at: Timestamp,
}

impl UpgradeQuiescence {
    /// Whether the ceremony's window has closed on `now`.
    ///
    /// Delegates to core's predicate rather than restating it. An expired quiescence is treated
    /// everywhere as *absent* — versioning.md step 3: on deadline expiry the upgrade aborts
    /// cleanly and the album returns to normal operation — so nothing has to run to clear it and
    /// a ceremony whose proposer vanished cannot freeze an album forever.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.intent.is_expired(self.received_at, now)
    }

    /// When the window closes, for the phase a client polls.
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        let secs = i64::try_from(self.intent.deadline_secs).unwrap_or(i64::MAX);
        crate::store::deadline(self.received_at, jiff::SignedDuration::from_secs(secs))
    }
}

/// What accepting or clearing an upgrade did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// The album is quiescing under the intent this call named.
    ///
    /// Also the answer to a **replayed** proposal: `intent_id` is the ceremony's idempotency key,
    /// and versioning.md is explicit that the same `UpgradeIntent` never produces two forks.
    Quiescing(Box<AlbumRecord>),
    /// The album is no longer quiescing — what clearing one returns.
    Cleared(Box<AlbumRecord>),
    /// A **different** ceremony is already in flight, and only one may be.
    ///
    /// Carries the live id so the caller can tell a proposer which ceremony it is losing to;
    /// reached only by the album's owner, so it discloses nothing.
    Conflict {
        /// The ceremony already in flight.
        intent_id: Uuid,
    },
    /// No such album, or not this caller's.
    NotFound,
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

    /// Put `album` into upgrade quiescence under `quiescence`, if nothing else holds it
    /// (`S-C24`).
    ///
    /// One operation for the same reason [`Self::provision`] is one: a caller that read the
    /// album, saw no ceremony, and then wrote one has a window in which a second proposer does
    /// the same, and both would believe they hold the album. `now` is passed in rather than read
    /// here because the expiry check and the write must see the *same* instant — a ceremony that
    /// expired between them would otherwise be neither cleared nor honoured.
    ///
    /// An **expired** ceremony is not a conflict: the window closing aborts the upgrade, so a new
    /// proposal simply replaces it.
    fn begin_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        quiescence: UpgradeQuiescence,
        now: Timestamp,
    ) -> StoreFuture<'a, UpgradeOutcome>;

    /// End the ceremony `intent_id` names, returning the album to normal operation (`S-C24`).
    ///
    /// Idempotent in the direction that matters: an album that is not quiescing is already in the
    /// state this asks for, so it answers [`UpgradeOutcome::Cleared`] rather than an error. A
    /// *different* live ceremony is a conflict — aborting somebody else's upgrade is not
    /// something an id you do not hold should buy.
    fn end_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        intent_id: Uuid,
        now: Timestamp,
    ) -> StoreFuture<'a, UpgradeOutcome>;
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

    fn begin_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        quiescence: UpgradeQuiescence,
        now: Timestamp,
    ) -> StoreFuture<'a, UpgradeOutcome> {
        Box::pin(async move {
            let mut albums = lock(&self.albums);
            let Some(record) = albums.get_mut(album) else {
                return Ok(UpgradeOutcome::NotFound);
            };
            // Not this caller's album is the same answer as no album, as everywhere else: the id
            // is client-derived, so a guess must buy nothing.
            if &record.owner_id != owner {
                return Ok(UpgradeOutcome::NotFound);
            }
            if let Some(live) = &record.upgrade
                && !live.is_expired(now)
                && live.intent.intent_id != quiescence.intent.intent_id
            {
                tracing::info!(
                    %album,
                    live = %live.intent.intent_id,
                    proposed = %quiescence.intent.intent_id,
                    "an upgrade proposal was refused: another ceremony is in flight"
                );
                return Ok(UpgradeOutcome::Conflict {
                    intent_id: live.intent.intent_id,
                });
            }
            tracing::info!(
                %album,
                intent = %quiescence.intent.intent_id,
                to = %quiescence.intent.to_protocol_version,
                "an album entered upgrade quiescence"
            );
            record.upgrade = Some(quiescence);
            Ok(UpgradeOutcome::Quiescing(Box::new(record.clone())))
        })
    }

    fn end_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        intent_id: Uuid,
        now: Timestamp,
    ) -> StoreFuture<'a, UpgradeOutcome> {
        Box::pin(async move {
            let mut albums = lock(&self.albums);
            let Some(record) = albums.get_mut(album) else {
                return Ok(UpgradeOutcome::NotFound);
            };
            if &record.owner_id != owner {
                return Ok(UpgradeOutcome::NotFound);
            }
            if let Some(live) = &record.upgrade
                && !live.is_expired(now)
                && live.intent.intent_id != intent_id
            {
                return Ok(UpgradeOutcome::Conflict {
                    intent_id: live.intent.intent_id,
                });
            }
            if record.upgrade.take().is_some() {
                tracing::info!(%album, intent = %intent_id, "an album left upgrade quiescence");
            }
            Ok(UpgradeOutcome::Cleared(Box::new(record.clone())))
        })
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
