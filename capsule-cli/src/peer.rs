//! `capsule peer` — upload delegation between two devices of one account
//! ([Peering § Delegation]).
//!
//! The rules are `capsule_sdk::peering::delegation`'s; this module supplies their local state and
//! network legs:
//!
//! - **`peer delegate`** (the origin): seals each named asset's upload bundle for another device
//!   of the account, writes the hand-off to a file, and records it in the origin ledger with a
//!   monotonic deadline. The original stays pinned.
//! - **`peer accept`** (the delegate): verifies a hand-off against the account's published
//!   directory, persists it in the delegate inbox (synced) and only then writes its
//!   acknowledgement beside it; then uploads every hand-off in the inbox under its own session
//!   and policy, and deletes each copy once the server holds it.
//! - **`peer status`** (the origin): takes the delegate's acknowledgements, re-queues what is
//!   overdue or whose delegate was revoked, and releases what the server holds by the origin's
//!   own custody check of its own hashes and a read-back of its own copy.
//!
//! The hand-off rides a file here: the courier is opaque and the hand-off self-verifying, so
//! the channel carries no trust ([`capsule_sdk::peering::delegation`]). The account's directory
//! is the one the server publishes, verified under the library's user identity key and held to
//! an anti-rollback floor this device persists.
//!
//! State lives under the CLI data directory, per device: the origin ledger (`origin.json`), the
//! delegate inbox (`inbox/`), and the directory floor (`directory_version`). A delegate copy is
//! never a library asset.
//!
//! [Peering § Delegation]: https://docs/design/peering/#delegation

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use capsule_core::crypto::hash::hash_bytes;
use capsule_library::import::UploadPolicy;
use capsule_library::library::RetainReason;
use capsule_library::lifecycle::Workspace;
use capsule_sdk::auth::Session;
use capsule_sdk::directory::DirectoryClient;
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::peering::delegation::DEFAULT_HAND_OFF_DEADLINE_MS;
use capsule_sdk::peering::{
    DelegateInbox, DelegationError, DelegationState, DirectoryVersions, HandOffAck, OriginLedger,
    PeeringError, PinnedTrust, ReleaseOutcome, RequeueReason, Settled, accept_hand_off, seal,
    settle, upload_hand_off, verify_hand_off,
};
use capsule_sdk::staged::StagedScheduler;
use capsule_sdk::upload::{UploadClient, UploadTransport};
use capsule_sdk::verify::{HeldRelease, ReleaseCoordinator, StorageVerifyClient, VerifyTransport};
use serde::Serialize;
use thiserror::Error;
use tracing::instrument;
use uuid::Uuid;

use crate::remote::{self, RemoteConfig, RemoteError};
use crate::session::SessionStore;

/// The file suffix a hand-off is written under.
pub const HAND_OFF_SUFFIX: &str = "capsule-handoff";

/// The file suffix a delegate's acknowledgement is written under, beside the hand-off it names.
pub const ACK_SUFFIX: &str = "ack.json";

/// What `capsule peer` can fail with as a whole run. A per-asset failure is reported in the
/// run's summary instead.
#[derive(Debug, Error)]
pub enum PeerError {
    /// A network leg failed (no session, the directory, the attestation keys).
    #[error(transparent)]
    Remote(#[from] RemoteError),
    /// The account's directory or a device in it was refused.
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    /// Reading or writing this device's peer state failed.
    #[error("peer state: {0}")]
    State(String),
}

impl From<PeeringError> for PeerError {
    fn from(error: PeeringError) -> Self {
        Self::Delegation(DelegationError::Peering(error))
    }
}

fn state_error(what: &str) -> impl Fn(io::Error) -> PeerError + '_ {
    move |error| PeerError::State(format!("{what}: {error}"))
}

// ─── Local state ─────────────────────────────────────────────────────────────

/// Write `bytes` to `path` so that a crash leaves either the old file or the new one, and the
/// new one is on disk when this returns: a temporary file, synced, renamed over `path`, and the
/// directory synced.
pub(crate) fn write_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("the path has no parent directory"))?;
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
    ));
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    fs::File::open(dir)?.sync_all()
}

/// One device's peer state: the origin ledger, the delegate inbox and the directory floor.
#[derive(Debug, Clone)]
pub struct PeerState {
    root: PathBuf,
}

impl PeerState {
    /// The state rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The state for `device` under the CLI's data directory.
    ///
    /// # Errors
    ///
    /// [`PeerError::State`] when the platform has no data directory.
    pub fn for_device(device: Uuid) -> Result<Self, PeerError> {
        let data = crate::utils::directories::get_data_dir()
            .ok_or_else(|| PeerError::State("no data directory".into()))?;
        Ok(Self::new(
            data.join("peer").join(device.simple().to_string()),
        ))
    }

    fn ledger_path(&self) -> PathBuf {
        self.root.join("origin.json")
    }

    /// The origin ledger, empty when none was written yet.
    ///
    /// # Errors
    ///
    /// [`PeerError::State`] when it cannot be read or does not parse.
    pub fn load_ledger(&self) -> Result<OriginLedger, PeerError> {
        match fs::read(self.ledger_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| PeerError::State(format!("the origin ledger: {error}"))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(OriginLedger::new()),
            Err(error) => Err(state_error("read the origin ledger")(error)),
        }
    }

    /// Persist the origin ledger durably.
    ///
    /// # Errors
    ///
    /// [`PeerError::State`] when it cannot be written.
    pub fn save_ledger(&self, ledger: &OriginLedger) -> Result<(), PeerError> {
        let bytes = serde_json::to_vec_pretty(ledger)
            .map_err(|error| PeerError::State(format!("the origin ledger: {error}")))?;
        write_durably(&self.ledger_path(), &bytes).map_err(state_error("write the origin ledger"))
    }

    /// The delegate inbox.
    #[must_use]
    pub fn inbox(&self) -> FileInbox {
        FileInbox {
            dir: self.root.join("inbox"),
        }
    }

    /// The persisted directory anti-rollback floor.
    #[must_use]
    pub fn directory_versions(&self) -> FileDirectoryVersions {
        FileDirectoryVersions {
            path: self.root.join("directory_version"),
        }
    }
}

/// A [`DelegateInbox`] over a directory: one file per hand-off, written durably.
#[derive(Debug, Clone)]
pub struct FileInbox {
    dir: PathBuf,
}

impl FileInbox {
    fn path(&self, address: &str) -> io::Result<PathBuf> {
        // An address is a content address this device computed; anything else is refused rather
        // than joined onto a path.
        if address.is_empty() || !address.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::other("not a content address"));
        }
        Ok(self.dir.join(format!("{address}.{HAND_OFF_SUFFIX}")))
    }
}

impl DelegateInbox for FileInbox {
    fn persist(&self, address: &str, bytes: &[u8]) -> io::Result<()> {
        write_durably(&self.path(address)?, bytes)
    }

    fn load(&self, address: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(address)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn addresses(&self) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let suffix = format!(".{HAND_OFF_SUFFIX}");
        let mut addresses = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            if let Some(address) = name.to_str().and_then(|n| n.strip_suffix(&suffix)) {
                addresses.push(address.to_owned());
            }
        }
        addresses.sort();
        Ok(addresses)
    }

    fn remove(&self, address: &str) -> io::Result<()> {
        match fs::remove_file(self.path(address)?) {
            Ok(()) => fs::File::open(&self.dir)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// A [`DirectoryVersions`] over one file holding the highest version verified.
#[derive(Debug, Clone)]
pub struct FileDirectoryVersions {
    path: PathBuf,
}

impl DirectoryVersions for FileDirectoryVersions {
    fn highest(&self) -> Result<u64, PeeringError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => text
                .trim()
                .parse()
                .map_err(|_| PeeringError::Codec("the directory floor does not parse".into())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(PeeringError::Io(error.to_string())),
        }
    }

    fn record(&self, version: u64) -> Result<(), PeeringError> {
        if version <= self.highest()? {
            return Ok(());
        }
        write_durably(&self.path, version.to_string().as_bytes())
            .map_err(|error| PeeringError::Io(error.to_string()))
    }
}

/// The monotonic clock a hand-off's deadline is set on, and the identity of the boot it counts
/// from: a deadline set under another boot cannot be measured and is re-queued.
pub trait BootClock {
    /// This boot's identity.
    fn epoch(&self) -> String;
    /// Milliseconds since this boot's origin, never going backwards within it.
    fn now_ms(&self) -> u64;
}

/// The system's boot clock: on Linux the kernel's boot id and uptime (which counts suspended
/// time and is never set by anyone); elsewhere the life of this process, so a deadline is
/// re-queued by the next run rather than measured on a clock that may have been reset.
#[derive(Debug)]
pub struct SystemBootClock {
    process: Uuid,
    started: Instant,
}

impl Default for SystemBootClock {
    fn default() -> Self {
        Self {
            process: Uuid::new_v4(),
            started: Instant::now(),
        }
    }
}

impl SystemBootClock {
    fn linux() -> Option<(String, u64)> {
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let uptime = fs::read_to_string("/proc/uptime").ok()?;
        let seconds: f64 = uptime.split_whitespace().next()?.parse().ok()?;
        Some((boot.trim().to_owned(), (seconds * 1000.0) as u64))
    }
}

impl BootClock for SystemBootClock {
    fn epoch(&self) -> String {
        Self::linux().map_or_else(|| format!("process-{}", self.process), |(boot, _)| boot)
    }

    fn now_ms(&self) -> u64 {
        Self::linux().map_or_else(
            || u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            |(_, ms)| ms,
        )
    }
}

// ─── The account's directory ─────────────────────────────────────────────────

/// The account's directory as the server publishes it, verified under the library's user
/// identity key, then admitted against this device's floor so a stale one is refused.
async fn pinned_trust(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    versions: &dyn DirectoryVersions,
) -> Result<PinnedTrust, PeerError> {
    let user_ik = workspace.user_ik_public();
    let directory = DirectoryClient::new(session, &remote.auth_endpoint)
        .fetch(workspace.user_id(), &user_ik)
        .await
        .map_err(RemoteError::from)?;
    let trust = PinnedTrust { user_ik, directory };
    let floor = versions.highest()?;
    if trust.directory_version() < floor {
        return Err(PeeringError::DirectoryRollback {
            pinned: trust.directory_version(),
            highest: floor,
        }
        .into());
    }
    versions.record(trust.directory_version())?;
    tracing::debug!(
        directory_version = trust.directory_version(),
        devices = trust.directory.core.devices.len(),
        "peer: the account's directory is pinned"
    );
    Ok(trust)
}

// ─── peer delegate ───────────────────────────────────────────────────────────

/// What `capsule peer delegate` hands off.
#[derive(Debug, Clone)]
pub struct DelegateOptions {
    /// The device to hand the assets to.
    pub to: Uuid,
    /// The assets.
    pub assets: Vec<Uuid>,
    /// The directory the hand-off files are written to.
    pub out: PathBuf,
    /// How long to wait for the server to hold them before re-queuing, in milliseconds.
    pub deadline_ms: u64,
}

impl DelegateOptions {
    /// Hand `assets` to `to` through files in `out`, with the default deadline.
    #[must_use]
    pub fn new(to: Uuid, assets: Vec<Uuid>, out: PathBuf) -> Self {
        Self {
            to,
            assets,
            out,
            deadline_ms: DEFAULT_HAND_OFF_DEADLINE_MS,
        }
    }
}

/// One asset handed off.
#[derive(Debug, Clone, Serialize)]
pub struct HandedOff {
    /// The asset.
    pub asset_id: Uuid,
    /// The hand-off file.
    pub file: String,
    /// Its content address.
    pub hand_off: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// One asset that could not be handed off, and why.
#[derive(Debug, Clone, Serialize)]
pub struct PeerFailure {
    /// The asset, or the file, it concerns.
    pub subject: String,
    /// Why.
    pub error: String,
}

/// The `capsule peer delegate` run, as `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct DelegateSummary {
    /// The delegate.
    pub delegate: Uuid,
    /// What was handed off.
    pub handed_off: Vec<HandedOff>,
    /// What was not.
    pub failed: Vec<PeerFailure>,
}

/// `capsule peer delegate`.
///
/// # Errors
///
/// [`PeerError`] for a run that could not start: no session, a directory that does not verify or
/// does not admit the delegate, or peer state that cannot be written.
#[instrument(skip_all, fields(to = %options.to, assets = options.assets.len()))]
pub async fn delegate(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    state: &PeerState,
    clock: &dyn BootClock,
    options: &DelegateOptions,
) -> Result<DelegateSummary, PeerError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = delegate_with(remote, workspace, session.clone(), state, clock, options).await;
    remote::checkpoint(store, &session).await;
    outcome
}

async fn delegate_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    state: &PeerState,
    clock: &dyn BootClock,
    options: &DelegateOptions,
) -> Result<DelegateSummary, PeerError> {
    let trust = pinned_trust(remote, workspace, session, &state.directory_versions()).await?;
    let mut ledger = state.load_ledger()?;
    let mut summary = DelegateSummary {
        delegate: options.to,
        handed_off: Vec::new(),
        failed: Vec::new(),
    };
    for asset_id in &options.assets {
        let sealed = match workspace
            .upload_bundle(asset_id)
            .map_err(|error| error.to_string())
            .and_then(|bundle| seal(&bundle, options.to, &trust).map_err(|e| e.to_string()))
        {
            Ok(sealed) => sealed,
            Err(error) => {
                // A delegate the directory does not admit fails every asset alike; it is still
                // reported per asset so the run's report names what was not handed off.
                tracing::warn!(%asset_id, %error, "peer delegate: not handed off");
                summary.failed.push(PeerFailure {
                    subject: asset_id.to_string(),
                    error,
                });
                continue;
            }
        };
        let file = options
            .out
            .join(format!("{}.{HAND_OFF_SUFFIX}", asset_id.simple()));
        write_durably(&file, &sealed.bytes).map_err(state_error("write the hand-off"))?;
        ledger.hand_off(&sealed, &clock.epoch(), clock.now_ms(), options.deadline_ms);
        state.save_ledger(&ledger)?;
        summary.handed_off.push(HandedOff {
            asset_id: *asset_id,
            file: file.display().to_string(),
            hand_off: sealed.address,
            bytes: sealed.bytes.len() as u64,
        });
    }
    tracing::info!(
        handed_off = summary.handed_off.len(),
        failed = summary.failed.len(),
        "peer delegate: done"
    );
    Ok(summary)
}

// ─── peer accept ─────────────────────────────────────────────────────────────

/// What `capsule peer accept` takes and how it uploads.
#[derive(Debug, Clone)]
pub struct AcceptOptions {
    /// Hand-off files to take before uploading what the inbox holds.
    pub from: Vec<PathBuf>,
    /// This device's upload policy.
    pub policy: UploadPolicy,
    /// This device's connection class now.
    pub connection: ConnectionClass,
}

/// One hand-off taken.
#[derive(Debug, Clone, Serialize)]
pub struct Accepted {
    /// The file it came from.
    pub file: String,
    /// The acknowledgement, also written beside the file.
    pub ack: HandOffAck,
    /// Where the acknowledgement was written.
    pub ack_file: String,
}

/// What became of one hand-off in the inbox.
#[derive(Debug, Clone, Serialize)]
pub struct Courier {
    /// The asset.
    pub asset_id: Uuid,
    /// The hand-off's content address.
    pub hand_off: String,
    /// `removed` (the server holds it; the copy is gone), `kept` (not yet), or `failed`.
    pub outcome: &'static str,
    /// Why it was kept or failed.
    pub reason: Option<String>,
}

/// The `capsule peer accept` run, as `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct AcceptSummary {
    /// The hand-offs taken this run.
    pub accepted: Vec<Accepted>,
    /// The hand-offs refused, with why. Nothing of a refused one is kept.
    pub refused: Vec<PeerFailure>,
    /// Every hand-off the inbox held, uploaded or not.
    pub uploads: Vec<Courier>,
}

impl AcceptSummary {
    /// Whether anything was refused or failed.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        !self.refused.is_empty() || self.uploads.iter().any(|u| u.outcome == "failed")
    }
}

/// `capsule peer accept`.
///
/// # Errors
///
/// [`PeerError`] for a run that could not start: no session, a directory that does not verify,
/// or attestation keys that contradict the pin.
#[instrument(skip_all, fields(files = options.from.len()))]
pub async fn accept(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    state: &PeerState,
    options: &AcceptOptions,
) -> Result<AcceptSummary, PeerError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = accept_with(remote, workspace, session.clone(), state, options).await;
    remote::checkpoint(store, &session).await;
    outcome
}

async fn accept_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    state: &PeerState,
    options: &AcceptOptions,
) -> Result<AcceptSummary, PeerError> {
    let versions = state.directory_versions();
    let trust = pinned_trust(remote, workspace, session.clone(), &versions).await?;
    let inbox = state.inbox();
    let device = workspace.device_id();
    let mut summary = AcceptSummary {
        accepted: Vec::new(),
        refused: Vec::new(),
        uploads: Vec::new(),
    };

    for file in &options.from {
        let taken = fs::read(file)
            .map_err(|error| DelegationError::Inbox(error.to_string()))
            .and_then(|bytes| {
                accept_hand_off(
                    &bytes,
                    &trust,
                    &versions,
                    device,
                    &remote.protocol_version,
                    &inbox,
                )
            });
        match taken {
            Ok((_, ack)) => {
                let ack_file = file.with_extension(ACK_SUFFIX);
                let bytes = serde_json::to_vec_pretty(&ack)
                    .map_err(|error| PeerError::State(error.to_string()))?;
                write_durably(&ack_file, &bytes).map_err(state_error("write the ack"))?;
                summary.accepted.push(Accepted {
                    file: file.display().to_string(),
                    ack,
                    ack_file: ack_file.display().to_string(),
                });
            }
            Err(error) => {
                tracing::warn!(file = %file.display(), %error, "peer accept: refused");
                summary.refused.push(PeerFailure {
                    subject: file.display().to_string(),
                    error: error.to_string(),
                });
            }
        }
    }

    let upload = UploadClient::new(UploadTransport::with_session(
        session.clone(),
        &remote.upload_endpoint,
        &remote.protocol_version,
    ))
    .with_connection_class(options.connection);
    let scheduler = StagedScheduler::new(options.policy, options.connection);
    let verify = StorageVerifyClient::new(VerifyTransport::with_session(
        session,
        format!("{}/v1", remote.sync_endpoint.trim_end_matches('/')),
    ));
    let keys = remote::pinned_attestation_keys(remote, workspace, &verify).await?;
    let coordinator = ReleaseCoordinator::new(verify, keys);

    let addresses = inbox.addresses().map_err(state_error("list the inbox"))?;
    for address in addresses {
        summary.uploads.push(
            courier(
                &inbox,
                &address,
                &trust,
                &versions,
                device,
                remote,
                &upload,
                &scheduler,
                &coordinator,
            )
            .await,
        );
    }
    tracing::info!(
        accepted = summary.accepted.len(),
        refused = summary.refused.len(),
        uploads = summary.uploads.len(),
        "peer accept: done"
    );
    Ok(summary)
}

/// Upload one hand-off the inbox holds and settle it. Re-verified first: the inbox is local
/// state, and what was verified when it was taken is checked again before it is sent.
#[allow(clippy::too_many_arguments)]
async fn courier(
    inbox: &dyn DelegateInbox,
    address: &str,
    trust: &PinnedTrust,
    versions: &dyn DirectoryVersions,
    device: Uuid,
    remote: &RemoteConfig,
    upload: &UploadClient,
    scheduler: &StagedScheduler,
    coordinator: &ReleaseCoordinator,
) -> Courier {
    let failed = |asset_id: Uuid, reason: String| Courier {
        asset_id,
        hand_off: address.to_owned(),
        outcome: "failed",
        reason: Some(reason),
    };
    let bytes = match inbox.load(address) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return failed(Uuid::nil(), "the copy is gone".into()),
        Err(error) => return failed(Uuid::nil(), error.to_string()),
    };
    let verified = match verify_hand_off(&bytes, trust, versions, device, &remote.protocol_version)
    {
        Ok(verified) => verified,
        Err(error) => return failed(Uuid::nil(), error.to_string()),
    };
    let asset_id = verified.asset_id();
    let report = match upload_hand_off(upload, scheduler, &verified).await {
        Ok(report) => report,
        Err(error) => return failed(asset_id, error.to_string()),
    };
    match settle(coordinator, &verified, &report.deferred, inbox).await {
        Ok(Settled::Removed) => Courier {
            asset_id,
            hand_off: address.to_owned(),
            outcome: "removed",
            reason: None,
        },
        Ok(Settled::Kept(reason)) => Courier {
            asset_id,
            hand_off: address.to_owned(),
            outcome: "kept",
            reason: Some(kept_because(&reason).to_owned()),
        },
        Err(error) => failed(asset_id, error.to_string()),
    }
}

/// The stable name a retain reason is reported under.
fn kept_because(reason: &RetainReason) -> &'static str {
    match reason {
        RetainReason::NotDurable => "not_durable",
        RetainReason::ReceiptMissing => "receipt_missing",
        RetainReason::VerifyUnavailable => "verify_unavailable",
        RetainReason::ReceiptUnavailable => "receipt_unavailable",
        RetainReason::ReadBackMismatch => "read_back_mismatch",
    }
}

// ─── peer status ─────────────────────────────────────────────────────────────

/// One delegation as `capsule peer status` reports it.
#[derive(Debug, Clone, Serialize)]
pub struct DelegationView {
    /// The asset.
    pub asset_id: Uuid,
    /// The delegate.
    pub delegate: Uuid,
    /// Where it stands after this run.
    pub state: DelegationState,
    /// The line shown for it.
    pub status: String,
    /// Why it was re-queued, if it was.
    pub requeued_because: Option<RequeueReason>,
    /// Why it is still pinned, when the server does not hold it yet.
    pub kept_because: Option<&'static str>,
}

/// The `capsule peer status` run, as `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct StatusSummary {
    /// Every delegation the origin ledger holds.
    pub delegations: Vec<DelegationView>,
    /// Acknowledgements that were refused, with why.
    pub refused_acks: Vec<PeerFailure>,
}

/// `capsule peer status`: take `acks`, re-queue what is due, and release what the server holds
/// by this device's own check.
///
/// # Errors
///
/// [`PeerError`] for a run that could not start: no session, a directory that does not verify,
/// attestation keys that contradict the pin, or a ledger that cannot be written.
#[instrument(skip_all, fields(acks = acks.len()))]
pub async fn status(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    state: &PeerState,
    clock: &dyn BootClock,
    acks: &[PathBuf],
) -> Result<StatusSummary, PeerError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = status_with(remote, workspace, session.clone(), state, clock, acks).await;
    remote::checkpoint(store, &session).await;
    outcome
}

async fn status_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    state: &PeerState,
    clock: &dyn BootClock,
    acks: &[PathBuf],
) -> Result<StatusSummary, PeerError> {
    let trust = pinned_trust(
        remote,
        workspace,
        session.clone(),
        &state.directory_versions(),
    )
    .await?;
    let mut ledger = state.load_ledger()?;
    let mut refused_acks = Vec::new();
    for file in acks {
        let taken = fs::read(file)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<HandOffAck>(&bytes).map_err(|error| error.to_string())
            })
            .and_then(|ack| ledger.acknowledge(&ack).map_err(|error| error.to_string()));
        if let Err(error) = taken {
            refused_acks.push(PeerFailure {
                subject: file.display().to_string(),
                error,
            });
        }
    }
    ledger.due(&trust, &clock.epoch(), clock.now_ms());
    state.save_ledger(&ledger)?;

    // The origin's own custody check of its own hashes, with its own session: nothing the
    // delegate relays is consulted.
    let verify = StorageVerifyClient::new(VerifyTransport::with_session(
        session,
        format!("{}/v1", remote.sync_endpoint.trim_end_matches('/')),
    ));
    let keys = remote::pinned_attestation_keys(remote, workspace, &verify).await?;
    let coordinator = ReleaseCoordinator::new(verify, keys);
    let pending: Vec<Uuid> = ledger
        .delegations()
        .filter(|d| d.state != DelegationState::Released)
        .map(|d| d.asset_id)
        .collect();
    let releases: Vec<HeldRelease> = pending
        .iter()
        .filter_map(|id| workspace.upload_bundle(id).ok())
        .map(|bundle| HeldRelease::for_original(&bundle))
        .collect();
    let verifier = coordinator.resolve_custody(&releases).await;

    let mut kept = std::collections::BTreeMap::new();
    for asset_id in pending {
        let read_back = reads_back(workspace, &asset_id);
        let accepted = verify_asset_accepts(workspace, &asset_id);
        match ledger.release(asset_id, &verifier, accepted, read_back)? {
            ReleaseOutcome::Released { deletes } => {
                tracing::info!(%asset_id, held_deletes = deletes.len(), "peer status: released");
            }
            ReleaseOutcome::Retained(reason) => {
                kept.insert(asset_id, kept_because(&reason));
            }
        }
    }
    state.save_ledger(&ledger)?;

    let delegations = ledger
        .delegations()
        .map(|d| DelegationView {
            asset_id: d.asset_id,
            delegate: d.delegate,
            state: d.state,
            status: d.status_line(&format!("device {}", d.delegate)),
            requeued_because: d.requeued_because,
            kept_because: kept.get(&d.asset_id).copied(),
        })
        .collect();
    Ok(StatusSummary {
        delegations,
        refused_acks,
    })
}

/// Whether this library's own `verify_asset` accepts `asset_id`: the crypto half of the gate.
fn verify_asset_accepts(workspace: &Workspace, asset_id: &Uuid) -> bool {
    workspace
        .verify(asset_id)
        .is_ok_and(|outcome| outcome == capsule_core::crypto::verify_asset::VerifyOutcome::Accept)
}

/// Whether the library's copy of `asset_id` still hashes to the signed original.
fn reads_back(workspace: &Workspace, asset_id: &Uuid) -> bool {
    let Some(asset) = workspace.asset(asset_id) else {
        return false;
    };
    workspace
        .read_plaintext(asset_id)
        .is_ok_and(|bytes| hash_bytes(&bytes) == asset.sidecar.hash)
}

#[cfg(test)]
mod tests;
