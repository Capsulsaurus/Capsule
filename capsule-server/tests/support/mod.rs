//! The doubles the auth suite drives the server with, and the fixture that assembles them.
//!
//! # Why the doubles live here and not in `src/`
//!
//! `S-C29` put its in-memory [`AuthStateStore`] adapter in `src/`, because a shared conformance
//! suite has to be runnable against an adapter written in another crate. Nothing here has that
//! constraint, and one of these types is a **credential directory that accepts whatever password
//! it was told to accept**. That belongs in a test binary and nowhere a server could link it.
//!
//! The [`SessionTokens`] signer is *not* doubled: it is the real one over a generated key, so
//! every 401 in the suite is produced by a token that genuinely does not verify.
//!
//! # Why both collaborators can be broken on demand rather than replaced
//!
//! `assert_declared_responses_covered` walks the whole document against **one** client's
//! recording, so every response the description promises — the two 500s included — has to be
//! produced by one server. A second fixture with a permanently broken store could not
//! contribute to that walk. So the store and the directory each carry a switch, and the
//! coverage test breaks them for one request and repairs them.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of the fixture"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use capsule_server::App;
use capsule_server::auth::{
    AccountDirectory, AuthContext, Authentication, DirectoryError, DirectoryFuture, SessionTokens,
};
use capsule_server::blob::{
    BlobFuture, BlobPage, BlobStat, BlobStore, ContentAddress, InMemoryBlobStore, Placement,
    QuarantineReason, QuarantinedBlob,
};
use capsule_server::index::memory::InMemoryAssetIndex;
use capsule_server::index::{
    AssetIndex, AssetRow, BlobOutcome, BlobRecord, FeedEntry, IndexFuture, PendingAsset,
    Reservation,
};
use capsule_server::serve::ServeContext;
use capsule_server::store::memory::{InMemoryAuthState, InMemoryUploadSessions, ManualClock};
use capsule_server::store::{
    AcceptedChunk, AlbumId, AssetId, AuthStateStore, Clock, FinalizeClaim, OwnerId, SessionId,
    SessionRecord, StoreError, StoreFuture, UploadId, UploadSessionRecord, UploadSessionStatus,
    UploadSessionStore, UserId,
};
use capsule_server::sync::{CURSOR_KEY_LEN, CursorCodec, SyncContext};
use capsule_server::upload::authority::{
    AlbumWriteAccess, AuthorityError, AuthorityFuture, WriteAuthority,
};
use capsule_server::upload::{UploadContext, UploadPolicy};
use capsule_server::verify::VerifyContext;
use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{DecodingKey, EncodingKey};
use kynos::test::{TestClient, TestRequest};
use uuid::Uuid;

/// The session lifetime the fixture's store is built with.
///
/// Seven days, matching the refresh-token lifetime the Salvo deployment configured, so the suite
/// exercises a realistic "the refresh token dies with its record" window rather than a shorter
/// one that would hide the arrangement.
pub(crate) const SESSION_TTL: SignedDuration = SignedDuration::from_hours(24 * 7);

/// The account [`Fixture::working`] seeds.
pub(crate) const EMAIL: &str = "somebody@example.test";

/// The password that account authenticates with.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

/// What a broken collaborator says. Asserted against, so a 500 body that leaked it would fail.
const REFUSAL: &str = "the double refuses on purpose";

/// The sync-cursor MAC key the fixture builds every server with.
///
/// A constant, so a test can build a second codec over the same key and mint a cursor the
/// server accepts — and a *different* key is how the "cursor from another server" case is
/// written without reaching inside the codec.
pub(crate) const CURSOR_KEY: [u8; CURSOR_KEY_LEN] = [0x5C; CURSOR_KEY_LEN];

// ===========================================================================================
// Account directory double
// ===========================================================================================

/// An account directory holding whatever the test told it.
///
/// Passwords are compared verbatim: hashing them would be a test of `argon2`, which is the real
/// adapter's business rather than this port's contract. What *is* the contract — three outcomes,
/// and a credential that never rises above the port — is exercised exactly as it would be
/// against Postgres.
#[derive(Debug, Default)]
pub(crate) struct InMemoryAccounts {
    accounts: Mutex<BTreeMap<String, Account>>,
    unavailable: AtomicBool,
}

#[derive(Debug, Clone)]
struct Account {
    user_id: UserId,
    password: String,
    locked: bool,
}

impl InMemoryAccounts {
    /// An empty directory.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record an account that will authenticate with `password`.
    pub(crate) fn insert(&self, email: &str, password: &str, user_id: &UserId) {
        self.accounts().insert(
            email.to_owned(),
            Account {
                user_id: user_id.clone(),
                password: password.to_owned(),
                locked: false,
            },
        );
    }

    /// Put an existing account into the locked-out state.
    pub(crate) fn lock(&self, email: &str) {
        if let Some(account) = self.accounts().get_mut(email) {
            account.locked = true;
        }
    }

    /// Make every subsequent lookup fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn accounts(&self) -> MutexGuard<'_, BTreeMap<String, Account>> {
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl AccountDirectory for InMemoryAccounts {
    fn authenticate<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }

            let accounts = self.accounts();
            let Some(account) = accounts.get(email) else {
                return Ok(Authentication::Refused);
            };
            if account.locked {
                return Ok(Authentication::Locked);
            }
            if account.password == password {
                Ok(Authentication::Granted(account.user_id.clone()))
            } else {
                Ok(Authentication::Refused)
            }
        })
    }
}

// ===========================================================================================
// Session store double
// ===========================================================================================

/// `S-C29`'s in-memory session store, with a switch that makes it unreachable.
///
/// Delegation rather than reimplementation: when the switch is off this *is* the adapter the
/// shared conformance suite passes, so a test asserting session state is asserting against the
/// real thing. `ttl()` answers either way — a store that could not report its own configured
/// lifetime would be a different failure from a store that cannot be reached.
#[derive(Debug)]
pub(crate) struct SwitchableSessions {
    inner: InMemoryAuthState,
    unavailable: AtomicBool,
}

impl SwitchableSessions {
    /// A working store on `clock`, with [`SESSION_TTL`].
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryAuthState::new(clock, SESSION_TTL),
            unavailable: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "auth-state",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl AuthStateStore for SwitchableSessions {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.open_session(record)
    }

    fn read_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read_session(session)
    }

    fn touch_session<'a>(
        &'a self,
        session: &'a SessionId,
        last_active_at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.touch_session(session, last_active_at)
    }

    fn close_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.close_session(session)
    }

    fn sessions_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.sessions_for_user(user)
    }

    fn close_all_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.close_all_for_user(user)
    }
}

// ===========================================================================================
// Upload-session store double
// ===========================================================================================

/// `S-C29`'s in-memory upload store, with the same switch [`SwitchableSessions`] carries.
///
/// Delegation, not reimplementation: with the switch off this *is* the adapter the shared
/// conformance suite passes, so an assertion about a session's status or its accepted chunks is
/// an assertion about the real thing.
#[derive(Debug)]
pub(crate) struct SwitchableUploads {
    inner: InMemoryUploadSessions,
    unavailable: AtomicBool,
    claim_after_progress: AtomicBool,
}

impl SwitchableUploads {
    /// A working store on `clock`, with the port's own 24-hour lifetime cap.
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryUploadSessions::with_default_ttl(clock),
            unavailable: AtomicBool::new(false),
            claim_after_progress: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "upload-session",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }

    /// The record `id` names, for an assertion about what the server wrote.
    ///
    /// A test asserts against the store the server just wrote to, never against a second
    /// reading of the response body — a response can say anything.
    pub(crate) async fn read_for_test(&self, id: &str) -> Option<UploadSessionRecord> {
        self.inner
            .read(&UploadId::new(id))
            .await
            .expect("the in-memory store answers")
    }

    /// Let a concurrent finalizer win the claim in the window the racing request cannot see:
    /// between its chunk being recorded and its own `claim_finalize`.
    ///
    /// The window is real and is exactly one `await` wide, so it cannot be driven from outside
    /// the server. Arming it here is the only way to make the losing side of the race a case
    /// rather than a comment.
    pub(crate) fn claim_after_next_progress(&self) {
        self.claim_after_progress.store(true, Ordering::SeqCst);
    }

    /// Claim `id`'s finalization out from under the server, as a concurrent finalizer would.
    pub(crate) async fn claim_for_test(&self, id: &str) {
        let claim = self
            .inner
            .claim_finalize(&UploadId::new(id))
            .await
            .expect("the in-memory store answers");
        assert!(
            matches!(claim, FinalizeClaim::Won(_)),
            "the test must be the one that wins the claim it is simulating"
        );
    }
}

impl UploadSessionStore for SwitchableUploads {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn open(&self, record: UploadSessionRecord) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.open(record)
    }

    fn read<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read(upload)
    }

    fn sessions_for_uploader<'a>(
        &'a self,
        uploader: &'a UserId,
    ) -> StoreFuture<'a, Vec<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.sessions_for_uploader(uploader)
    }

    fn record_progress<'a>(
        &'a self,
        upload: &'a UploadId,
        chunk: AcceptedChunk,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        Box::pin(async move {
            let updated = self.inner.record_progress(upload, chunk).await?;
            if self.claim_after_progress.swap(false, Ordering::SeqCst) {
                self.inner.claim_finalize(upload).await?;
            }
            Ok(updated)
        })
    }

    fn chunk_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
    ) -> StoreFuture<'a, Option<AcceptedChunk>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.chunk_at(upload, offset)
    }

    fn reconcile_received_bytes<'a>(
        &'a self,
        upload: &'a UploadId,
        on_disk: u64,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.reconcile_received_bytes(upload, on_disk)
    }

    fn set_status<'a>(
        &'a self,
        upload: &'a UploadId,
        status: UploadSessionStatus,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.set_status(upload, status)
    }

    fn claim_finalize<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, FinalizeClaim> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.claim_finalize(upload)
    }

    fn discard<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.discard(upload)
    }

    fn least_recently_progressed(
        &self,
        not_progressed_since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<UploadId>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner
            .least_recently_progressed(not_progressed_since, limit)
    }
}

// ===========================================================================================
// Blob store double
// ===========================================================================================

/// `S-C35`'s in-memory blob store, with one seam: it can **swallow** an append.
///
/// The seam simulates the one failure the upload protocol's validation section calls out and
/// that no in-process test could otherwise reach — a crash between a durable append and the
/// counter that records it. With `swallow_next_append` armed, the store answers as though it
/// wrote the bytes and writes nothing, so the session's counter runs ahead of the stage. The
/// next append then meets the port's own offset cross-check and the divergence surfaces as
/// `500 error.upload.storage_inconsistent` — which is exactly what it is: the server's
/// inconsistency, never the client's fault.
#[derive(Debug)]
pub(crate) struct SwallowingBlobs {
    inner: InMemoryBlobStore,
    swallow: AtomicBool,
}

impl SwallowingBlobs {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self {
            inner: InMemoryBlobStore::new(),
            swallow: AtomicBool::new(false),
        }
    }

    /// Drop the next append on the floor while telling the caller it landed.
    pub(crate) fn swallow_next_append(&self) {
        self.swallow.store(true, Ordering::SeqCst);
    }

    /// The staged length for `id`, or `None` when nothing is staged.
    pub(crate) async fn staged_len_for_test(&self, id: &str) -> Option<u64> {
        self.inner
            .staged_len(&UploadId::new(id))
            .await
            .expect("the in-memory store answers")
    }

    /// The committed blob at `hash`, whole.
    pub(crate) async fn blob_for_test(&self, hash: &str) -> Option<Vec<u8>> {
        let address = ContentAddress::parse(hash).expect("a content address");
        let stat = self
            .inner
            .stat(&address)
            .await
            .expect("the in-memory store answers")?;
        self.inner
            .read_at(&address, 0, stat.size as usize)
            .await
            .expect("the in-memory store answers")
    }

    /// How many blobs the store holds — the assertion deduplication is made with.
    pub(crate) async fn blob_count_for_test(&self) -> usize {
        self.inner
            .enumerate(None, 100)
            .await
            .expect("the in-memory store answers")
            .entries
            .len()
    }
}

impl BlobStore for SwallowingBlobs {
    fn begin<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, ()> {
        self.inner.begin(upload)
    }

    fn append<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, u64> {
        if self.swallow.swap(false, Ordering::SeqCst) {
            return Box::pin(async move { Ok(offset + bytes.len() as u64) });
        }
        self.inner.append(upload, offset, bytes)
    }

    fn staged_len<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, Option<u64>> {
        self.inner.staged_len(upload)
    }

    fn read_staged_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        self.inner.read_staged_at(upload, offset, len)
    }

    fn abandon<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, bool> {
        self.inner.abandon(upload)
    }

    fn staged(&self) -> BlobFuture<'_, Vec<UploadId>> {
        self.inner.staged()
    }

    fn commit<'a>(
        &'a self,
        upload: &'a UploadId,
        address: &'a ContentAddress,
    ) -> BlobFuture<'a, Placement> {
        self.inner.commit(upload, address)
    }

    fn put<'a>(
        &'a self,
        address: &'a ContentAddress,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, Placement> {
        self.inner.put(address, bytes)
    }

    fn stat<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, Option<BlobStat>> {
        self.inner.stat(address)
    }

    fn read_at<'a>(
        &'a self,
        address: &'a ContentAddress,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        self.inner.read_at(address, offset, len)
    }

    fn enumerate<'a>(
        &'a self,
        after: Option<&'a ContentAddress>,
        limit: usize,
    ) -> BlobFuture<'a, BlobPage> {
        self.inner.enumerate(after, limit)
    }

    fn remove<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, bool> {
        self.inner.remove(address)
    }

    fn quarantine<'a>(
        &'a self,
        address: &'a ContentAddress,
        reason: QuarantineReason,
    ) -> BlobFuture<'a, bool> {
        self.inner.quarantine(address, reason)
    }

    fn quarantined(&self) -> BlobFuture<'_, Vec<QuarantinedBlob>> {
        self.inner.quarantined()
    }
}

// ===========================================================================================
// Write-authority double
// ===========================================================================================

/// The album and device facts invariants 6 and 7 are decided against.
///
/// There is no adapter for [`WriteAuthority`] in `src/` — the real one is Postgres — so this is
/// the only implementation the suite has, and it is deliberately a *directory*, not a
/// permission bit: a test revokes a device by removing its row, exactly as the real directory
/// would, rather than by flipping a flag the port does not have.
#[derive(Debug, Default)]
pub(crate) struct TestAuthority {
    albums: Mutex<BTreeMap<(String, String), String>>,
    devices: Mutex<BTreeMap<(String, Uuid), Timestamp>>,
    unavailable: AtomicBool,
}

impl TestAuthority {
    /// An authority that knows nothing.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record `album` as writable by `owner`, pinned to `protocol_pin`.
    pub(crate) fn allow_album(&self, owner: &OwnerId, album: &AlbumId, protocol_pin: &str) {
        self.albums().insert(
            (owner.as_str().to_owned(), album.as_str().to_owned()),
            protocol_pin.to_owned(),
        );
    }

    /// Forget an album, as a closed or unshared one would be.
    pub(crate) fn close_album(&self, owner: &OwnerId, album: &AlbumId) {
        self.albums()
            .remove(&(owner.as_str().to_owned(), album.as_str().to_owned()));
    }

    /// Record `device` as entering `user`'s directory at `added_at`.
    pub(crate) fn add_device(&self, user: &UserId, device: Uuid, added_at: Timestamp) {
        self.devices()
            .insert((user.as_str().to_owned(), device), added_at);
    }

    /// Remove a device from the directory, as a revocation would.
    pub(crate) fn revoke_device(&self, user: &UserId, device: Uuid) {
        self.devices().remove(&(user.as_str().to_owned(), device));
    }

    /// Make every subsequent lookup fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn albums(&self) -> MutexGuard<'_, BTreeMap<(String, String), String>> {
        self.albums.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn devices(&self) -> MutexGuard<'_, BTreeMap<(String, Uuid), Timestamp>> {
        self.devices.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl WriteAuthority for TestAuthority {
    fn album_write_access<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
    ) -> AuthorityFuture<'a, AlbumWriteAccess> {
        Box::pin(async move {
            if self.is_down() {
                return Err(AuthorityError::unavailable(REFUSAL));
            }
            Ok(self
                .albums()
                .get(&(owner.as_str().to_owned(), album.as_str().to_owned()))
                .map_or(AlbumWriteAccess::Denied, |pin| AlbumWriteAccess::Writable {
                    protocol_pin: pin.clone(),
                }))
        })
    }

    fn device_added_at<'a>(
        &'a self,
        user: &'a UserId,
        device: Uuid,
    ) -> AuthorityFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            if self.is_down() {
                return Err(AuthorityError::unavailable(REFUSAL));
            }
            Ok(self
                .devices()
                .get(&(user.as_str().to_owned(), device))
                .copied())
        })
    }
}

// ===========================================================================================
// The fixture
// ===========================================================================================

/// The asset index, with a switch that makes it refuse.
///
/// Delegation, not reimplementation: with the switch off this *is* the adapter the shared
/// conformance suite passes, so an assertion about a published asset is an assertion about the
/// real thing.
#[derive(Debug, Default)]
pub(crate) struct SwitchableIndex {
    inner: InMemoryAssetIndex,
    unavailable: AtomicBool,
}

impl SwitchableIndex {
    /// A working index.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "asset-index",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl AssetIndex for SwitchableIndex {
    fn reserve(&self, asset: PendingAsset) -> IndexFuture<'_, Reservation> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.reserve(asset)
    }

    fn read<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read(asset)
    }

    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> IndexFuture<'a, BlobOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.record_blob(asset, blob)
    }

    fn tombstone<'a>(
        &'a self,
        asset: &'a AssetId,
        at: Timestamp,
    ) -> IndexFuture<'a, Option<AssetRow>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.tombstone(asset, at)
    }

    fn find_reference<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<capsule_server::index::BlobReference>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.find_reference(address)
    }

    fn find_by_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<AssetId>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.find_by_address(owner, album, address)
    }

    fn feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.feed_page(owner, after, limit)
    }

    fn head_seq<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.head_seq(owner)
    }
}

/// A built server, plus handles on everything behind it.
///
/// The handles matter: an assertion about a session is made against the store the server just
/// wrote to, not against a second reading of the response body.
pub(crate) struct Fixture {
    /// The in-process client. No socket, no port, no runtime flavour.
    pub(crate) client: TestClient<App>,
    /// The store the server opened its sessions in.
    pub(crate) sessions: Arc<SwitchableSessions>,
    /// The directory the server authenticated against.
    pub(crate) accounts: Arc<InMemoryAccounts>,
    /// The signer the server minted with — the *same* one, so a test can mint a token the
    /// server will accept, or one it must not.
    pub(crate) tokens: Arc<SessionTokens>,
    /// The clock every record and every deadline is stamped from.
    pub(crate) clock: Arc<ManualClock>,
    /// The store the server opened its upload sessions in.
    pub(crate) uploads: Arc<SwitchableUploads>,
    /// The store the server staged and committed bytes through.
    pub(crate) blobs: Arc<SwallowingBlobs>,
    /// The album and device facts invariants 6 and 7 were decided against.
    pub(crate) authority: Arc<TestAuthority>,
    /// The durable asset index the feed reads from.
    pub(crate) index: Arc<SwitchableIndex>,
    /// The cursor codec the server mints with — the *same* one, so a test can mint a cursor
    /// the server will accept, or one it must not.
    pub(crate) cursors: Arc<CursorCodec>,
}

impl Fixture {
    /// A server whose collaborators all work, with one account, one writable album and one
    /// directory device seeded.
    pub(crate) fn working() -> Self {
        let clock = Arc::new(ManualClock::default());
        let sessions = Arc::new(SwitchableSessions::new(clock.clone()));
        let accounts = Arc::new(InMemoryAccounts::new());
        accounts.insert(EMAIL, PASSWORD, &user());
        let tokens = Arc::new(signer(clock.clone()));

        let uploads = Arc::new(SwitchableUploads::new(clock.clone()));
        let blobs = Arc::new(SwallowingBlobs::new());
        let authority = Arc::new(TestAuthority::new());
        // The album the suite uploads into, pinned to the protocol version core speaks — the
        // pin the request is compared against, never the request's own value.
        authority.allow_album(&owner(), &album(), PROTOCOL_VERSION);
        // The device the suite's manifests are written by, admitted at the clock's own zero so
        // every manifest timestamp the suite writes is at or after it.
        authority.add_device(&user(), device(), clock.now());

        let index = Arc::new(SwitchableIndex::new());
        let cursors = Arc::new(CursorCodec::new(&CURSOR_KEY));

        // One index behind both modules, which is what makes "upload it, then read it back off
        // the feed" a test of the server rather than of two disconnected doubles.
        let app = App::new(
            AuthContext::new(
                sessions.clone(),
                accounts.clone(),
                tokens.clone(),
                clock.clone(),
            ),
            UploadContext::new(
                uploads.clone(),
                blobs.clone(),
                index.clone(),
                authority.clone(),
                clock.clone(),
                UploadPolicy::default(),
            ),
            SyncContext::new(index.clone(), blobs.clone(), cursors.clone()),
            ServeContext::new(index.clone(), blobs.clone()),
            VerifyContext::new(index.clone(), blobs.clone(), clock.clone()),
        );

        Self {
            client: TestClient::new(capsule_server::service(app).expect("the router builds")),
            sessions,
            accounts,
            tokens,
            clock,
            uploads,
            blobs,
            authority,
            index,
            cursors,
        }
    }

    /// Just the application context, for a test that needs no handles on what is behind it.
    pub(crate) fn working_app() -> App {
        Self::working_context().0
    }

    /// The application context, plus the clock it was built on.
    fn working_context() -> (App, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::default());
        let accounts = Arc::new(InMemoryAccounts::new());
        accounts.insert(EMAIL, PASSWORD, &user());
        let authority = Arc::new(TestAuthority::new());
        authority.allow_album(&owner(), &album(), PROTOCOL_VERSION);
        authority.add_device(&user(), device(), clock.now());

        let blobs = Arc::new(SwallowingBlobs::new());
        let index = Arc::new(SwitchableIndex::new());
        let app = App::new(
            AuthContext::new(
                Arc::new(SwitchableSessions::new(clock.clone())),
                accounts,
                Arc::new(signer(clock.clone())),
                clock.clone(),
            ),
            UploadContext::new(
                Arc::new(SwitchableUploads::new(clock.clone())),
                blobs.clone(),
                index.clone(),
                authority,
                clock.clone(),
                UploadPolicy::default(),
            ),
            SyncContext::new(
                index.clone(),
                blobs.clone(),
                Arc::new(CursorCodec::new(&CURSOR_KEY)),
            ),
            ServeContext::new(index.clone(), blobs.clone()),
            VerifyContext::new(index, blobs, clock.clone()),
        );
        (app, clock)
    }

    /// Sign in with the seeded account and return the pair.
    pub(crate) async fn login(&self) -> capsule_server::routes::auth::TokenResponse {
        self.client
            .post("/v1/auth/login")
            .header("accept", "application/json")
            .json(&serde_json::json!({ "email": EMAIL, "password": PASSWORD }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json()
    }

    /// An access token for the seeded account, as the `Authorization` header carries it.
    pub(crate) async fn bearer(&self) -> String {
        format!("Bearer {}", self.login().await.access_token)
    }

    /// An access token for an account that is not the seeded one.
    ///
    /// Minted with the server's own signer rather than by signing in, because the account it
    /// names has no directory row — which is exactly the point: the credential is valid, and
    /// the session it asks about is somebody else's.
    pub(crate) fn other_bearer(&self, user_id: &str) -> String {
        let issued = self
            .tokens
            .issue(
                &UserId::new(user_id),
                &SessionId::new("01937b7c-0000-7000-8000-00000000000f"),
                SESSION_TTL,
            )
            .expect("the signer mints");
        format!("Bearer {}", issued.access_token)
    }

    /// Open a session for `bytes` and return its identifier.
    ///
    /// Panics rather than returning a `Result`: a fixture that cannot open a session has
    /// nothing to test, and the failure a test wants to see is the one it is about to cause.
    pub(crate) async fn open_session(&self, bytes: &[u8], role: &str, bearer: &str) -> String {
        self.open_session_with(&create_request(&self.clock, bytes, role), bearer)
            .await
    }

    /// The same, over a body the caller has already mutated — the shape a case wants when it
    /// is varying a field the default body fixes, such as the album.
    pub(crate) async fn open_session_with(
        &self,
        request: &serde_json::Value,
        bearer: &str,
    ) -> String {
        let body: serde_json::Value = self
            .client
            .post("/v1/upload")
            .header("authorization", bearer)
            .header("x-capsule-protocol", PROTOCOL_VERSION)
            .json(request)
            .send()
            .await
            .assert_status(kynos::http::StatusCode::CREATED)
            .json();
        body["id"].as_str().expect("a session id").to_owned()
    }

    /// A well-formed `PATCH` of `payload` at `offset`.
    ///
    /// Every header the protocol requires is set, so a test that wants one wrong overrides it
    /// — the later `header` call wins.
    pub(crate) fn chunk<'a>(
        &'a self,
        id: &str,
        offset: u64,
        payload: &[u8],
        bearer: &str,
    ) -> TestRequest<'a, App> {
        self.client
            .patch(&format!("/v1/upload/{id}"))
            .header("authorization", bearer)
            .header("x-capsule-protocol", PROTOCOL_VERSION)
            .header("x-capsule-offset", &offset.to_string())
            .header("x-capsule-checksum", &checksum(payload))
            .body("application/octet-stream", payload.to_vec())
    }
}

/// The protocol version the suite's manifests and sessions are written under.
pub(crate) const PROTOCOL_VERSION: &str = capsule_core::crypto::primitives::PROTOCOL_VERSION;

/// The album [`Fixture::working`] seeds as writable.
pub(crate) fn album() -> AlbumId {
    AlbumId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60")
}

/// A second album under the same owner, for cases about album scoping.
///
/// Not seeded by [`Fixture::working`]: a case that wants it writable says so, so the album
/// gate stays the thing the case is asserting rather than fixture background.
pub(crate) fn second_album() -> AlbumId {
    AlbumId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70")
}

/// The owner the seeded album belongs to — the seeded account, filing under itself.
pub(crate) fn owner() -> OwnerId {
    OwnerId::new(user().as_str())
}

/// The device [`Fixture::working`] seeds in the directory.
pub(crate) fn device() -> Uuid {
    Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f").expect("the literal is a uuid")
}

// ===========================================================================================
// Upload helpers
// ===========================================================================================

/// A blob of exactly `len` bytes, filled with `marker`.
///
/// Opaque bytes as far as the server is concerned — it holds ciphertext it cannot read, so a
/// repeating ASCII fill is as representative as random noise and makes a failure legible.
pub(crate) fn payload(marker: u8, len: usize) -> Vec<u8> {
    vec![marker; len]
}

/// The SHA-256 of `bytes`, spelled as the wire spells it.
pub(crate) fn checksum(bytes: &[u8]) -> String {
    capsule_core::crypto::hash::hash_bytes(bytes).to_hex()
}

/// A `POST /v1/upload` body for `bytes`, internally consistent in every field the gate compares.
///
/// Tests mutate the returned value to make exactly one thing wrong, which is what keeps a
/// rejection test about the invariant it names rather than about the fixture.
pub(crate) fn create_request(clock: &ManualClock, bytes: &[u8], role: &str) -> serde_json::Value {
    let hash = checksum(bytes);
    let timestamp = clock.now().to_string();
    // The metadata role is the one whose manifest must commit to this blob's own address
    // (invariant 25); every other role's commitment is to a different object.
    let metadata_blob_hash = if role == "metadata" {
        serde_json::Value::String(hash.clone())
    } else {
        serde_json::Value::Null
    };

    serde_json::json!({
        "size": bytes.len(),
        "hash": hash,
        "content_type": if role == "original" { "image/jpeg" } else { "application/octet-stream" },
        "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
        "protocol_version": PROTOCOL_VERSION,
        "blob_role": role,
        "album_id": album().as_str(),
        "manifest_envelope": {
            "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "album_id": album().as_str(),
            "file_id": "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61",
            "amk_version": 1,
            "ciphertext_hash": hash,
            "plaintext_size": bytes.len(),
            "chunk_size": 65_536,
            "key_mode": "derived",
            "metadata_blob_hash": metadata_blob_hash,
            "created_by_user": user().as_str(),
            "created_by_device": device().to_string(),
            "client_version": "capsule-cli/0.1.0",
            "timestamp": timestamp,
            "action": "create",
            "prior_provenance_hash": serde_json::Value::Null,
            "retention_until": serde_json::Value::Null,
        },
        "owner_id": serde_json::Value::Null,
        "intent_id": serde_json::Value::Null,
    })
}

/// The account [`Fixture::working`] seeds.
pub(crate) fn user() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

/// A signer over a freshly generated Ed25519 key pair.
///
/// Generated rather than read from a checked-in PEM: a private key in the repository is a
/// private key somebody eventually reuses.
pub(crate) fn signer(clock: Arc<ManualClock>) -> SessionTokens {
    use ring::signature::KeyPair as _;

    let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("the platform can generate an Ed25519 key");
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(der.as_ref())
        .expect("a key just generated parses");

    SessionTokens::new(
        EncodingKey::from_ed_der(der.as_ref()),
        DecodingKey::from_ed_der(pair.public_key().as_ref()),
        clock,
    )
}
