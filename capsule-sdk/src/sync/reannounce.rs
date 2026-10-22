//! Re-announcing held manifests after a sync epoch change (#781).
//!
//! When the feed answers a cursor with `409 error.sync.cursor_stale`
//! ([`SyncError::is_stale_cursor`](super::SyncError::is_stale_cursor)), the server rebuilt this
//! library's index and may have lost rows. The client resyncs from zero, and then every asset it
//! holds that the resynced feed does not list is **re-announced**: its chain-head provenance
//! record goes to `POST /v1/sync/reannounce`, manifests only, never bytes
//! ([Server Filesystem § Sync Epoch and Client Re-Announce]).
//!
//! Two pieces, so the orchestration is testable without a server:
//!
//! - [`unlisted`] is the pure walk: the held assets the feed does not list, in the order held;
//! - [`batches`] cuts them into requests the server accepts, by count and by size;
//! - [`SyncConsumer::reannounce`] sends one batch through the generated operation.
//!
//! [`reannounce_unlisted`] is the three together. What the server answers is per manifest: a
//! `missing_blobs` answer is the caller's cue to re-upload through the transfer protocol, which
//! is the push path's to do, never this module's.
//!
//! [Server Filesystem § Sync Epoch and Client Re-Announce]: https://docs/design/filesystem/server/#sync-epoch-and-client-re-announce

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tracing::instrument;

use super::{SyncConsumer, SyncError};
use crate::rest;

/// The most manifests one request carries: half the server's per-request ceiling, so a server
/// that lowers it within reason still accepts a full batch.
pub const MAX_BATCH_MANIFESTS: usize = 32;

/// The most manifest bytes one request carries, before base64.
///
/// The route sits behind the server's bounded body cap (one mebibyte); base64 inflates by a
/// third and JSON adds a little, so half of it keeps every batch well under the cap.
pub const MAX_BATCH_BYTES: usize = 512 * 1024;

/// One asset the client holds: its id and its chain head's provenance record, the exact bytes
/// an upload sends as the `provenance` blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldManifest {
    /// The asset id, as the feed spells it.
    pub asset_id: String,
    /// The chain head's provenance record.
    pub manifest: Vec<u8>,
}

/// What the server did with one re-announced manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReannounceOutcome {
    /// The server lacked the row and now holds it, published at `sync_seq`.
    Restored {
        /// The position the restored row was published at, when the server reported one.
        sync_seq: Option<u64>,
    },
    /// The server already holds the row.
    AlreadyIndexed,
    /// The server's store lacks these blobs; re-upload them through the transfer protocol.
    MissingBlobs(Vec<String>),
    /// Refused, with the stable `error.sync.reannounce_*` code.
    Refused {
        /// The code the client localizes.
        code: String,
    },
}

/// One manifest's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReannounceResult {
    /// The asset the manifest names, when the server could read one.
    pub asset_id: Option<String>,
    /// What happened.
    pub outcome: ReannounceOutcome,
}

/// The held assets `listed` does not name, in the order held: what to re-announce.
#[must_use]
pub fn unlisted<'a, S: std::hash::BuildHasher>(
    held: &'a [HeldManifest],
    listed: &HashSet<String, S>,
) -> Vec<&'a HeldManifest> {
    held.iter()
        .filter(|manifest| !listed.contains(&manifest.asset_id))
        .collect()
}

/// Cut `manifests` into requests of at most [`MAX_BATCH_MANIFESTS`] manifests and
/// [`MAX_BATCH_BYTES`] bytes, in order.
///
/// A single manifest larger than the byte budget rides alone: the server answers it (it is far
/// past any real manifest, so the answer is `malformed`), which is better than never sending it.
#[must_use]
pub fn batches<'a>(manifests: &[&'a HeldManifest]) -> Vec<Vec<&'a HeldManifest>> {
    let mut out: Vec<Vec<&HeldManifest>> = Vec::new();
    let mut current: Vec<&HeldManifest> = Vec::new();
    let mut bytes = 0_usize;
    for manifest in manifests {
        let size = manifest.manifest.len();
        if !current.is_empty()
            && (current.len() == MAX_BATCH_MANIFESTS || bytes + size > MAX_BATCH_BYTES)
        {
            out.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += size;
        current.push(manifest);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

impl SyncConsumer {
    /// Re-announce one batch of manifests: `POST /v1/sync/reannounce`.
    ///
    /// One answer per manifest, in order. Refreshes once on a `401`, as a pull does.
    ///
    /// # Errors
    ///
    /// [`SyncError::Rejected`] with the server's code when it refused the request as a whole
    /// (an empty or oversized batch, a collaborator that could not answer, the protocol gate),
    /// [`SyncError::Transport`] when the request did not complete, and
    /// [`SyncError::MalformedManifest`] when the server answered a different number of results
    /// than manifests sent.
    #[instrument(skip(self, manifests), fields(manifests = manifests.len()))]
    pub async fn reannounce(
        &self,
        manifests: &[&HeldManifest],
    ) -> Result<Vec<ReannounceResult>, SyncError> {
        let body = rest::types::ReannounceRequest {
            manifests: manifests
                .iter()
                .map(|held| BASE64.encode(&held.manifest))
                .collect(),
        };
        let response = match self.call_reannounce(&body).await {
            Ok(response) => response,
            Err(error) if is_unauthenticated(&error) => match &self.auth {
                super::SyncAuth::Session(session) => {
                    tracing::info!("re-announce answered 401; refreshing once and retrying");
                    session.refresh().await?;
                    self.call_reannounce(&body).await.map_err(map_error)?
                }
                super::SyncAuth::Static => return Err(map_error(error)),
            },
            Err(error) => return Err(map_error(error)),
        };
        if response.results.len() != manifests.len() {
            return Err(SyncError::MalformedManifest(format!(
                "the server answered {} of {} re-announced manifests",
                response.results.len(),
                manifests.len()
            )));
        }
        Ok(response.results.into_iter().map(decode_result).collect())
    }

    async fn call_reannounce(
        &self,
        body: &rest::types::ReannounceRequest,
    ) -> Result<rest::types::ReannounceResponse, rest::Error<rest::ReannounceManifestsError>> {
        Ok(self
            .client
            .reannounce_manifests(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                None,
                body,
            )
            .await?
            .into_inner())
    }
}

/// Re-announce every held asset `listed` does not name, batch by batch, and return each answer
/// paired with the asset the client sent it for.
///
/// # Errors
///
/// The first batch [`SyncConsumer::reannounce`] could not complete. Batches before it were
/// answered and their restores stand; re-running re-announces what is still unlisted, and the
/// server answers a restored asset `already_indexed`.
#[instrument(skip(consumer, held, listed), fields(held = held.len(), listed = listed.len()))]
pub async fn reannounce_unlisted<S: std::hash::BuildHasher>(
    consumer: &SyncConsumer,
    held: &[HeldManifest],
    listed: &HashSet<String, S>,
) -> Result<Vec<(String, ReannounceResult)>, SyncError> {
    let pending = unlisted(held, listed);
    tracing::info!(
        unlisted = pending.len(),
        "re-announcing what the feed no longer lists"
    );
    let mut answers = Vec::with_capacity(pending.len());
    for batch in batches(&pending) {
        let results = consumer.reannounce(&batch).await?;
        answers.extend(batch.iter().map(|held| held.asset_id.clone()).zip(results));
    }
    Ok(answers)
}

/// Whether the route refused the credential.
fn is_unauthenticated(error: &rest::Error<rest::ReannounceManifestsError>) -> bool {
    matches!(
        error,
        rest::Error::Api(response)
            if matches!(response.inner(), rest::ReannounceManifestsError::Status401(_))
    )
}

/// Map the generated operation's error onto [`SyncError`], keeping the stable code.
fn map_error(error: rest::Error<rest::ReannounceManifestsError>) -> SyncError {
    use rest::ReannounceManifestsError as Refusal;
    match error {
        rest::Error::Api(response) => {
            let (code, message) = match response.into_inner() {
                Refusal::Status400(problem)
                | Refusal::Status401(problem)
                | Refusal::Status403(problem)
                | Refusal::Status413(problem)
                | Refusal::Status415(problem)
                | Refusal::Status422(problem)
                | Refusal::Status426(problem)
                | Refusal::Status500(problem) => (
                    Some(problem.code.clone()),
                    problem.detail.clone().unwrap_or_default(),
                ),
            };
            SyncError::Rejected { code, message }
        }
        other => SyncError::Transport(other.to_string()),
    }
}

/// One wire answer, as the SDK speaks it.
fn decode_result(result: rest::types::ReannounceResult) -> ReannounceResult {
    let outcome = match result.outcome {
        rest::types::ReannounceOutcome::Restored => ReannounceOutcome::Restored {
            sync_seq: result.sync_seq.and_then(|seq| u64::try_from(seq).ok()),
        },
        rest::types::ReannounceOutcome::AlreadyIndexed => ReannounceOutcome::AlreadyIndexed,
        rest::types::ReannounceOutcome::MissingBlobs => {
            ReannounceOutcome::MissingBlobs(result.missing.into_iter().flatten().collect())
        }
        rest::types::ReannounceOutcome::Refused => ReannounceOutcome::Refused {
            code: result.code.unwrap_or_default(),
        },
    };
    ReannounceResult {
        asset_id: result.asset_id,
        outcome,
    }
}

#[cfg(test)]
mod tests;
