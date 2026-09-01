//! [`ProvisionedAuthority`] — the two facts a write request cannot carry, answered from stores.
//!
//! [`WriteAuthority`](crate::upload::WriteAuthority) is the seam invariants 6 and 7 are decided
//! at. Until now the only implementation was a test double, which meant every write path was
//! proven against facts a fixture asserted rather than facts the server holds. This is the
//! production one.
//!
//! # Invariant 6, from the album store (`S-C25`)
//!
//! An album is writable by the account it was provisioned to, pinned to the protocol the server
//! spoke when it was created. Sharing widens that set — an album writable by a *member* rather
//! than only its owner — and that is `S-C4`/`S-C5`'s to add. Until then an unprovisioned or
//! somebody-else's album is [`AlbumWriteAccess::Denied`], which is the safe direction: a write
//! that should have been allowed is refused, never the reverse.
//!
//! # Invariant 7, from the published device directory (`S-C20`)
//!
//! The floor was account-creation time, with a comment saying "until the directory table lands"
//! that had outlived the table landing. It is now the device's own `added_at` from the account's
//! published [`DeviceDirectory`](capsule_core::crypto::keys::DeviceDirectory) — which is what
//! makes invariant 7 mean what it says: *this* device was in the directory before it signed
//! that manifest, not merely that the account existed.
//!
//! **A revoked device is refused outright**, whatever its `added_at`. The entry is retained
//! rather than deleted, so that manifests it signed *before* revocation stay verifiable forever
//! — but it may not sign new ones, and the directory carries `revoked_at` precisely so the
//! difference is expressible.
//!
//! **An account with no published directory has no floor, and is refused.** That is a change
//! from the retired behaviour, which fell back to account-creation time. The fallback made
//! invariant 7 vacuous for exactly the accounts most likely to be wrong about their devices,
//! and the honest answer to "was this device in the directory" for an account with no directory
//! is no. A client publishes its directory at first-device enrollment, so the state is
//! transient by design.

use std::sync::Arc;

use jiff::Timestamp;
use uuid::Uuid;

use super::AlbumStore;
use crate::directory::DeviceDirectoryStore;
use crate::store::{AlbumId, OwnerId, UserId};
use crate::upload::{AlbumWriteAccess, AuthorityError, AuthorityFuture, WriteAuthority};

/// The write authority the server runs on.
#[derive(Debug, Clone)]
pub struct ProvisionedAuthority {
    albums: Arc<dyn AlbumStore>,
    directories: Arc<dyn DeviceDirectoryStore>,
    clock: Arc<dyn crate::store::Clock>,
}

impl ProvisionedAuthority {
    /// Assembles the authority over the stores that hold its facts.
    ///
    /// The clock is here because an upgrade ceremony's window is evaluated on the **server's**
    /// clock and nowhere else (`S-C24`) — the deadline is a duration precisely so that a member's
    /// clock cannot move it.
    pub fn new(
        albums: Arc<dyn AlbumStore>,
        directories: Arc<dyn DeviceDirectoryStore>,
        clock: Arc<dyn crate::store::Clock>,
    ) -> Self {
        Self {
            albums,
            directories,
            clock,
        }
    }
}

/// Map a store failure onto the authority's own error.
///
/// The authority does not report *which* store could not answer: a caller can only act on "no
/// decision was reached", and the store's own detail is already in the log line it wrote.
fn unavailable(error: &crate::store::StoreError) -> AuthorityError {
    AuthorityError::unavailable(error.to_string())
}

impl WriteAuthority for ProvisionedAuthority {
    fn album_write_access<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
    ) -> AuthorityFuture<'a, AlbumWriteAccess> {
        Box::pin(async move {
            let record = self.albums.read(album).await.map_err(|error| {
                tracing::error!(%error, %album, "the album store could not answer");
                unavailable(&error)
            })?;
            let now = self.clock.now();
            Ok(match record {
                Some(record) if &record.owner_id == owner => AlbumWriteAccess::Writable {
                    // `S-C24`: an expired ceremony is reported as none, because the deadline
                    // passing *is* the abort. Nothing has to run to clear it, which is what stops
                    // a proposer who vanished from freezing an album forever.
                    quiescing_under: record
                        .upgrade
                        .as_ref()
                        .filter(|quiescence| !quiescence.is_expired(now))
                        .map(|quiescence| quiescence.intent.intent_id),
                    protocol_pin: record.protocol_version,
                },
                // Unprovisioned, or somebody else's. One answer: the id is client-derived and
                // unguessable, and distinguishing the two would say whether it is taken.
                _ => AlbumWriteAccess::Denied,
            })
        })
    }

    fn device_added_at<'a>(
        &'a self,
        user: &'a UserId,
        device: Uuid,
    ) -> AuthorityFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let Some(published) = self.directories.fetch(user).await.map_err(|error| {
                tracing::error!(%error, %user, "the device directory store could not answer");
                unavailable(&error)
            })?
            else {
                tracing::info!(%user, "no published device directory, so no invariant-7 floor");
                return Ok(None);
            };

            // The bytes are the account's own signed document. Failing to decode one the server
            // itself accepted is a server-side inconsistency, so it is logged loudly and
            // answered as "not in the directory" — the refusing direction.
            let Ok(directory) = capsule_core::cbor::from_slice::<
                capsule_core::crypto::keys::DeviceDirectory,
            >(&published.document) else {
                tracing::error!(%user, "a stored device directory does not decode");
                return Ok(None);
            };

            let Some(entry) = directory
                .core
                .devices
                .iter()
                .find(|entry| entry.device_id == device)
            else {
                return Ok(None);
            };
            if entry.revoked_at.is_some() {
                tracing::info!(%user, %device, "a revoked device may not sign new manifests");
                return Ok(None);
            }
            match entry.added_at.parse::<Timestamp>() {
                Ok(added_at) => Ok(Some(added_at)),
                Err(error) => {
                    // A signed field the server cannot read. Refusing is the only safe answer:
                    // an unparseable floor cannot be compared against anything.
                    tracing::error!(%user, %device, %error, "a directory entry's added_at is unreadable");
                    Ok(None)
                }
            }
        })
    }
}
