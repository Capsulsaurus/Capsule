use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::*;

/// A clock whose reading the test advances by hand.
struct ManualClock {
    ms: Cell<i64>,
}

impl ManualClock {
    fn new() -> Self {
        Self { ms: Cell::new(0) }
    }
    fn advance(&self, by: Duration) {
        self.ms.set(self.ms.get() + by.as_millis() as i64);
    }
}

impl GraceClock for ManualClock {
    fn now_millis(&self) -> i64 {
        self.ms.get()
    }
}

/// A gate that always succeeds, counting the challenges it saw (atomics keep it `Send + Sync`,
/// which the [`LocalAuthGate`] bound requires).
struct AllowGate {
    calls: AtomicU32,
}
impl AllowGate {
    fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
        }
    }
    fn calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }
}
impl LocalAuthGate for AllowGate {
    fn authenticate(&self, _view: GatedView) -> Result<(), LocalAuthError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// A gate that always refuses with a fixed error.
struct DenyGate(LocalAuthError);
impl LocalAuthGate for DenyGate {
    fn authenticate(&self, _view: GatedView) -> Result<(), LocalAuthError> {
        Err(self.0.clone())
    }
}

/// Models a platform adapter whose biometric is unavailable and which therefore falls back to
/// the device credential — succeeding, and recording that the fallback path was taken.
struct BiometricFallbackToCredentialGate {
    fell_back: AtomicBool,
}
impl BiometricFallbackToCredentialGate {
    fn new() -> Self {
        Self {
            fell_back: AtomicBool::new(false),
        }
    }
}
impl LocalAuthGate for BiometricFallbackToCredentialGate {
    fn authenticate(&self, _view: GatedView) -> Result<(), LocalAuthError> {
        // Biometric unavailable → the adapter (platform side) uses the credential and succeeds.
        self.fell_back.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn locked_until_opened_then_refuses_after_grace_expiry() {
    let clock = ManualClock::new();
    let mut gk = GateKeeper::with_clock_and_grace(clock, DEFAULT_GRACE);
    let gate = AllowGate::new();

    // Locked before any grant.
    assert!(!gk.is_open(GatedView::RecentlyDeleted));
    assert_eq!(
        gk.guard(GatedView::RecentlyDeleted).unwrap_err(),
        GateError::Locked
    );

    // Open mints one grant.
    gk.open(GatedView::RecentlyDeleted, &gate).unwrap();
    assert!(gk.is_open(GatedView::RecentlyDeleted));
    assert_eq!(gate.calls(), 1);

    // Within the window, re-opening reuses the grant (no second challenge).
    gk.clock.advance(Duration::from_mins(4));
    gk.open(GatedView::RecentlyDeleted, &gate).unwrap();
    assert_eq!(gate.calls(), 1, "re-open within grace does not re-auth");
    assert!(gk.is_open(GatedView::RecentlyDeleted));

    // Past the window measured from the original mint, the view re-locks.
    gk.clock.advance(Duration::from_mins(2)); // now 6 min since mint
    assert!(!gk.is_open(GatedView::RecentlyDeleted));
    assert_eq!(
        gk.guard(GatedView::RecentlyDeleted).unwrap_err(),
        GateError::Locked
    );

    // Re-opening now issues a fresh challenge.
    gk.open(GatedView::RecentlyDeleted, &gate).unwrap();
    assert_eq!(gate.calls(), 2, "re-open after expiry re-auths");
}

#[test]
fn grace_boundary_is_exclusive() {
    let clock = ManualClock::new();
    let mut gk = GateKeeper::with_clock_and_grace(clock, DEFAULT_GRACE);
    gk.open(GatedView::Hidden, &AllowGate::new()).unwrap();

    // Exactly at the window edge (300_000 ms) the grant is expired (`<` is strict).
    gk.clock.advance(DEFAULT_GRACE);
    assert!(!gk.is_open(GatedView::Hidden));

    // One millisecond before the edge it is still live.
    let clock2 = ManualClock::new();
    let mut gk2 = GateKeeper::with_clock_and_grace(clock2, DEFAULT_GRACE);
    gk2.open(GatedView::Hidden, &AllowGate::new()).unwrap();
    gk2.clock
        .advance(DEFAULT_GRACE.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(gk2.is_open(GatedView::Hidden));
}

#[test]
fn grants_are_per_view_independent() {
    let clock = ManualClock::new();
    let mut gk = GateKeeper::with_clock(clock);
    let gate = AllowGate::new();

    // Opening Recently Deleted must NOT open Hidden.
    gk.open(GatedView::RecentlyDeleted, &gate).unwrap();
    assert!(gk.is_open(GatedView::RecentlyDeleted));
    assert!(!gk.is_open(GatedView::Hidden));
    assert_eq!(gk.guard(GatedView::Hidden).unwrap_err(), GateError::Locked);

    // Opening Hidden leaves both open, tracked independently.
    gk.open(GatedView::Hidden, &gate).unwrap();
    assert!(gk.is_open(GatedView::RecentlyDeleted));
    assert!(gk.is_open(GatedView::Hidden));
    assert_eq!(gate.calls(), 2, "each view authenticated separately");

    // Revoking one leaves the other.
    gk.revoke(GatedView::RecentlyDeleted);
    assert!(!gk.is_open(GatedView::RecentlyDeleted));
    assert!(gk.is_open(GatedView::Hidden));
}

#[test]
fn open_propagates_platform_denial_and_stays_locked() {
    let mut gk = GateKeeper::with_clock(ManualClock::new());

    let err = gk
        .open(
            GatedView::RecentlyDeleted,
            &DenyGate(LocalAuthError::Cancelled),
        )
        .unwrap_err();
    assert_eq!(err, GateError::Auth(LocalAuthError::Cancelled));
    assert!(
        !gk.is_open(GatedView::RecentlyDeleted),
        "denial mints no grant"
    );

    let err = gk
        .open(GatedView::Hidden, &DenyGate(LocalAuthError::Unavailable))
        .unwrap_err();
    assert_eq!(err, GateError::Auth(LocalAuthError::Unavailable));
}

#[test]
fn biometric_fallback_to_credential_opens_the_view() {
    // The biometric→credential decision is the platform adapter's; core accepts any Ok as a
    // fresh grant. This proves the seam honors a credential-fallback success.
    let mut gk = GateKeeper::with_clock(ManualClock::new());
    let gate = BiometricFallbackToCredentialGate::new();
    gk.open(GatedView::Hidden, &gate).unwrap();
    assert!(
        gate.fell_back.load(Ordering::Relaxed),
        "adapter fell back to the credential"
    );
    assert!(gk.is_open(GatedView::Hidden));
}

#[test]
fn lock_revokes_every_grant() {
    let mut gk = GateKeeper::with_clock(ManualClock::new());
    let gate = AllowGate::new();
    gk.open(GatedView::RecentlyDeleted, &gate).unwrap();
    gk.open(GatedView::Hidden, &gate).unwrap();

    gk.lock();
    assert!(!gk.is_open(GatedView::RecentlyDeleted));
    assert!(!gk.is_open(GatedView::Hidden));
}

#[test]
fn gated_trash_query_refuses_without_grant_and_serves_with_one() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.init_schema().unwrap();

    // One soft-deleted asset in the index, plus a live one that must never appear in trash.
    let mut row = AssetRow {
        uuid: "11110000-0000-0000-0000-000000000001".to_string(),
        asset_type: "photo".to_string(),
        capture_timestamp: 1,
        capture_utc: None,
        capture_tz_source: None,
        import_timestamp: 1,
        hash_sha256: "a".repeat(64),
        width: None,
        height: None,
        duration_ms: None,
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: None,
        rating: 0,
        is_deleted: true,
        deleted_at: Some(100),
        is_hidden: false,
        caption: None,
        latitude: None,
        longitude: None,
    };
    db.insert_asset(&row).unwrap();
    row.uuid = "11110000-0000-0000-0000-000000000002".to_string();
    row.is_deleted = false;
    row.deleted_at = None;
    row.hash_sha256 = "b".repeat(64);
    db.insert_asset(&row).unwrap();

    let mut gk = GateKeeper::with_clock(ManualClock::new());

    // Refuses before a grant.
    let err = gk.query_recently_deleted(&db, 0, 100).unwrap_err();
    assert!(matches!(err, GatedQueryError::Gate(GateError::Locked)));

    // Serves the soft-deleted asset once a grant exists.
    gk.open(GatedView::RecentlyDeleted, &AllowGate::new())
        .unwrap();
    let trash = gk.query_recently_deleted(&db, 0, 100).unwrap();
    assert_eq!(trash.len(), 1);
    assert!(trash[0].is_deleted);

    // A Hidden grant does NOT unlock the Recently Deleted query.
    gk.revoke(GatedView::RecentlyDeleted);
    gk.open(GatedView::Hidden, &AllowGate::new()).unwrap();
    let err = gk.query_recently_deleted(&db, 0, 100).unwrap_err();
    assert!(matches!(err, GatedQueryError::Gate(GateError::Locked)));
}

/// The Hidden view under the *same* gate contract as Recently Deleted. This is
/// the deliberate mirror of `gated_trash_query_refuses_without_grant_and_serves_with_one`
/// above: one policy, two views. Refuses without a grant, serves with one, and a
/// Recently Deleted grant never substitutes for a Hidden one.
#[test]
fn gated_hidden_query_refuses_without_grant_and_serves_with_one() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.init_schema().unwrap();

    // One hidden asset in the index, plus a visible one that must never appear.
    let mut row = AssetRow {
        uuid: "22220000-0000-0000-0000-000000000001".to_string(),
        asset_type: "photo".to_string(),
        capture_timestamp: 1,
        capture_utc: None,
        capture_tz_source: None,
        import_timestamp: 1,
        hash_sha256: "a".repeat(64),
        width: None,
        height: None,
        duration_ms: None,
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: None,
        rating: 0,
        is_deleted: false,
        deleted_at: None,
        is_hidden: true,
        caption: None,
        latitude: None,
        longitude: None,
    };
    db.insert_asset(&row).unwrap();
    row.uuid = "22220000-0000-0000-0000-000000000002".to_string();
    row.is_hidden = false;
    row.hash_sha256 = "b".repeat(64);
    db.insert_asset(&row).unwrap();

    let mut gk = GateKeeper::with_clock(ManualClock::new());

    // Refuses before a grant.
    let err = gk.query_hidden(&db, 0, 100).unwrap_err();
    assert!(matches!(err, GatedQueryError::Gate(GateError::Locked)));

    // Serves the hidden asset once a grant exists.
    gk.open(GatedView::Hidden, &AllowGate::new()).unwrap();
    let hidden = gk.query_hidden(&db, 0, 100).unwrap();
    assert_eq!(hidden.len(), 1);
    assert!(hidden[0].is_hidden);

    // A Recently Deleted grant does NOT unlock the Hidden query.
    gk.revoke(GatedView::Hidden);
    gk.open(GatedView::RecentlyDeleted, &AllowGate::new())
        .unwrap();
    let err = gk.query_hidden(&db, 0, 100).unwrap_err();
    assert!(matches!(err, GatedQueryError::Gate(GateError::Locked)));
}

/// The Hidden view inherits the *same* 5-minute grace window as Recently Deleted — not a
/// second policy. Mirrors `locked_until_opened_then_refuses_after_grace_expiry`, but
/// asserted through the gated query rather than `is_open`.
#[test]
fn hidden_view_re_locks_on_the_same_five_minute_grace_as_recently_deleted() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.init_schema().unwrap();

    let clock = ManualClock::new();
    let mut gk = GateKeeper::with_clock_and_grace(clock, DEFAULT_GRACE);
    let gate = AllowGate::new();
    gk.open(GatedView::Hidden, &gate).unwrap();

    // Within the window the query is served without re-authenticating.
    gk.clock.advance(Duration::from_mins(4));
    assert!(gk.query_hidden(&db, 0, 100).is_ok());
    assert_eq!(gate.calls(), 1, "re-query within grace does not re-auth");

    // Past the window, measured from the original mint, the view re-locks.
    gk.clock.advance(Duration::from_mins(2));
    let err = gk.query_hidden(&db, 0, 100).unwrap_err();
    assert!(matches!(err, GatedQueryError::Gate(GateError::Locked)));

    // Re-opening issues a fresh challenge, exactly as for the trash.
    gk.open(GatedView::Hidden, &gate).unwrap();
    assert_eq!(gate.calls(), 2, "re-open after expiry re-auths");
    assert!(gk.query_hidden(&db, 0, 100).is_ok());
}
