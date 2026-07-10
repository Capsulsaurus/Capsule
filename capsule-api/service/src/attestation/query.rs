//! Read access to the append-only custody-receipt log.
//!
//! Backs `GET /upload/{id}/receipt` (the session-window fetch, paired with lost-ACK
//! recovery) and `GET /assets/{asset_id}/receipts` (the durable, uploader-or-owner fetch).
//! Receipts are permanent objects, exempt from session GC.

use ::entity::custody_receipt;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

/// Read side of the custody-receipt log.
pub struct Query;

impl Query {
    /// The receipt for one upload session, if it has been issued (i.e. the session reached
    /// `Completed` and the finalization transaction committed).
    pub async fn receipt_by_upload<C: ConnectionTrait>(
        db: &C,
        upload_id: &str,
    ) -> Result<Option<custody_receipt::Model>, DbErr> {
        custody_receipt::Entity::find()
            .filter(custody_receipt::Column::UploadId.eq(upload_id))
            .order_by_asc(custody_receipt::Column::ReceiptSeq)
            .one(db)
            .await
    }

    /// Every receipt covering an asset, in chain order (`receipt_seq` ascending).
    pub async fn receipts_by_asset<C: ConnectionTrait>(
        db: &C,
        asset_id: &str,
    ) -> Result<Vec<custody_receipt::Model>, DbErr> {
        custody_receipt::Entity::find()
            .filter(custody_receipt::Column::AssetId.eq(asset_id))
            .order_by_asc(custody_receipt::Column::ReceiptSeq)
            .all(db)
            .await
    }
}
