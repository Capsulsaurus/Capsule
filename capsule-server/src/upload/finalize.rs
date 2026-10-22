//! Finalization — the one place a staged upload becomes a blob.
//!
//! # The order is the contract
//!
//! [Transfer — Upload: Finalization and
//! Integrity](../../../capsule-docs/src/content/docs/design/transfer/upload.md) fixes
//! the order, and this module is those steps in that order:
//!
//! 1. **Claim.** [`claim_finalize`](crate::store::UploadSessionStore::claim_finalize) is a
//!    compare-and-set into
//!    `WaitingForProcessing`, so two racing finalizers cannot both proceed.
//! 2. **Recompute the hash** over the staged bytes and compare it to the declared one.
//! 3. **Re-run the envelope battery**, against the clock and the authority *now* — a device
//!    revoked or an album closed since creation does not slip through.
//! 4. **Commit** the stage onto its content address, atomically.
//! 5. **Ledger the custody**: a [`LedgerEvent::Custody`] entry is appended to the owner's
//!    ledger and fsynced, after the blob commit and before anything is indexed.
//! 6. **Record and receipt**, in one transaction: the blob is recorded against its asset — where
//!    the asset's next sequence number is minted when the change is one a client can observe
//!    (`S-C37`) — and the custody receipt, the server's signed admission of what it accepted
//!    (`S-C15`), is signed and logged with it.
//! 7. **Ledger the index write**: a [`LedgerEvent::Indexed`] entry carries the sequence number
//!    and the receipt step 6 minted — or, when the index refused the write, a
//!    [`LedgerEvent::Refused`] entry resolves the custody entry so a rebuild does not replay it.
//! 8. **Complete**, or fail the session and drop the staged bytes.
//!
//! # Why the ledger brackets the index
//!
//! The owner ledger is what makes the index rebuildable ([`crate::ledger`]), so the order is
//! the one that never leaves a row without an entry: a ledger append that fails fails the
//! finalization before the index is touched, and a crash between step 5 and step 6 leaves an
//! entry without a row, which rebuild replays. Step 7 cannot come first — the sequence number
//! and the receipt do not exist until step 6 commits — and its failure fails nothing: the row
//! already has its custody entry, the client already has custody, and a rebuild treats a
//! missing step 7 as a sequence it cannot prove. It is logged loudly instead.
//!
//! # Why the receipt is written by the index
//!
//! The contract (invariant 33) is that the index rows and the receipt commit together: never an
//! indexed blob without a receipt, never a receipt without custody. Two calls to two ports can
//! only be ordered, and either order leaves a crash window with one of those states in it. So
//! finalization hands the receipt draft and the attestation key to
//! [`AssetIndex::record_blob_with_receipt`](crate::index::AssetIndex::record_blob_with_receipt)
//! (or [`apply_op_with_receipt`](crate::index::AssetIndex::apply_op_with_receipt) for a
//! `replace`), and the index appends the receipt inside the critical section that writes its
//! rows. A failure anywhere in step 6 leaves neither. See [`crate::attestation`].
//!
//! # Why an index failure fails the whole finalization
//!
//! Step 6 comes after the bytes are committed, so a failure there leaves a blob at its content
//! address that no asset references. That is the *safe* half of the trade and the reason the
//! order is this way round: an unreferenced blob is what refcount GC exists to collect, while
//! an asset row claiming a blob the store does not hold is a dangling reference the feed would
//! serve. So an index that cannot answer marks the session `FailedProcessing` and the client
//! retries — the commit is idempotent, because an identical ciphertext is one object.
//!
//! Completing the session *without* recording would be the worst of the three: the client would
//! be told its upload succeeded, and the asset would never become visible to anything.
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

use capsule_core::crypto::hash::{Hash32, Sha256Hasher};
use capsule_core::crypto::receipts::CustodyReceipt;

use super::envelope::{GateContext, GateReject, ManifestEnvelope, check_finalize};
use super::{AlbumWriteAccess, UploadContext};
use crate::attestation::{ReceiptDraft, ReceiptLog};
use crate::blob::{BlobError, ContentAddress, Placement};
use crate::index::{BlobOutcome, BlobRecord, Custody, Receipted};
use crate::ledger::LedgerEvent;
use crate::store::{
    FinalizeClaim, OwnerId, StoreError, UploadId, UploadSessionRecord, UploadSessionStatus,
};

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

    /// A `replace` names a blob the store does not hold yet (`S-C43`).
    ///
    /// Transient and the client's to fix: the manifest is the member that applies a replace, so
    /// it is the member that lands last. Retrying the manifest once the rest of the bundle has
    /// committed succeeds.
    #[error("the replace names a {field} blob the store does not hold")]
    ReplaceIncomplete {
        /// Which member is missing.
        field: &'static str,
    },

    /// A `replace` was refused by the index (`S-C43`).
    ///
    /// Invariant 17's stale chain, invariant 18's epoch regression, or an asset that is no
    /// longer there. One variant because the client's action is the same for all three — re-read
    /// the asset and rebase — and because distinguishing them tells a caller about state it may
    /// not be entitled to.
    #[error("the replace was refused: {reason}")]
    ReplaceRefused {
        /// What the index decided, for the log line and the English detail.
        reason: &'static str,
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
    attestation: &crate::attestation::AttestationContext,
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

    match run_claimed(context, attestation, upload, &record).await {
        Ok((placement, minted)) => {
            complete(context, upload).await?;
            tracing::info!(
                %upload,
                asset_id = %record.asset_id,
                blob_role = record.blob_role.as_str(),
                placement = ?placement,
                completes_index_tier = super::visibility::completes_index_tier(record.blob_role),
                sync_seq = minted,
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

/// Steps 2–7, for a session this caller has claimed.
async fn run_claimed(
    context: &UploadContext,
    attestation: &crate::attestation::AttestationContext,
    upload: &UploadId,
    record: &UploadSessionRecord,
) -> Result<(Placement, Option<u64>), FinalizeFailure> {
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
    let digest = recompute_hash(context, upload, record.total_size).await?;
    if digest.address != record.expected_hash {
        return Err(FinalizeFailure::ContentHashMismatch {
            expected: record.expected_hash.clone(),
            actual: digest.address,
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
    let placement = context
        .blobs()
        .commit(upload, &address)
        .await
        .map_err(blob_unavailable("commit the staged upload"))?;

    // The asset's chain position, and only for the blob that *is* the manifest (`S-C31`). It
    // travels as its own value rather than being read back off the content address, because
    // `prior_provenance_hash` is a SHA-256 by definition while an address is whatever digest the
    // suite chose — equal today, not the same identifier.
    let manifest_sha256 =
        (record.blob_role == crate::store::BlobRole::Provenance).then_some(digest.sha256);

    // 5. The custody entry, durable before the index is touched: a row never exists without one.
    let draft = receipt_draft(context, record, &address, manifest_sha256)?;
    ledger_custody(context, attestation, &record.owner_id, &draft).await?;

    // 6. Record and receipt: the durable half and the signed half, in one transaction. Nothing
    //    before this point is observable to another device.
    let (minted, receipt) = match record_against_asset(
        context,
        attestation,
        &draft,
        record,
        &address,
        manifest_sha256,
    )
    .await
    {
        Ok(written) => written,
        Err(failure) => {
            // 7, refused: resolve the custody entry, so a rebuild does not replay a write the
            // server refused. An outage is not a refusal — whether it committed is unknown — and
            // is left unresolved, which is the crash case.
            if !matches!(failure, FinalizeFailure::Unavailable(_)) {
                ledger_refused(
                    context,
                    attestation,
                    &record.owner_id,
                    &draft,
                    failure.to_string(),
                )
                .await;
            }
            return Err(failure);
        }
    };

    // 7. What the transaction minted, for the rebuild that has to reproduce it.
    ledger_indexed(
        context,
        attestation,
        &record.owner_id,
        &draft,
        minted,
        receipt,
    )
    .await;

    Ok((placement, minted))
}

/// Step 7 for a write the index refused: append a `refused` entry naming the upload.
///
/// Best-effort, as [`ledger_indexed`] is: the finalization has already failed, and a replay of
/// an unresolved custody entry re-runs the checks that refused it.
pub(super) async fn ledger_refused(
    context: &UploadContext,
    attestation: &crate::attestation::AttestationContext,
    owner: &OwnerId,
    draft: &ReceiptDraft,
    reason: String,
) {
    let event = LedgerEvent::Refused {
        upload_id: draft.upload_id.as_str().to_owned(),
        asset_id: draft.asset_id.as_str().to_owned(),
        reason,
    };
    match attestation
        .ledger()
        .append(owner, event, context.clock().now(), attestation.signer())
        .await
    {
        Ok(entry) => tracing::debug!(
            %owner,
            upload = %draft.upload_id,
            ledger_seq = entry.core.seq,
            "ledgered a refused write"
        ),
        Err(error) => tracing::error!(
            %error,
            %owner,
            upload = %draft.upload_id,
            "the owner ledger could not record a refused write; \
             a rebuild will re-run the checks that refused it"
        ),
    }
}

/// Step 5: append the custody entry and wait for it to be durable.
///
/// A failure fails the finalization: the bytes are committed but nothing references them, which
/// is the orphan GC collects, rather than an index row the ledger cannot account for.
pub(super) async fn ledger_custody(
    context: &UploadContext,
    attestation: &crate::attestation::AttestationContext,
    owner: &OwnerId,
    draft: &ReceiptDraft,
) -> Result<(), FinalizeFailure> {
    let event = LedgerEvent::Custody {
        upload_id: draft.upload_id.as_str().to_owned(),
        asset_id: draft.asset_id.as_str().to_owned(),
        blob_role: draft.blob_role.clone(),
        content_hash: draft.ciphertext_hash,
        size: draft.size,
        manifest_sha256: draft.envelope_hash,
        upload_user_id: draft.uploaded_by_user.clone(),
        uploader_device: draft.uploaded_by_device.clone(),
        received_at: draft.received_at.clone(),
    };
    let entry = attestation
        .ledger()
        .append(owner, event, context.clock().now(), attestation.signer())
        .await
        .map_err(|error| {
            tracing::error!(
                %error,
                %owner,
                upload = %draft.upload_id,
                "the owner ledger could not record custody; nothing was indexed"
            );
            FinalizeFailure::Unavailable("could not record custody in the owner ledger".to_owned())
        })?;
    tracing::debug!(
        %owner,
        upload = %draft.upload_id,
        ledger_seq = entry.core.seq,
        "ledgered custody before indexing"
    );
    Ok(())
}

/// Step 7: append what the index transaction minted. Never fails the finalization; see the
/// module docs.
pub(super) async fn ledger_indexed(
    context: &UploadContext,
    attestation: &crate::attestation::AttestationContext,
    owner: &OwnerId,
    draft: &ReceiptDraft,
    sync_seq: Option<u64>,
    receipt: Option<CustodyReceipt>,
) {
    let event = LedgerEvent::Indexed {
        upload_id: draft.upload_id.as_str().to_owned(),
        asset_id: draft.asset_id.as_str().to_owned(),
        sync_seq,
        receipt: receipt.map(Box::new),
    };
    match attestation
        .ledger()
        .append(owner, event, context.clock().now(), attestation.signer())
        .await
    {
        Ok(entry) => tracing::debug!(
            %owner,
            upload = %draft.upload_id,
            ledger_seq = entry.core.seq,
            sync_seq,
            "ledgered the index write"
        ),
        Err(error) => tracing::error!(
            %error,
            %owner,
            upload = %draft.upload_id,
            sync_seq,
            "the owner ledger could not record an index write that committed; \
             a rebuild will not be able to prove this owner's sequence"
        ),
    }
}

/// The receipt the server signs for what it is taking custody of.
///
/// The facts come from what the server *established*, never from the request: the address is the
/// one finalization recomputed over the stored bytes, and the size is the stage's own length.
/// A receipt echoing a client's declaration would attest to the claim rather than to the custody.
fn receipt_draft(
    context: &UploadContext,
    record: &UploadSessionRecord,
    address: &ContentAddress,
    manifest_sha256: Option<Hash32>,
) -> Result<ReceiptDraft, FinalizeFailure> {
    let ciphertext_hash = Hash32::from_hex(address.as_str()).map_err(|_| {
        // Unreachable: the address was just built from a digest this module computed.
        FinalizeFailure::Unavailable("a computed address is not a digest".to_owned())
    })?;
    Ok(ReceiptDraft {
        crypto_suite_id: record.crypto_suite_id,
        protocol_version: record.protocol_version.clone(),
        upload_id: record.upload_id.clone(),
        asset_id: record.asset_id.clone(),
        blob_role: record.blob_role.as_str().to_owned(),
        ciphertext_hash,
        size: record.total_size,
        envelope_hash: manifest_sha256,
        uploaded_by_user: record.upload_user_id.as_str().to_owned(),
        uploaded_by_device: None,
        received_at: context.clock().now().to_string(),
    })
}

/// A custody write that could not complete, named by what was being attempted.
///
/// One answer whether the index or the receipt failed, because they are one transaction: the
/// caller cannot have half of it, so there is no half to name.
fn custody_unavailable<'a>(
    record: &'a UploadSessionRecord,
    doing: &'static str,
) -> impl Fn(StoreError) -> FinalizeFailure + 'a {
    move |error| {
        tracing::error!(
            %error,
            asset = %record.asset_id,
            upload = %record.upload_id,
            operation = doing,
            "the asset index could not record custody; neither the rows nor the receipt committed"
        );
        FinalizeFailure::Unavailable(format!("could not {doing}"))
    }
}

/// Say what custody receipt a finalization's write committed with.
fn log_receipt(record: &UploadSessionRecord, receipt: Option<&CustodyReceipt>) {
    if let Some(receipt) = receipt {
        tracing::info!(
            upload = %record.upload_id,
            asset = %record.asset_id,
            receipt_seq = receipt.core.receipt_seq,
            "custody receipt committed with the write"
        );
    } else {
        tracing::debug!(
            upload = %record.upload_id,
            asset = %record.asset_id,
            "the index refused the write, so no custody receipt was written"
        );
    }
}

/// Step 6: tell the index the blob landed, with its receipt, and hand back the sequence number it
/// minted and the receipt it issued.
async fn record_against_asset(
    context: &UploadContext,
    attestation: &crate::attestation::AttestationContext,
    draft: &ReceiptDraft,
    record: &UploadSessionRecord,
    address: &ContentAddress,
    manifest_sha256: Option<Hash32>,
) -> Result<(Option<u64>, Option<CustodyReceipt>), FinalizeFailure> {
    let custody = Custody {
        draft,
        signer: attestation.signer(),
    };
    // `S-C43`: a replace is applied by the member that can carry the whole change, and the rest
    // of its bundle deliberately touches the index not at all.
    let envelope: super::envelope::ManifestEnvelope =
        serde_json::from_str(&record.manifest_envelope).map_err(|error| {
            tracing::error!(%error, "a stored envelope does not decode");
            FinalizeFailure::Unavailable("the stored envelope does not decode".to_owned())
        })?;
    if envelope.action == "replace" {
        return apply_replace(
            context,
            custody,
            attestation.receipts(),
            record,
            address,
            manifest_sha256,
            &envelope,
        )
        .await;
    }

    let Receipted { outcome, receipt } = context
        .index()
        .record_blob_with_receipt(
            &record.asset_id,
            BlobRecord {
                role: record.blob_role,
                address: address.clone(),
                size: record.total_size,
                manifest_sha256,
                finalized_at: context.clock().now(),
            },
            custody,
        )
        .await
        .map_err(custody_unavailable(
            record,
            "record the blob and its receipt against its asset",
        ))?;
    log_receipt(record, receipt.as_ref());

    match outcome {
        BlobOutcome::Recorded { minted, .. } => Ok((minted, receipt)),
        // A retried finalization of a blob already held. Not an error: the asset already says
        // what this finalization was going to make it say.
        BlobOutcome::AlreadyHeld(row) => Ok((row.sync_seq, receipt)),
        // The asset already holds a *different* address under a role that admits one. The
        // envelope contradicts the asset it names, which is exactly invariant 15's shape, so it
        // is answered as an envelope failure rather than as a new status.
        BlobOutcome::Conflict => {
            tracing::info!(
                asset = %record.asset_id,
                role = record.blob_role.as_str(),
                "a finalization tried to re-point a role the asset already holds"
            );
            Err(FinalizeFailure::EnvelopeRejected(
                GateReject::EnvelopeMismatch("blob_role"),
            ))
        }
        // The row is gone, or was never reserved. The server's own inconsistency: creation
        // reserves before it opens a session.
        BlobOutcome::NotFound => {
            tracing::error!(
                asset = %record.asset_id,
                "finalization found no asset row for a session that reserved one"
            );
            Err(FinalizeFailure::Unavailable(
                "the asset row this session reserved is gone".to_owned(),
            ))
        }
    }
}

/// Apply one member of a `replace` bundle (`S-C43`).
///
/// # Why the manifest is the only member that writes
///
/// A `create` assembles its bundle incrementally: its row is `Pending`, nobody can see it, and
/// no member has to know about another. A `replace` mutates an asset that is **already
/// visible**, so it cannot be assembled the same way — a window in which the new original is
/// referenced by the old manifest is a window in which `verify_asset` fails for every client
/// that fetches the asset, and that is a correctness hole rather than a latency one.
///
/// So a replace is applied as one act, at the moment its provenance blob — the signed manifest —
/// finalizes. The other members commit their bytes into the content-addressed store and record
/// nothing. That makes the manifest the **last** member of a replace bundle to land, which is
/// the one ordering rule this protocol has; the upload protocol's "no wire ordering" promise is
/// about a `create`, whose bundle has nowhere to be half-applied.
///
/// # And why the manifest can name what it commits to
///
/// It could not, until this slice. `manifest_envelope.ciphertext_hash` names *the blob this
/// session is uploading*, not the manifest's own — a conflation that is invisible for a create
/// and blocking here, because the provenance session's value is the manifest's own address.
/// `original_blob_hash` is the manifest's commitment under a name that cannot be confused with
/// it, required on a replace and optional otherwise.
///
/// # A bundle whose bytes are not all there is refused, not partially applied
///
/// The named blobs are checked for presence *before* the index is touched. A `replace` that
/// applied with a missing original would leave a visible asset pointing at bytes nobody holds —
/// the dangling reference the integrity scrub exists to report, created on purpose.
async fn apply_replace(
    context: &UploadContext,
    custody: Custody<'_>,
    receipts: &dyn ReceiptLog,
    record: &UploadSessionRecord,
    address: &ContentAddress,
    manifest_sha256: Option<Hash32>,
    envelope: &super::envelope::ManifestEnvelope,
) -> Result<(Option<u64>, Option<CustodyReceipt>), FinalizeFailure> {
    if record.blob_role != crate::store::BlobRole::Provenance {
        // Bytes committed, index untouched. If the manifest never lands the blob is unreferenced
        // and the collector reclaims it on its ordinary schedule, crediting the quota back
        // (`S-C44`) — an abandoned replace costs its uploader nothing permanent.
        //
        // The receipt is this finalization's only durable write, so it is atomic on its own:
        // there is no index row for it to be committed apart from.
        let receipt = receipts
            .issue(custody.draft.clone(), custody.signer)
            .await
            .map_err(custody_unavailable(record, "issue the custody receipt"))?;
        log_receipt(record, Some(&receipt));
        tracing::debug!(
            asset = %record.asset_id,
            role = record.blob_role.as_str(),
            "a replace's bytes landed; the manifest is what applies them"
        );
        return Ok((None, Some(receipt)));
    }

    let manifest_hash = manifest_sha256.ok_or_else(|| {
        // Unreachable: `manifest_sha256` is `Some` for exactly the provenance role.
        FinalizeFailure::Unavailable("a provenance blob has no manifest digest".to_owned())
    })?;

    let original = named_blob(context, envelope.original_blob_hash.as_deref(), "original").await?;
    let metadata = named_blob(context, envelope.metadata_blob_hash.as_deref(), "metadata").await?;
    let prior = match &envelope.prior_provenance_hash {
        Some(hex) => Some(Hash32::from_hex(hex).map_err(|_| {
            FinalizeFailure::EnvelopeRejected(GateReject::EnvelopeMismatch("prior_provenance_hash"))
        })?),
        // Refused at the gate (`GateReject::ReplaceDoesNotChain`), so reaching here would mean
        // a session opened before that check existed.
        None => {
            return Err(FinalizeFailure::EnvelopeRejected(
                GateReject::ReplaceDoesNotChain,
            ));
        }
    };

    let Some(album_id) = record.album_id.clone() else {
        return Err(FinalizeFailure::EnvelopeRejected(
            GateReject::EnvelopeMismatch("album_id"),
        ));
    };

    let Receipted { outcome, receipt } = context
        .index()
        .apply_op_with_receipt(
            crate::index::LifecycleOp {
                asset_id: record.asset_id.clone(),
                owner_id: record.owner_id.clone(),
                album_id,
                action: crate::index::OpAction::Replace,
                manifest_hash,
                prior_provenance_hash: prior,
                amk_version: u64::from(envelope.amk_version),
                provenance: address.clone(),
                metadata: Some(metadata),
                original: Some(original),
                retention_until: None,
                at: context.clock().now(),
            },
            custody,
        )
        .await
        .map_err(custody_unavailable(
            record,
            "apply the replace and its receipt",
        ))?;
    log_receipt(record, receipt.as_ref());

    match outcome {
        crate::index::OpOutcome::Applied { sync_seq, .. } => {
            tracing::info!(asset = %record.asset_id, sync_seq, "a replace superseded an asset");
            Ok((Some(sync_seq), receipt))
        }
        // The same manifest finalizing twice — a retried finalization, not a second replace.
        crate::index::OpOutcome::Replayed { sync_seq } => Ok((Some(sync_seq), receipt)),
        crate::index::OpOutcome::StaleChain { head } => {
            tracing::info!(
                asset = %record.asset_id,
                stored_head = ?head,
                "a replace was refused: it does not chain onto the stored head"
            );
            Err(FinalizeFailure::ReplaceRefused {
                reason: "the manifest does not follow the asset's current chain head",
            })
        }
        crate::index::OpOutcome::AmkRegressed { stored } => {
            tracing::info!(asset = %record.asset_id, stored, "a replace was refused: epoch regression");
            Err(FinalizeFailure::ReplaceRefused {
                reason: "the album epoch this manifest was written under has been superseded",
            })
        }
        // The asset is gone, or was never this caller's. Creation reserved the row, so this is
        // a race with a delete rather than a server inconsistency.
        crate::index::OpOutcome::NotFound => Err(FinalizeFailure::ReplaceRefused {
            reason: "the asset this manifest replaces is no longer there",
        }),
    }
}

/// The address `hex` names, once the store confirms the bytes are there.
///
/// The presence check is the point: a replace applies every role at once, so a member whose
/// bytes have not landed would be applied as a dangling reference.
async fn named_blob(
    context: &UploadContext,
    hex: Option<&str>,
    field: &'static str,
) -> Result<ContentAddress, FinalizeFailure> {
    let Some(hex) = hex else {
        // Refused at the gate; reaching here means a session predates that check.
        return Err(FinalizeFailure::EnvelopeRejected(
            GateReject::ReplaceIncomplete(field),
        ));
    };
    let address = ContentAddress::parse(hex)
        .map_err(|_| FinalizeFailure::EnvelopeRejected(GateReject::EnvelopeMismatch(field)))?;
    let present = context
        .blobs()
        .stat(&address)
        .await
        .map_err(blob_unavailable("stat a blob a replace names"))?;
    if present.is_none() {
        tracing::info!(%address, field, "a replace named a blob the store does not hold");
        return Err(FinalizeFailure::ReplaceIncomplete { field });
    }
    Ok(address)
}

/// What one pass over the staged bytes established.
///
/// Two fields from one hasher, because they are two *identifiers* that happen to share a
/// digest today and must not share a variable (`S-C31`): the content address is whatever digest
/// the crypto suite selects, and the chain position is a SHA-256 by definition. The day a suite
/// selects something else, this struct grows a second hasher and every caller keeps compiling —
/// which is the whole reason they are separate here while they are equal.
#[derive(Debug, Clone)]
struct StagedDigest {
    /// The content address, as the wire spells it.
    address: String,
    /// SHA-256 over the same bytes.
    sha256: Hash32,
}

/// Recompute the ciphertext hash over the staged bytes, window by window.
async fn recompute_hash(
    context: &UploadContext,
    upload: &UploadId,
    expected_len: u64,
) -> Result<StagedDigest, FinalizeFailure> {
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
    let sha256 = hasher.finalize();
    Ok(StagedDigest {
        address: sha256.to_hex(),
        sha256,
    })
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

    // Asked for the **uploader**, as creation asked: a member whose write access was withdrawn
    // between creation and finalization is refused here, exactly as a closed album is.
    let access = context
        .authority()
        .album_write_access(&record.upload_user_id, album)
        .await
        .map_err(|error| FinalizeFailure::Unavailable(error.to_string()))?;
    let AlbumWriteAccess::Writable {
        owner_id,
        protocol_pin,
        ..
    } = access
    else {
        // The album closed, or write capability was withdrawn, since creation. The taxonomy
        // answers a finalization-time envelope failure with one code, so this is not a second
        // `album_access_denied`.
        return Err(FinalizeFailure::EnvelopeRejected(
            GateReject::AlbumPinMismatch,
        ));
    };
    if owner_id != record.owner_id {
        // The album changed hands since the session was opened. Not a state this server can
        // produce today, and refused rather than re-filed because the session's namespace is
        // what its reserved asset row was minted under.
        return Err(FinalizeFailure::EnvelopeRejected(
            GateReject::AlbumPinMismatch,
        ));
    }

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

/// Step 8, the winning half.
async fn complete(context: &UploadContext, upload: &UploadId) -> Result<(), FinalizeFailure> {
    context
        .sessions()
        .set_status(upload, UploadSessionStatus::Completed)
        .await
        .map_err(unavailable("complete the session"))?;
    Ok(())
}

/// Step 8, the losing half: the bytes go and the session is terminal.
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
