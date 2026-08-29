//! Finalization — the one place a staged upload becomes a blob.
//!
//! # The order is the contract
//!
//! [Upload Protocol — Finalization and
//! Integrity](../../../capsule-docs/src/content/docs/design/import/upload-protocol.md) fixes
//! five steps, and this module is those five steps in that order:
//!
//! 1. **Claim.** [`claim_finalize`](crate::store::UploadSessionStore::claim_finalize) is a
//!    compare-and-set into
//!    `WaitingForProcessing`, so two racing finalizers cannot both proceed.
//! 2. **Recompute the hash** over the staged bytes and compare it to the declared one.
//! 3. **Re-run the envelope battery**, against the clock and the authority *now* — a device
//!    revoked or an album closed since creation does not slip through.
//! 4. **Commit** the stage onto its content address, atomically.
//! 5. **Complete**, or fail the session and drop the staged bytes.
//!
//! # Losing the claim is not an error
//!
//! [`FinalizeClaim`] has three answers and this module keeps all three. A caller that loses
//! the race has *not* failed: another finalizer holds the session and is driving it to a
//! terminal state, and the losing caller's own work — its chunk — was accepted. So
//! [`Outcome::AlreadyClaimed`] is a success shape, not a rejection, and the route answers the
//! chunk that triggered it with the same `204` it would have answered anyway. The client
//! learns the terminal outcome by listing its sessions. The Salvo server returned
//! `409 finalize_in_progress` here, which told a client its own accepted chunk had failed.
//!
//! # Nothing here produces manifest bytes (`S-C30`)
//!
//! Finalization stores what the client sent and nothing else. It does not re-serialize the
//! envelope projection into CBOR, does not hash such a re-serialization into an attestation,
//! and does not synthesize a manifest for a feed. The signed manifest is a `provenance` blob
//! that travelled this same path and is held verbatim at its content address.

use capsule_core::crypto::hash::Sha256Hasher;

use super::envelope::{GateContext, GateReject, ManifestEnvelope, check_finalize};
use super::{AlbumWriteAccess, UploadContext};
use crate::blob::{BlobError, ContentAddress, Placement};
use crate::store::{FinalizeClaim, StoreError, UploadId, UploadSessionRecord, UploadSessionStatus};

/// How many bytes of the stage are read and hashed at a time.
///
/// 1 MiB: large enough that a gigabyte blob is a thousand round trips through the port rather
/// than a quarter of a million, and small enough that the hash update between two `await`s is
/// a couple of milliseconds of CPU rather than a stalled reactor. The design doc's
/// "hashing runs on a blocking thread pool" describes a synchronous whole-file read; a
/// windowed hash over an async port is the same guarantee — the reactor is never held for the
/// length of a file — expressed in the shape the port has.
const HASH_WINDOW_BYTES: usize = 1024 * 1024;

/// What finalization did.
#[derive(Debug)]
pub enum Outcome {
    /// This caller won the claim and the blob is now at its content address.
    Committed {
        /// Whether the bytes were written or the address was already occupied (deduplication).
        placement: Placement,
        /// The session as it was claimed, for the caller's log line and visibility derivation.
        record: Box<UploadSessionRecord>,
    },
    /// Another caller holds the claim, or the session already reached a terminal state.
    ///
    /// A normal race. See the module docs.
    AlreadyClaimed,
    /// The session was gone by the time the claim was attempted.
    NotFound,
}

/// Why finalization refused, after winning the claim.
///
/// Every variant leaves the session `FailedProcessing` and the staged bytes dropped, which is
/// what makes the state machine's `WaitingForProcessing → FailedProcessing` edge total: a
/// claimed session is always driven to a terminal state.
#[derive(Debug, thiserror::Error)]
pub enum FinalizeFailure {
    /// Invariant 14: the recomputed ciphertext hash is not the declared one.
    #[error("the stored bytes hash to {actual}, not to the declared {expected}")]
    ContentHashMismatch {
        /// What the session declared at creation.
        expected: String,
        /// What the bytes actually hash to.
        actual: String,
    },

    /// Invariant 15: the envelope no longer passes the battery it passed at creation.
    #[error("the manifest envelope did not survive re-validation: {0:?}")]
    EnvelopeRejected(GateReject),

    /// The staged file's length is not the session's received-byte count, or the stage is gone.
    ///
    /// The server's own inconsistency, never the client's fault.
    #[error("the staged upload is {on_disk:?} bytes where the session recorded {expected}")]
    StorageInconsistent {
        /// The session's received-byte count.
        expected: u64,
        /// The stage's length, or `None` when there is no stage at all.
        on_disk: Option<u64>,
    },

    /// A collaborator could not answer.
    #[error("finalization could not be completed: {0}")]
    Unavailable(String),
}

/// Run finalization for `upload`.
///
/// Total over its inputs: every path either commits, hands back one of the two non-winning
/// outcomes, or leaves the session terminal and returns why.
#[tracing::instrument(skip(context), fields(upload = %upload))]
pub async fn finalize(
    context: &UploadContext,
    upload: &UploadId,
) -> Result<Outcome, FinalizeFailure> {
    // 1. Claim. Only a `Pending` or `Uploading` session transitions, so the winner is unique
    //    and the loser is told so rather than racing it.
    let record = match context
        .sessions()
        .claim_finalize(upload)
        .await
        .map_err(unavailable("claim finalization"))?
    {
        FinalizeClaim::Won(record) => record,
        FinalizeClaim::AlreadyClaimed => {
            tracing::info!(%upload, "finalization was already claimed; this caller stands down");
            return Ok(Outcome::AlreadyClaimed);
        }
        FinalizeClaim::NotFound => {
            tracing::info!(%upload, "finalization found no session to claim");
            return Ok(Outcome::NotFound);
        }
    };

    match run_claimed(context, upload, &record).await {
        Ok(placement) => {
            complete(context, upload).await?;
            tracing::info!(
                %upload,
                asset_id = %record.asset_id,
                blob_role = record.blob_role.as_str(),
                placement = ?placement,
                flips_visibility = super::visibility::finalization_makes_visible(record.blob_role),
                original_held = super::visibility::derive_original_held(
                    record.blob_role == crate::store::BlobRole::Original
                ),
                "upload finalized"
            );
            Ok(Outcome::Committed { placement, record })
        }
        Err(failure) => {
            // A claimed session is never left claimed: the bytes go and the status is terminal,
            // so the receipt a resuming client reads says "failed" rather than "in progress
            // forever".
            fail(context, upload).await;
            tracing::warn!(%upload, %failure, "finalization refused the upload");
            Err(failure)
        }
    }
}

/// Steps 2–4, for a session this caller has claimed.
async fn run_claimed(
    context: &UploadContext,
    upload: &UploadId,
    record: &UploadSessionRecord,
) -> Result<Placement, FinalizeFailure> {
    // The stage is the truth the session's counter caches, so a divergence here is a
    // server-side inconsistency and never a reason to commit.
    let staged = context
        .blobs()
        .staged_len(upload)
        .await
        .map_err(blob_unavailable("measure the staged upload"))?;
    if staged != Some(record.total_size) {
        return Err(FinalizeFailure::StorageInconsistent {
            expected: record.total_size,
            on_disk: staged,
        });
    }

    // 2. Invariant 14.
    let actual = recompute_hash(context, upload, record.total_size).await?;
    if actual != record.expected_hash {
        return Err(FinalizeFailure::ContentHashMismatch {
            expected: record.expected_hash.clone(),
            actual,
        });
    }

    // 3. Invariant 15, against the album, the directory and the clock as they are now.
    revalidate(context, record).await?;

    // 4. The stage becomes a blob. `AlreadyPresent` is a success that wrote nothing: an
    //    identical ciphertext — or an identical signed manifest — is one object, which is the
    //    deduplication the content address exists for.
    let address = ContentAddress::parse(&record.expected_hash).map_err(|error| {
        // Unreachable while invariant 3 gates creation: a session's hash is 64 lowercase hex
        // characters or the session was never opened. Kept because "unreachable while an
        // earlier check holds" is exactly the assumption worth a cheap guard.
        FinalizeFailure::Unavailable(format!("the session's hash is not an address: {error}"))
    })?;
    context
        .blobs()
        .commit(upload, &address)
        .await
        .map_err(blob_unavailable("commit the staged upload"))
}

/// Recompute the ciphertext hash over the staged bytes, window by window.
async fn recompute_hash(
    context: &UploadContext,
    upload: &UploadId,
    expected_len: u64,
) -> Result<String, FinalizeFailure> {
    let mut hasher = Sha256Hasher::new();
    let mut offset = 0_u64;

    loop {
        let window = context
            .blobs()
            .read_staged_at(upload, offset, HASH_WINDOW_BYTES)
            .await
            .map_err(blob_unavailable("read the staged upload"))?
            .ok_or(FinalizeFailure::StorageInconsistent {
                expected: expected_len,
                on_disk: None,
            })?;
        if window.is_empty() {
            break;
        }
        offset += window.len() as u64;
        hasher.update(&window);
    }

    if offset != expected_len {
        return Err(FinalizeFailure::StorageInconsistent {
            expected: expected_len,
            on_disk: Some(offset),
        });
    }

    tracing::debug!(%upload, bytes = offset, "recomputed the ciphertext hash");
    Ok(hasher.finalize().to_hex())
}

/// Step 3: the battery, re-run against durable state as it is at this instant.
async fn revalidate(
    context: &UploadContext,
    record: &UploadSessionRecord,
) -> Result<(), FinalizeFailure> {
    let envelope: ManifestEnvelope = serde_json::from_str(&record.manifest_envelope)
        .map_err(|error| FinalizeFailure::Unavailable(format!("stored envelope: {error}")))?;

    let Some(album) = record.album_id.as_ref() else {
        // A session this server opened always names an album (see [`crate::routes::upload`]),
        // so this is a record written by something else.
        return Err(FinalizeFailure::EnvelopeRejected(
            GateReject::AlbumPinMismatch,
        ));
    };

    let access = context
        .authority()
        .album_write_access(&record.owner_id, album)
        .await
        .map_err(|error| FinalizeFailure::Unavailable(error.to_string()))?;
    let AlbumWriteAccess::Writable { protocol_pin } = access else {
        // The album closed, or write capability was withdrawn, since creation. The taxonomy
        // answers a finalization-time envelope failure with one code, so this is not a second
        // `album_access_denied`.
        return Err(FinalizeFailure::EnvelopeRejected(
            GateReject::AlbumPinMismatch,
        ));
    };

    let device =
        super::envelope::created_by_device(&envelope).map_err(FinalizeFailure::EnvelopeRejected)?;
    let Some(device_added_at) = context
        .authority()
        .device_added_at(&record.upload_user_id, device)
        .await
        .map_err(|error| FinalizeFailure::Unavailable(error.to_string()))?
    else {
        return Err(FinalizeFailure::EnvelopeRejected(
            GateReject::DeviceNotAuthorized,
        ));
    };

    check_finalize(
        &envelope,
        &GateContext {
            policy: context.policy(),
            album_pin: &protocol_pin,
            device_added_at,
            server_clock: context.clock().now(),
        },
    )
    .map_err(FinalizeFailure::EnvelopeRejected)
}

/// Step 5, the winning half.
async fn complete(context: &UploadContext, upload: &UploadId) -> Result<(), FinalizeFailure> {
    context
        .sessions()
        .set_status(upload, UploadSessionStatus::Completed)
        .await
        .map_err(unavailable("complete the session"))?;
    Ok(())
}

/// Step 5, the losing half: the bytes go and the session is terminal.
///
/// Best-effort by construction — it runs *because* something already failed, and a second
/// failure here must not replace the diagnosis the caller is about to return. Both halves are
/// logged so an operator sees a stage that outlived its session.
async fn fail(context: &UploadContext, upload: &UploadId) {
    match context.blobs().abandon(upload).await {
        Ok(dropped) => tracing::debug!(%upload, dropped, "dropped the staged bytes"),
        Err(error) => {
            tracing::error!(%upload, %error, "a failed upload's stage could not be dropped");
        }
    }
    if let Err(error) = context
        .sessions()
        .set_status(upload, UploadSessionStatus::FailedProcessing)
        .await
    {
        tracing::error!(%upload, %error, "a failed upload's session could not be marked");
    }
}

/// A store failure, named by what was being attempted.
fn unavailable(doing: &'static str) -> impl Fn(StoreError) -> FinalizeFailure {
    move |error| {
        tracing::error!(%error, operation = doing, "the upload session store could not answer");
        FinalizeFailure::Unavailable(format!("could not {doing}"))
    }
}

/// A blob-store failure, named by what was being attempted.
fn blob_unavailable(doing: &'static str) -> impl Fn(BlobError) -> FinalizeFailure {
    move |error| {
        tracing::error!(%error, operation = doing, "the blob store could not answer");
        FinalizeFailure::Unavailable(format!("could not {doing}"))
    }
}
