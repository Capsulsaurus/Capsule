//! Fresh-local-auth gates for the sensitive local-gallery views (SSoT: [Local Gallery — SR1]).
//!
//! Opening the **Recently Deleted** (trash) or **Hidden** view requires *fresh local
//! authentication* — biometric where enrolled (Face ID / Touch ID / BiometricPrompt), else the
//! device or account credential. This module owns the **policy**: which views are gated, the
//! per-view grace clock (default 5 minutes), and the refuse-without-grant query surface. It does
//! **not** own the authentication itself — biometrics and the credential fallback are a platform
//! concern, reached through the [`LocalAuthGate`] seam. This mirrors the
//! [`Signer`](capsule_core::crypto::keys::Signer) /
//! [`HardwareSigner`](capsule_core::crypto::keys::HardwareSigner) discipline: the trait lives in core,
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
pub enum GatedView {
    /// The trash / "Recently Deleted" listing of soft-deleted assets.
    RecentlyDeleted,
    /// The user-hidden set (assets whose sidecar `hidden` register is set).
    Hidden,
}

/// The per-platform local-authentication seam, implemented by native code (Swift/Kotlin) behind the
/// `capsule_core_ffi` namespace's `LocalAuthGate` foreign-trait mirror; a plain Rust trait here.
/// Rust calls *into* it to perform the fresh-auth challenge; core never sees the biometric or the
/// credential.
///
/// The **biometric → credential fallback** is entirely the implementation's concern: an adapter
/// tries the enrolled biometric first (Face ID / Touch ID / BiometricPrompt) and falls back to the
/// device or account credential, surfacing only the *outcome* through this trait. Core treats any
/// [`Ok`] as a successful fresh auth regardless of which method produced it.
pub trait LocalAuthGate: Send + Sync {
    /// Perform a fresh local-authentication challenge for `view`. Returns [`Ok`] on success (by
    /// any enrolled method), or a [`LocalAuthError`] on denial, cancellation, or unavailability.
    fn authenticate(&self, view: GatedView) -> Result<(), LocalAuthError>;
}

/// Failure surfaced by a [`LocalAuthGate`] backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
mod tests;
