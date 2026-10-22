//! The deposit: filing a guest drop whose every byte has landed, and driving it to a terminal
//! state whatever happens to the request that started it.
//!
//! # Claimed, as an album finalization is
//!
//! A drop session that holds every declared byte is finished in three steps: its stage is hashed
//! against the address the guest declared and committed there, its held declaration is filed in
//! the owner's inbox, and the session is marked `Completed`. The steps run under the session's finalization claim
//! ([`claim_finalize`](crate::store::UploadSessionStore::claim_finalize)) — the album path's
//! claim, on the same session store — so exactly one caller drives them, and a guest's replay
//! that races the request completing the drop stands down instead of committing twice.
//!
//! # Driven to a terminal state without its request
//!
//! The claim is a promise that the session reaches `Completed` or `FailedProcessing`, and it is
//! kept the way the album path keeps it (`crate::upload::finalize`):
//!
//! - **The request goes away.** The chunk route runs the append and the deposit it triggers as
//!   one task it awaits but does not own, so a guest who disconnects stops neither.
//! - **The request that completed the session never claimed it.** The process died between the
//!   append's record and the claim. The session is full and unclaimed, and the guest's replay of
//!   any of its chunks deposits it (`crate::upload::chunk::Accepted::Replayed`).
//! - **The process goes away holding the claim.** [`recover`] drives every drop claim boot lists,
//!   through the same steps, before this process has claimed anything.
//!
//! # Every step resumes
//!
//! A re-driven claim may find any prefix of the steps done. A stage already committed is found
//! at its address instead, re-hashed there, and the commit is skipped. The declaration is moved
//! into the inbox by
//! one store operation ([`DropStore::file`](super::DropStore::file)), so it is either still held
//! — and filed now — or gone because it was filed; it is never taken and lost between the two,
//! which would leave bytes the owner can never decrypt. Only the status write is left, and it is
//! idempotent.
//!
//! # A failure is terminal, and gives the link back
//!
//! A deposit that cannot file the drop leaves the session `FailedProcessing`, drops the stage,
//! and refunds the link the reservation the drop took — a drop that never landed must not spend
//! a guest's caps. The one exception is the final status write: the drop is already filed, so
//! failing it would tell the guest a lie. That session stays claimed for the next boot's
//! recovery, which finds the drop filed and completes it.

use capsule_core::crypto::hash::Sha256Hasher;

use super::DropContext;
use crate::blob::{BlobError, ContentAddress};
use crate::store::{FinalizeClaim, StoreError, UploadId, UploadSessionRecord, UploadSessionStatus};
use crate::upload::finalize::Recovery;

/// What a deposit did, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// This caller claimed the session, the drop is in its owner's inbox, and the session is
    /// `Completed`.
    Filed,
    /// Another caller holds the claim and is driving the deposit, or the session already left the
    /// active states. Not a failure: the chunk that triggered this was accepted.
    AlreadyClaimed,
    /// No such session.
    NotFound,
}

/// Why a claimed deposit did not complete.
#[derive(Debug, thiserror::Error)]
pub enum DepositFailure {
    /// The bytes do not hash to the address the guest declared. Nothing is committed.
    #[error("the drop's bytes hash to {actual}, not to the declared {expected}")]
    ContentHashMismatch {
        /// What the guest declared at creation.
        expected: String,
        /// What the bytes actually hash to.
        actual: String,
    },
    /// The stage is not the session's declared length, or neither the stage nor a committed blob
    /// of that length is there. The server's own inconsistency, never the guest's fault.
    #[error("the staged drop is {on_disk:?} bytes where the session recorded {expected}")]
    StorageInconsistent {
        /// The session's declared length.
        expected: u64,
        /// The stage's length, or `None` when there is no stage at all.
        on_disk: Option<u64>,
    },
    /// A collaborator could not answer.
    #[error("the drop could not be deposited: {0}")]
    Unavailable(String),
}

/// Claim `upload`'s deposit and drive it to a terminal state.
///
/// Called for a session that holds every declared byte — by the chunk that completed it, or by a
/// replay on it. Total over its inputs: it files the drop, hands back one of the two non-winning
/// outcomes, or leaves the session terminal and returns why.
#[tracing::instrument(skip(drops), fields(upload = %upload))]
pub async fn deposit(drops: &DropContext, upload: &UploadId) -> Result<Outcome, DepositFailure> {
    let record = match drops
        .sessions()
        .claim_finalize(upload)
        .await
        .map_err(unavailable("claim the deposit"))?
    {
        FinalizeClaim::Won(record) => record,
        FinalizeClaim::AlreadyClaimed => {
            tracing::info!(%upload, "the deposit was already claimed; this caller stands down");
            return Ok(Outcome::AlreadyClaimed);
        }
        FinalizeClaim::NotFound => {
            tracing::info!(%upload, "the deposit found no session to claim");
            return Ok(Outcome::NotFound);
        }
    };
    drive(drops, &record).await?;
    Ok(Outcome::Filed)
}

/// Drive every guest-drop session in `stranded` — claims whose depositor is gone — to a
/// terminal state.
///
/// `stranded` is what [`claimed`](crate::store::UploadSessionStore::claimed) listed before this
/// process accepted a request, so no live depositor holds any of them. Album sessions in it are
/// skipped: [`crate::upload::finalize::recover`] drives those. Sequential, for the reason the
/// album recovery is.
#[tracing::instrument(skip_all, fields(stranded = stranded.len()))]
pub async fn recover(drops: &DropContext, stranded: Vec<UploadSessionRecord>) -> Recovery {
    let mut recovery = Recovery::default();
    for record in stranded
        .into_iter()
        .filter(UploadSessionRecord::is_guest_drop)
    {
        let upload = record.upload_id.clone();
        tracing::info!(%upload, "re-driving a drop deposit whose depositor is gone");
        match drive(drops, &record).await {
            Ok(()) => recovery.completed.push(upload),
            Err(failure) => {
                tracing::warn!(%upload, %failure, "a recovered deposit did not complete");
                recovery.failed.push(upload);
            }
        }
    }
    tracing::info!(
        completed = recovery.completed.len(),
        failed = recovery.failed.len(),
        "recovered the drop deposits a previous process left"
    );
    recovery
}

/// Commit and file a session whose claim this caller holds, then leave it terminal.
async fn drive(drops: &DropContext, record: &UploadSessionRecord) -> Result<(), DepositFailure> {
    let upload = &record.upload_id;
    if let Err(failure) = file(drops, record).await {
        fail(drops, upload).await;
        tracing::warn!(%upload, %failure, "the deposit refused the drop");
        return Err(failure);
    }
    // The drop is filed: a status write that fails here leaves the claim for the next boot's
    // recovery, which finds the drop filed and completes it, rather than failing a drop that
    // landed.
    drops
        .sessions()
        .set_status(upload, UploadSessionStatus::Completed)
        .await
        .map_err(unavailable("close a filed drop's session"))?;
    tracing::info!(%upload, size = record.total_size, "a guest drop was deposited");
    Ok(())
}

/// Commit the stage, unless a lost depositor already did, and file the declaration.
async fn file(drops: &DropContext, record: &UploadSessionRecord) -> Result<(), DepositFailure> {
    let upload = &record.upload_id;
    let address = ContentAddress::parse(&record.expected_hash).map_err(|error| {
        // Unreachable while the create's gate holds: a drop's hash is an address or the session
        // was never opened.
        DepositFailure::Unavailable(format!("the session's hash is not an address: {error}"))
    })?;

    let staged = drops
        .blobs()
        .staged_len(upload)
        .await
        .map_err(blob_unavailable("measure the staged drop"))?;
    let source = match staged {
        Some(length) if length == record.total_size => Source::Stage(upload),
        Some(length) => {
            return Err(DepositFailure::StorageInconsistent {
                expected: record.total_size,
                on_disk: Some(length),
            });
        }
        None => {
            let committed = drops
                .blobs()
                .stat(&address)
                .await
                .map_err(blob_unavailable("stat the drop's content address"))?;
            if committed.is_none_or(|stat| stat.size != record.total_size) {
                return Err(DepositFailure::StorageInconsistent {
                    expected: record.total_size,
                    on_disk: None,
                });
            }
            tracing::info!(
                %upload,
                %address,
                "the drop was committed before its depositor was lost; resuming after the commit"
            );
            Source::Committed(&address)
        }
    };

    // The bytes are the bytes the guest declared, or nothing is committed: a content address is
    // shared by every holder of those bytes, so placing other bytes there corrupts every asset
    // that will ever name it, not only this drop (web-upload.md: the hash runs at the drop's
    // finalization, and adoption relies on it).
    let actual = recompute_hash(drops, source, record.total_size).await?;
    if actual != record.expected_hash {
        return Err(DepositFailure::ContentHashMismatch {
            expected: record.expected_hash.clone(),
            actual,
        });
    }
    if let Source::Stage(_) = source {
        drops
            .blobs()
            .commit(upload, &address)
            .await
            .map_err(blob_unavailable("commit the staged drop"))?;
    }

    let filed = drops
        .drops()
        .file(upload, &address, drops.clock().now())
        .await
        .map_err(unavailable("file the drop in its owner's inbox"))?;
    if filed.is_none() {
        // Moved in one operation, so a declaration that is gone was filed — by the depositor this
        // claim outlived.
        tracing::info!(%upload, "the drop was filed before its depositor was lost");
    }
    Ok(())
}

/// How many bytes one read of the hash takes, so a deposit holds one window and never the drop.
const HASH_WINDOW_BYTES: usize = 1024 * 1024;

/// Where the bytes a claimed deposit files are read from.
#[derive(Debug, Clone, Copy)]
enum Source<'a> {
    /// The session's own stage: the ordinary case.
    Stage(&'a UploadId),
    /// The blob at the drop's content address: a depositor that was lost committed it.
    Committed(&'a ContentAddress),
}

/// The lowercase-hex SHA-256 of the `expected_len` bytes `source` names, read window by window.
async fn recompute_hash(
    drops: &DropContext,
    source: Source<'_>,
    expected_len: u64,
) -> Result<String, DepositFailure> {
    let mut hasher = Sha256Hasher::new();
    let mut offset = 0_u64;
    loop {
        let window = match source {
            Source::Stage(upload) => drops
                .blobs()
                .read_staged_at(upload, offset, HASH_WINDOW_BYTES)
                .await
                .map_err(blob_unavailable("read the staged drop"))?,
            Source::Committed(address) => drops
                .blobs()
                .read_at(address, offset, HASH_WINDOW_BYTES)
                .await
                .map_err(blob_unavailable("read the committed drop"))?,
        }
        .ok_or(DepositFailure::StorageInconsistent {
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
        return Err(DepositFailure::StorageInconsistent {
            expected: expected_len,
            on_disk: Some(offset),
        });
    }
    tracing::debug!(
        ?source,
        bytes = offset,
        "recomputed a drop's ciphertext hash"
    );
    Ok(hasher.finalize().to_hex())
}

/// Leave a claimed session whose drop was not filed terminal: the stage goes, the link gets its
/// reservation back, and the session is `FailedProcessing`. Each step is logged rather than
/// failed — the caller is already reporting the failure that brought it here.
async fn fail(drops: &DropContext, upload: &UploadId) {
    match drops.blobs().abandon(upload).await {
        Ok(dropped) => tracing::debug!(%upload, dropped, "dropped the staged drop"),
        Err(error) => {
            tracing::error!(%upload, %error, "a failed drop's stage could not be dropped");
        }
    }
    match drops.drops().take_reservation(upload).await {
        Ok(Some(pending)) => {
            if let Err(error) = drops.drops().refund(&pending.opaque_id, pending.size).await {
                tracing::error!(%upload, %error, "a failed drop's reservation could not be refunded");
            }
        }
        Ok(None) => tracing::debug!(%upload, "a failed drop held no declaration to give back"),
        Err(error) => {
            tracing::error!(%upload, %error, "a failed drop's declaration could not be released");
        }
    }
    if let Err(error) = drops
        .sessions()
        .set_status(upload, UploadSessionStatus::FailedProcessing)
        .await
    {
        tracing::error!(%upload, %error, "a failed drop's session could not be marked");
    }
}

/// A store failure, named by what was being attempted.
fn unavailable(doing: &'static str) -> impl Fn(StoreError) -> DepositFailure {
    move |error| {
        tracing::error!(%error, operation = doing, "a store could not answer for a deposit");
        DepositFailure::Unavailable(format!("could not {doing}"))
    }
}

/// A blob-store failure, named by what was being attempted.
fn blob_unavailable(doing: &'static str) -> impl Fn(BlobError) -> DepositFailure {
    move |error| {
        tracing::error!(%error, operation = doing, "the blob store could not answer for a deposit");
        DepositFailure::Unavailable(format!("could not {doing}"))
    }
}
