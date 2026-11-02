//! [`RemotePuller`]: the production [`Puller`] — one pull of one federated album, from its home
//! server, through re-validation, into [`PulledAlbums`].
//!
//! # One pull, in order
//!
//! 1. **The credential.** Of every subscription feeding the job, the live one whose capability
//!    lives longest ([`SubscriptionStore::of_job`]). None live: a transport failure, since the
//!    peer did nothing wrong.
//! 2. **The refresh.** A capability with less than [`REFRESH_AHEAD`] left is exchanged for its
//!    successor (`POST /v1/federation/capabilities/refresh`). The successor is a peer's value
//!    like any other: it is verified against the issuer's pinned key, for this server and this
//!    album, before it is kept, and it is kept through [`SubscriptionStore::put`], where the
//!    longer-lived token wins — so a replayed answer is harmless. A refusal while the current
//!    token is still live is logged and the pull goes on with it.
//! 3. **The pages.** From the album's last cursor, at most [`Rules::max_pages`] pages of at most
//!    [`Rules::page_size`] entries. Each page is screened ([`screen_page`]), then the metadata
//!    blobs it commits to are fetched and checked ([`check_metadata_blob`]), then the page is
//!    committed as one act. A page that fails writes nothing; pages before it stay written.
//! 4. **The report.** The time spent validating — screening, blob checks, the successor's
//!    verification — is what [`PullReport::validation`] carries, so the worker charges the
//!    peer's CPU budget for exactly the work its input cost.
//!
//! # What a failure is
//!
//! A peer that could not be reached, failed on its side, or refused this server is a
//! [`PullFailure::Transport`]: it backs off and costs the peer nothing. A peer whose answer broke
//! the contract or a rule — a body that does not decode or exceeds its bound, a redirect, an
//! entry that fails re-validation, a successor that does not verify — is a
//! [`PullFailure::Validation`], which spends its error budget.
//!
//! Every URL is built from the job's peer id ([`origin_of`]); no value a peer sends chooses a
//! URL, a local path, or a local account.

use std::sync::Arc;
use std::time::{Duration, Instant};

use jiff::SignedDuration;
use tracing::instrument;

use super::pulled::PulledAlbums;
use super::remote::{PeerClient, RemoteError, origin_of, rest};
use super::signer::RequestSigner;
use super::validate::{
    Context, Refused, Rules, check_metadata_blob, metadata_read_limit, screen_page,
};
use super::{JobKey, PullFailure, PullReport, Puller};
use crate::federation::subscriptions::{
    CapabilityToken, MAX_CAPABILITY_BYTES, Subscription, SubscriptionStore,
};
use crate::federation::{PeerId, PeerStore, PeerTier, verify_received};
use crate::store::Clock;
use crate::upload::UploadPolicy;

/// How long before its capability lapses a pull refreshes it.
///
/// Six hours, the back-off cap: a job backed off as far as it goes still wakes to a token it can
/// present. A capability lives at most 24 hours, so this refreshes about every 18.
pub const REFRESH_AHEAD: SignedDuration = SignedDuration::from_hours(6);

/// The most bytes a refresh answer may be: a token under a kibibyte and three short fields.
pub const MAX_REFRESH_BYTES: u64 = 16 * 1024;

/// What [`RemotePuller::new`] is built from.
#[derive(Debug)]
pub struct RemotePullerParts {
    /// Where each job's capabilities are, and where a refreshed one is kept.
    pub subscriptions: Arc<dyn SubscriptionStore>,
    /// The peers, for the issuer's pinned key a successor is verified under.
    pub peers: Arc<dyn PeerStore>,
    /// Where what verifies is written.
    pub pulled: Arc<dyn PulledAlbums>,
    /// This server's operational key, which signs every presentation.
    pub signer: Arc<dyn RequestSigner>,
    /// What executes a request: the reqwest backend over
    /// [`egress_client`](super::remote::egress_client) in production.
    pub backend: Arc<dyn rest::HttpBackend>,
    /// The clock every signature and every check reads.
    pub clock: Arc<dyn Clock>,
    /// The rules this server applies to its own writes, re-applied to pulled ones.
    pub policy: UploadPolicy,
    /// This server's own identity: the `sub` a successor must name.
    pub server_id: PeerId,
}

/// The production [`Puller`].
#[derive(Debug)]
pub struct RemotePuller {
    parts: RemotePullerParts,
}

impl RemotePuller {
    /// A puller over `parts`.
    pub fn new(parts: RemotePullerParts) -> Self {
        Self { parts }
    }

    #[instrument(skip(self), fields(job = %job, tier = tier.as_str()))]
    async fn run(&self, job: &JobKey, tier: PeerTier) -> PullReport {
        let started = Instant::now();
        let mut validation = Duration::ZERO;
        let result = self.pull_job(job, tier, &mut validation).await;
        match &result {
            Ok(pages) => tracing::info!(
                pages,
                validation_ms = validation.as_millis(),
                elapsed_ms = started.elapsed().as_millis(),
                "pulled"
            ),
            Err(failure) => tracing::warn!(
                kind = failure.kind(),
                failure = ?failure,
                validation_ms = validation.as_millis(),
                elapsed_ms = started.elapsed().as_millis(),
                "the pull failed"
            ),
        }
        PullReport {
            result: result.map(|_| ()),
            validation,
        }
    }

    /// The pull; the pages committed when it succeeds.
    async fn pull_job(
        &self,
        job: &JobKey,
        tier: PeerTier,
        validation: &mut Duration,
    ) -> Result<u32, PullFailure> {
        let parts = &self.parts;
        let rules = Rules::for_tier(tier);
        let origin = origin_of(&job.peer).map_err(transport)?;
        let client = PeerClient::new(
            parts.backend.clone(),
            origin,
            parts.signer.clone(),
            parts.clock.clone(),
        );
        let credential = self.credential(job, &client, validation).await?;
        let token = credential.token.expose();

        let mut state =
            parts.pulled.read(job).await.map_err(|error| {
                transport(format!("the pulled album could not be read: {error}"))
            })?;
        let mut pages = 0;
        while pages < rules.max_pages {
            let page = match client
                .sync_page(
                    token,
                    &job.album,
                    state.cursor.as_deref(),
                    rules.page_size,
                    rules.max_page_bytes,
                )
                .await
            {
                Ok(page) => page,
                Err(RemoteError::Refused {
                    status: 409,
                    code: Some(code),
                }) if code == capsule_i18n::error_codes::SYNC_CURSOR_STALE => {
                    // The home server's index was rebuilt. Start over from zero next time,
                    // keeping every head this server accepted to re-validate against.
                    parts.pulled.restart(job).await.map_err(|error| {
                        transport(format!("a stale cursor could not be dropped: {error}"))
                    })?;
                    return Err(transport(
                        "the home server's feed restarted; the next pull resyncs from zero",
                    ));
                }
                Err(error) => return Err(remote(error)),
            };
            let received = page.entries.len();
            let context = Context {
                album: &job.album,
                pin: &credential.min_protocol_version,
                policy: &parts.policy,
                now: parts.clock.now(),
            };
            let screening = Instant::now();
            let screened = screen_page(page, &state, &context, &rules);
            *validation += screening.elapsed();
            let screened = match screened {
                Ok(screened) => screened,
                Err(Refused::Invalid(invalid)) => return Err(invalid_input(invalid.to_string())),
                Err(newer @ Refused::Newer { .. }) => return Err(transport(newer.to_string())),
            };

            let mut page = screened.page;
            for need in &screened.needed {
                let bytes = client
                    .blob(
                        token,
                        &need.hash.to_hex(),
                        metadata_read_limit(need, &rules),
                    )
                    .await
                    .map_err(remote)?;
                let checking = Instant::now();
                let checked = check_metadata_blob(need, &bytes, &rules);
                *validation += checking.elapsed();
                checked.map_err(|invalid| invalid_input(invalid.to_string()))?;
                page.metadata_blobs.insert(need.hash, bytes);
            }

            let accepted = page.entries.len();
            let next = page.state.clone();
            match parts.pulled.commit(job, state.high_water, page).await {
                Ok(true) => {}
                Ok(false) => {
                    return Err(transport(
                        "the album moved while this page was validated; nothing was written",
                    ));
                }
                Err(error) => {
                    return Err(transport(format!("the page could not be written: {error}")));
                }
            }
            pages += 1;
            tracing::debug!(
                page = pages,
                received,
                accepted,
                metadata_blobs = screened.needed.len(),
                high_water = ?next.high_water,
                "a pulled page was written"
            );
            state = next;
            if !screened.has_more || received == 0 {
                break;
            }
        }
        Ok(pages)
    }

    /// The subscription whose capability this pull presents, refreshed when it is near its end.
    async fn credential(
        &self,
        job: &JobKey,
        client: &PeerClient,
        validation: &mut Duration,
    ) -> Result<Subscription, PullFailure> {
        let parts = &self.parts;
        let now = parts.clock.now();
        let subscriptions =
            parts.subscriptions.of_job(job).await.map_err(|error| {
                transport(format!("the subscriptions could not be read: {error}"))
            })?;
        let Some(live) = subscriptions
            .into_iter()
            .filter(|subscription| subscription.is_live(now))
            .max_by_key(|subscription| subscription.expires_at)
        else {
            return Err(transport("no subscription holds a live capability"));
        };
        if live.expires_at.duration_since(now) > REFRESH_AHEAD {
            return Ok(live);
        }

        let refreshed = match client.refresh(live.token.expose(), MAX_REFRESH_BYTES).await {
            Ok(refreshed) => refreshed,
            Err(RemoteError::Malformed(detail)) => {
                return Err(invalid_input(format!("the refresh answer: {detail}")));
            }
            Err(error) => {
                tracing::warn!(%error, jti = %live.jti, expires_at = %live.expires_at, "the capability could not be refreshed; pulling with the current one");
                return Ok(live);
            }
        };

        let key = match parts.peers.read(&job.peer).await {
            Ok(Some(record)) => record.signing_key,
            Ok(None) => None,
            Err(error) => {
                return Err(transport(format!(
                    "the peer store could not answer: {error}"
                )));
            }
        };
        let Some(key) = key else {
            tracing::warn!(
                "the peer has no pinned key; a refreshed capability cannot be verified, and is not kept"
            );
            return Ok(live);
        };
        if refreshed.token.is_empty() || refreshed.token.len() > MAX_CAPABILITY_BYTES {
            return Err(invalid_input(
                "the refreshed capability is outside the bound a subscription keeps",
            ));
        }
        let verifying = Instant::now();
        let verified = verify_received(&refreshed.token, &job.peer, &key, &parts.server_id, now);
        *validation += verifying.elapsed();
        let grant = verified
            .map_err(|error| invalid_input(format!("the refreshed capability: {error}")))?;
        if grant.album != job.album || grant.jti != refreshed.jti {
            return Err(invalid_input(
                "the refreshed capability is for another album, or not the one the answer names",
            ));
        }
        let successor = Subscription {
            subscriber: live.subscriber.clone(),
            peer: job.peer.clone(),
            album: grant.album,
            scope: grant.scope,
            jti: grant.jti,
            token: CapabilityToken::new(refreshed.token),
            issued_at: grant.issued_at,
            expires_at: grant.expires_at,
            min_protocol_version: grant.min_protocol_version,
            subscribed_at: live.subscribed_at,
            updated_at: now,
        };
        match parts.subscriptions.put(successor.clone()).await {
            Ok(outcome) => {
                tracing::info!(
                    outcome = outcome.as_str(),
                    jti = %outcome.subscription().jti,
                    expires_at = %outcome.subscription().expires_at,
                    replayed = refreshed.replayed,
                    "refreshed the capability"
                );
                Ok(outcome.subscription().clone())
            }
            Err(error) => {
                // The predecessor is revoked the moment the successor is issued, so this pull
                // goes on with the successor it holds; the next refresh replays the same one.
                tracing::error!(%error, jti = %successor.jti, "a refreshed capability could not be kept");
                Ok(successor)
            }
        }
    }
}

impl Puller for RemotePuller {
    fn pull<'a>(
        &'a self,
        job: &'a JobKey,
        tier: PeerTier,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PullReport> + Send + 'a>> {
        Box::pin(self.run(job, tier))
    }
}

fn transport(detail: impl Into<String>) -> PullFailure {
    PullFailure::Transport {
        detail: detail.into(),
    }
}

fn invalid_input(detail: impl Into<String>) -> PullFailure {
    PullFailure::Validation {
        detail: detail.into(),
    }
}

/// A call's failure as a pull's: the peer's malformed answer spends its error budget, anything
/// else only backs off.
fn remote(error: RemoteError) -> PullFailure {
    match error {
        RemoteError::Malformed(detail) => invalid_input(detail),
        other => transport(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
