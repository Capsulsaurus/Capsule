use entity::{drop_inbox, upload_link};
use jiff::Timestamp;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

/// A resolved **live** upload link — the facts the serve path gates a drop-session on before
/// reserving (passphrase, per-file caps, pins). Cumulative caps are re-checked under a row
/// lock in [`super::Mutation::open_drop_reservation`].
#[derive(Debug, Clone)]
pub struct LiveLink {
    /// The link's revocation handle.
    pub link_id: String,
    /// The provisioning owner (whose quota a drop debits).
    pub owner_id: String,
    /// The pinned protocol version.
    pub protocol_version: String,
    /// The pinned crypto suite id.
    pub crypto_suite_id: u16,
    /// Whether the link dies after its first successful drop.
    pub single_use: bool,
    /// Cap: per-file (ciphertext) size (invariant 28).
    pub max_file_size: Option<u64>,
    /// Optional Argon2id abuse-gate verifier (JSON of the S-A6 `PassphraseVerifier`).
    pub passphrase_verifier: Option<serde_json::Value>,
}

/// One pending drop in the provisioning owner's inbox.
#[derive(Debug, Clone)]
pub struct PendingDrop {
    /// The inbox row id.
    pub drop_id: String,
    /// The content address of the staged drop blob.
    pub ciphertext_hash: String,
    /// Ciphertext size in bytes.
    pub size: u64,
    /// The guest-declared content type.
    pub content_type: String,
    /// Guest-supplied, unverified name.
    pub suggested_filename: Option<String>,
    /// Server-attested arrival time (RFC 3339).
    pub received_at: String,
}

pub struct Query;

impl Query {
    /// Resolve the opaque id to a **live** upload link (invariant 26): it exists, is not
    /// revoked, and is not past its `expires_at`. A not-found / revoked / expired link all
    /// return `Ok(None)` indistinguishably; the caller renders a uniform `404`.
    #[tracing::instrument(skip(db))]
    pub async fn live_link_by_opaque<C: ConnectionTrait>(
        db: &C,
        opaque_id: &str,
        now: Timestamp,
    ) -> Result<Option<LiveLink>, DbErr> {
        let Some(link) = upload_link::Entity::find()
            .filter(upload_link::Column::OpaqueId.eq(opaque_id))
            .one(db)
            .await?
        else {
            return Ok(None);
        };
        if !Self::is_live(&link, now) {
            return Ok(None);
        }
        Ok(Some(LiveLink {
            link_id: link.link_id,
            owner_id: link.owner_id,
            protocol_version: link.protocol_version,
            crypto_suite_id: u16::try_from(link.crypto_suite_id).unwrap_or(0),
            single_use: link.single_use,
            max_file_size: link.max_file_size.and_then(|v| u64::try_from(v).ok()),
            passphrase_verifier: link.passphrase_verifier,
        }))
    }

    /// Whether a fetched link row is live at `now` (not revoked, not expired).
    pub(super) fn is_live(link: &upload_link::Model, now: Timestamp) -> bool {
        if link.revoked_at.is_some() {
            return false;
        }
        match link.expires_at {
            Some(exp) => entity::time::entity_to_ts(exp) > now,
            None => true,
        }
    }

    /// The provisioning owner's pending drops, newest first. Drops are visible only to their
    /// own owner and never appear on any album's sync feed.
    #[tracing::instrument(skip(db))]
    pub async fn inbox<C: ConnectionTrait>(
        db: &C,
        owner_id: &str,
    ) -> Result<Vec<PendingDrop>, DbErr> {
        let rows = drop_inbox::Entity::find()
            .filter(drop_inbox::Column::OwnerId.eq(owner_id))
            .order_by_desc(drop_inbox::Column::ReceivedAt)
            .all(db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| PendingDrop {
                drop_id: r.drop_id,
                ciphertext_hash: r.ciphertext_hash,
                size: u64::try_from(r.size).unwrap_or(0),
                content_type: r.content_type,
                suggested_filename: r.suggested_filename,
                received_at: entity::time::entity_to_ts(r.received_at).to_string(),
            })
            .collect())
    }

    /// Fetch one of the caller's own inbox rows by id (owner-scoped), for discard.
    pub async fn find_drop<C: ConnectionTrait>(
        db: &C,
        owner_id: &str,
        drop_id: &str,
    ) -> Result<Option<drop_inbox::Model>, DbErr> {
        drop_inbox::Entity::find_by_id(drop_id)
            .filter(drop_inbox::Column::OwnerId.eq(owner_id))
            .one(db)
            .await
    }
}
