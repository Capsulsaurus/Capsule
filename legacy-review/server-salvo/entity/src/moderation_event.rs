//! The user-visible moderation audit log (slice `S-C8`).
//!
//! The server-visible **moderation provenance record** the user sees in their audit log. Every
//! moderation action that affects a user's account or asset — a takedown, a takedown lift, a
//! legal hold, a suspension, an un-suspension — appends one append-only row here, honoring the
//! "[No silent operations]" rule: a user whose asset stops serving or whose account is
//! suspended is never left to guess why, and the action is itself auditable after the fact.
//!
//! This is a *server-side* record, distinct from the client-signed
//! [provenance chain](https://docs/design/cryptography/provenance/): the E2EE server holds no
//! key and cannot append to the signed chain, so the moderation record it *can* produce is a
//! plaintext-metadata row keyed to the affected user. SSoT:
//! [Moderation — Takedown](https://docs/design/moderation/#takedown).
//!
//! [No silent operations]: https://docs/design/moderation/#what-moderation-cannot-do-structural

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "moderation_events")]
pub struct Model {
    /// UUIDv7 event id (time-ordered).
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// The affected account.
    #[sea_orm(indexed)]
    pub user_id: String,
    /// The affected asset, for a takedown; `None` for account-level events (suspension).
    pub asset_id: Option<String>,
    /// The moderation action: `takedown | takedown_lifted | legal_hold | suspended |
    /// unsuspended`.
    #[sea_orm(column_type = "String(StringLen::N(32))")]
    pub kind: String,
    /// Admin-supplied reason, where policy permits surfacing it.
    pub reason: Option<String>,
    /// When the action was applied.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
