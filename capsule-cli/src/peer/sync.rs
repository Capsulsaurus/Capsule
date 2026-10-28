//! `capsule peer offer` and `capsule peer pull` — sync between two devices of one account over
//! the LAN peering channel ([Peering § Sync]).
//!
//! - **`peer offer`** (the device ahead): dials the other device's `peer pull --listen`, cuts a
//!   delta-scoped backup artifact from its managed assets ([`Workspace::peer_export`]) sealed
//!   under the session's artifact secret, and serves it over the channel, authorizing each of the
//!   peer's requests against the directory the server publishes at that moment.
//! - **`peer pull`** (the device behind): loads the tombstones this device persisted
//!   ([`crate::peering_state::load_tombstones`]) **before** anything touches the network, and
//!   refuses the run when they cannot be read (fail closed: an empty set would let a peer
//!   resurrect a purged asset). It then admits one verified session, pulls every artifact the
//!   peer lists, ingests each through [`capsule_sdk::peering::ingest`] against this library's
//!   heads and those tombstones, and writes every applied asset
//!   ([`Workspace::apply_peer_restored`]).
//!
//! The channel carries no offer exchange yet, so the offering device cannot learn what the
//! receiver holds: it sends every managed asset (or the ones `--asset` names), and the
//! receiver's ingest makes each one it already holds at that head a no-op (`identical`).
//!
//! [Peering § Sync]: https://docs/design/peering/#sync

use std::collections::{BTreeMap, BTreeSet};

use capsule_core::crypto::hash::Hash32;
use capsule_library::lifecycle::Workspace;
use capsule_sdk::fetch::FetchScope;
use capsule_sdk::peering::{
    DelegationError, DeltaExport, DirectoryVersions, HandshakeLimiter, LocalLibrary, LocalPeer,
    Offering, PeerClient, PeerRestore, PeeringError, PinnedTrust, TrustSource,
    build_delta_artifact, ingest, pull_over_channel, serve,
};
use sea_orm::ConnectionTrait;
use serde::Serialize;
use tokio::net::TcpStream;
use tracing::instrument;
use uuid::Uuid;

use super::{
    Admission, Admitted, PeerError, PeerFailure, PeerListener, PublishedTrust, dial, pinned_trust,
};
use crate::peering_state::{DbDirectoryVersions, load_tombstones};
use crate::remote::{self, RemoteConfig};
use crate::session::SessionStore;

/// The library version a peer's delta artifact names as its source.
const SOURCE_LIBRARY_VERSION: &str = "1";

// ─── peer offer ──────────────────────────────────────────────────────────────

/// What `capsule peer offer` sends, and to whom.
#[derive(Debug, Clone)]
pub struct OfferOptions {
    /// The receiving device's `capsule peer pull --listen` address, `HOST:PORT`.
    pub peer: String,
    /// The device the channel must verify, when named.
    pub to: Option<Uuid>,
    /// The assets to send; empty sends every managed asset.
    pub assets: BTreeSet<Uuid>,
}

/// The `capsule peer offer` run, as `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct OfferSummary {
    /// The `--peer` address dialed.
    pub peer: String,
    /// The device the channel verified (`null` when no channel was established).
    pub receiver: Option<Uuid>,
    /// The artifact's content address, once one was built and offered.
    pub artifact: Option<String>,
    /// The artifact's size in bytes (`0` when none was offered).
    pub bytes: u64,
    /// The assets the artifact carries.
    pub assets: Vec<Uuid>,
    /// How many bytes the receiver pulled.
    pub served_bytes: u64,
    /// What could not be offered, and why: a named asset this library does not hold
    /// (`not_held`), the library that could not be exported (`export`), the device at `--peer`
    /// that is not `--to` (`peer_mismatch`), or the channel's or artifact's [`PeeringError`].
    pub failed: Vec<PeerFailure>,
}

/// `capsule peer offer`.
///
/// # Errors
///
/// [`PeerError`] for a run that could not start: no session, or a directory that does not
/// verify or is below this device's floor.
#[instrument(skip_all, fields(peer = %options.peer, assets = options.assets.len()))]
pub async fn offer(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    versions: &DbDirectoryVersions,
    options: &OfferOptions,
) -> Result<OfferSummary, PeerError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = async {
        let trust = pinned_trust(remote, workspace, session.clone(), versions).await?;
        let published = PublishedTrust {
            directory: capsule_sdk::directory::DirectoryClient::new(
                session.clone(),
                &remote.auth_endpoint,
            ),
            user_id: workspace.user_id(),
            user_ik: workspace.user_ik_public(),
            versions,
        };
        let local = LocalPeer {
            device_id: workspace.device_id(),
            signer: workspace.device_signer(),
            trust: &trust,
            versions,
        };
        Ok(offer_over_peer(workspace, local, &published, options).await)
    }
    .await;
    remote::checkpoint(store, &session).await;
    outcome
}

/// The content address a delta is expressed over: the head manifest's ciphertext hash.
fn delta_address(asset: &capsule_core::backup::BackupAsset) -> Option<Hash32> {
    asset
        .provenance
        .last()
        .map(|record| record.manifest.core.ciphertext_hash)
}

/// `peer offer`'s session: dial the receiver, cut the artifact under the session's secret, and
/// serve it, authorizing each request against `trust` now. A failure is reported in the summary;
/// a receiver whose session ends without a byte is still told the session is over.
pub(crate) async fn offer_over_peer<T: TrustSource>(
    workspace: &Workspace,
    local: LocalPeer<'_>,
    trust: &T,
    options: &OfferOptions,
) -> OfferSummary {
    let mut summary = OfferSummary {
        peer: options.peer.clone(),
        receiver: None,
        artifact: None,
        bytes: 0,
        assets: Vec::new(),
        served_bytes: 0,
        failed: Vec::new(),
    };
    let export = match workspace.peer_export() {
        Ok(export) => export,
        Err(error) => {
            tracing::warn!(%error, "peer offer: the library could not be exported");
            summary
                .failed
                .push(PeerFailure::new("library", "export", error.to_string()));
            return summary;
        }
    };
    let held: BTreeSet<Uuid> = export.assets.iter().map(|asset| asset.asset_id).collect();
    for missing in options.assets.difference(&held) {
        summary.failed.push(PeerFailure::new(
            missing.to_string(),
            "not_held",
            "this library holds no such asset",
        ));
    }
    let chosen: Vec<_> = export
        .assets
        .into_iter()
        .filter(|asset| options.assets.is_empty() || options.assets.contains(&asset.asset_id))
        .collect();

    let mut channel = match dial(&options.peer, local).await {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(peer = %options.peer, %error, "peer offer: no verified channel");
            summary.failed.push(PeerFailure::new(
                &options.peer,
                error.kind(),
                error.to_string(),
            ));
            return summary;
        }
    };
    let receiver = channel.peer().device_id;
    summary.receiver = Some(receiver);

    let mut offering = Offering::new();
    if let Some(to) = options.to
        && to != receiver
    {
        let message = format!("the device at {} is {receiver}, not {to}", options.peer);
        tracing::warn!(%message, "peer offer: refusing to offer");
        summary
            .failed
            .push(PeerFailure::new(&options.peer, "peer_mismatch", message));
    } else {
        let delta: BTreeSet<Hash32> = chosen.iter().filter_map(delta_address).collect();
        let input = DeltaExport {
            assets: &chosen,
            delta: &delta,
            // A CLI library holds every original, and so takes a whole artifact.
            scope: FetchScope::MetadataThumbnailsOriginal,
            amks: &export.amks,
            exporter_device: local.device_id,
            source_library_version: SOURCE_LIBRARY_VERSION.to_owned(),
            export_timestamp: jiff::Timestamp::now().to_string(),
        };
        let built = build_delta_artifact(&input, &channel.peer().artifact_secret, local.signer)
            .and_then(|bytes| {
                let len = bytes.len() as u64;
                offering.offer(bytes).map(|address| (address, len))
            });
        match built {
            Ok((address, len)) => {
                tracing::info!(%address, len, assets = chosen.len(), "peer offer: artifact built");
                summary.artifact = Some(address);
                summary.bytes = len;
                summary.assets = chosen.iter().map(|asset| asset.asset_id).collect();
            }
            Err(error) => {
                tracing::warn!(%error, "peer offer: the artifact could not be built");
                summary.failed.push(PeerFailure::new(
                    "artifact",
                    error.kind(),
                    error.to_string(),
                ));
            }
        }
    }

    // Served even when empty, so the receiver's session ends cleanly instead of waiting out its
    // timeout. A receiver acknowledges nothing here: an acknowledgement is a delegation's.
    let served = serve(&mut channel, &offering, trust, local.versions, |_| {
        Err(DelegationError::AckMismatch)
    })
    .await;
    match served {
        Ok(report) => {
            summary.served_bytes = report.served.iter().map(|range| range.len).sum();
        }
        Err(error) => {
            tracing::warn!(%error, "peer offer: the session ended early");
            summary.failed.push(PeerFailure::new(
                receiver.to_string(),
                error.kind(),
                error.to_string(),
            ));
        }
    }
    summary
}

// ─── peer pull ───────────────────────────────────────────────────────────────

/// The `capsule peer pull` run, as `--json` prints it. Every asset an artifact carried lands in
/// exactly one of `applied`, `quarantined`, `tombstoned`, `identical`, `incomplete`, or `failed`
/// (applied by the ingest but not written).
#[derive(Debug, Clone, Default, Serialize)]
pub struct PullSummary {
    /// The device the session verified (`null` when none did).
    pub peer: Option<Uuid>,
    /// The content address of each artifact pulled and verified.
    pub artifacts: Vec<String>,
    /// Assets written into this library: new ones, and forward updates of held ones.
    pub applied: Vec<Uuid>,
    /// Assets whose offered head this library has already superseded or diverged from: the
    /// peer sent stale state, and nothing was written.
    pub quarantined: Vec<Uuid>,
    /// Assets this device holds a purge tombstone for: refused, never resurrected.
    pub tombstoned: Vec<Uuid>,
    /// Assets already held at the offered head.
    pub identical: Vec<Uuid>,
    /// Assets that failed to restore or arrived without the metadata their manifest commits to.
    pub incomplete: Vec<Uuid>,
    /// What failed: an asset the ingest applied that could not be written (`not_written`), an
    /// artifact that could not be pulled or opened, no peer before the listen timeout, or a
    /// listener that failed before a peer was verified.
    pub failed: Vec<PeerFailure>,
    /// The connections refused while listening (off the local network, over the handshake
    /// budget, or a handshake that failed), one entry per source address and kind.
    pub refused: Vec<PeerFailure>,
}

impl PullSummary {
    /// Whether anything failed, so the run exits non-zero. A quarantined, tombstoned or
    /// incomplete asset is the ingest refusing what a peer sent, and is reported, not failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    fn absorb(&mut self, outcome: &PeerRestore) {
        self.quarantined.extend(&outcome.quarantined);
        self.tombstoned.extend(&outcome.tombstoned);
        self.identical.extend(&outcome.identical);
        self.incomplete.extend(&outcome.incomplete);
    }
}

/// The tombstone set a `peer pull` ingests against, or the run's refusal when it cannot be
/// read: the ingest never runs with an empty set in its place.
///
/// # Errors
///
/// [`PeerError::Tombstones`] when the store cannot be read or a row does not decode.
pub async fn ingest_tombstones<C: ConnectionTrait>(db: &C) -> Result<BTreeSet<Uuid>, PeerError> {
    load_tombstones(db).await.map_err(|error| {
        tracing::warn!(%error, "peer pull: the tombstones cannot be read; refusing the ingest");
        PeerError::Tombstones(error.to_string())
    })
}

/// `capsule peer pull`: load the tombstones first (refusing the run when they cannot be read),
/// then admit one verified session on `listener`, pull and ingest what the peer offers, and
/// write what applies.
///
/// # Errors
///
/// [`PeerError::Tombstones`] when the tombstones cannot be read (nothing touches the network),
/// or [`PeerError`] for a run that could not start: no session, or a directory that does not
/// verify or is below this device's floor.
#[instrument(skip_all)]
pub async fn pull<C: ConnectionTrait>(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &mut Workspace,
    db: &C,
    versions: &DbDirectoryVersions,
    listener: &PeerListener,
) -> Result<PullSummary, PeerError> {
    let tombstones = ingest_tombstones(db).await?;
    let session = remote::resume_session(remote, store).await?;
    let outcome = match pinned_trust(remote, workspace, session.clone(), versions).await {
        Ok(trust) => Ok(receive(workspace, &trust, versions, &tombstones, listener).await),
        Err(error) => Err(error),
    };
    remote::checkpoint(store, &session).await;
    outcome
}

/// What one verified session yielded: the peer, and each artifact it listed, pulled or not.
struct Pulled {
    peer: Uuid,
    secret: capsule_sdk::peering::ArtifactSecret,
    artifacts: Vec<(String, Result<Vec<u8>, PeeringError>)>,
}

/// `peer pull`'s session and ingest: admit one verified session on `listener` (connections from
/// off the local network, and failed handshakes, are refused and reported while it waits), pull
/// every artifact the peer lists, end the session, then ingest each against this library's
/// heads and `tombstones` and write what applies.
pub(crate) async fn receive(
    workspace: &mut Workspace,
    trust: &PinnedTrust,
    versions: &dyn DirectoryVersions,
    tombstones: &BTreeSet<Uuid>,
    listener: &PeerListener,
) -> PullSummary {
    let mut summary = PullSummary::default();
    let pulled = {
        let local = LocalPeer {
            device_id: workspace.device_id(),
            signer: workspace.device_signer(),
            trust,
            versions,
        };
        session(listener, local, &mut summary).await
    };
    let Some(pulled) = pulled else {
        return summary;
    };
    summary.peer = Some(pulled.peer);
    let Some(exporter) = trust
        .directory
        .device(&pulled.peer)
        .map(|entry| entry.dsk_public.clone())
    else {
        // The handshake admitted the peer under this same directory, so this cannot happen;
        // refuse rather than ingest under no key.
        summary.failed.push(PeerFailure::new(
            pulled.peer.to_string(),
            "unknown_device",
            "the peer is not in the pinned device directory",
        ));
        return summary;
    };

    for (address, bytes) in pulled.artifacts {
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%address, %error, "peer pull: an artifact was not pulled");
                summary
                    .failed
                    .push(PeerFailure::new(&address, error.kind(), error.to_string()));
                continue;
            }
        };
        // Read for each artifact, so one carrying an asset the previous one applied reconciles
        // against what this library now holds.
        let heads: BTreeMap<Uuid, Hash32> = workspace.local_heads();
        let local = LocalLibrary {
            heads: &heads,
            tombstones,
        };
        let outcome = match ingest(&bytes, &pulled.secret, &exporter, local) {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(%address, %error, "peer pull: the artifact did not open");
                summary
                    .failed
                    .push(PeerFailure::new(&address, error.kind(), error.to_string()));
                continue;
            }
        };
        summary.artifacts.push(address);
        summary.absorb(&outcome);
        for asset in &outcome.applied {
            match workspace.apply_peer_restored(asset) {
                Ok(()) => summary.applied.push(asset.asset_id),
                Err(error) => {
                    tracing::warn!(asset_id = %asset.asset_id, %error, "peer pull: an applied asset was not written");
                    summary.failed.push(PeerFailure::new(
                        asset.asset_id.to_string(),
                        "not_written",
                        error.to_string(),
                    ));
                }
            }
        }
    }
    tracing::info!(
        artifacts = summary.artifacts.len(),
        applied = summary.applied.len(),
        quarantined = summary.quarantined.len(),
        tombstoned = summary.tombstoned.len(),
        identical = summary.identical.len(),
        incomplete = summary.incomplete.len(),
        failed = summary.failed.len(),
        "peer pull: done"
    );
    summary
}

/// The session an admission got, or `None` with the reason recorded in `summary.failed`: the
/// listen timeout passed (`peering_timeout`), or the listener itself failed (`peering_io`,
/// whose detail the admission's refusals carry). Either way nothing was pulled, so the run
/// must exit non-zero.
fn admitted_client<T>(
    admitted: Admitted<T>,
    subject: &str,
    summary: &mut PullSummary,
) -> Option<T> {
    match admitted {
        Admitted::Session(source, client) => {
            tracing::debug!(%source, "peer pull: a handshake completed");
            Some(client)
        }
        Admitted::TimedOut => {
            summary.failed.push(PeerFailure::new(
                subject,
                "peering_timeout",
                "no peer connected and verified before the listen timeout",
            ));
            None
        }
        Admitted::Failed => {
            tracing::warn!(listening = %subject, "peer pull: the listener failed; nothing was pulled");
            summary.failed.push(PeerFailure::new(
                subject,
                "peering_io",
                "the listener failed before a peer was verified",
            ));
            None
        }
    }
}

/// Admit one verified session on `listener` and pull every artifact its peer lists, then end
/// it. `None` when no session was verified before the listener's timeout (recorded in
/// `summary`). A session is not resumed by a later one: each session seals its artifact under
/// its own secret, so a new session offers a new artifact.
async fn session(
    listener: &PeerListener,
    local: LocalPeer<'_>,
    summary: &mut PullSummary,
) -> Option<Pulled> {
    let subject = listener
        .local_addr()
        .map_or_else(|_| "the listener".to_owned(), |address| address.to_string());
    tracing::info!(listening = %subject, timeout = ?listener.timeout, "peer pull: listening for a peer");
    let limiter = HandshakeLimiter::default();
    let limiter = &limiter;
    let deadline = tokio::time::Instant::now() + listener.timeout;
    let handshake = |tcp: TcpStream, source: std::net::SocketAddr| async move {
        let client = match capsule_sdk::peering::accept(tcp, local, limiter).await {
            Ok(channel) => PeerClient::open(channel).await,
            Err(error) => Err(error),
        };
        (source, client)
    };
    let mut admission = Admission::new();
    let admitted = admission
        .next_session(&listener.listener, &subject, deadline, &handshake)
        .await;
    let client = admitted_client(admitted, &subject, summary);
    summary.refused.extend(admission.into_refusals());
    let client = client?;
    tracing::info!(peer = %client.peer().device_id, "peer pull: verified session");

    let mut artifacts = Vec::new();
    for offered in client.listing().to_vec() {
        let mut held = Vec::new();
        let bytes = pull_over_channel(&client, &offered.address, offered.len, &mut held).await;
        artifacts.push((offered.address, bytes));
    }
    if let Err(error) = client.finish().await {
        tracing::debug!(%error, "peer pull: the peer was not told the session is over");
    }
    Some(Pulled {
        peer: client.peer().device_id,
        secret: client.peer().artifact_secret.clone(),
        artifacts,
    })
}

#[cfg(test)]
mod tests;
