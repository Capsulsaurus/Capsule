use super::super::conformance;
use super::*;

/// A harness whose stores all die after a minute, so an expiry case advances a readable
/// amount and the non-expiry cases never trip over it.
fn harness() -> InMemoryStores {
    InMemoryStores::with_uniform_ttl(SignedDuration::from_mins(1))
}

/// Declares one `#[tokio::test]` per conformance case.
///
/// One test each, on a fresh harness each: a failure names the property that broke, and no
/// case can pass because a previous one left the store in a convenient state.
macro_rules! conformance_cases {
    ($($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                conformance::$case(&harness()).await;
            }
        )+
    };
}

conformance_cases! {
    session_round_trips_every_field,
    reading_an_unknown_session_is_none,
    user_listing_is_ordered_and_scoped,
    closing_one_session_removes_it_from_the_user_listing,
    revoke_all_reports_only_sessions_actually_removed,
    closing_an_unknown_session_removes_nothing,
    reopening_a_session_id_does_not_duplicate_the_listing,
    touching_a_session_records_activity_without_extending_its_life,
    an_expired_session_leaves_no_listing_entry_behind,
    upload_session_round_trips_every_field,
    uploader_listing_is_ordered_and_scoped,
    a_pending_address_is_uploader_scoped_and_ends_with_the_session,
    discarding_a_session_withdraws_its_pending_address,
    recording_progress_advances_bytes_clock_and_replay_together,
    chunk_replay_is_offset_addressed,
    finalization_is_claimed_exactly_once,
    a_claimed_session_leaves_the_eviction_view,
    reconciling_received_bytes_does_not_move_the_progress_clock,
    a_terminal_session_is_not_an_eviction_candidate,
    discarding_removes_the_record_its_chunks_and_its_listing,
    eviction_candidates_are_ordered_by_progress_and_bounded,
    the_claimed_listing_is_exactly_the_sessions_awaiting_their_finalizer,
    an_expired_upload_session_leaves_no_listing_entry_behind,
    a_challenge_is_single_use,
    a_challenge_expires_with_its_store,
    an_enrollment_redeems_by_either_spelling_and_burns_both,
    an_unissued_enrollment_code_is_not_taken,
    an_enrollment_expires_with_its_store,
    relaying_requires_a_live_channel,
    relayed_payloads_drain_in_order_and_by_direction,
    closing_a_channel_drops_both_mailboxes,
    an_oidc_authorization_is_single_use,
    an_oidc_authorization_expires_with_its_store,
    an_oidc_authorization_is_live_until_its_deadline,
    a_device_poll_is_due_only_after_its_interval,
    a_device_poll_too_soon_raises_the_interval,
    two_polls_racing_one_handle_claim_one_slot,
    a_provider_slow_down_raises_the_interval,
    a_finished_device_authorization_is_gone,
    a_device_authorization_expires_with_its_store,
}

/// The same, for the cases that take a [`conformance::CohortHarness`].
///
/// A second macro because the cohort map is a different port with a different production
/// backend, so its cases take the narrower harness — and one `#[tokio::test]` each here
/// rather than only inside `run_all`, which is what makes a cohort failure name the property
/// that broke.
macro_rules! cohort_conformance_cases {
    ($($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                conformance::$case(&harness()).await;
            }
        )+
    };
}

cohort_conformance_cases! {
    observing_a_cohort_twice_is_one_row_that_moves_last_seen,
    cohorts_are_listed_oldest_first,
    a_cohort_is_scoped_to_its_account,
    the_cohort_map_does_not_expire,
}

/// The whole suite, in one pass on one harness.
///
/// This is the entry point a container-backed adapter uses, so it is exercised here too —
/// otherwise the first time anyone runs it would be against Valkey, where a failure is
/// hardest to read. It also proves the cases really are independent: they share a harness
/// and a clock that only moves forward.
#[tokio::test]
async fn the_whole_suite_passes_in_one_pass() {
    conformance::run_all(&harness()).await;
}

/// A store built with production lifetimes really carries them.
///
/// The suite runs on a uniform short TTL, so without this nothing would notice a wiring
/// mistake that gave every store the same lifetime in production too.
#[test]
fn production_lifetimes_are_per_ceremony() {
    let stores = InMemoryStores::new();
    assert_eq!(AuthStateStore::ttl(&stores.auth), DEFAULT_SESSION_TTL);
    assert_eq!(
        AuthStateStore::hard_cap(&stores.auth),
        DEFAULT_SESSION_HARD_CAP
    );
    assert_eq!(UploadSessionStore::ttl(&stores.uploads), LIFETIME_CAP);
    assert_eq!(ChallengeStore::ttl(&stores.challenges), CHALLENGE_TTL);
    assert_eq!(
        EnrollmentStore::ttl(&stores.enrollments),
        ENROLLMENT_CODE_TTL
    );
    assert_eq!(ChannelStore::ttl(&stores.channels), RELAY_CHANNEL_TTL);
    assert_eq!(
        OidcAuthorizationStore::ttl(&stores.oidc_authorizations),
        OIDC_AUTHORIZATION_TTL
    );
    assert_eq!(OidcDeviceStore::ttl(&stores.oidc_devices), OIDC_DEVICE_TTL);
    assert_ne!(
        CHALLENGE_TTL, ENROLLMENT_CODE_TTL,
        "a ceremony's window belongs to what it is; if these ever coincide by accident \
             this assertion stops being evidence"
    );
}

/// A record opened at `opened_at` in a lineage that signed in at the epoch.
fn lineage_record(id: &str, opened_at: Timestamp) -> SessionRecord {
    SessionRecord {
        session_id: SessionId::new(id),
        user_id: UserId::new("lineage-user"),
        created_at: opened_at,
        authenticated_at: Timestamp::UNIX_EPOCH,
        signed_in_at: Timestamp::UNIX_EPOCH,
        last_active_at: opened_at,
        user_agent: None,
        ip_address: None,
        cohort_hash: None,
        device_id: None,
    }
}

/// Each open slides the window; none of them moves the hard cap.
///
/// The rotation a refresh performs is a close and an open of a record carrying the lineage's
/// `signed_in_at`, so this is the cap surviving rotation at the store's level: re-opened
/// sessions keep living a full window until the window would cross the cap, and then live
/// only until the cap.
#[tokio::test]
async fn reopening_slides_the_window_but_never_past_the_hard_cap() {
    let clock = ManualClock::default();
    let store = InMemoryAuthState::new(Arc::new(clock.clone()), SignedDuration::from_hours(24))
        .with_hard_cap(SignedDuration::from_hours(60));
    let tick = SignedDuration::from_nanos(1);

    // A sign-in and two rotations, each inside the previous window: hours 0, 23 and 46.
    for (wait, id) in [(0, "first"), (23, "second"), (23, "third")] {
        clock.advance(SignedDuration::from_hours(wait));
        store
            .open_session(lineage_record(id, clock.now()))
            .await
            .expect("opens");
    }
    assert!(
        store
            .read_session(&SessionId::new("second"))
            .await
            .expect("answers")
            .is_some(),
        "the second record, opened at hour 23, still has its full window"
    );
    // The third opened at hour 46: a full window would run to hour 70, the cap stops it at 60.
    assert_eq!(
        store.expires_at(&lineage_record("probe", clock.now()), clock.now()),
        deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(60))
    );

    clock.advance(SignedDuration::from_hours(14) - tick);
    assert!(
        store
            .read_session(&SessionId::new("third"))
            .await
            .expect("answers")
            .is_some(),
        "just inside the cap the latest rotation is live"
    );

    clock.advance(tick);
    assert_eq!(
        store
            .read_session(&SessionId::new("third"))
            .await
            .expect("answers"),
        None,
        "at the cap it is gone, ten hours before its window would have closed"
    );

    // And a record opened past the cap is born expired.
    store
        .open_session(lineage_record("late", clock.now()))
        .await
        .expect("opens");
    assert_eq!(
        store
            .read_session(&SessionId::new("late"))
            .await
            .expect("answers"),
        None,
        "a lineage past its cap cannot be re-opened into life"
    );
}

/// A full OIDC ceremony store refuses, and expired ceremonies never count against it.
#[tokio::test]
async fn a_full_oidc_store_refuses_until_its_ceremonies_expire() {
    use super::super::ceremony::{OidcAuthorizationStore, PendingAuthorization};
    use super::super::ids::{OidcNonce, OidcState, PkceVerifier};

    let clock = ManualClock::default();
    let store =
        InMemoryOidcAuthorizations::new(Arc::new(clock.clone()), SignedDuration::from_mins(10))
            .with_ceiling(2);
    let pending = |tag: &str| PendingAuthorization {
        nonce: OidcNonce::new(format!("{tag}-nonce")),
        verifier: PkceVerifier::new(format!("{tag}-verifier")),
        redirect_uri: "http://127.0.0.1:1/cb".to_owned(),
        binding_challenge: format!("{tag}-binding"),
        issued_at: clock.now(),
    };
    store
        .begin(&OidcState::new("a"), pending("a"))
        .await
        .expect("room");
    store
        .begin(&OidcState::new("b"), pending("b"))
        .await
        .expect("room");
    assert!(
        matches!(
            store.begin(&OidcState::new("c"), pending("c")).await,
            Err(StoreError::Rejected { .. })
        ),
        "the third is refused at a ceiling of two"
    );
    // The expired ones are purged on the next begin, so the refusal is not permanent.
    clock.advance(SignedDuration::from_mins(10));
    store
        .begin(&OidcState::new("c"), pending("c"))
        .await
        .expect("the expired ceremonies made room");
    assert!(
        store
            .consume(&OidcState::new("a"))
            .await
            .expect("answers")
            .is_none()
    );
}

/// A full device store refuses, and expired ceremonies never count against it.
#[tokio::test]
async fn a_full_device_store_refuses_until_its_ceremonies_expire() {
    use super::super::ids::DeviceCode;

    let clock = ManualClock::default();
    let store = InMemoryOidcDevices::new(Arc::new(clock.clone()), SignedDuration::from_mins(15))
        .with_ceiling(1);
    let pending = |tag: &str| PendingDeviceAuthorization {
        device_code: DeviceCode::new(format!("{tag}-code")),
        binding_challenge: format!("{tag}-binding"),
        interval: SignedDuration::from_secs(5),
        issued_at: clock.now(),
    };
    store
        .begin(&OidcDeviceHandle::new("a"), pending("a"))
        .await
        .expect("room");
    assert!(
        matches!(
            store.begin(&OidcDeviceHandle::new("b"), pending("b")).await,
            Err(StoreError::Rejected { .. })
        ),
        "the second is refused at a ceiling of one"
    );
    clock.advance(SignedDuration::from_mins(15));
    store
        .begin(&OidcDeviceHandle::new("b"), pending("b"))
        .await
        .expect("the expired ceremony made room");
}

/// The manual clock only moves when a test moves it.
#[test]
fn the_manual_clock_is_deterministic() {
    let clock = ManualClock::default();
    let start = clock.now();
    assert_eq!(
        clock.now(),
        start,
        "a manual clock does not tick on its own"
    );
    clock.advance(SignedDuration::from_secs(90));
    assert_eq!(clock.now(), deadline(start, SignedDuration::from_secs(90)));
}
