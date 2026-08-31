//! The one suite every adapter must pass.
//!
//! # Why this is the deliverable, not the tests
//!
//! It lives in `src/`, not in `tests/`, because it is part of the contract: a port is only
//! worth having if "the in-memory double behaves like Valkey" is *asserted* rather than
//! assumed, and an assertion that lives in one crate's test directory cannot be run against an
//! adapter written elsewhere. Everything here is generic over [`Harness`], so it costs nothing
//! in a binary that never instantiates it, and a container-backed smoke test is one
//! [`run_all`] call.
//!
//! It is also what lets the rest of the Kynos rebuild be tested without live infrastructure —
//! the acceptance gap design/module-map.md sets for the framework. The in-memory adapter is
//! only a legitimate stand-in for Valkey to the extent it passes this.
//!
//! # What is asserted, and what cannot be
//!
//! Two of the slice's three properties are *type-level* and have no runtime case here, by
//! design — a test for them would be a test that the code compiles:
//!
//! - **No operation takes an arbitrary serializable payload.** There is no `T: Serialize` in
//!   [`super`]; a violation is a compile error in the port, not a failure here.
//! - **TTL is not an argument.** No method accepts one. What *is* asserted is the consequence:
//!   every store expires its records at its own `ttl()`, so an adapter that quietly ignores
//!   TTL — as the Salvo in-memory double did — fails.
//!
//! The third is asserted here, repeatedly and from both directions: a session record and its
//! per-user index entry are one fact. See
//! [`closing_one_session_removes_it_from_the_user_listing`],
//! [`revoke_all_reports_only_sessions_actually_removed`] and
//! [`an_expired_session_leaves_no_listing_entry_behind`].
//!
//! # Reusing a harness
//!
//! Every case scopes its identifiers to itself, so cases may share one harness and
//! [`run_all`] does. A case may move the harness clock forward and never moves it back.

use jiff::{SignedDuration, Timestamp};
use uuid::Uuid;

use super::auth::{AuthStateStore, CohortStore, SessionRecord};
use super::ceremony::{
    AuthenticationCeremony, CeremonyState, ChallengeStore, ChannelStore, Direction, DrainOutcome,
    EnrollmentStore, PendingEnrollment, RegistrationCeremony, RelayChannel, RelayOutcome,
    RelayPayload, RevokeAllChallenge, WebauthnCeremonyStore,
};
use super::ids::{
    AssetId, CeremonyId, ChallengeToken, ChannelId, EnrollmentCode, OwnerId, SessionId, UploadId,
    UserId,
};
use super::upload::{
    AcceptedChunk, BlobRole, FinalizeClaim, UploadSessionRecord, UploadSessionStatus,
    UploadSessionStore,
};
use super::{StoreError, StoreFuture, deadline};

/// The six stores under test, plus the one thing a suite cannot do through a port: move time.
///
/// `advance` is the seam that keeps the suite backend-agnostic. The deterministic double
/// advances a manual clock; a Valkey- or Postgres-backed harness sleeps, or resets its stores
/// with a lifetime short enough to wait out. Either way the cases below are identical.
pub trait Harness: Send + Sync {
    /// The authentication-state store under test.
    fn auth(&self) -> &dyn AuthStateStore;
    /// The upload-session store under test.
    fn uploads(&self) -> &dyn UploadSessionStore;
    /// The revoke-all challenge store under test.
    fn challenges(&self) -> &dyn ChallengeStore;
    /// The device-enrollment store under test.
    fn enrollments(&self) -> &dyn EnrollmentStore;
    /// The enrollment relay-channel store under test.
    fn channels(&self) -> &dyn ChannelStore;
    /// The WebAuthn ceremony store under test.
    fn webauthn(&self) -> &dyn WebauthnCeremonyStore;
    /// The durable device-cohort map under test.
    fn cohorts(&self) -> &dyn CohortStore;

    /// Move every store in this harness `by` forward in its own time.
    fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()>;
}

/// Unwrap a store result, failing with the operation that was expected to work.
fn ok<T>(result: Result<T, StoreError>, operation: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming adapter must succeed at {operation}: {error}"),
    }
}

/// Unwrap an expected-present value.
fn present<T>(value: Option<T>, what: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{what} must be present"),
    }
}

/// A readable base instant for record timestamps, independent of the harness clock.
fn base() -> Timestamp {
    deadline(
        Timestamp::UNIX_EPOCH,
        SignedDuration::from_secs(1_700_000_000),
    )
}

/// One nanosecond — the smallest step either side of an expiry boundary.
fn tick() -> SignedDuration {
    SignedDuration::from_nanos(1)
}

/// A session for `user` opened `offset` seconds after [`base`].
fn session(case: &str, tag: &str, user: &str, offset: i64) -> SessionRecord {
    let created_at = deadline(base(), SignedDuration::from_secs(offset));
    SessionRecord {
        session_id: SessionId::new(format!("{case}-sid-{tag}")),
        user_id: UserId::new(format!("{case}-{user}")),
        created_at,
        authenticated_at: created_at,
        last_active_at: created_at,
        user_agent: Some(format!("agent-{tag}")),
        ip_address: Some("203.0.113.7".to_owned()),
        cohort_hash: Some(format!("cohort-{tag}")),
        device_id: Some(Uuid::from_u128(0x1f2e_3d4c_5b6a_4978_8899_aabb_ccdd_eeff)),
    }
}

/// An upload session for `uploader` created `offset` seconds after [`base`].
fn upload(case: &str, tag: &str, uploader: &str, offset: i64) -> UploadSessionRecord {
    let created_at = deadline(base(), SignedDuration::from_secs(offset));
    UploadSessionRecord {
        upload_id: UploadId::new(format!("{case}-up-{tag}")),
        asset_id: AssetId::new(format!("{case}-asset-{tag}")),
        owner_id: OwnerId::new(format!("{case}-owner")),
        upload_user_id: UserId::new(format!("{case}-{uploader}")),
        album_id: None,
        content_type: Some("application/octet-stream".to_owned()),
        expected_hash: "a".repeat(64),
        crypto_suite_id: 1,
        protocol_version: "2026-08-29".to_owned(),
        blob_role: BlobRole::Original,
        intent_id: None,
        manifest_envelope: format!("{{\"tag\":\"{tag}\"}}"),
        received_bytes: 0,
        total_size: 8192,
        status: UploadSessionStatus::Pending,
        created_at,
        last_progress_at: created_at,
    }
}

// ===========================================================================================
// AuthStateStore
// ===========================================================================================

/// Every field of a session survives a write and a read.
pub async fn session_round_trips_every_field(h: &dyn Harness) {
    let store = h.auth();
    let record = session("round-trip", "a", "user", 0);

    ok(store.open_session(record.clone()).await, "open_session");
    let read = present(
        ok(store.read_session(&record.session_id).await, "read_session"),
        "a session just opened",
    );

    assert_eq!(read, record, "a session must round-trip unchanged");
}

/// An id that was never opened reads as absent, not as an error.
pub async fn reading_an_unknown_session_is_none(h: &dyn Harness) {
    let missing = SessionId::new("unknown-sid-never-opened");
    assert_eq!(
        ok(h.auth().read_session(&missing).await, "read_session"),
        None,
        "an unknown session is absent, not a failure"
    );
}

/// A user's listing is ordered oldest-first and contains only that user's sessions.
pub async fn user_listing_is_ordered_and_scoped(h: &dyn Harness) {
    let store = h.auth();
    let later = session("listing", "later", "user-a", 200);
    let earlier = session("listing", "earlier", "user-a", 100);
    let other = session("listing", "other", "user-b", 150);

    // Written newest-first so a store that simply echoes insertion order fails.
    ok(store.open_session(later.clone()).await, "open_session");
    ok(store.open_session(earlier.clone()).await, "open_session");
    ok(store.open_session(other.clone()).await, "open_session");

    let listed = ok(
        store.sessions_for_user(&earlier.user_id).await,
        "sessions_for_user",
    );
    assert_eq!(
        listed,
        vec![earlier, later],
        "a user's sessions list oldest-first and never include another user's"
    );

    let listed_other = ok(
        store.sessions_for_user(&other.user_id).await,
        "sessions_for_user",
    );
    assert_eq!(
        listed_other,
        vec![other],
        "the other user sees only its own"
    );
}

/// Closing a session removes it from the user's listing too.
///
/// This is the bug the port exists to make unrepresentable: the Salvo `revoke_session` deleted
/// the record and left the id in `capsule:user_sessions:<user>`.
pub async fn closing_one_session_removes_it_from_the_user_listing(h: &dyn Harness) {
    let store = h.auth();
    let kept = session("close-one", "kept", "user", 0);
    let closed = session("close-one", "closed", "user", 10);

    ok(store.open_session(kept.clone()).await, "open_session");
    ok(store.open_session(closed.clone()).await, "open_session");

    let removed = ok(
        store.close_session(&closed.session_id).await,
        "close_session",
    );
    assert_eq!(
        removed,
        Some(closed.clone()),
        "closing returns the record it removed"
    );

    assert_eq!(
        ok(store.read_session(&closed.session_id).await, "read_session"),
        None,
        "the closed session's record is gone"
    );
    assert_eq!(
        ok(
            store.sessions_for_user(&kept.user_id).await,
            "sessions_for_user"
        ),
        vec![kept],
        "and so is its entry in the user's listing — the record and the index are one fact"
    );
}

/// A revoke-all reports exactly the sessions it removed.
///
/// The Salvo shape counted index entries, so every prior refresh added a phantom device to the
/// "signed out N devices" the user was shown.
pub async fn revoke_all_reports_only_sessions_actually_removed(h: &dyn Harness) {
    let store = h.auth();
    let first = session("revoke-all", "first", "user-a", 0);
    let second = session("revoke-all", "second", "user-a", 10);
    let refreshed_away = session("revoke-all", "stale", "user-a", 20);
    let untouched = session("revoke-all", "other-user", "user-b", 30);

    for record in [&first, &second, &refreshed_away, &untouched] {
        ok(store.open_session(record.clone()).await, "open_session");
    }
    // The refresh path: one session closed individually before the revoke-all runs.
    ok(
        store.close_session(&refreshed_away.session_id).await,
        "close_session",
    );

    let revoked = ok(
        store.close_all_for_user(&first.user_id).await,
        "close_all_for_user",
    );
    assert_eq!(
        revoked,
        vec![first.clone(), second],
        "a revoke-all returns the sessions it actually removed, not index residue"
    );

    assert!(
        ok(
            store.sessions_for_user(&first.user_id).await,
            "sessions_for_user"
        )
        .is_empty(),
        "nothing is left for the user"
    );
    assert!(
        ok(
            store.close_all_for_user(&first.user_id).await,
            "close_all_for_user"
        )
        .is_empty(),
        "a second revoke-all removes nothing and reports nothing"
    );
    assert_eq!(
        ok(
            store.sessions_for_user(&untouched.user_id).await,
            "sessions_for_user"
        ),
        vec![untouched],
        "another user's sessions are untouched"
    );
}

/// Closing an id that is not open removes nothing and reports nothing.
pub async fn closing_an_unknown_session_removes_nothing(h: &dyn Harness) {
    let store = h.auth();
    let live = session("close-unknown", "live", "user", 0);
    ok(store.open_session(live.clone()).await, "open_session");

    let missing = SessionId::new("close-unknown-sid-never-opened");
    assert_eq!(
        ok(store.close_session(&missing).await, "close_session"),
        None,
        "closing an unknown session removes nothing"
    );
    assert_eq!(
        ok(
            store.sessions_for_user(&live.user_id).await,
            "sessions_for_user"
        ),
        vec![live],
        "and disturbs nothing"
    );
}

/// Re-opening a live session id replaces the record without duplicating the listing.
pub async fn reopening_a_session_id_does_not_duplicate_the_listing(h: &dyn Harness) {
    let store = h.auth();
    let first = session("reopen", "a", "user", 0);
    let mut again = first.clone();
    again.user_agent = Some("a-different-agent".to_owned());

    ok(store.open_session(first.clone()).await, "open_session");
    ok(store.open_session(again.clone()).await, "open_session");

    assert_eq!(
        ok(
            store.sessions_for_user(&first.user_id).await,
            "sessions_for_user"
        ),
        vec![again],
        "one id is one session however many times it is written"
    );
}

/// A touch records activity and does not extend the session's life.
pub async fn touching_a_session_records_activity_without_extending_its_life(h: &dyn Harness) {
    let store = h.auth();
    let record = session("touch", "a", "user", 0);
    let ttl = store.ttl();
    ok(store.open_session(record.clone()).await, "open_session");

    ok(
        h.advance(ttl - tick()).await,
        "advance to just inside the window",
    );

    let seen_at = deadline(base(), SignedDuration::from_secs(5));
    let touched = present(
        ok(
            store.touch_session(&record.session_id, seen_at).await,
            "touch_session",
        ),
        "a live session",
    );
    assert_eq!(touched.last_active_at, seen_at, "activity is recorded");
    assert_eq!(
        touched.created_at, record.created_at,
        "a touch does not rewrite when the session opened"
    );

    ok(h.advance(tick()).await, "advance past the window");
    assert_eq!(
        ok(store.read_session(&record.session_id).await, "read_session"),
        None,
        "the lifetime is absolute: a touch must not slide the expiry"
    );
    assert_eq!(
        ok(
            store.touch_session(&record.session_id, seen_at).await,
            "touch_session"
        ),
        None,
        "and touching an expired session finds nothing"
    );
}

/// An expired session leaves neither a record nor a listing entry.
///
/// The two halves had independent TTLs in the Salvo adapter — the record's `SET EX` and the
/// index's `EXPIRE`, refreshed on different events — so this is where the split leaked even
/// when every call site was correct.
pub async fn an_expired_session_leaves_no_listing_entry_behind(h: &dyn Harness) {
    let store = h.auth();
    let record = session("expiry", "a", "user", 0);
    let ttl = store.ttl();
    ok(store.open_session(record.clone()).await, "open_session");

    ok(h.advance(ttl).await, "advance to the expiry");

    assert_eq!(
        ok(store.read_session(&record.session_id).await, "read_session"),
        None,
        "an expired session's record is gone"
    );
    assert!(
        ok(
            store.sessions_for_user(&record.user_id).await,
            "sessions_for_user"
        )
        .is_empty(),
        "an expired session leaves no listing entry"
    );
    assert!(
        ok(
            store.close_all_for_user(&record.user_id).await,
            "close_all_for_user"
        )
        .is_empty(),
        "and a revoke-all cannot count one"
    );
}

// ===========================================================================================
// UploadSessionStore
// ===========================================================================================

/// Every field of an upload session survives a write and a read.
pub async fn upload_session_round_trips_every_field(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("up-round-trip", "a", "uploader", 0);

    ok(store.open(record.clone()).await, "open");
    let read = present(
        ok(store.read(&record.upload_id).await, "read"),
        "an upload session just opened",
    );

    assert_eq!(read, record, "an upload session must round-trip unchanged");
    assert_eq!(
        ok(
            store
                .read(&UploadId::new("up-round-trip-never-opened"))
                .await,
            "read"
        ),
        None,
        "an unknown upload session is absent, not a failure"
    );
}

/// The uploader listing is ordered oldest-first and scoped to the resuming party.
pub async fn uploader_listing_is_ordered_and_scoped(h: &dyn Harness) {
    let store = h.uploads();
    let later = upload("up-listing", "later", "uploader-a", 200);
    let earlier = upload("up-listing", "earlier", "uploader-a", 100);
    let other = upload("up-listing", "other", "uploader-b", 150);

    ok(store.open(later.clone()).await, "open");
    ok(store.open(earlier.clone()).await, "open");
    ok(store.open(other.clone()).await, "open");

    assert_eq!(
        ok(
            store.sessions_for_uploader(&earlier.upload_user_id).await,
            "sessions_for_uploader"
        ),
        vec![earlier, later],
        "an uploader's resumable sessions list oldest-first"
    );
    assert_eq!(
        ok(
            store.sessions_for_uploader(&other.upload_user_id).await,
            "sessions_for_uploader"
        ),
        vec![other],
        "and never include another uploader's"
    );
}

/// An active session is the promise that bytes are coming; a terminal one is not (`S-C40`).
pub async fn a_pending_address_is_owner_scoped_and_ends_with_the_session(h: &dyn Harness) {
    let store = h.uploads();
    let mine = upload("pending-addr", "mine", "uploader-a", 0);
    let hash = mine.expected_hash.clone();
    let owner = mine.owner_id.clone();
    ok(store.open(mine.clone()).await, "open");

    assert_eq!(
        ok(
            store.pending_for_address(&owner, &hash).await,
            "pending_for_address"
        ),
        Some(mine.upload_id.clone()),
        "an active session declaring the hash is the promise that its bytes are coming"
    );

    // Another account uploading the same bytes is not this account's answer. The scope is what
    // keeps the serve path's transient status from reporting on somebody else's upload.
    let stranger = OwnerId::new("someone-else");
    assert_eq!(
        ok(
            store.pending_for_address(&stranger, &hash).await,
            "pending_for_address"
        ),
        None,
        "the promise is scoped to the account that made it"
    );

    // A hash nobody declared was never promised.
    assert_eq!(
        ok(
            store.pending_for_address(&owner, &"f".repeat(64)).await,
            "pending_for_address"
        ),
        None,
        "an undeclared hash is not pending"
    );

    // And the promise ends when the session leaves the active states — a completed session's
    // bytes are committed, a failed one's are not coming, and neither is "still uploading".
    present(
        ok(
            store
                .set_status(&mine.upload_id, UploadSessionStatus::Completed)
                .await,
            "set_status",
        ),
        "set_status",
    );
    assert_eq!(
        ok(
            store.pending_for_address(&owner, &hash).await,
            "pending_for_address"
        ),
        None,
        "a terminal session promises nothing"
    );
}

/// A discarded session takes its promise with it (`S-C40`).
pub async fn discarding_a_session_withdraws_its_pending_address(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("pending-discard", "a", "uploader", 0);
    let hash = record.expected_hash.clone();
    let owner = record.owner_id.clone();
    ok(store.open(record.clone()).await, "open");
    present(
        ok(store.discard(&record.upload_id).await, "discard"),
        "discard",
    );

    assert_eq!(
        ok(
            store.pending_for_address(&owner, &hash).await,
            "pending_for_address"
        ),
        None,
        "an abandoned upload's promise expires with it, which is why the promise does not need          a lifetime of its own"
    );
}

/// Accepting a chunk advances the byte counter, the progress clock and the replay store — as
/// one operation, because they describe one event.
pub async fn recording_progress_advances_bytes_clock_and_replay_together(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("progress", "a", "uploader", 0);
    ok(store.open(record.clone()).await, "open");

    let accepted_at = deadline(base(), SignedDuration::from_secs(60));
    let chunk = AcceptedChunk {
        offset: 0,
        chunk_hash: "b".repeat(64),
        next_offset: 4096,
        accepted_at,
    };
    let updated = present(
        ok(
            store
                .record_progress(&record.upload_id, chunk.clone())
                .await,
            "record_progress",
        ),
        "a live session",
    );

    assert_eq!(updated.received_bytes, 4096, "the byte counter advances");
    assert_eq!(
        updated.last_progress_at, accepted_at,
        "the progress clock advances"
    );
    assert_eq!(
        updated.status,
        UploadSessionStatus::Uploading,
        "the first accepted chunk moves a pending session to uploading"
    );
    assert_eq!(
        ok(store.chunk_at(&record.upload_id, 0).await, "chunk_at"),
        Some(chunk),
        "and the chunk is recorded for replay — all three, or none"
    );
    assert_eq!(
        ok(store.read(&record.upload_id).await, "read"),
        Some(updated),
        "the persisted record matches what the write returned"
    );

    let missing = UploadId::new("progress-never-opened");
    let orphan = AcceptedChunk {
        offset: 0,
        chunk_hash: "c".repeat(64),
        next_offset: 1,
        accepted_at,
    };
    assert_eq!(
        ok(
            store.record_progress(&missing, orphan).await,
            "record_progress"
        ),
        None,
        "progress against an unknown session advances nothing"
    );
}

/// Replay lookups are addressed by offset, and an offset never accepted is absent.
pub async fn chunk_replay_is_offset_addressed(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("replay", "a", "uploader", 0);
    ok(store.open(record.clone()).await, "open");

    for (offset, next) in [(0_u64, 4096_u64), (4096, 8192)] {
        let chunk = AcceptedChunk {
            offset,
            chunk_hash: format!("{offset:064x}"),
            next_offset: next,
            accepted_at: base(),
        };
        ok(
            store.record_progress(&record.upload_id, chunk).await,
            "record_progress",
        );
    }

    let second = present(
        ok(store.chunk_at(&record.upload_id, 4096).await, "chunk_at"),
        "the chunk accepted at 4096",
    );
    assert_eq!(second.next_offset, 8192, "each offset keeps its own record");
    assert_eq!(
        ok(store.chunk_at(&record.upload_id, 1234).await, "chunk_at"),
        None,
        "an offset never accepted has no replay entry"
    );
}

/// Exactly one caller wins the right to finalize.
pub async fn finalization_is_claimed_exactly_once(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("finalize", "a", "uploader", 0);
    ok(store.open(record.clone()).await, "open");

    let won = ok(
        store.claim_finalize(&record.upload_id).await,
        "claim_finalize",
    );
    match won {
        FinalizeClaim::Won(claimed) => assert_eq!(
            claimed.upload_id, record.upload_id,
            "the winner is handed the session it claimed"
        ),
        other => panic!("the first claim must win, got {other:?}"),
    }

    assert_eq!(
        ok(
            store.claim_finalize(&record.upload_id).await,
            "claim_finalize"
        ),
        FinalizeClaim::AlreadyClaimed,
        "a second claim loses — two finalizers cannot both win"
    );
    assert_eq!(
        present(
            ok(store.read(&record.upload_id).await, "read"),
            "the session"
        )
        .status,
        UploadSessionStatus::WaitingForProcessing,
        "the claim is what moved the status"
    );
    assert_eq!(
        ok(
            store
                .claim_finalize(&UploadId::new("finalize-never-opened"))
                .await,
            "claim_finalize"
        ),
        FinalizeClaim::NotFound,
        "claiming an unknown session is distinguishable from losing the race"
    );
}

/// The startup scrub sets the byte counter absolutely and does not fake progress.
pub async fn reconciling_received_bytes_does_not_move_the_progress_clock(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("reconcile", "a", "uploader", 0);
    ok(store.open(record.clone()).await, "open");

    let reconciled = present(
        ok(
            store
                .reconcile_received_bytes(&record.upload_id, 12_288)
                .await,
            "reconcile_received_bytes",
        ),
        "a live session",
    );
    assert_eq!(
        reconciled.received_bytes, 12_288,
        "the counter is set to the on-disk truth, not incremented"
    );
    assert_eq!(
        reconciled.last_progress_at, record.last_progress_at,
        "a scrub is not progress and must not refresh the survival-floor anchor"
    );
    assert_eq!(
        ok(
            store
                .reconcile_received_bytes(&UploadId::new("reconcile-never-opened"), 1)
                .await,
            "reconcile_received_bytes"
        ),
        None,
        "there is nothing to reconcile for an unknown session"
    );
}

/// A session that reached a terminal state is never offered for pressure eviction.
///
/// The eviction view is global rather than per-uploader, so this case and
/// [`eviction_candidates_are_ordered_by_progress_and_bounded`] each work in their own band of
/// progress time and clear up after themselves. That is what keeps them meaningful when the
/// suite shares one harness.
pub async fn a_terminal_session_is_not_an_eviction_candidate(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("terminal", "a", "uploader", -10_000);
    ok(store.open(record.clone()).await, "open");

    let horizon = deadline(base(), SignedDuration::from_secs(-9_000));
    assert_eq!(
        ok(
            store.least_recently_progressed(horizon, 10).await,
            "least_recently_progressed"
        ),
        vec![record.upload_id.clone()],
        "an active, stalled session is a candidate"
    );

    let completed = present(
        ok(
            store
                .set_status(&record.upload_id, UploadSessionStatus::Completed)
                .await,
            "set_status",
        ),
        "a live session",
    );
    assert_eq!(completed.status, UploadSessionStatus::Completed);
    assert!(
        ok(
            store.least_recently_progressed(horizon, 10).await,
            "least_recently_progressed"
        )
        .is_empty(),
        "a terminal session's bytes are already committed, so it is exempt from eviction"
    );
    assert!(
        ok(store.read(&record.upload_id).await, "read").is_some(),
        "and its receipt is retained until the lifetime cap"
    );

    ok(store.discard(&record.upload_id).await, "discard");
}

/// Discarding removes the record, its replay entries and its listing entry together.
pub async fn discarding_removes_the_record_its_chunks_and_its_listing(h: &dyn Harness) {
    let store = h.uploads();
    let kept = upload("discard", "kept", "uploader", 0);
    let dropped = upload("discard", "dropped", "uploader", 10);
    ok(store.open(kept.clone()).await, "open");
    ok(store.open(dropped.clone()).await, "open");
    ok(
        store
            .record_progress(
                &dropped.upload_id,
                AcceptedChunk {
                    offset: 0,
                    chunk_hash: "d".repeat(64),
                    next_offset: 4096,
                    accepted_at: base(),
                },
            )
            .await,
        "record_progress",
    );

    let removed = ok(store.discard(&dropped.upload_id).await, "discard");
    assert_eq!(
        present(removed, "the discarded session").upload_id,
        dropped.upload_id,
        "discarding returns the record it removed"
    );
    assert_eq!(
        ok(store.read(&dropped.upload_id).await, "read"),
        None,
        "the record is gone"
    );
    assert_eq!(
        ok(store.chunk_at(&dropped.upload_id, 0).await, "chunk_at"),
        None,
        "its replay entries go with it"
    );
    assert_eq!(
        ok(
            store.sessions_for_uploader(&kept.upload_user_id).await,
            "sessions_for_uploader"
        ),
        vec![kept],
        "and so does its place in the uploader's listing"
    );
    assert_eq!(
        ok(store.discard(&dropped.upload_id).await, "discard"),
        None,
        "discarding twice removes nothing the second time"
    );
}

/// Eviction candidates come back least-recently-progressed first, bounded, and filtered by the
/// caller's floor.
pub async fn eviction_candidates_are_ordered_by_progress_and_bounded(h: &dyn Harness) {
    let store = h.uploads();
    let stalest = upload("eviction", "stalest", "uploader", -3_000);
    let middle = upload("eviction", "middle", "uploader", -2_000);
    let freshest = upload("eviction", "freshest", "uploader", -1_000);

    for record in [&freshest, &stalest, &middle] {
        ok(store.open(record.clone()).await, "open");
    }

    // Below every other case's progress band, so what comes back is this case's own doing.
    let horizon = deadline(base(), SignedDuration::from_secs(-1_500));
    assert_eq!(
        ok(
            store.least_recently_progressed(horizon, 10).await,
            "least_recently_progressed"
        ),
        vec![stalest.upload_id.clone(), middle.upload_id.clone()],
        "candidates are least-recently-progressed first, and the floor excludes the rest"
    );
    assert_eq!(
        ok(
            store.least_recently_progressed(horizon, 1).await,
            "least_recently_progressed"
        ),
        vec![stalest.upload_id.clone()],
        "the limit is respected and takes the stalest"
    );

    for record in [&stalest, &middle, &freshest] {
        ok(store.discard(&record.upload_id).await, "discard");
    }
}

/// An expired upload session leaves nothing behind in either view.
pub async fn an_expired_upload_session_leaves_no_listing_entry_behind(h: &dyn Harness) {
    let store = h.uploads();
    let record = upload("up-expiry", "a", "uploader", 0);
    let ttl = store.ttl();
    ok(store.open(record.clone()).await, "open");
    ok(
        store
            .record_progress(
                &record.upload_id,
                AcceptedChunk {
                    offset: 0,
                    chunk_hash: "e".repeat(64),
                    next_offset: 4096,
                    accepted_at: base(),
                },
            )
            .await,
        "record_progress",
    );

    ok(h.advance(ttl).await, "advance to the lifetime cap");

    assert_eq!(
        ok(store.read(&record.upload_id).await, "read"),
        None,
        "the record is gone at the cap"
    );
    assert!(
        ok(
            store.sessions_for_uploader(&record.upload_user_id).await,
            "sessions_for_uploader"
        )
        .is_empty(),
        "and so is its listing entry"
    );
    assert_eq!(
        ok(store.chunk_at(&record.upload_id, 0).await, "chunk_at"),
        None,
        "and its replay entries, which have no lifetime of their own"
    );
}

// ===========================================================================================
// Ceremony stores
// ===========================================================================================

/// A revoke-all challenge is burned by the first attempt, successful or not.
pub async fn a_challenge_is_single_use(h: &dyn Harness) {
    let store = h.challenges();
    let token = ChallengeToken::new("challenge-single-use");
    let record = RevokeAllChallenge {
        user_id: UserId::new("challenge-user"),
        issued_at: base(),
    };

    ok(store.issue(&token, record.clone()).await, "issue");
    assert_eq!(
        ok(store.consume(&token).await, "consume"),
        Some(record),
        "the first attempt gets the challenge"
    );
    assert_eq!(
        ok(store.consume(&token).await, "consume"),
        None,
        "a consumed challenge cannot be replayed"
    );
    assert_eq!(
        ok(
            store
                .consume(&ChallengeToken::new("challenge-never-issued"))
                .await,
            "consume"
        ),
        None,
        "an unknown challenge is indistinguishable from a spent one"
    );
}

/// A challenge dies at its store's TTL, with no caller involved.
pub async fn a_challenge_expires_with_its_store(h: &dyn Harness) {
    let store = h.challenges();
    let token = ChallengeToken::new("challenge-expiry");
    ok(
        store
            .issue(
                &token,
                RevokeAllChallenge {
                    user_id: UserId::new("challenge-expiry-user"),
                    issued_at: base(),
                },
            )
            .await,
        "issue",
    );

    ok(h.advance(store.ttl()).await, "advance to the challenge TTL");

    assert_eq!(
        ok(store.consume(&token).await, "consume"),
        None,
        "expiry is the store's property — an adapter that ignores TTL fails here"
    );
}

/// An enrollment redeems under either spelling, and redeeming burns both.
pub async fn an_enrollment_redeems_by_either_spelling_and_burns_both(h: &dyn Harness) {
    let store = h.enrollments();
    let code = EnrollmentCode::new("enroll-full-entropy-code");
    let fallback = EnrollmentCode::new("0123456789");
    let record = PendingEnrollment {
        user_id: UserId::new("enroll-user"),
        code: code.clone(),
        text_fallback: fallback.clone(),
        issued_at: base(),
    };

    ok(store.issue(record.clone()).await, "issue");
    assert!(
        ok(store.is_taken(&code).await, "is_taken"),
        "the full code is live"
    );
    assert!(
        ok(store.is_taken(&fallback).await, "is_taken"),
        "and so is the fallback"
    );

    // Redeem by the *fallback*; the full-entropy spelling must die with it.
    assert_eq!(
        ok(store.redeem(&fallback).await, "redeem"),
        Some(record),
        "either spelling redeems the same enrollment"
    );
    assert_eq!(
        ok(store.redeem(&code).await, "redeem"),
        None,
        "redeeming one spelling burns the other — they are one fact"
    );
    assert!(
        !ok(store.is_taken(&code).await, "is_taken"),
        "and neither remains taken"
    );
    assert!(!ok(store.is_taken(&fallback).await, "is_taken"));
}

/// A code that was never issued is not taken and does not redeem.
pub async fn an_unissued_enrollment_code_is_not_taken(h: &dyn Harness) {
    let store = h.enrollments();
    let code = EnrollmentCode::new("enroll-never-issued");
    assert!(
        !ok(store.is_taken(&code).await, "is_taken"),
        "an unissued code is free for the generator to use"
    );
    assert_eq!(
        ok(store.redeem(&code).await, "redeem"),
        None,
        "and redeems nothing"
    );
}

/// An enrollment dies at its store's TTL.
pub async fn an_enrollment_expires_with_its_store(h: &dyn Harness) {
    let store = h.enrollments();
    let code = EnrollmentCode::new("enroll-expiry-code");
    let fallback = EnrollmentCode::new("9876543210");
    ok(
        store
            .issue(PendingEnrollment {
                user_id: UserId::new("enroll-expiry-user"),
                code: code.clone(),
                text_fallback: fallback.clone(),
                issued_at: base(),
            })
            .await,
        "issue",
    );

    ok(h.advance(store.ttl()).await, "advance to the code TTL");

    assert_eq!(ok(store.redeem(&code).await, "redeem"), None);
    assert_eq!(ok(store.redeem(&fallback).await, "redeem"), None);
    assert!(
        !ok(store.is_taken(&code).await, "is_taken"),
        "an expired code frees its spelling"
    );
}

/// Relaying is refused unless the channel is live — before it opens, and after it dies.
pub async fn relaying_requires_a_live_channel(h: &dyn Harness) {
    let store = h.channels();
    let channel = ChannelId::new("relay-liveness");

    assert_eq!(
        ok(
            store
                .enqueue(&channel, Direction::ToEnrollee, RelayPayload::new("early"))
                .await,
            "enqueue"
        ),
        RelayOutcome::NoChannel,
        "nothing may be relayed before the channel opens"
    );
    assert_eq!(
        ok(store.drain(&channel, Direction::ToEnrollee).await, "drain"),
        DrainOutcome::NoChannel,
        "and nothing may be drained"
    );

    let record = RelayChannel {
        initiator_user_id: UserId::new("relay-initiator"),
        opened_at: base(),
    };
    ok(store.open(&channel, record.clone()).await, "open");
    assert_eq!(
        ok(store.lookup(&channel).await, "lookup"),
        Some(record),
        "an open channel is visible to the routes that authorize against it"
    );
    assert_eq!(
        ok(
            store
                .enqueue(&channel, Direction::ToEnrollee, RelayPayload::new("live"))
                .await,
            "enqueue"
        ),
        RelayOutcome::Enqueued { depth: 1 },
        "and accepts payloads"
    );

    ok(h.advance(store.ttl()).await, "advance to the channel TTL");
    assert_eq!(
        ok(store.lookup(&channel).await, "lookup"),
        None,
        "the ceremony window closes on the store's own TTL"
    );
    assert_eq!(
        ok(
            store
                .enqueue(&channel, Direction::ToEnrollee, RelayPayload::new("late"))
                .await,
            "enqueue"
        ),
        RelayOutcome::NoChannel,
        "and nothing may be relayed after it"
    );
}

/// Payloads drain in arrival order, verbatim, and one direction does not disturb the other.
pub async fn relayed_payloads_drain_in_order_and_by_direction(h: &dyn Harness) {
    let store = h.channels();
    let channel = ChannelId::new("relay-ordering");
    ok(
        store
            .open(
                &channel,
                RelayChannel {
                    initiator_user_id: UserId::new("relay-ordering-initiator"),
                    opened_at: base(),
                },
            )
            .await,
        "open",
    );

    for (index, payload) in ["first", "second", "third"].iter().enumerate() {
        assert_eq!(
            ok(
                store
                    .enqueue(&channel, Direction::ToEnrollee, RelayPayload::new(*payload))
                    .await,
                "enqueue"
            ),
            RelayOutcome::Enqueued { depth: index + 1 },
            "each append reports the resulting depth — no payload is lost to a read-modify-write"
        );
    }
    ok(
        store
            .enqueue(
                &channel,
                Direction::ToInitiator,
                RelayPayload::new("toward-a"),
            )
            .await,
        "enqueue",
    );

    assert_eq!(
        ok(store.drain(&channel, Direction::ToEnrollee).await, "drain"),
        DrainOutcome::Drained(vec![
            RelayPayload::new("first"),
            RelayPayload::new("second"),
            RelayPayload::new("third"),
        ]),
        "payloads come back in arrival order and byte-identical"
    );
    assert_eq!(
        ok(store.drain(&channel, Direction::ToEnrollee).await, "drain"),
        DrainOutcome::Drained(Vec::new()),
        "a drain is destructive — a payload is delivered once"
    );
    assert_eq!(
        ok(store.drain(&channel, Direction::ToInitiator).await, "drain"),
        DrainOutcome::Drained(vec![RelayPayload::new("toward-a")]),
        "and draining one direction leaves the other intact"
    );
}

/// Closing a channel takes both mailboxes with it.
pub async fn closing_a_channel_drops_both_mailboxes(h: &dyn Harness) {
    let store = h.channels();
    let channel = ChannelId::new("relay-close");
    ok(
        store
            .open(
                &channel,
                RelayChannel {
                    initiator_user_id: UserId::new("relay-close-initiator"),
                    opened_at: base(),
                },
            )
            .await,
        "open",
    );
    for direction in [Direction::ToInitiator, Direction::ToEnrollee] {
        ok(
            store
                .enqueue(&channel, direction, RelayPayload::new("pending"))
                .await,
            "enqueue",
        );
    }

    assert!(
        ok(store.close(&channel).await, "close"),
        "the channel was live"
    );
    assert_eq!(
        ok(store.lookup(&channel).await, "lookup"),
        None,
        "a closed channel is gone"
    );
    assert!(
        !ok(store.close(&channel).await, "close"),
        "closing twice reports nothing the second time"
    );

    // Re-opening the same id must not resurrect the old ceremony's undelivered payloads.
    ok(
        store
            .open(
                &channel,
                RelayChannel {
                    initiator_user_id: UserId::new("relay-close-initiator"),
                    opened_at: base(),
                },
            )
            .await,
        "open",
    );
    for direction in [Direction::ToInitiator, Direction::ToEnrollee] {
        assert_eq!(
            ok(store.drain(&channel, direction).await, "drain"),
            DrainOutcome::Drained(Vec::new()),
            "the mailboxes died with the channel — they have no lifetime of their own"
        );
    }
}

/// A registration and an authentication under one ceremony id do not see each other.
///
/// The Salvo store namespaced these by a hand-formatted `passkey_reg:` / `passkey_auth:` key
/// prefix and typed both as `T`, so reading one as the other compiled and failed at runtime.
pub async fn webauthn_registration_and_authentication_do_not_collide(h: &dyn Harness) {
    let store = h.webauthn();
    let ceremony = CeremonyId::new("webauthn-shared-id");
    let registration = RegistrationCeremony {
        user_id: UserId::new("webauthn-user"),
        state: CeremonyState::new("{\"registration\":true}"),
    };
    let authentication = AuthenticationCeremony {
        state: CeremonyState::new("{\"authentication\":true}"),
    };

    ok(
        store
            .begin_registration(&ceremony, registration.clone())
            .await,
        "begin_registration",
    );
    ok(
        store
            .begin_authentication(&ceremony, authentication.clone())
            .await,
        "begin_authentication",
    );

    assert_eq!(
        ok(
            store.finish_registration(&ceremony).await,
            "finish_registration"
        ),
        Some(registration),
        "the registration comes back as a registration"
    );
    assert_eq!(
        ok(
            store.finish_authentication(&ceremony).await,
            "finish_authentication"
        ),
        Some(authentication),
        "and the authentication is untouched by finishing the registration"
    );
}

/// Finishing a WebAuthn ceremony consumes it.
pub async fn a_webauthn_ceremony_is_consumed_by_its_finish(h: &dyn Harness) {
    let store = h.webauthn();
    let ceremony = CeremonyId::new("webauthn-single-use");
    ok(
        store
            .begin_registration(
                &ceremony,
                RegistrationCeremony {
                    user_id: UserId::new("webauthn-single-use-user"),
                    state: CeremonyState::new("state"),
                },
            )
            .await,
        "begin_registration",
    );

    assert!(
        ok(
            store.finish_registration(&ceremony).await,
            "finish_registration"
        )
        .is_some(),
        "the first finish gets the ceremony"
    );
    assert_eq!(
        ok(
            store.finish_registration(&ceremony).await,
            "finish_registration"
        ),
        None,
        "a finished ceremony cannot be replayed"
    );
    assert_eq!(
        ok(
            store
                .finish_authentication(&CeremonyId::new("webauthn-never-started"))
                .await,
            "finish_authentication"
        ),
        None,
        "an unknown ceremony is absent, not a failure"
    );
}

/// A WebAuthn ceremony dies at its store's TTL.
pub async fn a_webauthn_ceremony_expires_with_its_store(h: &dyn Harness) {
    let store = h.webauthn();
    let ceremony = CeremonyId::new("webauthn-expiry");
    ok(
        store
            .begin_authentication(
                &ceremony,
                AuthenticationCeremony {
                    state: CeremonyState::new("state"),
                },
            )
            .await,
        "begin_authentication",
    );

    ok(h.advance(store.ttl()).await, "advance to the ceremony TTL");

    assert_eq!(
        ok(
            store.finish_authentication(&ceremony).await,
            "finish_authentication"
        ),
        None,
        "the ceremony window is the store's property, restated by no route"
    );
}

// -------------------------------------------------------------------------------------------
// Device cohorts
// -------------------------------------------------------------------------------------------

/// A cohort is a fact about a device, not an event: seeing it twice is one row.
pub async fn observing_a_cohort_twice_is_one_row_that_moves_last_seen(h: &dyn Harness) {
    let user = UserId::new("cohort-user-1");
    let at = Timestamp::UNIX_EPOCH;

    let first = ok(
        h.cohorts().observe(&user, "cohort-a", at).await,
        "observe a cohort",
    );
    assert_eq!(first.first_seen, at);
    assert_eq!(first.last_seen, at);

    let later = crate::store::deadline(at, SignedDuration::from_hours(72));
    let second = ok(
        h.cohorts().observe(&user, "cohort-a", later).await,
        "observe the same cohort again",
    );
    assert_eq!(
        second.first_seen, at,
        "first_seen never moves: it is what lets a client say `a device you've used before`"
    );
    assert_eq!(second.last_seen, later);

    let held = ok(
        h.cohorts().cohorts_for_user(&user).await,
        "list an account's cohorts",
    );
    assert_eq!(held.len(), 1, "one physical device is one row");
}

/// Cohorts are listed oldest first sighting first, and the order is total.
pub async fn cohorts_are_listed_oldest_first(h: &dyn Harness) {
    let user = UserId::new("cohort-user-2");
    let base = Timestamp::UNIX_EPOCH;
    for (hash, hours) in [
        ("cohort-third", 48),
        ("cohort-first", 0),
        ("cohort-second", 24),
    ] {
        ok(
            h.cohorts()
                .observe(
                    &user,
                    hash,
                    crate::store::deadline(base, SignedDuration::from_hours(hours)),
                )
                .await,
            "observe a cohort",
        );
    }

    let held = ok(h.cohorts().cohorts_for_user(&user).await, "list cohorts");
    let order: Vec<&str> = held.iter().map(|c| c.cohort_hash.as_str()).collect();
    assert_eq!(order, ["cohort-first", "cohort-second", "cohort-third"]);
}

/// A cohort is scoped to its account, and the hash folds the account in besides.
pub async fn a_cohort_is_scoped_to_its_account(h: &dyn Harness) {
    let mine = UserId::new("cohort-user-3");
    let theirs = UserId::new("cohort-user-4");
    ok(
        h.cohorts()
            .observe(&mine, "shared-spelling", Timestamp::UNIX_EPOCH)
            .await,
        "observe a cohort",
    );

    assert!(
        ok(h.cohorts().cohorts_for_user(&theirs).await, "list cohorts").is_empty(),
        "one account's cohorts are not another's — and the hash folds `user_id` in besides, so \
         the same physical device under two accounts does not even produce this spelling twice"
    );
}

/// The cohort map does not expire with the sessions that carried it.
pub async fn the_cohort_map_does_not_expire(h: &dyn Harness) {
    // The one store in this module with no TTL, and deliberately: a cohort is worth recording
    // precisely because it outlives the sessions that named it. A map that expired with them
    // would forget exactly when "have I seen this device before?" starts being worth asking.
    let user = UserId::new("cohort-user-5");
    ok(
        h.cohorts()
            .observe(&user, "cohort-durable", Timestamp::UNIX_EPOCH)
            .await,
        "observe a cohort",
    );

    ok(
        h.advance(SignedDuration::from_hours(24 * 365)).await,
        "advance a year",
    );

    let held = ok(h.cohorts().cohorts_for_user(&user).await, "list cohorts");
    assert_eq!(held.len(), 1, "a cohort a year old is still a cohort");
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
///
/// For a backend where standing up a harness is expensive — a Valkey or Postgres container —
/// this is the entry point. A unit-tier adapter should prefer calling the cases individually,
/// one `#[tokio::test]` each, so a failure names the property that broke rather than the suite.
pub async fn run_all(h: &dyn Harness) {
    session_round_trips_every_field(h).await;
    reading_an_unknown_session_is_none(h).await;
    user_listing_is_ordered_and_scoped(h).await;
    closing_one_session_removes_it_from_the_user_listing(h).await;
    revoke_all_reports_only_sessions_actually_removed(h).await;
    closing_an_unknown_session_removes_nothing(h).await;
    reopening_a_session_id_does_not_duplicate_the_listing(h).await;
    touching_a_session_records_activity_without_extending_its_life(h).await;
    an_expired_session_leaves_no_listing_entry_behind(h).await;

    upload_session_round_trips_every_field(h).await;
    uploader_listing_is_ordered_and_scoped(h).await;
    a_pending_address_is_owner_scoped_and_ends_with_the_session(h).await;
    discarding_a_session_withdraws_its_pending_address(h).await;
    recording_progress_advances_bytes_clock_and_replay_together(h).await;
    chunk_replay_is_offset_addressed(h).await;
    finalization_is_claimed_exactly_once(h).await;
    reconciling_received_bytes_does_not_move_the_progress_clock(h).await;
    a_terminal_session_is_not_an_eviction_candidate(h).await;
    discarding_removes_the_record_its_chunks_and_its_listing(h).await;
    eviction_candidates_are_ordered_by_progress_and_bounded(h).await;
    an_expired_upload_session_leaves_no_listing_entry_behind(h).await;

    a_challenge_is_single_use(h).await;
    a_challenge_expires_with_its_store(h).await;
    an_enrollment_redeems_by_either_spelling_and_burns_both(h).await;
    an_unissued_enrollment_code_is_not_taken(h).await;
    an_enrollment_expires_with_its_store(h).await;
    relaying_requires_a_live_channel(h).await;
    relayed_payloads_drain_in_order_and_by_direction(h).await;
    closing_a_channel_drops_both_mailboxes(h).await;
    a_webauthn_ceremony_is_consumed_by_its_finish(h).await;
    webauthn_registration_and_authentication_do_not_collide(h).await;
    a_webauthn_ceremony_expires_with_its_store(h).await;
    observing_a_cohort_twice_is_one_row_that_moves_last_seen(h).await;
    cohorts_are_listed_oldest_first(h).await;
    a_cohort_is_scoped_to_its_account(h).await;
    the_cohort_map_does_not_expire(h).await;
}
