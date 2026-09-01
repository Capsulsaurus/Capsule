//! The append-only custody-receipt log (slice `S-C15`).
//!
//! One row per server-signed [`CustodyReceipt`], inserted **inside** the upload finalization
//! transaction (slice `S-C1`) atomically with the asset's `uploaded` flip — no receipt
//! without durable custody, no custody-marking without a receipt (invariant 33). Rows are
//! **append-only**: a migration-installed trigger rejects every `UPDATE`/`DELETE` at the
//! structural layer, so the server cannot destroy the record of its own liability.
//!
//! `receipt_seq` is strictly monotonic per `server_id` (minted under the counter row lock,
//! like the sync feed's `sync_seq`); `prior_receipt_hash` chains each row to the previous
//! receipt's content hash, so the log is a hash chain the server can enumerate but not
//! silently rewrite. The full signed receipt travels as canonical CBOR in `receipt_cbor`
//! (the served/persisted form); the scalar columns mirror it for indexed lookup.

use chrono::{DateTime, FixedOffset, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "custody_receipts")]
pub struct Model {
    /// This server's canonical origin (part of the per-server chain key).
    #[sea_orm(primary_key, auto_increment = false)]
    pub server_id: String,
    /// Strictly monotonic per server — the receipt log's append-only sequence.
    #[sea_orm(primary_key, auto_increment = false)]
    pub receipt_seq: i64,
    /// SHA-256 of the previous receipt in the log (hex); `None` only for the first receipt.
    #[sea_orm(column_type = "String(StringLen::N(64))", nullable)]
    pub prior_receipt_hash: Option<String>,
    /// This receipt's own content hash (hex) — the link the next receipt chains from.
    #[sea_orm(column_type = "String(StringLen::N(64))", indexed)]
    pub receipt_hash: String,
    /// The upload session that produced custody.
    #[sea_orm(indexed)]
    pub upload_id: String,
    /// The asset this receipt covers.
    #[sea_orm(indexed)]
    pub asset_id: String,
    /// `original | derivative | metadata | provenance`.
    pub blob_role: String,
    /// The server-recomputed ciphertext content address (hex).
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub ciphertext_hash: String,
    /// Ciphertext size in bytes.
    pub size: i64,
    /// SHA-256 of the manifest envelope CBOR (hex), when the session carried one.
    #[sea_orm(column_type = "String(StringLen::N(64))", nullable)]
    pub envelope_hash: Option<String>,
    /// The user that uploaded.
    pub uploaded_by_user: String,
    /// The device that uploaded, when known.
    #[sea_orm(nullable)]
    pub uploaded_by_device: Option<String>,
    /// The attestation key fingerprint that signed (hex) — selects the verification key.
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub server_key_id: String,
    /// The server's trusted clock at the finalization commit.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub received_at: DateTime<FixedOffset>,
    /// The full signed receipt as canonical CBOR (the served, evidentiary form).
    #[sea_orm(column_type = "Blob")]
    pub receipt_cbor: Vec<u8>,
    /// Row creation instant.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
