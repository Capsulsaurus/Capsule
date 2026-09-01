//! Federated peer identity, grounded in S-C8's single [`federation_peers`](entity::federation_peer)
//! table (slice `S-E2`).
//!
//! There is deliberately **one** peer-identity store. S-C8 created `federation_peers` to verify
//! federated moderation reports; S-E2 reuses the very same rows so that a pulling peer's identity
//! (its published Ed25519 operational key, and whether it is known at all) is never duplicated
//! into a competing table. First contact — no row — places a peer in the probationary tier the
//! per-peer compartment enforces.

use entity::federation_peer;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DbErr, EntityTrait, Set};
use tracing::instrument;

/// Read/registration surface over the shared `federation_peers` table.
pub struct Peers;

impl Peers {
    /// Register (or rotate) a peer's 32-byte Ed25519 operational public key. Idempotent — a
    /// re-registration overwrites the stored key (key rotation). Shares the table (and therefore
    /// the effect) with S-C8's report-intake registration.
    #[instrument(skip(db, public_key), fields(server_id = %server_id))]
    pub async fn register<C: ConnectionTrait>(
        db: &C,
        server_id: &str,
        public_key: &[u8; 32],
    ) -> Result<(), DbErr> {
        match federation_peer::Entity::find_by_id(server_id)
            .one(db)
            .await?
        {
            Some(model) => {
                let mut am: federation_peer::ActiveModel = model.into();
                am.ed25519_public_key = Set(public_key.to_vec());
                am.update(db).await?;
            }
            None => {
                federation_peer::ActiveModel {
                    server_id: Set(server_id.to_string()),
                    ed25519_public_key: Set(public_key.to_vec()),
                    created_at: Set(entity::time::now_entity()),
                }
                .insert(db)
                .await?;
            }
        }
        tracing::info!(server_id, "registered federated peer operational key");
        Ok(())
    }

    /// Resolve a peer's registered 32-byte Ed25519 operational key, or `None` when the peer is
    /// unknown (no row) or its stored key is not a well-formed 32-byte value. Used to verify a
    /// *remote issuer's* capability (and moderation reports) against the key on file.
    #[instrument(skip(db), fields(server_id = %server_id))]
    pub async fn resolve_key<C: ConnectionTrait>(
        db: &C,
        server_id: &str,
    ) -> Result<Option<[u8; 32]>, DbErr> {
        let Some(peer) = federation_peer::Entity::find_by_id(server_id)
            .one(db)
            .await?
        else {
            return Ok(None);
        };
        Ok(<[u8; 32]>::try_from(peer.ed25519_public_key.as_slice()).ok())
    }

    /// Whether a peer has a registered identity row. `false` = first contact ⇒ the caller places
    /// it in the probationary tier.
    #[instrument(skip(db), fields(server_id = %server_id))]
    pub async fn is_registered<C: ConnectionTrait>(db: &C, server_id: &str) -> Result<bool, DbErr> {
        Ok(federation_peer::Entity::find_by_id(server_id)
            .one(db)
            .await?
            .is_some())
    }
}
