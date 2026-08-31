//! The published device directory (`S-C9`) — the port, and its in-memory adapter.
//!
//! A user publishes a master-signed [`DeviceDirectory`] as opaque canonical CBOR; any
//! authenticated caller fetches it to pin and to verify manifests against. Nothing downstream
//! of this works without it: a sync consumer cannot check who signed a manifest, and `S-C23`'s
//! revoke-all has no identity to anchor against.
//!
//! # Verbatim bytes, one projected field
//!
//! The server stores exactly the bytes the client signed and serves exactly those bytes back.
//! It decodes the document to read **one** field — `directory_version` — and never
//! re-serializes it. This is the same discipline as the upload envelope and `S-C30`'s
//! provenance blob: a re-encoded signed structure is a structure whose signature no longer
//! verifies, and the failure looks like the *client's* bug.
//!
//! # The monotonic guard belongs to the store, not to a handler
//!
//! Invariant 23 — *a published directory's `directory_version` is **strictly greater** than the
//! version currently stored* — exists to stop a server rolling a directory back to un-revoke a
//! device. A handler that reads the stored version, compares, and then writes has a window
//! between the read and the write in which a concurrent publish can land, and two publishes
//! racing through that window can leave the *lower* version stored. So the comparison is an
//! operation on the port ([`DeviceDirectoryStore::publish`]) rather than a check a caller is
//! trusted to perform, and every adapter is obliged to make it atomic. That is the same lesson
//! `S-C37` learned about sequence numbers, applied before it could be got wrong here.
//!
//! # What is *not* checked, and why it matters more than it looks
//!
//! Invariant 23 reads in full: *"A published `DeviceDirectory` has `directory_version` strictly
//! greater than the version currently stored for that user, **and the master signature covers
//! it**."* This port enforces the first clause and **not** the second, exactly as the retired
//! implementation did not — see `S-C42`. The server has no anchor: the document carries the
//! IK's signature over its core but not the IK itself, and no account record holds one, so
//! there is nothing to verify against on a first publish.
//!
//! The consequence is worse than "an unverified document is stored". `S-C23` anchors revoke-all
//! by accepting a candidate IK **only if it verifies the account's stored directory** — so an
//! authenticated caller who publishes a directory that verifies under no key has permanently
//! disabled that account's global sign-out, which is the recovery path a user reaches for after
//! a device is stolen. Recorded rather than guessed at.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

use crate::store::{StoreFuture, UserId};

/// The largest signed directory this server will accept.
///
/// A hybrid-signed directory is a few kilobytes per device; a few hundred devices is already
/// implausible. The bound is here rather than only on the router's body limit because it is a
/// property of the document, not of the deployment.
pub const MAX_DIRECTORY_BYTES: usize = 512 * 1024;

/// A user's currently published directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedDirectory {
    /// The account it belongs to.
    pub user_id: UserId,
    /// The version projected out of the signed bytes — the only field the server reads.
    pub directory_version: u64,
    /// The client-signed canonical CBOR, exactly as it arrived.
    pub document: Vec<u8>,
    /// When the server accepted it.
    pub published_at: Timestamp,
}

/// What a publish did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    /// Stored. The version now in force.
    Published {
        /// The accepted version, which equals the submitted one.
        directory_version: u64,
    },
    /// Refused by invariant 23: the submitted version does not strictly advance.
    ///
    /// Carries the stored version, which is **not** a disclosure — a caller publishing to their
    /// own account may already fetch their own directory, and telling them what they are behind
    /// is the difference between a client that can recover and one that retries forever.
    Stale {
        /// The version currently in force.
        stored: u64,
    },
}

/// Where published directories live.
pub trait DeviceDirectoryStore: std::fmt::Debug + Send + Sync {
    /// Publish `record`, applying invariant 23 atomically.
    ///
    /// An adapter **must** make the comparison and the write one operation. A read-compare-write
    /// split by a handler is a rollback window, which is the exact thing the invariant exists to
    /// close.
    fn publish(&self, record: PublishedDirectory) -> StoreFuture<'_, PublishOutcome>;

    /// The directory currently published for `user`, or `None` if they have never published.
    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<PublishedDirectory>>;
}

/// A deterministic in-memory adapter.
///
/// One mutex over the whole map, which is what makes [`Self::publish`]'s compare-and-set
/// atomic. The Postgres adapter gets the same property from a guarded upsert whose `WHERE`
/// clause is the comparison, so a non-advancing publish updates nothing.
#[derive(Debug, Default)]
pub struct InMemoryDeviceDirectory {
    published: Mutex<BTreeMap<UserId, PublishedDirectory>>,
}

impl InMemoryDeviceDirectory {
    /// An empty directory store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Take the lock, recovering from a poisoned mutex.
///
/// A panic while holding it leaves the map intact — every mutation is a single `insert` — so
/// refusing every later publish would turn one panic into a permanently broken account.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl DeviceDirectoryStore for InMemoryDeviceDirectory {
    fn publish(&self, record: PublishedDirectory) -> StoreFuture<'_, PublishOutcome> {
        Box::pin(async move {
            let mut published = lock(&self.published);
            if let Some(stored) = published.get(&record.user_id)
                && record.directory_version <= stored.directory_version
            {
                tracing::info!(
                    user = %record.user_id,
                    stored = stored.directory_version,
                    submitted = record.directory_version,
                    "a device directory publish was refused: it does not advance"
                );
                return Ok(PublishOutcome::Stale {
                    stored: stored.directory_version,
                });
            }
            let accepted = record.directory_version;
            tracing::info!(
                user = %record.user_id,
                version = accepted,
                bytes = record.document.len(),
                "a device directory was published"
            );
            published.insert(record.user_id.clone(), record);
            Ok(PublishOutcome::Published {
                directory_version: accepted,
            })
        })
    }

    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<PublishedDirectory>> {
        Box::pin(async move { Ok(lock(&self.published).get(user).cloned()) })
    }
}

/// Why a submitted document could not be read as a directory.
#[derive(Debug, thiserror::Error)]
pub enum MalformedDirectory {
    /// The bytes are not a decodable signed directory.
    #[error("the document is not a decodable device directory: {0}")]
    Undecodable(String),
    /// The document is larger than any legitimate directory.
    #[error("the document is {size} bytes, past the {MAX_DIRECTORY_BYTES}-byte ceiling")]
    TooLarge {
        /// How large it was.
        size: usize,
    },
    /// The document belongs to somebody else.
    ///
    /// A caller may publish only their own directory. The `user_id` inside the *signed* core is
    /// what is compared, not a path parameter, so this is a statement about what was signed.
    #[error("the directory names a different account")]
    WrongAccount,
}

/// Read `document` as a signed directory and project the one field the server needs.
///
/// # Errors
///
/// Returns [`MalformedDirectory`] when the bytes are not a directory, are implausibly large, or
/// name an account other than `user`.
pub fn project_version(document: &[u8], user: &UserId) -> Result<u64, MalformedDirectory> {
    if document.len() > MAX_DIRECTORY_BYTES {
        return Err(MalformedDirectory::TooLarge {
            size: document.len(),
        });
    }
    let directory: capsule_core::crypto::keys::DeviceDirectory =
        capsule_core::cbor::from_slice(document)
            .map_err(|error| MalformedDirectory::Undecodable(error.to_string()))?;

    // The account is taken from the *signed* core rather than from the request, so a caller
    // cannot publish a document signed for one account under another's name.
    if directory.core.user_id.to_string() != user.as_str() {
        return Err(MalformedDirectory::WrongAccount);
    }
    Ok(directory.core.directory_version)
}

/// The device-directory module's collaborators, as one injectable value.
#[derive(Debug, Clone)]
pub struct DeviceDirectoryContext {
    store: Arc<dyn DeviceDirectoryStore>,
    clock: Arc<dyn crate::store::Clock>,
}

impl DeviceDirectoryContext {
    /// Assembles the module from its collaborators.
    pub fn new(store: Arc<dyn DeviceDirectoryStore>, clock: Arc<dyn crate::store::Clock>) -> Self {
        Self { store, clock }
    }

    /// Where published directories live.
    pub fn store(&self) -> &dyn DeviceDirectoryStore {
        self.store.as_ref()
    }

    /// The trusted clock a publication is stamped from.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }
}

#[cfg(test)]
mod tests;
