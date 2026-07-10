//! The generic lifecycle-write surface, `POST /albums/{album_id}/ops` (slice `S-C16`).
//!
//! One endpoint, one closed action set, one gate, one transaction shape — the
//! [Authorization — Lifecycle Write Surface](https://docs/design/authorization/#the-lifecycle-write-surface)
//! contract. Every non-upload lifecycle write (`delete`, `metadata-update`,
//! `derivative-add`/`derivative-replace` over already-stored blobs, `trash-restore`) rides
//! this path; `create`/`replace` and any byte-moving derivative ride the upload protocol
//! instead (a write that moves blob bytes is an upload by definition).
//!
//! Before any row is written the server runs the key-free structural battery uniformly for
//! every action:
//!
//! - **Invariant 16** — `action` is in the closed lifecycle set (an unknown value, or an
//!   upload-only `create`/`replace`, is refused `400 error.upload.invalid_action`).
//! - **Invariant 17** — `prior_provenance_hash` equals the asset's current provenance-chain
//!   head (the content hash of its last accepted manifest); a stale or forked position is
//!   `409 error.upload.stale_revival` (peer stale-revival).
//! - **Invariant 18** — `amk_version` never regresses below the album's recorded epoch
//!   (`400 error.upload.amk_regressed`). The server counter is the monotonic backstop; MLS is
//!   the authoritative ceiling (Lane-X, not gated here).
//! - **Invariant 25** — a bundled metadata blob's content hash equals the manifest's committed
//!   `metadata_blob_hash` (`400 error.upload.envelope_mismatch`).
//!
//! An accepted op appends the provenance record (as the opaque canonical-CBOR manifest on the
//! sync feed), mints the per-album `sync_seq`, charges quota for a metadata-growth blob, and
//! records the content-hash replay row — all in **one transaction**, the same finalization
//! rule the [sync feed](https://docs/design/import/download-sync/) relies on. A resubmission
//! of the byte-identical bundle short-circuits to the stored response (at-most-once).

use base64::Engine as _;
use capsule_core::crypto::hash::{Hash32, hash_bytes as hash32_bytes};
use capsule_core::crypto::keys::AmkVersion;
use capsule_core::crypto::provenance::action::Action;
use capsule_core::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use capsule_core::utils::hash::hash_bytes as hash_hex;
use capsule_core::validation::{
    EnvelopeContext, EnvelopeReject, check_manifest_envelope, check_metadata_blob_envelope,
};
use entity::{asset, lifecycle_op_replay, sync_entry};
use jiff::Timestamp;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::Serialize;
use service::quota::{self, BlobKind, WriteClass};
use service::{album as AlbumService, sync as SyncFeed};
use uuid::Uuid;

use crate::config::UploadServerConfig;
use crate::error::UploadError;
use crate::models::requests::{ManifestEnvelope, OpRequest};

/// A device-directory `added_at` floor for a user whose row is missing (the JWT would be
/// invalid anyway; keeps the invariant-7 check from spuriously failing).
const EPOCH_RFC3339: &str = "1970-01-01T00:00:00Z";

/// Upper bound on the metadata blob inlined onto a sync feed entry (mirrors the upload path's
/// `MAX_INLINE_METADATA`); a lifecycle metadata blob is small by design.
const MAX_INLINE_METADATA: usize = 1024 * 1024;

/// The JSON response body a lifecycle write returns and remembers for byte-identical replay.
#[derive(Debug, Serialize)]
pub(crate) struct OpResponse {
    /// The asset the op chained onto (the manifest's `file_id`).
    pub asset_id: String,
    /// The per-album `sync_seq` minted for this op (the feed position).
    pub sync_seq: i64,
    /// The applied lifecycle action (wire string).
    pub action: String,
}

/// The result of a lifecycle write: the HTTP status and the byte-identical body, plus whether
/// it was a fresh application or a replayed prior response (for logging).
pub(crate) struct OpResult {
    /// `200` on success.
    pub status: u16,
    /// The JSON response body (byte-identical across replays).
    pub body: Vec<u8>,
    /// Whether this was a replay of an already-accepted bundle.
    pub replayed: bool,
}

/// The lifecycle-write service — the transaction-shaped body behind `POST /albums/{id}/ops`.
#[derive(Clone)]
pub(crate) struct OpService {
    config: UploadServerConfig,
    conn: DatabaseConnection,
}

/// Everything the pure gate needs, parsed once from the request ahead of any DB read.
struct ParsedOp {
    core: ManifestCore,
    action: Action,
    manifest_cbor: Vec<u8>,
    metadata_blob: Option<Vec<u8>>,
    op_hash: String,
    asset_id: String,
    protocol_version: String,
    action_wire: String,
}

impl OpService {
    pub(crate) fn new(config: UploadServerConfig, conn: DatabaseConnection) -> Self {
        Self { config, conn }
    }

    /// Apply one lifecycle write. `album_id` is the path segment; `user_id` the authenticated
    /// caller. Runs the full invariant battery ahead of any write, then appends provenance +
    /// mints `sync_seq` + charges quota + records the replay row in one transaction.
    #[tracing::instrument(skip(self, req), fields(album_id = %album_id, user_id = %user_id))]
    pub(crate) async fn apply(
        &self,
        album_id: &str,
        user_id: &str,
        req: &OpRequest,
    ) -> Result<OpResult, UploadError> {
        // ── Pure checks (invariant 16 + envelope consistency + blob presence), before any DB. ──
        let parsed = self.parse_and_check(album_id, req)?;

        // Invariant 6 (DB half): the album exists and the caller holds write capability on it.
        match AlbumService::Query::get_album_access(&self.conn, user_id, album_id).await {
            Ok(access)
                if access
                    .as_ref()
                    .is_some_and(capsule_core::models::album::AlbumAccess::is_write) => {}
            Ok(_) => return Err(UploadError::AlbumAccessDenied),
            Err(e) => {
                tracing::warn!("album access lookup failed: {}", e);
                return Err(UploadError::AlbumAccessDenied);
            }
        }

        // The invariant-7 floor (device authorization): the caller's account-creation time,
        // standing in for the device directory's `added_at` until that table lands.
        let device_added_at = self.uploader_added_at(user_id).await;
        let server_clock = Timestamp::now().to_string();

        // ── One transaction: replay check → stateful invariants 17/18/25 → apply → mint → record. ──
        let txn = self.conn.begin().await?;
        let outcome = self
            .apply_in_txn(
                &txn,
                album_id,
                user_id,
                &parsed,
                &device_added_at,
                &server_clock,
            )
            .await;
        match outcome {
            Ok(result) => {
                txn.commit().await?;
                tracing::info!(
                    asset_id = %parsed.asset_id,
                    action = %parsed.action_wire,
                    replayed = result.replayed,
                    "lifecycle write applied"
                );
                Ok(result)
            }
            Err(e) => {
                let _ = txn.rollback().await;
                tracing::info!(
                    asset_id = %parsed.asset_id,
                    action = %parsed.action_wire,
                    code = ?e.code(),
                    "lifecycle write rejected; nothing written"
                );
                Err(e)
            }
        }
    }

    /// The transaction-scoped body of [`apply`]. Reads the asset's chain head + the album's
    /// recorded epoch under the transaction, runs invariants 17/18/25, applies the state
    /// change, mints the feed entry, charges quota, and records the replay row — all or nothing.
    async fn apply_in_txn(
        &self,
        txn: &DatabaseTransaction,
        album_id: &str,
        user_id: &str,
        parsed: &ParsedOp,
        device_added_at: &str,
        server_clock: &str,
    ) -> Result<OpResult, UploadError> {
        // Content-hash replay idempotency: a byte-identical resubmission short-circuits to the
        // stored response (at-most-once, the lifecycle analogue of the chunk-replay tuple).
        if let Some(prior) = lifecycle_op_replay::Entity::find_by_id(&parsed.op_hash)
            .one(txn)
            .await?
        {
            return Ok(OpResult {
                status: u16::try_from(prior.status_code).unwrap_or(200),
                body: prior.response_body,
                replayed: true,
            });
        }

        // Serialize per-asset ops: lock the asset row (when present) so two concurrent writes
        // cannot both pass the chain-head check and double-apply. A best-effort lock — the
        // authoritative chain head is the feed, read next.
        let asset_row = asset::Entity::find_by_id(&parsed.asset_id)
            .lock_exclusive()
            .one(txn)
            .await?;

        // Stored chain head (invariant 17): the content hash of the asset's last accepted
        // manifest, derived from the head of its feed projection (the server holds no full
        // signed manifest, only the canonical-CBOR envelope it re-serialized).
        let head_entry = latest_entry_for_asset(txn, &parsed.asset_id).await?;
        let stored_chain_head = head_entry.as_ref().map(|e| hash32_bytes(&e.manifest_cbor));

        // The album's recorded epoch (invariant 18) and pin (invariant 6): from the head of the
        // album's feed. Monotonic acceptance keeps the head carrying the album's highest epoch.
        let album_head = latest_entry_for_album(txn, album_id).await?;
        let stored_amk = album_head
            .as_ref()
            .and_then(|e| decode_envelope(&e.manifest_cbor).map(|env| env.amk_version));
        let album_pin = album_head
            .as_ref()
            .map_or(parsed.protocol_version.as_str(), |e| {
                e.protocol_version.as_str()
            });

        // The key-free structural battery (invariants 2, 6, 7, 8, 17, 18).
        let ctx = EnvelopeContext {
            album_pin,
            device_added_at,
            server_clock,
            drift_days: self.config.timestamp_drift_days,
            stored_chain_head,
            stored_amk_version: stored_amk,
        };
        map_envelope_reject(check_manifest_envelope(&parsed.core, &ctx))?;

        // Invariant 25: a bundled metadata blob's content hash equals the committed
        // `metadata_blob_hash`. Runs only where the action carries a blob (`metadata-update`).
        if let Some(blob) = &parsed.metadata_blob {
            check_metadata_blob_envelope(&parsed.core, blob)
                .map_err(|_| UploadError::EnvelopeMismatch("metadata_blob_hash"))?;
        }

        // ── All checks passed: apply the state change, mint the feed entry, charge quota. ──
        let kind = change_kind(parsed.action);

        // Server-side soft-delete state: carry out (never authorize) the delete/restore on the
        // asset row when it is present under this id. The feed provenance record is the
        // authoritative audit trail regardless.
        if let Some(row) = asset_row {
            match parsed.action {
                Action::Delete => {
                    set_deleted_at(txn, row, Some(server_clock)).await?;
                }
                Action::TrashRestore => {
                    set_deleted_at(txn, row, None).await?;
                }
                _ => {}
            }
        }

        // Quota: a metadata-growth blob is refused only when Grace-expired; a keyless lifecycle
        // op (delete/restore/derivative-ref) is always admitted.
        let limits = self.config.quota_limits();
        if let (Some(blob), Some(committed)) =
            (&parsed.metadata_blob, parsed.core.metadata_blob_hash)
        {
            let blob_len = blob.len() as u64;
            quota::Mutation::check(txn, user_id, blob_len, WriteClass::MetadataGrowth, &limits)
                .await
                .map_err(map_quota_err)?;
            quota::Mutation::reserve(
                txn,
                user_id,
                &committed.to_hex(),
                blob_len,
                BlobKind::Metadata,
            )
            .await
            .map_err(map_quota_err)?;
        } else {
            quota::Mutation::check(txn, user_id, 0, WriteClass::Lifecycle, &limits)
                .await
                .map_err(map_quota_err)?;
        }

        // Carry forward the completeness fact; a lifecycle op does not change what originals
        // the server holds (delete tombstones logically, not the byte purge — that is GC).
        let original_held = head_entry.as_ref().is_some_and(|e| e.original_held);

        // Provenance append + per-album `sync_seq` mint (the sync feed's finalization rule).
        let inline_metadata = parsed
            .metadata_blob
            .as_ref()
            .filter(|b| b.len() <= MAX_INLINE_METADATA)
            .cloned();
        let sync_seq = SyncFeed::Mutation::record_finalization(
            txn,
            SyncFeed::FeedEntryInput {
                album_id: album_id.to_string(),
                protocol_version: parsed.protocol_version.clone(),
                kind,
                asset_id: parsed.asset_id.clone(),
                manifest_cbor: parsed.manifest_cbor.clone(),
                metadata_blob: inline_metadata,
                blobs: SyncFeed::FeedBlobManifest::default(),
                original_held,
            },
        )
        .await?;

        // The byte-identical response, recorded in the replay store inside this transaction so
        // the op is applied at most once even under a lost-ACK retry.
        let body = serde_json::to_vec(&OpResponse {
            asset_id: parsed.asset_id.clone(),
            sync_seq,
            action: parsed.action_wire.clone(),
        })?;
        let status: u16 = 200;

        // `ON CONFLICT DO NOTHING` so a concurrent identical bundle (racing past the top-level
        // replay read) does not poison the transaction: exactly one row wins, and a loser
        // re-reads the winner's stored response below.
        let inserted = self
            .insert_replay_row(txn, album_id, parsed, kind.as_i16(), status, &body)
            .await?;
        if inserted {
            Ok(OpResult {
                status,
                body,
                replayed: false,
            })
        } else if let Some(prior) = lifecycle_op_replay::Entity::find_by_id(&parsed.op_hash)
            .one(txn)
            .await?
        {
            Ok(OpResult {
                status: u16::try_from(prior.status_code).unwrap_or(200),
                body: prior.response_body,
                replayed: true,
            })
        } else {
            Err(UploadError::Unknown("replay insert conflict".to_string()))
        }
    }

    /// Insert the replay row idempotently (`ON CONFLICT (op_hash) DO NOTHING`). Returns whether
    /// this call inserted the row (`false` = a concurrent identical op already did).
    async fn insert_replay_row(
        &self,
        txn: &DatabaseTransaction,
        album_id: &str,
        parsed: &ParsedOp,
        action_kind: i16,
        status: u16,
        body: &[u8],
    ) -> Result<bool, UploadError> {
        let stmt = Statement::from_sql_and_values(
            txn.get_database_backend(),
            r"INSERT INTO lifecycle_op_replay
                (op_hash, album_id, asset_id, action, status_code, response_body, created_at)
              VALUES ($1, $2, $3, $4, $5, $6, now())
              ON CONFLICT (op_hash) DO NOTHING",
            [
                parsed.op_hash.clone().into(),
                album_id.to_string().into(),
                parsed.asset_id.clone().into(),
                action_kind.into(),
                i32::from(status).into(),
                body.to_vec().into(),
            ],
        );
        let result = txn.execute(stmt).await.map_err(UploadError::DbError)?;
        Ok(result.rows_affected() == 1)
    }

    /// Parse the bundle and run the pure checks that need no DB state: the path↔envelope
    /// consistency, invariant 16 (closed lifecycle action), and the blob presence-by-action
    /// rule. Returns the reconstructed [`ManifestCore`] and the derived op content hash.
    fn parse_and_check(&self, album_id: &str, req: &OpRequest) -> Result<ParsedOp, UploadError> {
        let env = &req.manifest_envelope;

        // The path album MUST match the signed envelope's album (envelope consistency).
        if env.album_id.as_deref() != Some(album_id) {
            return Err(UploadError::EnvelopeMismatch("album_id"));
        }

        // Invariant 16: the action is in the closed enum...
        let action: Action = parse_wire_enum(&env.action)
            .ok_or_else(|| UploadError::ActionNotAllowed(env.action.clone()))?;
        // ...and is a non-upload lifecycle action. `create`/`replace` (and any byte-moving
        // derivative) ride the upload protocol, never this surface.
        if matches!(action, Action::Create | Action::Replace) {
            return Err(UploadError::ActionNotAllowed(env.action.clone()));
        }

        // The metadata blob is present exactly when the action binds one (`metadata-update`).
        let metadata_blob = match &req.metadata_blob {
            Some(b64) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|_| {
                        UploadError::InvalidUpload("metadata_blob is not base64".into())
                    })?;
                Some(bytes)
            }
            None => None,
        };
        if metadata_blob.is_some() != action.binds_metadata_blob() {
            return Err(UploadError::EnvelopeMismatch("metadata_blob"));
        }

        let core = build_op_core(env, action)?;
        let manifest_cbor = capsule_core::cbor::to_canonical_vec(env)
            .map_err(|e| UploadError::ProcessingError(format!("manifest cbor: {e}")))?;

        // The op bundle's content address: the canonical manifest ‖ the metadata blob. Two
        // submissions are "the same op" iff every signed byte and the blob match.
        let mut bundle = manifest_cbor.clone();
        if let Some(blob) = &metadata_blob {
            bundle.extend_from_slice(blob);
        }
        let op_hash = hash_hex(&bundle);

        Ok(ParsedOp {
            core,
            action,
            manifest_cbor,
            metadata_blob,
            op_hash,
            asset_id: env.file_id.clone(),
            protocol_version: env.protocol_version.clone(),
            action_wire: env.action.clone(),
        })
    }

    /// The caller's device-authorization floor: the account-creation time (invariant 7).
    async fn uploader_added_at(&self, user_id: &str) -> String {
        entity::user::Entity::find_by_id(user_id)
            .one(&self.conn)
            .await
            .ok()
            .flatten()
            .map_or_else(|| EPOCH_RFC3339.to_string(), |u| u.created_at.to_rfc3339())
    }
}

/// The most recent feed entry for one asset (its provenance-chain head projection).
async fn latest_entry_for_asset(
    txn: &DatabaseTransaction,
    asset_id: &str,
) -> Result<Option<sync_entry::Model>, DbErr> {
    sync_entry::Entity::find()
        .filter(sync_entry::Column::AssetId.eq(asset_id))
        .order_by_desc(sync_entry::Column::FeedSeq)
        .one(txn)
        .await
}

/// The most recent feed entry for one album (its recorded epoch + pin).
async fn latest_entry_for_album(
    txn: &DatabaseTransaction,
    album_id: &str,
) -> Result<Option<sync_entry::Model>, DbErr> {
    sync_entry::Entity::find()
        .filter(sync_entry::Column::AlbumId.eq(album_id))
        .order_by_desc(sync_entry::Column::FeedSeq)
        .one(txn)
        .await
}

/// Set (or clear) an asset row's `deleted_at`. The server *carries out* the soft-delete; it
/// never authorizes it — the write-tier signature already did (verified client-side).
async fn set_deleted_at(
    txn: &DatabaseTransaction,
    row: asset::Model,
    at: Option<&str>,
) -> Result<(), UploadError> {
    let mut active: asset::ActiveModel = row.into();
    let value = match at {
        Some(clock) => {
            let ts: Timestamp = clock
                .parse()
                .map_err(|_| UploadError::ProcessingError("server clock unparseable".into()))?;
            Some(entity::time::ts_to_entity(ts))
        }
        None => None,
    };
    active.deleted_at = Set(value);
    active.modified_at = Set(entity::time::now_entity().into());
    active
        .update(txn)
        .await
        .map_err(UploadError::DbError)
        .map(|_| ())
}

/// Map an accepted lifecycle action to the sync feed's `ChangeKind`.
fn change_kind(action: Action) -> SyncFeed::ChangeKind {
    match action {
        // A soft-delete tombstones the asset on the feed.
        Action::Delete => SyncFeed::ChangeKind::Deleted,
        // A restore returns the asset to the live set (it re-appears for clients).
        Action::TrashRestore => SyncFeed::ChangeKind::Created,
        // Metadata + derivative edits advance the asset.
        _ => SyncFeed::ChangeKind::MetadataUpdated,
    }
}

/// Reconstruct a [`ManifestCore`] from the wire envelope for the key-free battery. Mirrors the
/// upload path's `build_manifest_core`, but additionally parses `metadata_blob_hash` (the
/// value half of invariant 25) and lets the caller supply the already-parsed `action`.
fn build_op_core(env: &ManifestEnvelope, action: Action) -> Result<ManifestCore, UploadError> {
    let key_mode: KeyMode =
        parse_wire_enum(&env.key_mode).ok_or(UploadError::EnvelopeMismatch("key_mode"))?;
    let prior_provenance_hash = match &env.prior_provenance_hash {
        Some(h) => Some(
            Hash32::from_hex(h)
                .map_err(|_| UploadError::EnvelopeMismatch("prior_provenance_hash"))?,
        ),
        None => None,
    };
    let metadata_blob_hash = match &env.metadata_blob_hash {
        Some(h) => Some(
            Hash32::from_hex(h).map_err(|_| UploadError::EnvelopeMismatch("metadata_blob_hash"))?,
        ),
        None => None,
    };

    Ok(ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: env.crypto_suite_id,
        protocol_version: env.protocol_version.clone(),
        file_id: Uuid::nil(),
        album_id: Uuid::nil(),
        amk_version: AmkVersion(env.amk_version),
        ciphertext_hash: Hash32([0u8; 32]),
        plaintext_size: env.plaintext_size,
        chunk_size: env.chunk_size,
        nonce_prefix: [0u8; 7],
        key_mode,
        wrapped_file_key: None,
        metadata_blob_hash,
        created_by_user: Uuid::nil(),
        created_by_device: Uuid::nil(),
        client_version: env.client_version.clone(),
        timestamp: env.timestamp.clone(),
        action,
        prior_provenance_hash,
        retention_until: env.retention_until.clone(),
    })
}

/// Decode a stored feed entry's opaque manifest CBOR back into the server's envelope
/// projection (the server's own re-serialization, so this round-trips).
fn decode_envelope(manifest_cbor: &[u8]) -> Option<ManifestEnvelope> {
    capsule_core::cbor::from_slice::<ManifestEnvelope>(manifest_cbor).ok()
}

/// Map the key-free envelope battery's rejection onto the lifecycle-write error taxonomy.
/// Invariants 17 and 18 get their own surface codes here (unlike the create path, which folds
/// a stale/regressed create envelope into `envelope_mismatch`).
fn map_envelope_reject(result: Result<(), EnvelopeReject>) -> Result<(), UploadError> {
    match result {
        Ok(()) => Ok(()),
        Err(EnvelopeReject::UnknownSuite) => Err(UploadError::UnknownCryptoSuite),
        Err(EnvelopeReject::AlbumPinMismatch) => Err(UploadError::AlbumAccessDenied),
        Err(EnvelopeReject::DeviceAddedAfter) => Err(UploadError::DeviceNotAuthorized),
        Err(EnvelopeReject::TimestampUnsane) => Err(UploadError::TimestampOutOfRange),
        // Invariant 17: a stale or forked chain position — stale-revival (409).
        Err(EnvelopeReject::StaleChain) => Err(UploadError::StaleRevival),
        // Invariant 18: the album epoch regressed (400).
        Err(EnvelopeReject::AmkRegressed) => Err(UploadError::AmkRegressed),
        Err(EnvelopeReject::MetadataBlobHashMismatch) => {
            Err(UploadError::EnvelopeMismatch("metadata_blob_hash"))
        }
    }
}

/// Map a quota rejection onto the lifecycle-write taxonomy.
fn map_quota_err(err: quota::QuotaError) -> UploadError {
    match err {
        quota::QuotaError::GraceLocked { .. } => UploadError::QuotaGraceLocked,
        quota::QuotaError::Exceeded { .. } => UploadError::QuotaExceeded,
        quota::QuotaError::Db(db) => UploadError::DbError(db),
        // Not reachable for a metadata-growth/lifecycle check, but mapped for totality.
        other @ quota::QuotaError::PeerBudgetExceeded { .. } => {
            UploadError::Unknown(other.to_string())
        }
    }
}

/// Parse a bare wire enum value (e.g. `"delete"`, `"derived"`) into its serde type.
fn parse_wire_enum<T: serde::de::DeserializeOwned>(value: &str) -> Option<T> {
    serde_json::from_str::<T>(&format!("\"{value}\"")).ok()
}
