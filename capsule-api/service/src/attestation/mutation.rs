//! Issuing a [`CustodyReceipt`](super::CustodyReceipt) into the append-only log.
//!
//! [`Mutation::issue_receipt`] MUST run **inside** the upload finalization transaction
//! (slice `S-C1`): it mints the next per-server `receipt_seq` under the counter row lock,
//! reads the prior receipt's content hash to chain from, signs the receipt with the server
//! attestation key, and inserts the row — all atomically with the asset's `uploaded` flip.
//! Roll the transaction back and neither the receipt nor the sequence advance (issuance
//! atomicity, invariant 33).

use ::entity::{custody_receipt, time};
use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Set, Statement};
use tracing::{debug, instrument};

use super::{AttestationKeyring, CustodyReceipt};

/// The finalization facts a receipt is minted from — everything the server recomputed or
/// verified itself at the commit point (never echoed unverified from the client).
#[derive(Debug, Clone)]
pub struct ReceiptInput {
    /// The album protocol pin (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// The upload session that produced custody.
    pub upload_id: String,
    /// The asset id.
    pub asset_id: String,
    /// `original | derivative | metadata | provenance`.
    pub blob_role: String,
    /// The server-recomputed ciphertext content address.
    pub ciphertext_hash: Hash32,
    /// Ciphertext size in bytes.
    pub size: u64,
    /// SHA-256 of the manifest envelope CBOR, when the session carried one.
    pub envelope_hash: Option<Hash32>,
    /// The user that uploaded.
    pub uploaded_by_user: String,
    /// The device that uploaded, when known.
    pub uploaded_by_device: Option<String>,
    /// The server's trusted clock at the finalization commit.
    pub received_at: Timestamp,
}

/// Write access to the custody-receipt log.
pub struct Mutation;

impl Mutation {
    /// Mint, sign, and persist the next custody receipt, returning it. MUST run inside the
    /// finalization transaction so the insert commits atomically with the `uploaded` flip and
    /// the `receipt_seq` advance rolls back together with it on failure.
    #[instrument(skip_all, fields(upload_id = %input.upload_id, asset = %input.asset_id))]
    pub async fn issue_receipt<C: ConnectionTrait>(
        db: &C,
        keyring: &AttestationKeyring,
        input: ReceiptInput,
    ) -> Result<CustodyReceipt, DbErr> {
        let server_id = keyring.server_id().to_string();
        let receipt_seq = Self::mint_next_seq(db, &server_id).await?;

        // Chain from the prior receipt's content hash (null only for the very first receipt).
        let prior_receipt_hash = if receipt_seq > 1 {
            let prior = custody_receipt::Entity::find_by_id((server_id.clone(), receipt_seq - 1))
                .one(db)
                .await?
                .ok_or_else(|| {
                    DbErr::Custom(format!(
                        "receipt-log gap: prior receipt {} missing for server {server_id}",
                        receipt_seq - 1
                    ))
                })?;
            Some(
                Hash32::from_hex(&prior.receipt_hash).map_err(|_| {
                    DbErr::Custom("prior receipt_hash is not valid hex".to_string())
                })?,
            )
        } else {
            None
        };

        let core = keyring.new_receipt_core(
            input.protocol_version,
            receipt_seq as u64,
            prior_receipt_hash,
            input.upload_id.clone(),
            input.asset_id.clone(),
            input.blob_role.clone(),
            input.ciphertext_hash,
            input.size,
            input.envelope_hash,
            input.uploaded_by_user.clone(),
            input.uploaded_by_device.clone(),
            input.received_at.to_string(),
        );
        let receipt = keyring.sign_receipt(core);
        let receipt_hash = receipt.content_hash();

        let row = custody_receipt::ActiveModel {
            server_id: Set(server_id),
            receipt_seq: Set(receipt_seq),
            prior_receipt_hash: Set(prior_receipt_hash.map(|h| h.to_hex())),
            receipt_hash: Set(receipt_hash.to_hex()),
            upload_id: Set(input.upload_id),
            asset_id: Set(input.asset_id),
            blob_role: Set(input.blob_role),
            ciphertext_hash: Set(input.ciphertext_hash.to_hex()),
            size: Set(input.size as i64),
            envelope_hash: Set(input.envelope_hash.map(|h| h.to_hex())),
            uploaded_by_user: Set(input.uploaded_by_user),
            uploaded_by_device: Set(input.uploaded_by_device),
            server_key_id: Set(receipt.core.server_key_id.to_hex()),
            received_at: Set(time::ts_to_entity_tz(input.received_at)),
            receipt_cbor: Set(receipt.to_canonical_cbor()),
            ..Default::default()
        };
        <custody_receipt::ActiveModel as sea_orm::ActiveModelTrait>::insert(row, db).await?;

        debug!(receipt_seq, "custody receipt issued and chained");
        Ok(receipt)
    }

    /// Atomically bump and return the server's next `receipt_seq`. The `ON CONFLICT DO UPDATE`
    /// takes the counter row's lock, so concurrent finalizations are serialised and the
    /// sequence is strictly increasing with no gaps or duplicates (invariant 33).
    async fn mint_next_seq<C: ConnectionTrait>(db: &C, server_id: &str) -> Result<i64, DbErr> {
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"INSERT INTO custody_receipt_seq (server_id, last_seq) VALUES ($1, 1)
              ON CONFLICT (server_id) DO UPDATE SET last_seq = custody_receipt_seq.last_seq + 1
              RETURNING last_seq",
            [server_id.into()],
        );
        let row = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbErr::Custom("receipt_seq mint returned no row".to_string()))?;
        row.try_get::<i64>("", "last_seq")
    }
}
