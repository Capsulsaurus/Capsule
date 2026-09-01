use entity::{asset, drop_inbox, upload_link};
use jiff::Timestamp;
use nanoid::nanoid;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    QueryFilter, QuerySelect, Set, TransactionTrait,
};

use super::query::Query;
use super::{AdoptInput, DropError, NewLink, StageInput};
use crate::quota::{self, BlobKind, QuotaError, QuotaLimits, WriteClass};
use crate::sync::{self, ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput};

/// The result of adopting a pending drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOutcome {
    /// The blob was atomically promoted from inbox to album asset.
    Promoted {
        /// The new album asset's id.
        asset_id: String,
    },
    /// A retry after a prior successful adoption: the inbox row is already gone and the
    /// already-promoted asset is returned (idempotency, invariant 32 / the idempotency table).
    AlreadyPromoted {
        /// The already-promoted asset's id.
        asset_id: String,
    },
}

/// The result of discarding a pending drop — the content address whose blob may now be GC'd.
#[derive(Debug, Clone)]
pub struct DiscardedDrop {
    /// The discarded drop's content address.
    pub ciphertext_hash: String,
    /// Whether the last quota reference dropped (the blob's bytes are now freed and its file
    /// may be removed from the store).
    pub freed: bool,
}

pub struct Mutation;

impl Mutation {
    /// Register a provisioned upload link (the server half of the Provision step). Returns the
    /// link's revocation handle (`link_id`).
    #[tracing::instrument(skip(db, link), fields(owner_id = %link.owner_id))]
    pub async fn create_link(db: &DatabaseConnection, link: NewLink) -> Result<String, DropError> {
        let link_id = uuid::Uuid::now_v7().to_string();
        upload_link::ActiveModel {
            link_id: Set(link_id.clone()),
            opaque_id: Set(link.opaque_id),
            owner_id: Set(link.owner_id),
            album_hint: Set(link.album_hint),
            protocol_version: Set(link.protocol_version),
            crypto_suite_id: Set(i32::from(link.crypto_suite_id)),
            expires_at: Set(link.expires_at.map(entity::time::ts_to_entity)),
            max_total_bytes: Set(link.max_total_bytes.and_then(|v| i64::try_from(v).ok())),
            max_file_count: Set(link.max_file_count.and_then(|v| i32::try_from(v).ok())),
            max_file_size: Set(link.max_file_size.and_then(|v| i64::try_from(v).ok())),
            single_use: Set(link.single_use),
            passphrase_verifier: Set(link.passphrase_verifier),
            revoked_at: Set(None),
            bytes_used: Set(0),
            files_used: Set(0),
            created_at: Set(entity::time::now_entity()),
        }
        .insert(db)
        .await?;
        tracing::info!(%link_id, "upload link provisioned");
        Ok(link_id)
    }

    /// Revoke an upload link (idempotent — a second revoke is a no-op). The serve path refuses
    /// it within its fail-closed cache window.
    #[tracing::instrument(skip(db))]
    pub async fn revoke_link(
        db: &DatabaseConnection,
        owner_id: &str,
        link_id: &str,
    ) -> Result<bool, DropError> {
        let Some(link) = upload_link::Entity::find_by_id(link_id)
            .filter(upload_link::Column::OwnerId.eq(owner_id))
            .one(db)
            .await?
        else {
            return Ok(false);
        };
        if link.revoked_at.is_none() {
            let mut am: upload_link::ActiveModel = link.into();
            am.revoked_at = Set(Some(entity::time::now_entity()));
            am.update(db).await?;
        }
        Ok(true)
    }

    /// Open a drop-session **reservation** for a live link in one transaction (invariants 26,
    /// 29): re-verify the link is live under a row lock, enforce the cumulative per-link caps,
    /// debit the provisioning **owner's** quota, reserve the drop's original blob in the ledger,
    /// and advance the link's cumulative-cap counters. The per-file size cap (invariant 28) is
    /// checked by the caller before this runs.
    #[tracing::instrument(skip(db, limits), fields(link_id = %link_id, owner_id = %owner_id, size))]
    pub async fn open_drop_reservation(
        db: &DatabaseConnection,
        link_id: &str,
        owner_id: &str,
        ciphertext_hash: &str,
        size: u64,
        limits: &QuotaLimits,
        now: Timestamp,
    ) -> Result<(), DropError> {
        let txn = db.begin().await?;

        let Some(link) = upload_link::Entity::find_by_id(link_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            txn.rollback().await?;
            return Err(DropError::LinkNotFound);
        };
        if !Query::is_live(&link, now) {
            txn.rollback().await?;
            return Err(DropError::LinkNotFound);
        }

        // Cumulative caps (invariant 26).
        let bytes_used = u64::try_from(link.bytes_used).unwrap_or(0);
        if let Some(cap) = link.max_total_bytes.and_then(|v| u64::try_from(v).ok())
            && bytes_used.saturating_add(size) > cap
        {
            txn.rollback().await?;
            return Err(DropError::CapExceeded("max_total_bytes"));
        }
        let files_used = u64::try_from(link.files_used).unwrap_or(0);
        if let Some(cap) = link.max_file_count.and_then(|v| u64::try_from(v).ok())
            && files_used + 1 > cap
        {
            txn.rollback().await?;
            return Err(DropError::CapExceeded("max_file_count"));
        }

        // Owner quota debit (invariant 29): the single hard gate, `upload_user_id = owner_id`.
        if let Err(e) =
            quota::Mutation::check(&txn, owner_id, size, WriteClass::UploadSession, limits).await
        {
            txn.rollback().await?;
            return Err(map_quota_err(e));
        }
        // Reserve the drop's original blob (the debit persists through the pending-inbox window;
        // adoption releases it as the `assets` row takes over the charge).
        if let Err(e) =
            quota::Mutation::reserve(&txn, owner_id, ciphertext_hash, size, BlobKind::Original)
                .await
        {
            txn.rollback().await?;
            return Err(map_quota_err(e));
        }

        // Advance the cumulative-cap counters.
        let mut am: upload_link::ActiveModel = link.into();
        am.bytes_used = Set(i64::try_from(bytes_used.saturating_add(size)).unwrap_or(i64::MAX));
        am.files_used = Set(i32::try_from(files_used + 1).unwrap_or(i32::MAX));
        am.update(&txn).await?;

        txn.commit().await?;
        tracing::info!(link_id, owner_id, size, "drop reservation opened");
        Ok(())
    }

    /// Release a drop reservation (owner quota + link cap counters) for a session that never
    /// finalized — the caller aborts a drop session. Best-effort; the blob (if any) is removed
    /// separately by the caller.
    #[tracing::instrument(skip(db), fields(link_id = %link_id, ciphertext_hash = %ciphertext_hash))]
    pub async fn release_reservation(
        db: &DatabaseConnection,
        link_id: &str,
        ciphertext_hash: &str,
        size: u64,
    ) -> Result<(), DropError> {
        let txn = db.begin().await?;
        quota::Mutation::release(&txn, ciphertext_hash).await?;
        if let Some(link) = upload_link::Entity::find_by_id(link_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        {
            let bytes_used = u64::try_from(link.bytes_used)
                .unwrap_or(0)
                .saturating_sub(size);
            let files_used = u32::try_from(link.files_used)
                .unwrap_or(0)
                .saturating_sub(1);
            let mut am: upload_link::ActiveModel = link.into();
            am.bytes_used = Set(i64::try_from(bytes_used).unwrap_or(0));
            am.files_used = Set(i32::try_from(files_used).unwrap_or(0));
            am.update(&txn).await?;
        }
        txn.commit().await?;
        Ok(())
    }

    /// Stage a finalized drop blob into the owner's inbox (and revoke a single-use link). The
    /// drop is written **only** here — never an album asset row, never on a sync feed.
    #[tracing::instrument(skip(db, input), fields(drop_id = %input.drop_id, owner_id = %input.owner_id))]
    pub async fn stage_drop(db: &DatabaseConnection, input: StageInput) -> Result<(), DropError> {
        let txn = db.begin().await?;
        drop_inbox::ActiveModel {
            drop_id: Set(input.drop_id),
            owner_id: Set(input.owner_id),
            link_id: Set(input.link_id.clone()),
            ciphertext_hash: Set(input.ciphertext_hash),
            size: Set(i64::try_from(input.size).unwrap_or(i64::MAX)),
            content_type: Set(input.content_type),
            suggested_filename: Set(input.suggested_filename),
            descriptor: Set(input.descriptor),
            received_at: Set(entity::time::now_entity()),
        }
        .insert(&txn)
        .await?;

        // A single-use link dies after its first successful drop.
        if input.single_use
            && let Some(link) = upload_link::Entity::find_by_id(&input.link_id)
                .lock_exclusive()
                .one(&txn)
                .await?
            && link.revoked_at.is_none()
        {
            let mut am: upload_link::ActiveModel = link.into();
            am.revoked_at = Set(Some(entity::time::now_entity()));
            am.update(&txn).await?;
        }

        txn.commit().await?;
        Ok(())
    }

    /// Adopt a pending drop into an album in **one transaction** (invariant 32): promote the
    /// inbox blob to an `assets` row, mint the album's `sync_seq` + append the
    /// provenance-bearing feed entry (S-C2's rule), release the original reservation, charge the
    /// new metadata blob, and delete the inbox row. A crash between any two steps rolls the
    /// whole thing back — no half-adopted drop, no orphaned asset, no zombie inbox row.
    #[tracing::instrument(skip(db, input, limits), fields(owner_id = %owner_id, album_id = %input.album_id))]
    pub async fn adopt(
        db: &DatabaseConnection,
        owner_id: &str,
        input: AdoptInput,
        limits: &QuotaLimits,
    ) -> Result<AdoptOutcome, DropError> {
        let txn = db.begin().await?;
        match Self::adopt_in_txn(&txn, owner_id, &input, limits).await {
            Ok(outcome) => {
                txn.commit().await?;
                tracing::info!(?outcome, "drop adopted");
                Ok(outcome)
            }
            Err(e) => {
                let _ = txn.rollback().await;
                Err(e)
            }
        }
    }

    /// The transaction-scoped body of [`adopt`]. Runs no commit of its own so a crash-injection
    /// test can drive it on a transaction and then roll back, asserting all-or-nothing.
    pub async fn adopt_in_txn(
        txn: &DatabaseTransaction,
        owner_id: &str,
        input: &AdoptInput,
        limits: &QuotaLimits,
    ) -> Result<AdoptOutcome, DropError> {
        // Invariant 32: the manifest's ciphertext_hash must reference a drop in the caller's own
        // inbox. Lock the row so two concurrent adopters cannot both promote it.
        let Some(inbox_row) = drop_inbox::Entity::find()
            .filter(drop_inbox::Column::OwnerId.eq(owner_id))
            .filter(drop_inbox::Column::CiphertextHash.eq(&input.ciphertext_hash))
            .lock_exclusive()
            .one(txn)
            .await?
        else {
            // No pending row: either never in the inbox, or a retry after a prior success.
            if let Some(existing) = asset::Entity::find()
                .filter(asset::Column::OwnerId.eq(owner_id))
                .filter(asset::Column::FileHash.eq(&input.ciphertext_hash))
                .filter(asset::Column::Uploaded.eq(true))
                .filter(asset::Column::DeletedAt.is_null())
                .one(txn)
                .await?
            {
                return Ok(AdoptOutcome::AlreadyPromoted {
                    asset_id: existing.id,
                });
            }
            return Err(DropError::NotInInbox);
        };

        // The adoption is a metadata-growth write (only the small metadata + provenance blobs
        // are new quota); refused only when the owner is Grace-expired.
        let metadata_len = input.metadata_blob.len() as u64;
        if let Err(e) = quota::Mutation::check(
            txn,
            owner_id,
            metadata_len,
            WriteClass::MetadataGrowth,
            limits,
        )
        .await
        {
            return Err(map_quota_err(e));
        }

        // Move the original blob's charge from the ledger reservation to the `assets` row: net
        // zero for the owner (same user, same size), so the drop's charge is unchanged across
        // adoption. Charge the new metadata blob.
        quota::Mutation::release(txn, &input.ciphertext_hash).await?;
        quota::Mutation::reserve(
            txn,
            owner_id,
            &input.metadata_hash,
            metadata_len,
            BlobKind::Metadata,
        )
        .await
        .map_err(map_quota_err)?;

        // Promote: write the album asset row (uploaded, external-origin, adopter-owned).
        let size = inbox_row.size;
        let content_type = inbox_row.content_type.clone();
        let asset_type = if content_type.starts_with("video/") {
            asset::AssetType::Video
        } else {
            asset::AssetType::Photo
        };
        let asset_id = nanoid!();
        asset::ActiveModel {
            id: Set(asset_id.clone()),
            owner_id: Set(owner_id.to_string()),
            upload_user_id: Set(owner_id.to_string()),
            album_id: Set(Some(input.album_id.clone())),
            asset_type: Set(asset_type),
            file_size: Set(size),
            file_hash: Set(input.ciphertext_hash.clone()),
            content_type: Set(content_type.clone()),
            uploaded: Set(true),
            uploaded_at: Set(entity::time::now_entity()),
            modified_at: Set(entity::time::now_entity().into()),
            deleted_at: Set(None),
            ..Default::default()
        }
        .insert(txn)
        .await?;

        // Mint the album's `sync_seq` + append the provenance-bearing feed entry (the signed
        // manifest travels as opaque CBOR). This joins the same transaction (S-C2's rule).
        let blobs = FeedBlobManifest {
            original: Some(FeedBlobRef {
                ciphertext_hash: input.ciphertext_hash.clone(),
                role: "original".to_string(),
                format: content_type,
                size: u64::try_from(size).unwrap_or(0),
            }),
            derivatives: Vec::new(),
        };
        sync::Mutation::record_finalization(
            txn,
            FeedEntryInput {
                album_id: input.album_id.clone(),
                protocol_version: input.protocol_version.clone(),
                kind: ChangeKind::Created,
                asset_id: asset_id.clone(),
                manifest_cbor: input.manifest_cbor.clone(),
                metadata_blob: Some(input.metadata_blob.clone()),
                blobs,
                original_held: true,
            },
        )
        .await?;

        // Delete the inbox row: the drop is now an ordinary library asset.
        drop_inbox::Entity::delete_by_id(&inbox_row.drop_id)
            .exec(txn)
            .await?;

        Ok(AdoptOutcome::Promoted { asset_id })
    }

    /// Discard a pending drop: delete the inbox row and release the owner's quota reservation,
    /// in one transaction. Returns the content address (and whether its bytes are now freed) so
    /// the caller can GC the blob file. `Ok(None)` = no such drop in the caller's inbox.
    #[tracing::instrument(skip(db), fields(owner_id = %owner_id, drop_id = %drop_id))]
    pub async fn discard(
        db: &DatabaseConnection,
        owner_id: &str,
        drop_id: &str,
    ) -> Result<Option<DiscardedDrop>, DropError> {
        let txn = db.begin().await?;
        let Some(row) = drop_inbox::Entity::find_by_id(drop_id)
            .filter(drop_inbox::Column::OwnerId.eq(owner_id))
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            txn.rollback().await?;
            return Ok(None);
        };
        let ciphertext_hash = row.ciphertext_hash.clone();
        drop_inbox::Entity::delete_by_id(&row.drop_id)
            .exec(&txn)
            .await?;
        let freed = matches!(
            quota::Mutation::release(&txn, &ciphertext_hash).await?,
            quota::ReleaseOutcome::GarbageCollected { .. }
        );
        txn.commit().await?;
        tracing::info!(owner_id, drop_id, freed, "drop discarded");
        Ok(Some(DiscardedDrop {
            ciphertext_hash,
            freed,
        }))
    }
}

/// Map a quota failure onto the drop error domain.
fn map_quota_err(e: QuotaError) -> DropError {
    match e {
        // PeerBudgetExceeded is not reachable on the local drop path; map it conservatively.
        QuotaError::Exceeded { .. } | QuotaError::PeerBudgetExceeded { .. } => {
            DropError::QuotaExceeded
        }
        QuotaError::GraceLocked { .. } => DropError::GraceLocked,
        QuotaError::Db(db) => DropError::Db(db),
    }
}
