//! Guest drops (`S-C5`) — upload links, the inbox, and the promotion that ends one.
//!
//! # A drop is not a small upload
//!
//! An album upload carries a signed manifest, names an album, and extends a provenance chain. A
//! drop carries **none of those** (invariant 30) and cannot: the party uploading is a guest with
//! no account, no keys and no membership. So a drop is not an upload with fields omitted, it is
//! a different write with a different destination — the provisioning user's **inbox** — and
//! nothing a guest sends can put bytes into an album. Adoption is what makes an asset, and
//! adoption is the *owner's* signed `create`.
//!
//! What is shared is the *mechanics*: the chunk rules in [`crate::upload::chunk`] are pure
//! functions and a drop reuses them unchanged, exactly as invariants 9–12 say it should.
//!
//! # The link is the credential, so caps are the authorization
//!
//! `/d/{opaque-id}` takes no credential. The link's caps are the whole authorization model, and
//! they are enforced in **one store operation** ([`DropStore::charge`]) rather than by a handler
//! that reads a counter, decides, and then writes: two guests uploading at once through a
//! read-decide-write would both see room under a cap and both be admitted, which is how a
//! "maximum 10 files" link ends up holding twenty. The check and the reservation are the same
//! operation, and every adapter owes that atomically — the `S-C37` lesson in the place where
//! getting it wrong is the abuse surface the caps exist to close.
//!
//! # Adoption is claimed, not taken
//!
//! Invariant 32 asks that the inbox row be deleted and the album asset created **in one
//! transaction**. Across two ports that is not available, and the interesting question is which
//! way to fail. Writing the asset first and deleting the row after can duplicate a photo; taking
//! the row first and failing to write it loses one. Neither is acceptable, so adoption is a
//! **two-phase claim** — the shape [`FinalizeClaim`](crate::store::FinalizeClaim) already uses
//! on the upload path. [`DropStore::claim`] moves the row to `Adopting` and hands it over;
//! [`DropStore::settle`] removes it once the asset exists; [`DropStore::release`] puts it back
//! if the write is refused. A crash between leaves a row visibly `Adopting` — recoverable, and
//! neither lost nor silently duplicated.
//!
//! # Rate limiting is absent, and named
//!
//! Invariant 31 wants the same two limiters the share-link serve path wants, and they need the
//! same counter port `S-C32` owns. The `429` is **not declared** (`S-C28`). The caps are not a
//! substitute — they bound total damage, not request rate — and saying so is the point.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

use crate::blob::ContentAddress;
use crate::store::{StoreFuture, UploadId, UserId};

/// The opaque id as it travels in a URL: lowercase hex over 128 bits.
///
/// The same shape and the same reason as a share link's — a structured id would cut real
/// entropy to about 62 bits, and enumeration resistance here cannot lean on a rate limiter that
/// does not exist.
pub const OPAQUE_ID_HEX_LEN: usize = 32;

/// Per-link caps, as the server holds them.
///
/// `capsule_core::drop::LinkCaps` is the *client's* shape and spells its expiry as an RFC 3339
/// string; this one is parsed, because a cap the server re-parses on every request is a cap that
/// can start failing open on a malformed value nobody notices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkCaps {
    /// When the link stops accepting drops. `None` means only revocation ends it.
    pub expires_at: Option<Timestamp>,
    /// Cumulative bytes across every drop on this link.
    pub max_total_bytes: Option<u64>,
    /// How many files the link may deposit.
    pub max_file_count: Option<u32>,
    /// The largest single file.
    pub max_file_size: Option<u64>,
    /// Whether the link dies after its first successful drop.
    pub single_use: bool,
}

/// One provisioned upload link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadLinkRecord {
    /// The URL path component: 32 lowercase hex characters.
    pub opaque_id: String,
    /// The provisioning account — the one quota is charged to (invariant 29) and the one whose
    /// inbox a drop lands in.
    pub owner_id: UserId,
    /// The Drop Key's public half, as the guest's client encapsulates to it.
    ///
    /// Held so a guest fetching the link can seal without the fragment carrying it. Opaque
    /// here: the server never decapsulates and holds no private half.
    pub drop_pubkey: Vec<u8>,
    /// The suite a drop must be sealed under, pinning `kem_ct`'s length (invariant 30).
    pub crypto_suite_id: u16,
    /// The protocol date the link is pinned to, which fixes the `content_type` enum
    /// (invariant 27).
    pub protocol_version: String,
    /// The caps.
    pub caps: LinkCaps,
    /// The Argon2id verifier, when the link is passphrase-gated.
    ///
    /// A **verifier**, never the passphrase, and never a wrap: this is an abuse gate the server
    /// checks, distinct from a share link's passphrase, which the server never sees at all
    /// because it protects *decryption* rather than *deposit*.
    pub passphrase_verifier: Option<Vec<u8>>,
    /// Bytes already deposited.
    pub used_bytes: u64,
    /// Files already deposited.
    pub used_files: u32,
    /// When it was revoked, if it was.
    pub revoked_at: Option<Timestamp>,
}

impl UploadLinkRecord {
    /// Whether the link may accept a **new** drop at `now`, ignoring the cumulative caps.
    pub fn is_live_at(&self, now: Timestamp) -> bool {
        if self.caps.single_use && self.used_files > 0 {
            return false;
        }
        self.is_live_for_chunks_at(now)
    }

    /// Whether a session already open under this link may still be appended to.
    ///
    /// Distinct from [`Self::is_live_at`] in exactly one place: a **single-use** link is spent
    /// the moment it admits a drop, and must nonetheless let that drop finish. Revocation and
    /// expiry stop chunks too — a guest mid-upload is precisely who an owner revoking a link is
    /// revoking against.
    pub fn is_live_for_chunks_at(&self, now: Timestamp) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        self.caps.expires_at.is_none_or(|expiry| now < expiry)
    }
}

/// What a drop-session creation was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Room under every cap; `size` bytes and one file are reserved.
    Admitted {
        /// The link's owner, for quota and for the inbox.
        owner_id: UserId,
        /// The suite the drop must be sealed under.
        crypto_suite_id: u16,
        /// The protocol date the link is pinned to.
        protocol_version: String,
    },
    /// The link is unknown, expired, revoked, or spent.
    ///
    /// One variant for all four: `/d/{opaque-id}` takes no credential, so distinguishing them
    /// would be an enumeration oracle exactly as it would on the share path.
    NotLive,
    /// The file alone is larger than the link permits (invariant 28).
    FileTooLarge {
        /// The largest single file this link accepts.
        limit: u64,
    },
    /// A cumulative cap is already exhausted, or this drop would exhaust it (invariant 26).
    ///
    /// Distinct from [`Admission::NotLive`] and deliberately: the contract says a cap exhausted
    /// on an *otherwise-live* link answers `409`/`413` rather than the indistinguishable `404`,
    /// because the guest has been handed a real link by someone who wants their photos and
    /// needs to be told to ask for a new one.
    CapExhausted,
}

/// What a guest declared, held between session creation and the drop landing.
///
/// The upload-session record has nowhere to carry `kem_ct` or a filename — nor should it, since
/// those are a drop's fields and an album upload has neither. So the declaration is the drop
/// store's, keyed by the session it belongs to, and it moves into the inbox entry when the
/// bytes land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDeposit {
    /// The drop id the inbox row will carry.
    pub drop_id: String,
    /// The link it arrived through.
    pub opaque_id: String,
    /// The owner whose inbox it is bound for.
    pub owner_id: UserId,
    /// `K` encapsulated to the link's Drop Key. Opaque here.
    pub kem_ct: Vec<u8>,
    /// The declared content type.
    pub content_type: String,
    /// Guest-supplied and unverified.
    pub suggested_filename: Option<String>,
    /// The declared length, for the refund when a session is abandoned.
    pub size: u64,
}

/// A drop waiting in the owner's inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxEntry {
    /// The drop's identifier.
    pub drop_id: String,
    /// Whose inbox it is.
    pub owner_id: UserId,
    /// The link it arrived through, for the owner's own attribution.
    pub opaque_id: String,
    /// The committed ciphertext's content address.
    pub address: ContentAddress,
    /// How many bytes.
    pub size: u64,
    /// The guest's declared content type.
    pub content_type: String,
    /// `K` encapsulated to the link's Drop Key. Opaque to the server.
    pub kem_ct: Vec<u8>,
    /// Guest-supplied and unverified. Advisory only, and the one field here a guest chose the
    /// text of — so a client rendering it treats it as untrusted input.
    pub suggested_filename: Option<String>,
    /// When the drop finished landing.
    pub received_at: Timestamp,
    /// Whether an adoption currently holds this row.
    pub adopting: bool,
}

/// Where links and pending drops live.
pub trait DropStore: std::fmt::Debug + Send + Sync {
    /// Record a link the owner provisioned.
    fn provision(&self, record: UploadLinkRecord) -> StoreFuture<'_, ()>;

    /// The link `opaque_id` names, whatever its state.
    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<UploadLinkRecord>>;

    /// Revoke `opaque_id` if it is `owner`'s.
    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, bool>;

    /// Decide and reserve in one operation.
    ///
    /// **Both halves together.** A handler that read the counters, compared, and then wrote
    /// would admit two concurrent guests through the same last slot under a cap, which is
    /// precisely the abuse the caps exist to bound.
    fn charge<'a>(
        &'a self,
        opaque_id: &'a str,
        size: u64,
        at: Timestamp,
    ) -> StoreFuture<'a, Admission>;

    /// Hold a guest's declaration until the bytes land.
    fn reserve(&self, pending: PendingDeposit, upload: &UploadId) -> StoreFuture<'_, ()>;

    /// Take back the declaration for `upload`, if there is one.
    fn take_reservation<'a>(
        &'a self,
        upload: &'a UploadId,
    ) -> StoreFuture<'a, Option<PendingDeposit>>;

    /// Give back a reservation a drop never used.
    ///
    /// A session that is abandoned or refused must not spend a link's caps forever; without
    /// this, a guest who starts and cancels ten uploads exhausts a ten-file link having
    /// deposited nothing.
    fn refund<'a>(&'a self, opaque_id: &'a str, size: u64) -> StoreFuture<'a, ()>;

    /// File a finished drop in its owner's inbox.
    fn deposit(&self, entry: InboxEntry) -> StoreFuture<'_, ()>;

    /// Everything waiting for `owner`, oldest first.
    fn inbox<'a>(&'a self, owner: &'a UserId) -> StoreFuture<'a, Vec<InboxEntry>>;

    /// Hold `drop_id` for an adoption, if it is `owner`'s and not already held.
    ///
    /// Phase one of two. See this module's documentation for why adoption is claimed rather
    /// than taken.
    fn claim<'a>(
        &'a self,
        owner: &'a UserId,
        drop_id: &'a str,
    ) -> StoreFuture<'a, Option<InboxEntry>>;

    /// Remove a claimed row, the asset now existing.
    fn settle<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()>;

    /// Return a claimed row to the inbox, the write having been refused.
    fn release<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()>;

    /// Discard `drop_id` outright, if it is `owner`'s.
    fn discard<'a>(&'a self, owner: &'a UserId, drop_id: &'a str) -> StoreFuture<'a, bool>;
}

/// A deterministic in-memory adapter.
#[derive(Debug, Default)]
pub struct InMemoryDrops {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    links: BTreeMap<String, UploadLinkRecord>,
    inbox: BTreeMap<String, InboxEntry>,
    pending: BTreeMap<UploadId, PendingDeposit>,
}

impl InMemoryDrops {
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

impl DropStore for InMemoryDrops {
    fn provision(&self, record: UploadLinkRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            tracing::info!(owner = %record.owner_id, "an upload link was provisioned");
            lock(&self.inner)
                .links
                .insert(record.opaque_id.clone(), record);
            Ok(())
        })
    }

    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<UploadLinkRecord>> {
        Box::pin(async move { Ok(lock(&self.inner).links.get(opaque_id).cloned()) })
    }

    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(link) = inner.links.get_mut(opaque_id) else {
                return Ok(false);
            };
            if &link.owner_id != owner || link.revoked_at.is_some() {
                return Ok(false);
            }
            link.revoked_at = Some(at);
            tracing::info!(%owner, "an upload link was revoked");
            Ok(true)
        })
    }

    fn charge<'a>(
        &'a self,
        opaque_id: &'a str,
        size: u64,
        at: Timestamp,
    ) -> StoreFuture<'a, Admission> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(link) = inner.links.get_mut(opaque_id) else {
                return Ok(Admission::NotLive);
            };
            if !link.is_live_at(at) {
                return Ok(Admission::NotLive);
            }

            // Invariant 28, before the cumulative caps: a file the link could never accept is
            // that answer whatever room is left, and telling a guest "the link is full" about a
            // file that was simply too big would send them to the wrong remedy.
            if let Some(limit) = link.caps.max_file_size
                && size > limit
            {
                return Ok(Admission::FileTooLarge { limit });
            }

            if let Some(cap) = link.caps.max_file_count
                && link.used_files >= cap
            {
                return Ok(Admission::CapExhausted);
            }
            if let Some(cap) = link.caps.max_total_bytes
                && link.used_bytes.saturating_add(size) > cap
            {
                return Ok(Admission::CapExhausted);
            }

            link.used_bytes = link.used_bytes.saturating_add(size);
            link.used_files = link.used_files.saturating_add(1);
            tracing::info!(
                owner = %link.owner_id,
                used_bytes = link.used_bytes,
                used_files = link.used_files,
                "an upload link admitted a drop"
            );
            Ok(Admission::Admitted {
                owner_id: link.owner_id.clone(),
                crypto_suite_id: link.crypto_suite_id,
                protocol_version: link.protocol_version.clone(),
            })
        })
    }

    fn reserve(&self, pending: PendingDeposit, upload: &UploadId) -> StoreFuture<'_, ()> {
        let upload = upload.clone();
        Box::pin(async move {
            lock(&self.inner).pending.insert(upload, pending);
            Ok(())
        })
    }

    fn take_reservation<'a>(
        &'a self,
        upload: &'a UploadId,
    ) -> StoreFuture<'a, Option<PendingDeposit>> {
        Box::pin(async move { Ok(lock(&self.inner).pending.remove(upload)) })
    }

    fn refund<'a>(&'a self, opaque_id: &'a str, size: u64) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            if let Some(link) = inner.links.get_mut(opaque_id) {
                link.used_bytes = link.used_bytes.saturating_sub(size);
                link.used_files = link.used_files.saturating_sub(1);
                tracing::debug!("an unused drop reservation was refunded to its link");
            }
            Ok(())
        })
    }

    fn deposit(&self, entry: InboxEntry) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            tracing::info!(
                owner = %entry.owner_id,
                size = entry.size,
                "a drop landed in an inbox"
            );
            lock(&self.inner).inbox.insert(entry.drop_id.clone(), entry);
            Ok(())
        })
    }

    fn inbox<'a>(&'a self, owner: &'a UserId) -> StoreFuture<'a, Vec<InboxEntry>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            let mut found: Vec<InboxEntry> = inner
                .inbox
                .values()
                .filter(|entry| &entry.owner_id == owner)
                .cloned()
                .collect();
            // Oldest first, ties broken by id so the order is total: an inbox is a user-visible
            // listing and must not reshuffle between page loads.
            found.sort_by(|a, b| {
                a.received_at
                    .cmp(&b.received_at)
                    .then_with(|| a.drop_id.cmp(&b.drop_id))
            });
            Ok(found)
        })
    }

    fn claim<'a>(
        &'a self,
        owner: &'a UserId,
        drop_id: &'a str,
    ) -> StoreFuture<'a, Option<InboxEntry>> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(entry) = inner.inbox.get_mut(drop_id) else {
                return Ok(None);
            };
            // Another account's row and an already-claimed one answer identically: the first
            // must not be an oracle, and the second is a concurrent adoption whose loser has
            // nothing to do but stand down.
            if &entry.owner_id != owner || entry.adopting {
                return Ok(None);
            }
            entry.adopting = true;
            Ok(Some(entry.clone()))
        })
    }

    fn settle<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            lock(&self.inner).inbox.remove(drop_id);
            tracing::info!(%drop_id, "an adopted drop left the inbox");
            Ok(())
        })
    }

    fn release<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            if let Some(entry) = lock(&self.inner).inbox.get_mut(drop_id) {
                entry.adopting = false;
                tracing::info!(%drop_id, "a refused adoption returned its drop to the inbox");
            }
            Ok(())
        })
    }

    fn discard<'a>(&'a self, owner: &'a UserId, drop_id: &'a str) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(entry) = inner.inbox.get(drop_id) else {
                return Ok(false);
            };
            if &entry.owner_id != owner {
                return Ok(false);
            }
            inner.inbox.remove(drop_id);
            tracing::info!(%owner, "a pending drop was discarded");
            Ok(true)
        })
    }
}

/// Whether an opaque id could be one this server issued.
pub fn is_opaque_id(raw: &str) -> bool {
    raw.len() == OPAQUE_ID_HEX_LEN
        && raw
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The drop module's collaborators.
#[derive(Debug, Clone)]
pub struct DropContext {
    drops: Arc<dyn DropStore>,
    sessions: Arc<dyn crate::store::UploadSessionStore>,
    blobs: Arc<dyn crate::blob::BlobStore>,
    clock: Arc<dyn crate::store::Clock>,
}

impl DropContext {
    /// Assembles the module.
    pub fn new(
        drops: Arc<dyn DropStore>,
        sessions: Arc<dyn crate::store::UploadSessionStore>,
        blobs: Arc<dyn crate::blob::BlobStore>,
        clock: Arc<dyn crate::store::Clock>,
    ) -> Self {
        Self {
            drops,
            sessions,
            blobs,
            clock,
        }
    }

    /// Where links and pending drops live.
    pub fn drops(&self) -> &dyn DropStore {
        self.drops.as_ref()
    }

    /// The **same** upload-session store the album path uses, deliberately: a drop's chunks
    /// obey invariants 9–12 unchanged, and a second session store would be a second answer to
    /// what an offset means.
    pub fn sessions(&self) -> &dyn crate::store::UploadSessionStore {
        self.sessions.as_ref()
    }

    /// The blob store a drop stages and commits through.
    pub fn blobs(&self) -> &dyn crate::blob::BlobStore {
        self.blobs.as_ref()
    }

    /// The clock the ceremony is timed by.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }

    /// A drop id, and the session id it uploads under.
    ///
    /// UUIDv4 rather than v7: a drop id is handed to a guest, and a v7 would carry the moment
    /// the owner provisioned their link. The Identifiers rule's exact carve-out.
    pub fn new_drop_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

#[cfg(test)]
mod tests;
