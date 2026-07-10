//! Server-level blocklists and per-user blocks (slice `S-C8`).
//!
//! Two distinct mechanisms, deliberately kept apart (SSoT:
//! [Moderation — Blocklists](https://docs/design/moderation/#blocklists)):
//!
//! - **Server-level blocklist.** An admin lists peer servers this server refuses federated
//!   requests from. [`ensure_server_allowed`](Blocklist::ensure_server_allowed) is the guard a
//!   federation pull (and the report intake) calls first: a blocked peer is refused before any
//!   work.
//! - **Per-user block.** A user blocks another user; the block is enforced by the blocker's
//!   home server — the blocked user is removed from albums shared with the blocker and cannot
//!   share new albums with them. A per-user block is **scoped to that user**: it does **not**
//!   propagate as a server-wide federation block, so one user (or a coordinated group) cannot
//!   weaponize blocks to sever an entire peer server from the federation.

use entity::{album, album_share, server_blocklist, user_block};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Set,
};
use tracing::instrument;

use super::ModerationError;

/// Server-level blocklist + per-user block operations.
pub struct Blocklist;

impl Blocklist {
    /// Add a peer server to the server-level blocklist. Idempotent (a re-block updates the
    /// note). After this, the peer's federated requests are refused.
    #[instrument(skip(db), fields(server_id = %server_id))]
    pub async fn block_server<C: ConnectionTrait>(
        db: &C,
        server_id: &str,
        reason: Option<&str>,
    ) -> Result<(), ModerationError> {
        match server_blocklist::Entity::find_by_id(server_id)
            .one(db)
            .await?
        {
            Some(model) => {
                let mut am: server_blocklist::ActiveModel = model.into();
                am.reason = Set(reason.map(str::to_string));
                am.update(db).await?;
            }
            None => {
                server_blocklist::ActiveModel {
                    server_id: Set(server_id.to_string()),
                    reason: Set(reason.map(str::to_string)),
                    blocked_at: Set(entity::time::now_entity()),
                }
                .insert(db)
                .await?;
            }
        }
        tracing::info!(server_id, "peer server added to blocklist");
        Ok(())
    }

    /// Remove a peer server from the blocklist (lift the block).
    #[instrument(skip(db), fields(server_id = %server_id))]
    pub async fn unblock_server<C: ConnectionTrait>(
        db: &C,
        server_id: &str,
    ) -> Result<(), ModerationError> {
        server_blocklist::Entity::delete_by_id(server_id)
            .exec(db)
            .await?;
        tracing::info!(server_id, "peer server removed from blocklist");
        Ok(())
    }

    /// Whether a peer server is on the server-level blocklist.
    #[instrument(skip(db), fields(server_id = %server_id))]
    pub async fn is_server_blocked<C: ConnectionTrait>(
        db: &C,
        server_id: &str,
    ) -> Result<bool, ModerationError> {
        Ok(server_blocklist::Entity::find_by_id(server_id)
            .one(db)
            .await?
            .is_some())
    }

    /// The federation guard: refuse any federated request from a blocklisted peer with
    /// [`ModerationError::ServerBlocked`]. Every federation pull and the report intake calls
    /// this first.
    #[instrument(skip(db), fields(server_id = %server_id))]
    pub async fn ensure_server_allowed<C: ConnectionTrait>(
        db: &C,
        server_id: &str,
    ) -> Result<(), ModerationError> {
        if Self::is_server_blocked(db, server_id).await? {
            tracing::info!(server_id, "federated request refused: peer is blocklisted");
            return Err(ModerationError::ServerBlocked {
                server: server_id.to_string(),
            });
        }
        Ok(())
    }

    /// Place a per-user block (`blocker` blocks `blocked`) and enforce it: record the block and
    /// remove the blocked user from every album owned by the blocker. Returns the number of
    /// album shares revoked.
    ///
    /// This is **scoped to the user**: it writes a `user_blocks` row and revokes album shares,
    /// and never touches [`server_blocklist`](entity::server_blocklist) — a per-user block does
    /// not sever the blocked user's home server. (The MLS `Remove` + AMK epoch bump this rides
    /// at the crypto layer is blocked upstream — see the MLS status note; the server-visible
    /// half, revoking share rows, is enforced here.)
    #[instrument(skip(db), fields(blocker = %blocker, blocked = %blocked))]
    pub async fn block_user<C: ConnectionTrait>(
        db: &C,
        blocker: &str,
        blocked: &str,
    ) -> Result<u64, ModerationError> {
        // Idempotent block ledger row.
        if user_block::Entity::find_by_id((blocker.to_string(), blocked.to_string()))
            .one(db)
            .await?
            .is_none()
        {
            user_block::ActiveModel {
                blocker_id: Set(blocker.to_string()),
                blocked_id: Set(blocked.to_string()),
                created_at: Set(entity::time::now_entity()),
            }
            .insert(db)
            .await?;
        }

        // Remove the blocked user from albums the blocker owns.
        let owned_album_ids: Vec<String> = album::Entity::find()
            .filter(album::Column::OwnerId.eq(blocker))
            .select_only()
            .column(album::Column::Id)
            .into_tuple::<String>()
            .all(db)
            .await?;

        let revoked = if owned_album_ids.is_empty() {
            0
        } else {
            album_share::Entity::delete_many()
                .filter(album_share::Column::UserId.eq(blocked))
                .filter(album_share::Column::AlbumId.is_in(owned_album_ids))
                .exec(db)
                .await?
                .rows_affected
        };

        tracing::info!(
            blocker,
            blocked,
            revoked_shares = revoked,
            "per-user block applied; blocked user removed from blocker's shared albums"
        );
        Ok(revoked)
    }

    /// Whether `blocker` currently blocks `blocked`.
    #[instrument(skip(db), fields(blocker = %blocker, blocked = %blocked))]
    pub async fn is_user_blocked<C: ConnectionTrait>(
        db: &C,
        blocker: &str,
        blocked: &str,
    ) -> Result<bool, ModerationError> {
        Ok(
            user_block::Entity::find_by_id((blocker.to_string(), blocked.to_string()))
                .one(db)
                .await?
                .is_some(),
        )
    }
}
