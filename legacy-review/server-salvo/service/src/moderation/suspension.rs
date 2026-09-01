//! Account suspension (slice `S-C8`).
//!
//! A server admin can suspend a user account on their home server. A suspended account cannot
//! upload — `POST /upload` session creation is refused with a structured `403`
//! [`MODERATION_ACCOUNT_SUSPENDED`](capsule_i18n::error_codes::MODERATION_ACCOUNT_SUSPENDED)
//! (distinct from quota and permission rejections, so the client surfaces the right
//! remediation) — and cannot share new albums or create new links. The user's *data* is
//! untouched: suspension is an access-level action, reversible by default. SSoT:
//! [Moderation — Account Suspension](https://docs/design/moderation/#account-suspension).
//!
//! The persisted flag lives on [`user_quota`](entity::user_quota) (shared with the quota
//! service, which only *reports* it as [`QuotaState::Suspended`](crate::quota::QuotaState));
//! this module owns the moderation semantics — flipping it **and** appending the audit-log
//! record — and the enforcement wired into slice `S-C1`'s create path reads
//! [`is_suspended`](Suspension::is_suspended).

use entity::user_quota;
use jiff::Timestamp;
use sea_orm::{ConnectionTrait, EntityTrait};
use tracing::instrument;

use super::ModerationError;
use super::takedown::{ModerationEventKind, append_event};
use crate::quota;

/// Account-suspension operations.
pub struct Suspension;

impl Suspension {
    /// Suspend `user_id`: set the moderation suspension flag and append a moderation provenance
    /// record to the user's audit log. Enforcement (refusing upload-session creation) reads the
    /// flag via [`is_suspended`](Self::is_suspended).
    #[instrument(skip(db), fields(user_id = %user_id))]
    pub async fn suspend<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<String, ModerationError> {
        quota::Mutation::set_suspended(db, user_id, true).await?;
        let event_id = append_event(
            db,
            user_id,
            None,
            ModerationEventKind::Suspended,
            reason,
            Timestamp::now(),
        )
        .await?;
        tracing::info!(user_id, "account suspended");
        Ok(event_id)
    }

    /// Lift a suspension: clear the flag and record the lift.
    #[instrument(skip(db), fields(user_id = %user_id))]
    pub async fn unsuspend<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<String, ModerationError> {
        quota::Mutation::set_suspended(db, user_id, false).await?;
        let event_id = append_event(
            db,
            user_id,
            None,
            ModerationEventKind::Unsuspended,
            reason,
            Timestamp::now(),
        )
        .await?;
        tracing::info!(user_id, "account suspension lifted");
        Ok(event_id)
    }

    /// Whether `user_id` is currently suspended. The upload-session create path (S-C1) calls
    /// this and refuses a suspended account. A missing quota row means "not suspended".
    #[instrument(skip(db), fields(user_id = %user_id))]
    pub async fn is_suspended<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<bool, ModerationError> {
        Ok(user_quota::Entity::find_by_id(user_id)
            .one(db)
            .await?
            .is_some_and(|row| row.suspended))
    }
}
