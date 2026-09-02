//! Share links (`S-C4`) — the public serve path, and what a key-free server can enforce on it.
//!
//! # The one thing this path must never do
//!
//! A share link is the only surface on this server that serves bytes to a caller with **no
//! account**. Everything here is shaped by that: the opaque id is the whole credential, so
//! every refusal is indistinguishable, and the set of blobs a link can reach is enumerated by
//! the link record rather than derived from an album.
//!
//! # The privacy strip cannot happen here, and pretending otherwise would be worse
//!
//! design/share-links.md says *"the serve path **always** applies the boundary-crossing
//! strip"*. **A key-free server cannot.** The metadata a share serves is ciphertext sealed
//! under material the server does not hold — the fragment secret never leaves the client — so
//! there is nothing here to read, let alone redact. design/metadata.md is the one that is
//! consistent with the architecture: *"Stripping happens at the moment of export"* — in the
//! issuing client, which is what `S-C50` settled. No strip is implemented anywhere yet: the
//! core module that claimed to be one had no callers and is gone, and no slice owns writing
//! the real one.
//!
//! So the strip is the **issuing client's**, and what this server enforces is the property that
//! makes it stick: a link serves **only the addresses its own record enumerates**, never the
//! album's ordinary blobs. A client that seals a stripped metadata blob and points the link at
//! it gets a stripped share; a link cannot be walked sideways into the unstripped one, because
//! the serve path has no path from an opaque id to an album's contents. That is checkable, and
//! [`ShareRecord::serves`] is where it is checked. The contradiction is recorded as `S-C50`
//! rather than resolved by writing a strip that cannot run.
//!
//! # Refusals are one answer
//!
//! Not found, revoked, and expired are the same `404` with the same body. Never `410` — which
//! would confirm a link once existed — and never a distinguishing code. The catalog specifies a
//! *bodyless* 404; what is served is a **constant** problem document with no extension members,
//! which carries the same property (all three render byte-identically) through a framework
//! whose error type always has a body.
//!
//! # Rate limiting is absent, and its absence is declared honestly
//!
//! The contract's two limiters — per source IP and per `{opaque-id}` — need the counter port
//! `S-C32` owns. The `429` is therefore **not declared** rather than declared and unreachable
//! (`S-C28`). What still holds without it is the structural defense the contract calls
//! *independent of rate limiting*: 128 bits of opaque id, so enumeration is not a matter of
//! throttling.
//!
//! # The revocation cache is a no-op here, by the contract's own words
//!
//! *"A single-process deployment reads revocation state directly and the cache is a no-op."*
//! This port resolves authoritatively on every request, so the 60-second fail-closed cache has
//! nothing to be stale about. It becomes real with the first multi-replica deployment, and the
//! fail-closed rule is what it must be written to.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use capsule_core::sharing::{OPAQUE_ID_LEN, ShareScope};
use jiff::Timestamp;

use crate::blob::ContentAddress;
use crate::store::{StoreFuture, UserId};

/// The opaque id as it travels in a URL: lowercase hex over 128 bits.
pub const OPAQUE_ID_HEX_LEN: usize = OPAQUE_ID_LEN * 2;

/// One share link, as the serving path needs it.
///
/// Deliberately **not** `capsule_core::sharing::ShareLinkRecord`: that is the *issuer's* record
/// and carries `wrapped_scope` plus the scope material's shape, which is the client's business.
/// What the server needs is narrower — who owns it, what it may serve, when it stops — and
/// keeping it narrow is what stops the serve path from growing a route into an album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRecord {
    /// The URL path component: 32 lowercase hex characters over the CSPRNG's 128 bits.
    pub opaque_id: String,
    /// The account that issued it. Never disclosed on the public path.
    pub owner_id: UserId,
    /// What it points at, for the owner's own listing. Never disclosed publicly either — the
    /// contract is explicit that the URL leaks nothing about what it points to, and a serve
    /// response that named an album id would leak it after one fetch.
    pub scope: ShareScope,
    /// Every address this link may serve, and nothing else.
    ///
    /// Enumerated rather than derived. This is the enforceable half of the privacy strip: a
    /// client points the link at the blobs it prepared for a boundary crossing, and the server
    /// has no path from an opaque id to anything outside this set.
    pub serves: BTreeSet<ContentAddress>,
    /// The metadata blob a viewer starts from. Always a member of [`Self::serves`].
    pub metadata: ContentAddress,
    /// The passphrase-wrapped scope material, when the link is passphrase-protected.
    ///
    /// Opaque. The passphrase never reaches this server — unwrap is client-side — so this is
    /// bytes to hand back and nothing more.
    pub wrapped_secret: Option<Vec<u8>>,
    /// When it stops being live, if ever.
    pub expires_at: Option<Timestamp>,
    /// When it was revoked, if it was.
    pub revoked_at: Option<Timestamp>,
}

impl ShareRecord {
    /// Whether the link may serve at `now`.
    pub fn is_live_at(&self, now: Timestamp) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        self.expires_at.is_none_or(|expiry| now < expiry)
    }

    /// Whether this link may serve `address`.
    ///
    /// Membership, never derivation. A link that resolved an album and served whatever it held
    /// would serve the *unstripped* metadata the user never meant to export.
    pub fn serves(&self, address: &ContentAddress) -> bool {
        self.serves.contains(address)
    }
}

/// Whether an opaque id could be one this server issued.
///
/// Checked before any lookup, so the serve path is not an oracle over arbitrary strings — the
/// same discipline [`crate::serve`] applies to a content address.
pub fn is_opaque_id(raw: &str) -> bool {
    raw.len() == OPAQUE_ID_HEX_LEN
        && raw
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Where share links live.
pub trait ShareStore: std::fmt::Debug + Send + Sync {
    /// Record a link the owner's client issued.
    fn issue(&self, record: ShareRecord) -> StoreFuture<'_, ()>;

    /// The link `opaque_id` names, whatever its state.
    ///
    /// Returns revoked and expired links too, and deliberately: the *route* collapses all three
    /// into one answer, and a store that pre-collapsed them would leave the owner's own listing
    /// unable to show a link they revoked.
    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<ShareRecord>>;

    /// Revoke `opaque_id` if it is `owner`'s, returning whether anything changed.
    ///
    /// Scoped to the owner inside the operation rather than checked by a caller: revocation is
    /// the one write on this record, and a read-then-write would let two revocations race into
    /// one being lost — which for a revocation is the direction that keeps a link alive.
    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, bool>;
}

/// A deterministic in-memory adapter.
#[derive(Debug, Default)]
pub struct InMemoryShares {
    links: Mutex<BTreeMap<String, ShareRecord>>,
}

impl InMemoryShares {
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

impl ShareStore for InMemoryShares {
    fn issue(&self, record: ShareRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            tracing::info!(
                owner = %record.owner_id,
                serves = record.serves.len(),
                "a share link was issued"
            );
            lock(&self.links).insert(record.opaque_id.clone(), record);
            Ok(())
        })
    }

    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<ShareRecord>> {
        Box::pin(async move { Ok(lock(&self.links).get(opaque_id).cloned()) })
    }

    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut links = lock(&self.links);
            let Some(record) = links.get_mut(opaque_id) else {
                return Ok(false);
            };
            if &record.owner_id != owner || record.revoked_at.is_some() {
                return Ok(false);
            }
            record.revoked_at = Some(at);
            tracing::info!(%owner, "a share link was revoked");
            Ok(true)
        })
    }
}

/// The share module's collaborators.
#[derive(Debug, Clone)]
pub struct ShareContext {
    shares: Arc<dyn ShareStore>,
    blobs: Arc<dyn crate::blob::BlobStore>,
    clock: Arc<dyn crate::store::Clock>,
}

impl ShareContext {
    /// Assembles the module.
    pub fn new(
        shares: Arc<dyn ShareStore>,
        blobs: Arc<dyn crate::blob::BlobStore>,
        clock: Arc<dyn crate::store::Clock>,
    ) -> Self {
        Self {
            shares,
            blobs,
            clock,
        }
    }

    /// Where links live.
    pub fn shares(&self) -> &dyn ShareStore {
        self.shares.as_ref()
    }

    /// The blob store a share serves ciphertext from.
    pub fn blobs(&self) -> &dyn crate::blob::BlobStore {
        self.blobs.as_ref()
    }

    /// A handle on the store, for a source that outlives the request borrow.
    pub fn blob_handle(&self) -> Arc<dyn crate::blob::BlobStore> {
        Arc::clone(&self.blobs)
    }

    /// The clock liveness is decided against.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }
}

#[cfg(test)]
mod tests;
