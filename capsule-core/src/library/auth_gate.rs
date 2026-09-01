//! Fresh-local-auth gates for the sensitive local-gallery views (SSoT: [Local Gallery — SR1]).
//!
//! Opening the **Recently Deleted** (trash) or **Hidden** view requires *fresh local
//! authentication* — biometric where enrolled (Face ID / Touch ID / BiometricPrompt), else the
//! device or account credential. This module owns the **policy**: which views are gated, the
//! per-view grace clock (default 5 minutes), and the refuse-without-grant query surface. It does
//! **not** own the authentication itself — biometrics and the credential fallback are a platform
//! concern, reached through the [`LocalAuthGate`] seam. This mirrors the
//! [`Signer`](crate::crypto::keys::Signer) /
//! [`HardwareSigner`](crate::crypto::keys::HardwareSigner) discipline: the trait lives in core,
//! the Secure Enclave / BiometricPrompt adapters live in `capsule-core-swift` / `capsule-core-kotlin`.
//!
//! The gate is view-time snoop protection, **not** a cryptographic boundary (SR1): the same bytes
//! are reachable through the filesystem by anyone who defeats the platform sandbox (see SR2). It
//! exists to stop a borrowed-unlocked-phone from casually browsing the trash or the hidden set.
//!
//! [Local Gallery — SR1]: https://docs/design/local-gallery/#security-requirements

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::db::DatabaseDriver;
use crate::db::rows::AssetRow;

/// The default per-view grace window (SR1): one fresh grant covers 5 minutes, after which the
/// view re-locks and re-authentication is required.
pub const DEFAULT_GRACE: Duration = Duration::from_mins(5);

/// A view that requires fresh local auth before it opens (SR1). A grant is **per-view**:
/// authenticating for one view never opens the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum GatedView {
    /// The trash / "Recently Deleted" listing of soft-deleted assets.
    RecentlyDeleted,
    /// The user-hidden set (assets whose sidecar `hidden` register is set).
    Hidden,
}

/// The per-platform local-authentication seam, implemented by native code (Swift/Kotlin) over the
/// uniffi foreign-trait boundary under the `ffi` feature, a plain Rust trait otherwise. Rust calls
/// *into* it to perform the fresh-auth challenge; core never sees the biometric or the credential.
///
/// The **biometric → credential fallback** is entirely the implementation's concern: an adapter
/// tries the enrolled biometric first (Face ID / Touch ID / BiometricPrompt) and falls back to the
/// device or account credential, surfacing only the *outcome* through this trait. Core treats any
/// [`Ok`] as a successful fresh auth regardless of which method produced it.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait LocalAuthGate: Send + Sync {
    /// Perform a fresh local-authentication challenge for `view`. Returns [`Ok`] on success (by
    /// any enrolled method), or a [`LocalAuthError`] on denial, cancellation, or unavailability.
    fn authenticate(&self, view: GatedView) -> Result<(), LocalAuthError>;
}

/// Failure surfaced by a [`LocalAuthGate`] backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error), uniffi(flat_error))]
pub enum LocalAuthError {
    /// The user dismissed or cancelled the authentication prompt.
    #[error("local authentication was cancelled")]
    Cancelled,
    /// No local authentication method is available (no biometric enrolled and no device
    /// credential set) — the platform cannot challenge.
    #[error("no local authentication method is available")]
    Unavailable,
    /// The challenge was presented and refused (wrong credential, failed biometric).
    #[error("local authentication failed")]
    Failed,
}

/// Refusal returned by the gate's grant surface.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    /// No live grant exists for the view: it is locked and requires a fresh
    /// [`open`](GateKeeper::open).
    #[error("view is locked: fresh local authentication is required")]
    Locked,
    /// The platform gate refused the fresh-auth challenge during [`open`](GateKeeper::open).
    #[error(transparent)]
    Auth(#[from] LocalAuthError),
}

/// Failure of a gated view query — either the gate refused, or the underlying index read failed.
#[derive(Debug, thiserror::Error)]
pub enum GatedQueryError {
    /// The gate refused the read (no live grant).
    #[error(transparent)]
    Gate(#[from] GateError),
    /// The underlying `library.sqlite` query failed.
    #[error("gated view query failed: {0}")]
    Db(String),
}

/// A monotonic clock for the grace window, injectable so tests drive time deterministically.
/// Only *differences* between readings are meaningful; the absolute value has no meaning.
pub trait GraceClock {
    /// A monotonically non-decreasing reading in milliseconds.
    fn now_millis(&self) -> i64;
}

/// The production [`GraceClock`]: a monotonic [`Instant`] anchored at construction. Monotonic (not
/// wall-clock) so a user or NTP nudging the system clock can neither extend nor prematurely expire
/// a grant.
pub struct SystemGraceClock {
    anchor: Instant,
}

impl SystemGraceClock {
    /// Anchor the clock at "now".
    pub fn new() -> Self {
        Self {
            anchor: Instant::now(),
        }
    }
}

impl Default for SystemGraceClock {
    fn default() -> Self {
        Self::new()
    }
}

impl GraceClock for SystemGraceClock {
    fn now_millis(&self) -> i64 {
        self.anchor.elapsed().as_millis() as i64
    }
}

/// A short-lived proof that a live grant existed for `view` at the moment it was minted. Consumed
/// immediately by a gated read; it carries no capability of its own beyond naming the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewGuard {
    view: GatedView,
}

impl ViewGuard {
    /// The view this guard proves access to.
    pub fn view(&self) -> GatedView {
        self.view
    }
}

/// Tracks per-view fresh-auth grants and enforces the grace window (SR1). Owns no authentication
/// itself: it drives a [`LocalAuthGate`] to mint grants and then gates the sensitive view reads.
///
/// Generic over the [`GraceClock`] so tests inject deterministic time; the default is the
/// monotonic [`SystemGraceClock`].
pub struct GateKeeper<C: GraceClock = SystemGraceClock> {
    clock: C,
    grace_ms: i64,
    /// view → the clock reading at which the grant was minted.
    grants: HashMap<GatedView, i64>,
}

impl Default for GateKeeper<SystemGraceClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl GateKeeper<SystemGraceClock> {
    /// A gate keeper on the monotonic system clock with the [`DEFAULT_GRACE`] window.
    pub fn new() -> Self {
        Self::with_clock(SystemGraceClock::new())
    }
}

impl<C: GraceClock> GateKeeper<C> {
    /// A gate keeper on `clock` with the [`DEFAULT_GRACE`] window.
    pub fn with_clock(clock: C) -> Self {
        Self::with_clock_and_grace(clock, DEFAULT_GRACE)
    }

    /// A gate keeper on `clock` with an explicit grace window.
    pub fn with_clock_and_grace(clock: C, grace: Duration) -> Self {
        Self {
            clock,
            grace_ms: grace.as_millis() as i64,
            grants: HashMap::new(),
        }
    }

    /// Whether `view` currently has a live grant (minted within the grace window).
    pub fn is_open(&self, view: GatedView) -> bool {
        match self.grants.get(&view) {
            Some(&minted_at) => self.clock.now_millis().saturating_sub(minted_at) < self.grace_ms,
            None => false,
        }
    }

    /// Open `view`, performing a fresh-auth challenge through `gate` **only if** there is no live
    /// grant already. A grant is *not* slid forward by re-opening within its window: the window is
    /// measured from the original mint, so a snoop cannot keep a view open indefinitely by
    /// re-tapping it. Returns [`GateError::Auth`] if the platform gate refuses.
    #[tracing::instrument(skip_all, fields(view = ?view))]
    pub fn open(&mut self, view: GatedView, gate: &dyn LocalAuthGate) -> Result<(), GateError> {
        if self.is_open(view) {
            tracing::debug!("local-auth gate: reusing live grant, no re-auth");
            return Ok(());
        }
        gate.authenticate(view)?;
        let minted_at = self.clock.now_millis();
        self.grants.insert(view, minted_at);
        tracing::info!("local-auth gate: fresh grant minted");
        Ok(())
    }

    /// A [`ViewGuard`] proving a live grant for `view`, or [`GateError::Locked`] if none. Performs
    /// **no** authentication — pure state check; use [`open`](Self::open) to acquire a grant.
    pub fn guard(&self, view: GatedView) -> Result<ViewGuard, GateError> {
        if self.is_open(view) {
            Ok(ViewGuard { view })
        } else {
            Err(GateError::Locked)
        }
    }

    /// Revoke the grant for a single view (e.g. the user backgrounded that surface).
    pub fn revoke(&mut self, view: GatedView) {
        if self.grants.remove(&view).is_some() {
            tracing::info!(?view, "local-auth gate: grant revoked");
        }
    }

    /// Revoke every grant (e.g. app backgrounded / device locked). The next open re-authenticates.
    pub fn lock(&mut self) {
        if !self.grants.is_empty() {
            self.grants.clear();
            tracing::info!("local-auth gate: all grants revoked (locked)");
        }
    }

    /// The **Recently Deleted** (trash) listing, gated: refuses with [`GateError::Locked`] unless a
    /// live [`GatedView::RecentlyDeleted`] grant exists. Ordering / paging are the driver's
    /// ([`DatabaseDriver::query_trash`]).
    pub fn query_recently_deleted(
        &self,
        db: &DatabaseDriver,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AssetRow>, GatedQueryError> {
        let _guard = self.guard(GatedView::RecentlyDeleted)?;
        db.query_trash(offset, limit)
            .map_err(|e| GatedQueryError::Db(e.to_string()))
    }

    /// The **Hidden** listing, gated: refuses with [`GateError::Locked`] unless a live
    /// [`GatedView::Hidden`] grant exists. The same 5-minute-grace contract as
    /// [`query_recently_deleted`](Self::query_recently_deleted) — one policy, two views —
    /// and the grants are independent, so opening the trash never opens this. Ordering /
    /// paging are the driver's ([`DatabaseDriver::query_hidden`]).
    ///
    /// This is the *only* way hidden assets surface: every default projection
    /// (timeline, album) excludes them (SSoT: design/organization § Hidden Assets).
    pub fn query_hidden(
        &self,
        db: &DatabaseDriver,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AssetRow>, GatedQueryError> {
        let _guard = self.guard(GatedView::Hidden)?;
        db.query_hidden(offset, limit)
            .map_err(|e| GatedQueryError::Db(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
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
        gk2.clock.advance(DEFAULT_GRACE - Duration::from_millis(1));
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

    /// S-D19 — the Hidden view under the *same* gate contract as Recently Deleted. This is
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
}
