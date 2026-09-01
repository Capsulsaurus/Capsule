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
//! # The identity anchor (`S-C42`)
//!
//! Invariant 23 reads in full: *"A published `DeviceDirectory` has `directory_version` strictly
//! greater than the version currently stored for that user, **and the master signature covers
//! it**."* The second clause needs a key, and the document does not carry one: it holds the
//! IK's signature over its core, and its `DeviceEntry` list holds *device* keys. So the server
//! needs an anchor before it can check anything.
//!
//! **The anchor is established on first publish and is immutable thereafter.** The publishing
//! client sends its identity public key beside the document (the `X-Capsule-Identity-Key`
//! header — the body stays the exact signed bytes, because those bytes are served back
//! verbatim). The first publish records it; every later publish must present the same key, and
//! the document must verify under it.
//!
//! The alternative the slice weighed was recording an IK at **registration**, which is
//! stronger: it removes the first-publish window in which a stolen session token could pin an
//! attacker's key. It is not available — registration is `S-N1` and is not ported, so there is
//! no account record to put an IK in and no registration contract to change. Trust-on-first-use
//! needs no new account surface, is self-consistent with the monotonic guard, and can be
//! *tightened* later by having registration pre-seed the anchor, with no change to this port's
//! contract: the anchor is already "the key this account's directories verify under", and where
//! it came from is the registration path's business. Choosing the weaker anchor now therefore
//! forecloses nothing, while leaving the clause unenforced forecloses `S-C23`.
//!
//! **Why leaving it unenforced was not an option.** `S-C23` anchors revoke-all by accepting a
//! candidate IK only if it verifies the account's stored directory. An authenticated caller who
//! published a document that verifies under no key would permanently disable that account's
//! global sign-out — the recovery path a user reaches for after a device is stolen, and the
//! ceremony designed so that a stolen session token could not deny it to them.
//!
//! **The migration the slice warned about does not exist, because of when this landed.** No
//! deployment has stored a directory under this port — every adapter here is in-memory and the
//! server has no binary — so turning verification on costs nothing. Doing it after the first
//! deployment would have meant an account whose stored document does not verify, and refusing
//! to serve that document is another way to break the same recovery path.
//!
//! # Where each half of the check lives
//!
//! Verification is a pure function of bytes and a key, so it happens before the port is
//! touched. Deciding whether that key is *this account's* is state, so it happens inside the
//! same critical section as the version guard — otherwise two concurrent **first** publishes,
//! each self-consistent under its own key, could both pass a read-then-write anchor check and
//! whichever landed second would be stored under an anchor it does not verify against.

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
    /// The identity public key the document verifies under, in
    /// [`HybridVerifyingKey::to_bytes`](capsule_core::crypto::keys::HybridVerifyingKey::to_bytes)
    /// layout.
    ///
    /// Raw bytes rather than the key type: this is a port record, and the anchor's storage form
    /// is what a `bytea` column holds. Comparison is over the bytes, which is what
    /// `HybridVerifyingKey`'s own equality does anyway.
    pub identity_key: Vec<u8>,
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
    /// Refused: the submitted identity key is not this account's anchor (`S-C42`).
    ///
    /// Carries nothing about the stored anchor. The anchor is public — it is the key every one
    /// of this account's directories verifies under, and anyone who has fetched one could
    /// recover it — but echoing it *here* would turn a refusal into a lookup, and a refusal
    /// that answers a question the caller did not ask is a refusal that will be used for it.
    IdentityMismatch,
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
    /// Publish `record`, applying the identity anchor and invariant 23 atomically.
    ///
    /// An adapter **must** make both comparisons and the write one operation. A
    /// read-compare-write split by a handler is a rollback window, which is the exact thing
    /// invariant 23 exists to close — and for the anchor it is worse: two concurrent *first*
    /// publishes, each self-consistent under its own key, would both pass a read-then-write
    /// check and the account would end up anchored to a key its stored document does not verify
    /// under.
    ///
    /// The anchor is established by the first publish and immutable after it (`S-C42`). The
    /// caller has already verified the document's signature under `record.identity_key`; what
    /// this decides is whether that key is *this account's*.
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
            if let Some(stored) = published.get(&record.user_id) {
                // The anchor first, and inside the lock (`S-C42`). Checked before the version
                // so that a document signed under the wrong key is refused for *that* reason
                // rather than for being stale — an attacker who guessed a high version would
                // otherwise be told which of the two checks they failed.
                if stored.identity_key != record.identity_key {
                    tracing::warn!(
                        user = %record.user_id,
                        "a device directory publish was refused: it is signed under a key that \
                         is not this account's identity anchor"
                    );
                    return Ok(PublishOutcome::IdentityMismatch);
                }
                if record.directory_version <= stored.directory_version {
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
            } else {
                tracing::info!(
                    user = %record.user_id,
                    "a first device directory establishes this account's identity anchor"
                );
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
    /// The submitted identity key is not a readable hybrid verifying key.
    #[error("the identity key could not be read: {0}")]
    UnreadableIdentityKey(String),
    /// The document does not verify under the submitted identity key.
    ///
    /// Distinct from [`MalformedDirectory::Undecodable`] because the client's remedy differs:
    /// one is a serialization bug, the other is a document signed by the wrong key or altered
    /// after signing.
    #[error("the directory's master signature does not verify under the submitted identity key")]
    SignatureInvalid,
}

/// Read `document` as a signed directory, check the master signature, and project the one field
/// the server keeps.
///
/// The signature check is here rather than in the store because it is a pure function of bytes
/// and a key — it needs no state, cannot race, and belongs where a malformed document is
/// already being refused. Whether `identity_key` is *this account's* anchor is a separate
/// question, and a stateful one: [`DeviceDirectoryStore::publish`] answers it.
///
/// # Errors
///
/// Returns [`MalformedDirectory`] when the bytes are not a directory, are implausibly large,
/// name an account other than `user`, carry an unreadable identity key, or do not verify under
/// it.
pub fn project_version(
    document: &[u8],
    user: &UserId,
    identity_key: &[u8],
) -> Result<u64, MalformedDirectory> {
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

    // Invariant 23's second clause (`S-C42`). Refused here, before the store is touched, so a
    // document that verifies under nothing never reaches the table — which is the state that
    // would brick `S-C23`'s revoke-all for this account.
    let key = capsule_core::crypto::keys::HybridVerifyingKey::from_bytes(identity_key)
        .map_err(|error| MalformedDirectory::UnreadableIdentityKey(error.to_string()))?;
    if !directory.verify(&key) {
        return Err(MalformedDirectory::SignatureInvalid);
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
