//! Storage-quota accounting and enforcement (slice `S-C6`).
//!
//! Quota is accounted to `upload_user_id` (the authenticated uploader), never to the
//! asset's `owner_id`, so uploading on behalf of another owner keeps storage cost attributed
//! correctly. This module owns the [Quota design doc][doc]'s two responsibilities:
//!
//! 1. **Accounting** ([`Query::used`]). A user's usage is the sum of
//!    - their **originals**, read from the `assets` index: each distinct ciphertext hash is
//!      charged once, to its **first** uploader (global content-addressed dedup — a blob
//!      shared between two uploaders counts against the first only), at full size while the
//!      row is present (trash-retained assets count; a hard-purged row is gone, so its bytes
//!      are released); plus
//!    - their **auxiliary + federated** blobs (metadata / derivative / provenance / a
//!      federated cache), read from the [`quota_ledger`] with the same content-hash dedup and
//!      a refcount that credits bytes back on garbage collection.
//! 2. **Enforcement** ([`Mutation::check`]) at the doc's enforcement points — upload-session
//!    creation (the one hard gate), session cancellation (release, handled by the pending
//!    asset row's deletion), and metadata-growth writes (refused only in the Grace-expired
//!    state).
//!
//! The five [`QuotaState`]s and the write-class rules are the frozen contract; concrete error
//! types and the `GET /quota` shape are implementation detail.
//!
//! [doc]: ../../../capsule-docs/src/content/docs/design/quota.md
//! [`quota_ledger`]: entity::quota_ledger

mod mutation;
mod query;

use capsule_i18n::error_codes;
use jiff::{SignedDuration, Timestamp};
pub use mutation::{ChargeOutcome, Mutation, ReleaseOutcome};
pub use query::Query;
use serde::Serialize;
use thiserror::Error;

/// Sentinel hard limit meaning "no quota" (self-hosted, no billing).
pub const UNLIMITED: u64 = u64::MAX;

/// Default grace window before the Grace-expired state engages: 14 days.
pub const DEFAULT_GRACE_WINDOW: SignedDuration = SignedDuration::from_hours(14 * 24);

/// Default per-`(receiving_user, source_peer)` federated caching budget, as a fraction of the
/// receiver's hard limit (25%).
pub const DEFAULT_PER_PEER_BUDGET_RATIO: f64 = 0.25;

/// Deployment-configurable quota limits. A billing/tier system, where present, only *sets*
/// these; self-hosted deployments run [`unlimited`](QuotaLimits::unlimited).
#[derive(Debug, Clone, Copy)]
pub struct QuotaLimits {
    /// Usage at/above which the UI warns (`SoftWarning`).
    pub soft_limit: u64,
    /// Usage at/above which new upload sessions are refused (`HardExceeded`). [`UNLIMITED`]
    /// disables all hard/grace behaviour.
    pub hard_limit: u64,
    /// How long a user may stay at/above the hard limit before metadata-growth writes are
    /// also refused (`GraceExpired`).
    pub grace_window: SignedDuration,
    /// Per-`(receiving_user, source_peer)` federated caching budget as a fraction of
    /// `hard_limit`.
    pub per_peer_budget_ratio: f64,
}

impl QuotaLimits {
    /// A no-quota configuration (`hard_limit = ∞`) — the self-hosted default.
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            soft_limit: UNLIMITED,
            hard_limit: UNLIMITED,
            grace_window: DEFAULT_GRACE_WINDOW,
            per_peer_budget_ratio: DEFAULT_PER_PEER_BUDGET_RATIO,
        }
    }

    /// Whether this deployment enforces no hard limit.
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.hard_limit == UNLIMITED
    }

    /// The absolute per-peer caching budget in bytes (unlimited under no hard limit).
    #[must_use]
    pub fn per_peer_budget(&self) -> u64 {
        if self.is_unlimited() {
            return UNLIMITED;
        }
        // Saturating, integer: ratio in [0, 1] against the hard limit.
        let ratio = self.per_peer_budget_ratio.clamp(0.0, 1.0);
        (self.hard_limit as f64 * ratio) as u64
    }
}

impl Default for QuotaLimits {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// The quota state of an account. Four states are derived from accounting; `Suspended` is an
/// admin/billing (moderation) flag this service only reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaState {
    /// `used < soft_limit` — all writes succeed normally.
    Ok,
    /// `soft_limit ≤ used < hard_limit` — writes succeed; the UI surfaces a warning.
    SoftWarning,
    /// `used ≥ hard_limit` — new upload sessions refused; metadata edits and every other
    /// write still work.
    HardExceeded,
    /// `used ≥ hard_limit` for longer than `grace_window` — additionally refuses
    /// metadata-growth writes. Reads, deletes, and restore-from-trash still work.
    GraceExpired,
    /// Admin/billing suspension (moderation-owned enforcement; reported here for `GET /quota`).
    Suspended,
}

impl QuotaState {
    /// The stable string form used on the wire and in logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaState::Ok => "ok",
            QuotaState::SoftWarning => "soft_warning",
            QuotaState::HardExceeded => "hard_exceeded",
            QuotaState::GraceExpired => "grace_expired",
            QuotaState::Suspended => "suspended",
        }
    }

    /// Classify a user's state from accounting facts alone. Pure — the SSoT for the state
    /// machine, unit-tested without a database.
    ///
    /// `hard_exceeded_since` is the persisted grace clock (set the first time the user is
    /// observed at/above the hard limit); `suspended` is the moderation flag; `now` is the
    /// comparison instant.
    #[must_use]
    pub fn classify(
        used: u64,
        limits: &QuotaLimits,
        hard_exceeded_since: Option<Timestamp>,
        suspended: bool,
        now: Timestamp,
    ) -> Self {
        if suspended {
            return QuotaState::Suspended;
        }
        if limits.is_unlimited() {
            // No hard limit: at most a soft warning (if a finite soft limit is configured).
            if limits.soft_limit != UNLIMITED && used >= limits.soft_limit {
                return QuotaState::SoftWarning;
            }
            return QuotaState::Ok;
        }
        if used >= limits.hard_limit {
            if let Some(since) = hard_exceeded_since
                && now.duration_since(since) > limits.grace_window
            {
                return QuotaState::GraceExpired;
            }
            return QuotaState::HardExceeded;
        }
        if used >= limits.soft_limit {
            return QuotaState::SoftWarning;
        }
        QuotaState::Ok
    }
}

/// A user's quota snapshot — the `GET /quota` payload and the input to enforcement decisions.
#[derive(Debug, Clone)]
pub struct QuotaStatus {
    /// Bytes currently charged to the user.
    pub used: u64,
    /// The soft-warning threshold.
    pub soft_limit: u64,
    /// The hard-refusal threshold ([`UNLIMITED`] when unenforced).
    pub hard_limit: u64,
    /// The classified state.
    pub state: QuotaState,
}

/// The class of write being checked. The rules differ by state: an upload session is the one
/// hard gate; a metadata-growth write is refused only when Grace-expired; a lifecycle write is
/// always admitted (a user must be able to delete their way back under quota).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteClass {
    /// A `POST /upload` session creation reserving `additional_bytes`.
    UploadSession,
    /// A metadata-growth write (caption/tag edit, new share or upload link) — a small
    /// non-zero blob delta.
    MetadataGrowth,
    /// A `delete` / `trash-restore` / trash-empty provenance write — always admitted.
    Lifecycle,
}

/// The blob classes tracked in the [`quota_ledger`](entity::quota_ledger); originals are
/// accounted from the `assets` index instead, so they are absent here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKind {
    /// A per-asset encrypted metadata blob.
    Metadata,
    /// A thumbnail/preview derivative.
    Derivative,
    /// A per-asset `.provenance.cbor` blob.
    Provenance,
    /// An original blob cached from a federated peer (federated caches are always ledgered).
    Original,
}

impl BlobKind {
    /// The stored string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BlobKind::Metadata => "metadata",
            BlobKind::Derivative => "derivative",
            BlobKind::Provenance => "provenance",
            BlobKind::Original => "original",
        }
    }
}

/// A quota accounting/enforcement failure. Each client-visible variant maps to a stable
/// `error.quota.*` catalog code; the HTTP surface (upload crate) renders status + code.
#[derive(Debug, Error)]
pub enum QuotaError {
    /// Session creation would cross the hard limit.
    #[error(
        "quota exceeded: {used} used + {additional} declared would cross hard limit {hard_limit}"
    )]
    Exceeded {
        /// Bytes already charged.
        used: u64,
        /// Bytes the rejected session declared.
        additional: u64,
        /// The hard limit crossed.
        hard_limit: u64,
    },
    /// A metadata-growth write refused because the account is Grace-expired (read-only).
    #[error("account is grace-expired (read-only): {used} used against hard limit {hard_limit}")]
    GraceLocked {
        /// Bytes already charged.
        used: u64,
        /// The hard limit.
        hard_limit: u64,
    },
    /// A federated cache would cross the per-`(receiving_user, source_peer)` budget.
    #[error("per-peer caching budget exceeded for {peer}: {used} + {additional} > {budget}")]
    PeerBudgetExceeded {
        /// The source peer whose budget is exhausted.
        peer: String,
        /// Bytes already cached from this peer for the receiver.
        used: u64,
        /// Bytes the refused cache would add.
        additional: u64,
        /// The per-peer budget.
        budget: u64,
    },
    /// A database failure.
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
}

impl QuotaError {
    /// The stable `error.quota.*` catalog code, when one applies to this rejection. `None`
    /// for internal (`Db`) faults.
    #[must_use]
    pub fn code(&self) -> Option<&'static str> {
        match self {
            QuotaError::Exceeded { .. } => Some(error_codes::QUOTA_EXCEEDED),
            QuotaError::GraceLocked { .. } => Some(error_codes::QUOTA_GRACE_LOCKED),
            QuotaError::PeerBudgetExceeded { .. } => Some(error_codes::QUOTA_PEER_BUDGET_EXCEEDED),
            QuotaError::Db(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> QuotaLimits {
        QuotaLimits {
            soft_limit: 80,
            hard_limit: 100,
            grace_window: SignedDuration::from_hours(24 * 14),
            per_peer_budget_ratio: 0.25,
        }
    }

    fn now() -> Timestamp {
        "2026-07-10T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn classify_ok_below_soft() {
        assert_eq!(
            QuotaState::classify(10, &limits(), None, false, now()),
            QuotaState::Ok
        );
    }

    #[test]
    fn classify_soft_warning_between_soft_and_hard() {
        assert_eq!(
            QuotaState::classify(80, &limits(), None, false, now()),
            QuotaState::SoftWarning
        );
        assert_eq!(
            QuotaState::classify(99, &limits(), None, false, now()),
            QuotaState::SoftWarning
        );
    }

    #[test]
    fn classify_hard_exceeded_at_limit_without_marker() {
        assert_eq!(
            QuotaState::classify(100, &limits(), None, false, now()),
            QuotaState::HardExceeded
        );
    }

    #[test]
    fn classify_hard_exceeded_within_grace_window() {
        let since = now() - SignedDuration::from_hours(24); // 1 day ago, inside 14-day window
        assert_eq!(
            QuotaState::classify(120, &limits(), Some(since), false, now()),
            QuotaState::HardExceeded
        );
    }

    #[test]
    fn classify_grace_expired_past_window() {
        let since = now() - SignedDuration::from_hours(24 * 15); // 15 days ago
        assert_eq!(
            QuotaState::classify(120, &limits(), Some(since), false, now()),
            QuotaState::GraceExpired
        );
    }

    #[test]
    fn classify_suspended_overrides_all() {
        // Suspended wins even below the soft limit.
        assert_eq!(
            QuotaState::classify(1, &limits(), None, true, now()),
            QuotaState::Suspended
        );
    }

    #[test]
    fn classify_unlimited_never_hard() {
        let l = QuotaLimits::unlimited();
        assert_eq!(
            QuotaState::classify(u64::MAX / 2, &l, None, false, now()),
            QuotaState::Ok
        );
    }

    #[test]
    fn per_peer_budget_is_quarter_of_hard() {
        assert_eq!(limits().per_peer_budget(), 25);
        assert_eq!(QuotaLimits::unlimited().per_peer_budget(), UNLIMITED);
    }
}
