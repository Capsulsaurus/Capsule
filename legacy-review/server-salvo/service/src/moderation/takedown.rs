//! Asset takedown + the user-visible moderation audit log (slice `S-C8`).
//!
//! When a moderation action requires the *home server* to stop serving a specific asset (a
//! legal request, or a CSAM report an admin verified by viewing it in their own album), the
//! asset is marked unservable (`served = false`). Federated peers fetching it then receive
//! `410 Gone` — deliberately distinct from the capability-URL serve paths, which return an
//! indistinguishable `404`: those must not confirm a URL ever existed, while a takedown
//! *intends* to signal removal of content the peer already knows.
//!
//! The underlying blob is **never** deleted — the user owns the data, and a takedown is a
//! serving constraint, not destruction; the user can still restore from their own backup. A
//! takedown is **reversible by default**; a **legal-hold** variant marks the asset indefinitely
//! unservable. Every takedown emits a server-visible [moderation provenance record][rec] the
//! user sees in their audit log — the "no silent operations" rule. SSoT:
//! [Moderation — Takedown](https://docs/design/moderation/#takedown).
//!
//! [rec]: entity::moderation_event

use entity::{asset, moderation_event};
use jiff::Timestamp;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};
use tracing::instrument;
use uuid::Uuid;

use super::ModerationError;

/// The kind of moderation action recorded in a [`moderation_event`](entity::moderation_event).
/// The stored string is the wire/audit-log form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationEventKind {
    /// An asset was taken down (reversible by default).
    Takedown,
    /// A takedown was lifted.
    TakedownLifted,
    /// An asset was placed under legal hold (indefinitely unservable).
    LegalHold,
    /// An account was suspended.
    Suspended,
    /// An account suspension was lifted.
    Unsuspended,
}

impl ModerationEventKind {
    /// The stored string form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ModerationEventKind::Takedown => "takedown",
            ModerationEventKind::TakedownLifted => "takedown_lifted",
            ModerationEventKind::LegalHold => "legal_hold",
            ModerationEventKind::Suspended => "suspended",
            ModerationEventKind::Unsuspended => "unsuspended",
        }
    }
}

/// Append one moderation provenance record to a user's audit log. Shared by takedown and
/// suspension so every moderation action is recorded uniformly. `asset_id` is `None` for
/// account-level events.
#[instrument(skip(db), fields(user_id = %user_id, kind = kind.as_str()))]
pub(super) async fn append_event<C: ConnectionTrait>(
    db: &C,
    user_id: &str,
    asset_id: Option<&str>,
    kind: ModerationEventKind,
    reason: Option<&str>,
    now: Timestamp,
) -> Result<String, ModerationError> {
    let id = Uuid::now_v7().to_string();
    moderation_event::ActiveModel {
        id: Set(id.clone()),
        user_id: Set(user_id.to_string()),
        asset_id: Set(asset_id.map(str::to_string)),
        kind: Set(kind.as_str().to_string()),
        reason: Set(reason.map(str::to_string)),
        created_at: Set(entity::time::ts_to_entity(now)),
    }
    .insert(db)
    .await?;
    tracing::info!(event_id = %id, kind = kind.as_str(), "moderation provenance record appended");
    Ok(id)
}

/// Asset takedown operations.
pub struct Takedown;

impl Takedown {
    /// Take down an asset: mark it unservable (`served = false`) and append a moderation
    /// provenance record to the owning user's audit log. The blob is never touched. `legal_hold`
    /// records the stronger, admin-non-discretionary variant. Returns the audit-log event id.
    #[instrument(skip(db), fields(asset_id = %asset_id, legal_hold))]
    pub async fn take_down<C: ConnectionTrait>(
        db: &C,
        asset_id: &str,
        reason: Option<&str>,
        legal_hold: bool,
    ) -> Result<String, ModerationError> {
        let model = Self::load(db, asset_id).await?;
        let user_id = model.owner_id.clone();
        let mut am: asset::ActiveModel = model.into();
        am.served = Set(false);
        am.update(db).await?;

        let kind = if legal_hold {
            ModerationEventKind::LegalHold
        } else {
            ModerationEventKind::Takedown
        };
        let event_id =
            append_event(db, &user_id, Some(asset_id), kind, reason, Timestamp::now()).await?;
        tracing::info!(asset_id, legal_hold, "asset taken down (served = false)");
        Ok(event_id)
    }

    /// Lift a takedown: mark the asset servable again and record the lift. (A legal hold is
    /// lifted only when the legal obligation ends, not at admin discretion — that policy gate
    /// is the caller's; the mechanism is the same.)
    #[instrument(skip(db), fields(asset_id = %asset_id))]
    pub async fn lift<C: ConnectionTrait>(
        db: &C,
        asset_id: &str,
        reason: Option<&str>,
    ) -> Result<String, ModerationError> {
        let model = Self::load(db, asset_id).await?;
        let user_id = model.owner_id.clone();
        let mut am: asset::ActiveModel = model.into();
        am.served = Set(true);
        am.update(db).await?;

        let event_id = append_event(
            db,
            &user_id,
            Some(asset_id),
            ModerationEventKind::TakedownLifted,
            reason,
            Timestamp::now(),
        )
        .await?;
        tracing::info!(asset_id, "takedown lifted (served = true)");
        Ok(event_id)
    }

    /// Whether an asset is currently servable. The media serve path calls this to decide
    /// between serving bytes and returning `410 Gone`.
    #[instrument(skip(db), fields(asset_id = %asset_id))]
    pub async fn is_served<C: ConnectionTrait>(
        db: &C,
        asset_id: &str,
    ) -> Result<bool, ModerationError> {
        Ok(Self::load(db, asset_id).await?.served)
    }

    async fn load<C: ConnectionTrait>(
        db: &C,
        asset_id: &str,
    ) -> Result<asset::Model, ModerationError> {
        asset::Entity::find_by_id(asset_id)
            .one(db)
            .await?
            .ok_or_else(|| ModerationError::AssetNotFound {
                asset_id: asset_id.to_string(),
            })
    }
}

/// The user-visible moderation audit log query surface.
pub struct AuditLog;

impl AuditLog {
    /// Every moderation provenance record for `user_id`, newest first — the user's audit log.
    #[instrument(skip(db), fields(user_id = %user_id))]
    pub async fn for_user<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<Vec<moderation_event::Model>, ModerationError> {
        Ok(moderation_event::Entity::find()
            .filter(moderation_event::Column::UserId.eq(user_id))
            .order_by_desc(moderation_event::Column::CreatedAt)
            .all(db)
            .await?)
    }
}
