//! The startup scrub of staged uploads: the pass a boot runs before boot recovery.
//!
//! transfer/upload.md, Session Lifetime and Discard: *on boot, before boot recovery, a staged
//! file with no session is deleted, and a `Pending` or `Uploading` session whose staged file is
//! missing is discarded.* Both directions repair the one pairing a session's creation and its
//! end write in two steps — a stage and a record — that a process can die between.
//!
//! # The two directions
//!
//! - **A stage with no session** is an orphan: creation began the stage and died before it
//!   opened the record, or an end dropped the record and died before the stage. Nothing can
//!   ever write to it again, so it is abandoned. A stage is kept whenever the store **holds**
//!   its session, in any status: a claimed one is finalization's, a terminal one is left to its
//!   cap, and an evictable one is somebody's transfer.
//! - **An evictable session with no stage** cannot take another byte: every write would answer
//!   `500 error.upload.storage_inconsistent` until its cap ended it. It is discarded as a
//!   cancellation is — under its admission turn, its reservation given back — so its client
//!   reads the uniform `404` and re-creates. There is no length check on a stage that *is*
//!   there: it is written positionally, so its length says nothing about what is committed.
//!
//! `WaitingForProcessing` and terminal sessions are never discarded here. The discard is the
//! store's compare-and-set [`UploadSessionStore::discard_unclaimed`], so a session claimed
//! since it was listed is left to its finalizer.
//!
//! # Nothing it cannot read is repaired
//!
//! A blob store that cannot list its stages skips the whole pass: without the listing, a
//! session's stage cannot be told missing from present. A session store that cannot list its
//! evictable sessions skips only the discards; one that cannot read a stage's session keeps that
//! stage. Each skip is logged and is not fatal — what it would have repaired is still there for
//! the next boot.
//!
//! [`UploadSessionStore::discard_unclaimed`]: crate::store::UploadSessionStore::discard_unclaimed

use std::collections::BTreeSet;

use jiff::Timestamp;

use crate::store::{UnclaimedDiscard, UploadId};
use crate::upload::UploadContext;
use crate::upload::chunk::admission_turn;

/// What one startup scrub repaired.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupScrub {
    /// The stages abandoned because the store holds no session for them, in ascending order.
    pub abandoned: Vec<UploadId>,
    /// The evictable sessions discarded because their stage was missing, in the order the
    /// store listed them.
    pub discarded: Vec<UploadId>,
}

/// Run the startup scrub over `context`'s stores.
///
/// Call it before boot recovery and before the listener accepts a request: it reads every
/// stage and every evictable session once and assumes nothing creates or ends one meanwhile.
/// Never fails; every store error is logged and leaves what it touched for the next boot.
#[tracing::instrument(skip_all)]
pub async fn startup(context: &UploadContext) -> StartupScrub {
    let mut report = StartupScrub::default();
    let staged: BTreeSet<UploadId> = match context.blobs().staged().await {
        Ok(staged) => staged.into_iter().collect(),
        Err(error) => {
            tracing::error!(
                %error,
                "could not list the staged uploads; the startup scrub waits for the next boot"
            );
            return report;
        }
    };
    tracing::debug!(staged = staged.len(), "listed the staged uploads");

    abandon_orphans(context, &staged, &mut report).await;
    discard_unstaged(context, &staged, &mut report).await;

    tracing::info!(
        staged = staged.len(),
        abandoned = report.abandoned.len(),
        discarded = report.discarded.len(),
        "scrubbed the staged uploads"
    );
    report
}

/// Abandon every stage in `staged` whose session the store does not hold.
async fn abandon_orphans(
    context: &UploadContext,
    staged: &BTreeSet<UploadId>,
    report: &mut StartupScrub,
) {
    for upload in staged {
        match context.sessions().read(upload).await {
            Ok(Some(record)) => {
                tracing::trace!(
                    upload_id = %upload,
                    status = record.status.as_str(),
                    "a stage's session is held; kept"
                );
            }
            Ok(None) => match context.blobs().abandon(upload).await {
                Ok(_) => {
                    tracing::info!(upload_id = %upload, "abandoned a stage no session holds");
                    report.abandoned.push(upload.clone());
                }
                Err(error) => {
                    tracing::error!(
                        %error,
                        upload_id = %upload,
                        "an orphaned stage could not be abandoned; it waits for the next boot"
                    );
                }
            },
            Err(error) => {
                tracing::error!(
                    %error,
                    upload_id = %upload,
                    "could not read a stage's session; the stage is kept"
                );
            }
        }
    }
}

/// Discard every evictable session whose stage is not in `staged`, giving its reservation back.
async fn discard_unstaged(
    context: &UploadContext,
    staged: &BTreeSet<UploadId>,
    report: &mut StartupScrub,
) {
    let evictable = match context
        .sessions()
        .least_recently_progressed(Timestamp::MAX, usize::MAX)
        .await
    {
        Ok(evictable) => evictable,
        Err(error) => {
            tracing::error!(
                %error,
                "could not list the evictable sessions; their stages are checked at the next boot"
            );
            return;
        }
    };
    tracing::debug!(evictable = evictable.len(), "listed the evictable sessions");

    for upload in evictable.iter().filter(|upload| !staged.contains(*upload)) {
        // Under the session's admission turn, as a cancellation is: no range is admitted against
        // a session being discarded.
        let turn = admission_turn(upload).await;
        let outcome = context.sessions().discard_unclaimed(upload).await;
        drop(turn);
        match outcome {
            Ok(UnclaimedDiscard::Discarded(record)) => {
                crate::upload::reservation::release_logged(context, &record).await;
                tracing::info!(
                    upload_id = %upload,
                    uploader = %record.upload_user_id,
                    size = record.total_size,
                    received = record.committed.covered(),
                    "discarded a session whose stage is missing"
                );
                report.discarded.push(upload.clone());
            }
            Ok(UnclaimedDiscard::Claimed) => {
                tracing::debug!(upload_id = %upload, "an unstaged session was claimed; left it");
            }
            Ok(UnclaimedDiscard::NotFound) => {
                tracing::debug!(upload_id = %upload, "an unstaged session has already gone");
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    upload_id = %upload,
                    "an unstaged session could not be discarded; it waits for the next boot"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
