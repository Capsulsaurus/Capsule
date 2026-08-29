//! The SR1 fresh-local-auth gate, projected across the UniFFI boundary.
//!
//! Policy — which views are gated, the per-view grace clock, the refuse-without-grant
//! query surface — is owned by [`capsule_core::library::GateKeeper`] and is *not*
//! reimplemented here. This module is the boundary projection only: a uniffi mirror of
//! [`capsule_core::library::GatedView`] / [`capsule_core::library::LocalAuthError`], and a
//! foreign-trait seam ([`LocalAuthGate`]) the Swift/Kotlin side implements with
//! `LAContext` / `BiometricPrompt`.
//!
//! **Why a mirror rather than a re-export.** `capsule-core`'s own uniffi surface lives
//! behind its `ffi` feature in the `capsule_core` namespace, which by the `S-F1` invariant
//! never shares a binary with `capsule_core_ffi` (see the crate docs). This crate therefore
//! depends on `capsule-core` with `ffi` **off** and re-declares the two boundary types in
//! its own namespace, converting at the edge. The policy still has exactly one owner.
//!
//! Scope, stated honestly (SR1): this gate is view-time UX protection against a
//! borrowed-unlocked-phone snoop. It is **not** a cryptographic boundary — the same bytes
//! are reachable through the filesystem by anyone who defeats the platform sandbox (SR2).

use std::sync::Arc;

use capsule_core::library as core_gate;

/// A view that requires fresh local authentication before it opens (SR1). Grants are
/// **per-view**: authenticating for one never opens the other.
///
/// The uniffi mirror of [`capsule_core::library::GatedView`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum GatedView {
    /// The trash / "Recently Deleted" listing of soft-deleted assets.
    RecentlyDeleted,
    /// The user-hidden set (assets whose sidecar `hidden` register is set).
    Hidden,
}

impl From<GatedView> for core_gate::GatedView {
    fn from(v: GatedView) -> Self {
        match v {
            GatedView::RecentlyDeleted => Self::RecentlyDeleted,
            GatedView::Hidden => Self::Hidden,
        }
    }
}

impl From<core_gate::GatedView> for GatedView {
    fn from(v: core_gate::GatedView) -> Self {
        match v {
            core_gate::GatedView::RecentlyDeleted => Self::RecentlyDeleted,
            core_gate::GatedView::Hidden => Self::Hidden,
        }
    }
}

/// Failure surfaced by a [`LocalAuthGate`] implementation — the uniffi mirror of
/// [`capsule_core::library::LocalAuthError`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
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

impl From<core_gate::LocalAuthError> for LocalAuthError {
    fn from(e: core_gate::LocalAuthError) -> Self {
        match e {
            core_gate::LocalAuthError::Cancelled => Self::Cancelled,
            core_gate::LocalAuthError::Unavailable => Self::Unavailable,
            core_gate::LocalAuthError::Failed => Self::Failed,
        }
    }
}

impl From<LocalAuthError> for core_gate::LocalAuthError {
    fn from(e: LocalAuthError) -> Self {
        match e {
            LocalAuthError::Cancelled => Self::Cancelled,
            LocalAuthError::Unavailable => Self::Unavailable,
            LocalAuthError::Failed => Self::Failed,
        }
    }
}

/// The per-platform local-authentication seam, implemented by native code (Swift/Kotlin)
/// over the uniffi foreign-trait boundary. Rust calls *into* it to perform the fresh-auth
/// challenge; the core never sees the biometric or the credential.
///
/// The **biometric → credential fallback** is entirely the implementation's concern: an
/// adapter tries the enrolled biometric first (Face ID / Touch ID / BiometricPrompt) and
/// falls back to the device or account credential, surfacing only the *outcome* here. Any
/// [`Ok`] counts as a successful fresh auth regardless of which method produced it.
#[uniffi::export(with_foreign)]
pub trait LocalAuthGate: Send + Sync {
    /// Perform a fresh local-authentication challenge for `view`.
    fn authenticate(&self, view: GatedView) -> Result<(), LocalAuthError>;
}

/// Adapts a foreign [`LocalAuthGate`] to the plain-Rust trait `GateKeeper` drives.
pub(crate) struct ForeignAuthGate(pub(crate) Arc<dyn LocalAuthGate>);

impl core_gate::LocalAuthGate for ForeignAuthGate {
    fn authenticate(&self, view: core_gate::GatedView) -> Result<(), core_gate::LocalAuthError> {
        self.0.authenticate(view.into()).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_view_round_trips_through_the_core_type() {
        for v in [GatedView::RecentlyDeleted, GatedView::Hidden] {
            assert_eq!(GatedView::from(core_gate::GatedView::from(v)), v);
        }
    }

    #[test]
    fn local_auth_error_round_trips_through_the_core_type() {
        for e in [
            LocalAuthError::Cancelled,
            LocalAuthError::Unavailable,
            LocalAuthError::Failed,
        ] {
            assert_eq!(
                LocalAuthError::from(core_gate::LocalAuthError::from(e.clone())),
                e
            );
        }
    }
}
