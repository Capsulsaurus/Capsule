//! Adoption — recording a blob the owner already holds against a second asset (#672).
//!
//! # The data-loss shape this closes
//!
//! A create whose `(owner, hash, album)` names bytes another asset already holds is answered
//! `409 duplicate_blob`, and a client resolves that as a **merge**: it treats the blob as held
//! and never uploads it. Before this module the second asset's row never named the blob, so the
//! collector — which counts the rows that name an address and nothing else — saw one reference
//! where two assets depended on the bytes. Purging the first asset then made the blob an orphan,
//! and the next collection past the grace window swept bytes the second asset still needed.
//!
//! So the refusal now **adopts**: before answering the `409`, the server records the existing
//! blob against the requesting asset. The wire answer is unchanged — the `409` and its
//! `existing_asset` are what the validation doc and the client's merge path already specify —
//! and the reference count becomes true.
//!
//! # Adoption is a finalization without bytes
//!
//! An adopted reference is an indexed blob like any other, so it carries what every indexed blob
//! carries: an owner-ledger custody entry before the index is touched, the index row and its
//! custody receipt in one critical section, and the ledger entry resolving the custody entry
//! after. The order and the reasons are [`super::finalize`]'s, and so are the helpers. Skipping
//! them would leave a row the ledger cannot rebuild — which would reintroduce the same lost
//! reference on the first index rebuild — and a blob the asset's receipt log does not account
//! for, which the client's custody check refuses.
//!
//! The "session" a custody entry names is an **adoption id**: a fresh UUIDv7 in the upload-id
//! space that no session store ever holds. It is what pairs the custody entry with its
//! `indexed` or `refused` resolution, which is all the ledger asks of it.
//!
//! # What is never adopted
//!
//! - **Bytes the store does not hold.** A row naming an address the store lacks is a dangling
//!   reference — reported by the collector and the scrub, never resolved by deletion — and
//!   writing a second receipt for it would attest to custody the server does not have. The
//!   caller opens an ordinary session instead, and the upload that follows puts the bytes back.
//! - **A signed manifest.** A provenance blob names its own asset, so two assets cannot
//!   legitimately share one, and recording it would need the chain position finalization
//!   computes over bytes it has just read. The plain `409` stands.
//!
//! Quota is not charged: no byte is stored, and the duplicate check is ahead of the charge so
//! that a refused duplicate costs nothing.

use uuid::Uuid;

use super::UploadContext;
use super::finalize::{FinalizeFailure, ledger_custody, ledger_indexed, ledger_refused};
use crate::attestation::{AttestationContext, ReceiptDraft};
use crate::blob::ContentAddress;
use crate::index::{BlobOutcome, BlobRecord, Custody, Receipted};
use crate::store::{AssetId, BlobRole, OwnerId, UploadId, UserId};

/// One blob to adopt: the requesting asset, and the address another asset already holds.
#[derive(Debug, Clone, Copy)]
pub struct Adoption<'a> {
    /// The owner both assets are filed under.
    pub owner: &'a OwnerId,
    /// The authenticated caller, named on the custody receipt as the uploader.
    pub uploader: &'a UserId,
    /// The asset asking for the blob — reserved by the caller before adoption runs.
    pub asset_id: &'a AssetId,
    /// The role the requesting asset gives the blob.
    pub role: BlobRole,
    /// The blob's content address.
    pub address: &'a ContentAddress,
    /// The crypto suite the requesting bundle is sealed under.
    pub crypto_suite_id: u16,
    /// The album's pinned protocol date.
    pub protocol_version: &'a str,
}

/// What adoption did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adopted {
    /// The requesting asset now names the blob.
    Recorded {
        /// The sequence number the reference minted, when it was observable.
        minted: Option<u64>,
    },
    /// The requesting asset already named the blob — a retried create. Nothing was written.
    AlreadyHeld,
    /// The role is never adopted ([`BlobRole::Provenance`]). Nothing was written.
    Declined,
    /// The store does not hold the bytes, so there is nothing to take custody of. Nothing was
    /// written; the caller opens a session so the bytes can be uploaded again.
    Missing,
    /// The requesting asset already holds a *different* address under this singular role.
    ///
    /// The same refusal finalization would make: a signed manifest names each singular role's
    /// bytes, and re-pointing one is not something a later upload may do.
    Conflict,
}

/// Record `adoption.address` against `adoption.asset_id`, ledgered and receipted as a
/// finalization is.
///
/// # Errors
///
/// [`FinalizeFailure::Unavailable`] when a collaborator could not answer. Nothing the caller can
/// observe has changed in that case unless the index write committed, which is the crash case
/// the ledger already replays.
#[tracing::instrument(
    skip(context, attestation, adoption),
    fields(asset = %adoption.asset_id, address = %adoption.address, role = adoption.role.as_str())
)]
pub async fn adopt(
    context: &UploadContext,
    attestation: &AttestationContext,
    adoption: Adoption<'_>,
) -> Result<Adopted, FinalizeFailure> {
    if adoption.role == BlobRole::Provenance {
        tracing::info!("a manifest is never adopted across assets; refusing without recording");
        return Ok(Adopted::Declined);
    }

    // A retried create of a blob already adopted writes nothing more: no second custody entry,
    // no second receipt for the same reference.
    let row = context
        .index()
        .read(adoption.asset_id)
        .await
        .map_err(store_unavailable("read the adopting asset"))?;
    if row.as_ref().is_some_and(|row| {
        row.blobs
            .iter()
            .any(|blob| &blob.address == adoption.address)
    }) {
        tracing::debug!("the asset already names the blob; nothing to adopt");
        return Ok(Adopted::AlreadyHeld);
    }

    // The size is the store's, never the request's: a receipt attests to what the server holds.
    let Some(stat) = context
        .blobs()
        .stat(adoption.address)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the blob store could not answer for an adopted blob");
            FinalizeFailure::Unavailable("could not stat the adopted blob".to_owned())
        })?
    else {
        tracing::error!(
            "an asset row names a blob the store does not hold; \
             opening a session instead of adopting a dangling reference"
        );
        return Ok(Adopted::Missing);
    };

    let draft = draft(context, &adoption, stat.size)?;
    ledger_custody(context, attestation, adoption.owner, &draft).await?;

    let Receipted { outcome, receipt } = context
        .index()
        .record_blob_with_receipt(
            adoption.asset_id,
            BlobRecord {
                role: adoption.role,
                address: adoption.address.clone(),
                size: stat.size,
                manifest_sha256: None,
                amk_version: None,
                finalized_at: context.clock().now(),
            },
            Custody {
                draft: &draft,
                signer: attestation.signer(),
            },
        )
        .await
        .map_err(|error| {
            tracing::error!(
                %error,
                adoption_id = %draft.upload_id,
                "the asset index could not record an adopted blob; \
                 neither the row nor the receipt committed"
            );
            FinalizeFailure::Unavailable("could not record the adopted blob".to_owned())
        })?;

    let adopted = match outcome {
        BlobOutcome::Recorded { minted, .. } => {
            ledger_indexed(
                context,
                attestation,
                adoption.owner,
                &draft,
                minted,
                receipt,
            )
            .await;
            Adopted::Recorded { minted }
        }
        BlobOutcome::AlreadyHeld(row) => {
            ledger_indexed(
                context,
                attestation,
                adoption.owner,
                &draft,
                row.sync_seq,
                receipt,
            )
            .await;
            Adopted::AlreadyHeld
        }
        BlobOutcome::Conflict => {
            ledger_refused(
                context,
                attestation,
                adoption.owner,
                &draft,
                "the asset already holds another blob under this role".to_owned(),
            )
            .await;
            Adopted::Conflict
        }
        // Unreachable: an adoption never records a manifest (see the module docs), so it never
        // carries an epoch to refuse. Kept total as the server's own inconsistency.
        BlobOutcome::AmkRegressed { stored } => {
            ledger_refused(
                context,
                attestation,
                adoption.owner,
                &draft,
                "the index refused an epoch the adoption does not carry".to_owned(),
            )
            .await;
            tracing::error!(
                stored,
                "an adoption was refused on an epoch it does not carry"
            );
            return Err(FinalizeFailure::Unavailable(
                "the index refused an adoption on an epoch".to_owned(),
            ));
        }
        BlobOutcome::NotFound => {
            ledger_refused(
                context,
                attestation,
                adoption.owner,
                &draft,
                "the adopting asset has no row".to_owned(),
            )
            .await;
            tracing::error!("the asset row the create reserved is gone; nothing was adopted");
            return Err(FinalizeFailure::Unavailable(
                "the adopting asset's row is gone".to_owned(),
            ));
        }
    };
    tracing::info!(
        adoption_id = %draft.upload_id,
        outcome = ?adopted,
        "a duplicate create adopted the blob its owner already held"
    );
    Ok(adopted)
}

/// The receipt the server signs for an adopted reference: the store's own size, a fresh
/// adoption id, and the caller as the uploader.
fn draft(
    context: &UploadContext,
    adoption: &Adoption<'_>,
    size: u64,
) -> Result<ReceiptDraft, FinalizeFailure> {
    let ciphertext_hash = capsule_core::crypto::hash::Hash32::from_hex(adoption.address.as_str())
        .map_err(|_| {
        // Unreachable: a content address is the hex of a digest by construction.
        FinalizeFailure::Unavailable("a content address is not a digest".to_owned())
    })?;
    Ok(ReceiptDraft {
        crypto_suite_id: adoption.crypto_suite_id,
        protocol_version: adoption.protocol_version.to_owned(),
        upload_id: UploadId::new(Uuid::now_v7().to_string()),
        asset_id: adoption.asset_id.clone(),
        blob_role: adoption.role.as_str().to_owned(),
        ciphertext_hash,
        size,
        envelope_hash: None,
        uploaded_by_user: adoption.uploader.as_str().to_owned(),
        uploaded_by_device: None,
        received_at: context.clock().now().to_string(),
    })
}

/// A store failure, logged with the operation and mapped onto the one failure adoption has.
fn store_unavailable(doing: &'static str) -> impl Fn(crate::store::StoreError) -> FinalizeFailure {
    move |error| {
        tracing::error!(%error, operation = doing, "the asset index could not answer");
        FinalizeFailure::Unavailable(format!("could not {doing}"))
    }
}
