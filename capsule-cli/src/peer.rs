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
//!   and policy, and deletes each copy once the server holds it. A hand-off that can never
//!   succeed is quarantined, its bytes kept, and is not retried.
//! - **`peer status`** (the origin): takes the delegate's acknowledgements, re-queues what is
//!   overdue or whose delegate was revoked, and releases what the server holds by the origin's
//!   own custody check of its own hashes and a read-back of its own copy, and lists every
//!   hand-off this device quarantined.
//! - **`peer discard`** (the delegate): deletes one quarantined hand-off, on the user's word.
//!
//! The hand-off rides a file here: the courier is opaque and the hand-off self-verifying, so
//! the channel carries no trust ([`capsule_sdk::peering::delegation`]). The account's directory
//! is the one the server publishes, verified under the library's user identity key and held to
//! the anti-rollback floor this device persists in the CLI database
//! ([`crate::peering_state::DbDirectoryVersions`]).
//!
//! State lives under the CLI data directory, per device: the origin ledger (`origin.json`) and
//! the delegate inbox (`inbox/`). A delegate copy is never a library asset.
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
    DelegateInbox, DelegationError, DelegationState, DirectoryVersions, HandOff, HandOffAck,
    HeldFailure, OriginLedger, PeeringError, PinnedTrust, Quarantine, ReleaseOutcome,
    RequeueReason, Settled, accept_hand_off, dispose_failure, seal, settle, upload_hand_off,
    verify_hand_off,
};
use capsule_sdk::staged::StagedScheduler;
use capsule_sdk::upload::{UploadClient, UploadTransport};
use capsule_sdk::verify::{HeldRelease, ReleaseCoordinator, StorageVerifyClient, VerifyTransport};
use serde::Serialize;
use thiserror::Error;
use tracing::instrument;
use uuid::Uuid;

use crate::peering_state::DbDirectoryVersions;
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
    /// `peer discard` named a hand-off this device does not hold in quarantine.
    #[error("no quarantined hand-off {0} on this device")]
    NotQuarantined(String),
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

/// One device's peer state: the origin ledger and the delegate inbox.
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

    /// The directory every piece of this device's peer state lives under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where an earlier build kept the directory floor, which
    /// [`DbDirectoryVersions::adopt_legacy_file`] moves into the CLI database.
    #[must_use]
    pub fn legacy_floor_path(&self) -> PathBuf {
        self.root.join("directory_version")
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

    fn quarantine_dir(&self) -> PathBuf {
        self.dir.join("quarantine")
    }

    /// Where the quarantined copy under `address` is kept: the file a user recovers it from.
    ///
    /// # Errors
    ///
    /// When `address` is not a content address.
    pub fn quarantined_path(&self, address: &str) -> io::Result<PathBuf> {
        let held = self.path(address)?;
        let name = held
            .file_name()
            .ok_or_else(|| io::Error::other("no file name"))?;
        Ok(self.quarantine_dir().join(name))
    }

    fn note_path(&self, address: &str) -> io::Result<PathBuf> {
        Ok(self
            .quarantined_path(address)?
            .with_extension(QUARANTINE_NOTE_SUFFIX))
    }
}

/// The file suffix a quarantined copy's note is written under, beside the copy.
const QUARANTINE_NOTE_SUFFIX: &str = "quarantine.json";

fn remove_if_present(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
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

    /// The note is written durably first, then the copy is renamed into `quarantine/` and both
    /// directories synced: a crash leaves the copy held (and a stale note a later quarantine
    /// overwrites) or quarantined with its note, never a quarantined copy without one.
    fn quarantine(&self, address: &str, note: &Quarantine) -> io::Result<()> {
        let held = self.path(address)?;
        if !held.exists() {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(note).map_err(io::Error::other)?;
        write_durably(&self.note_path(address)?, &bytes)?;
        fs::rename(&held, self.quarantined_path(address)?)?;
        fs::File::open(&self.dir)?.sync_all()?;
        fs::File::open(self.quarantine_dir())?.sync_all()
    }

    fn quarantined(&self) -> io::Result<Vec<(String, Quarantine)>> {
        let entries = match fs::read_dir(self.quarantine_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let suffix = format!(".{HAND_OFF_SUFFIX}");
        let mut quarantined = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            let Some(address) = name.to_str().and_then(|n| n.strip_suffix(&suffix)) else {
                continue;
            };
            let note = fs::read(self.note_path(address)?)?;
            let note = serde_json::from_slice(&note)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            quarantined.push((address.to_owned(), note));
        }
        quarantined.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(quarantined)
    }

    fn load_quarantined(&self, address: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.quarantined_path(address)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The copy is removed before its note, so a crash between the two leaves an orphaned note
    /// no listing reports, never a copy without its note.
    fn discard(&self, address: &str) -> io::Result<Option<Quarantine>> {
        let copy = self.quarantined_path(address)?;
        if !copy.exists() {
            return Ok(None);
        }
        let note = serde_json::from_slice(&fs::read(self.note_path(address)?)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        remove_if_present(&copy)?;
        remove_if_present(&self.note_path(address)?)?;
        fs::File::open(self.quarantine_dir())?.sync_all()?;
        Ok(Some(note))
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
/// identity key, then admitted against this device's floor so a stale one is refused, and the
/// floor raised to it durably before anything is verified under it.
async fn pinned_trust(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    versions: &DbDirectoryVersions,
) -> Result<PinnedTrust, PeerError> {
    let user_ik = workspace.user_ik_public();
    let directory = DirectoryClient::new(session, &remote.auth_endpoint)
        .fetch(workspace.user_id(), &user_ik)
        .await
        .map_err(RemoteError::from)?;
    let trust = PinnedTrust { user_ik, directory };
    versions.admit(trust.directory_version()).await?;
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

/// One asset, hand-off file or acknowledgement that failed, and why, in the shape `--json`
/// reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerFailure {
    /// The asset, or the file, it concerns.
    pub subject: String,
    /// A stable name for why: one per [`DelegationError`] and [`PeeringError`] variant
    /// ([`delegation_kind`]), or `bundle` (the library could not build the asset's upload
    /// bundle), `read` (the file could not be read) or `ack_malformed` (it is not an
    /// acknowledgement).
    pub kind: &'static str,
    /// The server's stable `error.*` code, when it supplied one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
}

impl PeerFailure {
    fn new(subject: impl Into<String>, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            kind,
            code: None,
            message: message.into(),
        }
    }

    fn delegation(subject: impl Into<String>, error: &DelegationError) -> Self {
        Self {
            subject: subject.into(),
            kind: delegation_kind(error),
            code: error.error_code().map(str::to_owned),
            message: error.to_string(),
        }
    }
}

/// The stable name `--json` reports a [`DelegationError`] under. A refused device or directory
/// is named by its [`PeeringError`] ([`peering_kind`]); every other variant has its own name.
#[must_use]
pub fn delegation_kind(error: &DelegationError) -> &'static str {
    match error {
        DelegationError::Peering(error) => peering_kind(error),
        DelegationError::SelfDelegation => "self_delegation",
        DelegationError::Malformed(_) => "malformed",
        DelegationError::UnsupportedFormat { .. } => "unsupported_format",
        DelegationError::WrongDelegate { .. } => "wrong_delegate",
        DelegationError::ProtocolUnsupported { .. } => "protocol_unsupported",
        DelegationError::Tampered(_) => "tampered",
        DelegationError::Inbox(_) => "inbox",
        DelegationError::Upload(_) => "upload",
        DelegationError::UnknownDelegation(_) => "unknown_delegation",
        DelegationError::AckMismatch => "ack_mismatch",
    }
}

/// The stable name `--json` reports a [`PeeringError`] under.
#[must_use]
pub fn peering_kind(error: &PeeringError) -> &'static str {
    match error {
        PeeringError::Tls(_) => "peering_tls",
        PeeringError::Io(_) => "peering_io",
        PeeringError::VersionDisjoint { .. } => "version_disjoint",
        PeeringError::ForeignIdentity => "foreign_identity",
        PeeringError::DirectoryRollback { .. } => "directory_rollback",
        PeeringError::PeerDirectoryStale { .. } => "peer_directory_stale",
        PeeringError::RateLimited(_) => "rate_limited",
        PeeringError::OutOfScope(_) => "out_of_scope",
        PeeringError::UnknownDevice(_) => "unknown_device",
        PeeringError::RevokedDevice(_) => "revoked_device",
        PeeringError::HybridCheckFailed => "hybrid_check_failed",
        PeeringError::Discovery(_) => "discovery",
        PeeringError::Codec(_) => "codec",
        PeeringError::Crypto(_) => "crypto",
        PeeringError::Backup(_) => "backup",
        PeeringError::Transfer(_) => "transfer",
    }
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
    versions: &DbDirectoryVersions,
    clock: &dyn BootClock,
    options: &DelegateOptions,
) -> Result<DelegateSummary, PeerError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = delegate_with(
        remote,
        workspace,
        session.clone(),
        state,
        versions,
        clock,
        options,
    )
    .await;
    remote::checkpoint(store, &session).await;
    outcome
}

async fn delegate_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    state: &PeerState,
    versions: &DbDirectoryVersions,
    clock: &dyn BootClock,
    options: &DelegateOptions,
) -> Result<DelegateSummary, PeerError> {
    let trust = pinned_trust(remote, workspace, session, versions).await?;
    let mut ledger = state.load_ledger()?;
    let mut summary = DelegateSummary {
        delegate: options.to,
        handed_off: Vec::new(),
        failed: Vec::new(),
    };
    for asset_id in &options.assets {
        let sealed = match workspace
            .upload_bundle(asset_id)
            .map_err(|error| PeerFailure::new(asset_id.to_string(), "bundle", error.to_string()))
            .and_then(|bundle| {
                seal(&bundle, options.to, &trust)
                    .map_err(|error| PeerFailure::delegation(asset_id.to_string(), &error))
            }) {
            Ok(sealed) => sealed,
            Err(failure) => {
                // A delegate the directory does not admit fails every asset alike; it is still
                // reported per asset so the run's report names what was not handed off.
                tracing::warn!(%asset_id, kind = failure.kind, error = %failure.message, "peer delegate: not handed off");
                summary.failed.push(failure);
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Courier {
    /// The asset; nil when the held copy could not be read or verified.
    pub asset_id: Uuid,
    /// The hand-off's content address.
    pub hand_off: String,
    /// `removed` (the server holds it; the copy is gone), `kept` (not yet), `failed` (a
    /// transient failure; the copy is kept for the next run) or `quarantined` (a permanent
    /// refusal; the copy and its bytes are kept, no later run retries it, `peer status` lists
    /// it and `peer discard` deletes it).
    pub outcome: &'static str,
    /// A stable name for why it was kept (a retain reason: `not_durable`, `receipt_missing`,
    /// `verify_unavailable`, `receipt_unavailable` or `read_back_mismatch`), failed or was
    /// quarantined (a [`PeerFailure::kind`], or `copy_missing` when the inbox no longer holds
    /// it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    /// The server's stable `error.*` code, when it supplied one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The English detail of a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Courier {
    fn settled(
        asset_id: Uuid,
        address: &str,
        outcome: &'static str,
        kind: Option<&'static str>,
    ) -> Self {
        Self {
            asset_id,
            hand_off: address.to_owned(),
            outcome,
            kind,
            code: None,
            message: None,
        }
    }

    fn failed(asset_id: Uuid, address: &str, kind: &'static str, message: String) -> Self {
        Self {
            asset_id,
            hand_off: address.to_owned(),
            outcome: "failed",
            kind: Some(kind),
            code: None,
            message: Some(message),
        }
    }
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
        !self.refused.is_empty()
            || self
                .uploads
                .iter()
                .any(|u| matches!(u.outcome, "failed" | "quarantined"))
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
    versions: &DbDirectoryVersions,
    options: &AcceptOptions,
) -> Result<AcceptSummary, PeerError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = accept_with(remote, workspace, session.clone(), state, versions, options).await;
    remote::checkpoint(store, &session).await;
    outcome
}

async fn accept_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    state: &PeerState,
    versions: &DbDirectoryVersions,
    options: &AcceptOptions,
) -> Result<AcceptSummary, PeerError> {
    let trust = pinned_trust(remote, workspace, session.clone(), versions).await?;
    let inbox = state.inbox();
    let device = workspace.device_id();
    let mut summary = AcceptSummary {
        accepted: Vec::new(),
        refused: Vec::new(),
        uploads: Vec::new(),
    };

    for file in &options.from {
        let subject = file.display().to_string();
        let taken = fs::read(file)
            .map_err(|error| PeerFailure::new(&subject, "read", error.to_string()))
            .and_then(|bytes| {
                accept_hand_off(
                    &bytes,
                    &trust,
                    versions,
                    device,
                    &remote.protocol_version,
                    &inbox,
                )
                .map_err(|error| PeerFailure::delegation(&subject, &error))
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
            Err(failure) => {
                tracing::warn!(file = %subject, kind = failure.kind, error = %failure.message, "peer accept: refused");
                summary.refused.push(failure);
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
                versions,
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
/// state, and what was verified when it was taken is checked again before it is sent. A failure
/// that can never clear quarantines the copy ([`refused`]).
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
    let bytes = match inbox.load(address) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Courier::failed(
                Uuid::nil(),
                address,
                "copy_missing",
                "the copy is gone".into(),
            );
        }
        Err(error) => return Courier::failed(Uuid::nil(), address, "inbox", error.to_string()),
    };
    let verified = match verify_hand_off(&bytes, trust, versions, device, &remote.protocol_version)
    {
        Ok(verified) => verified,
        Err(error) => {
            // The asset is named from the unverified header, so a quarantined copy says which
            // asset it would recover; nil when even the header does not read.
            let asset_id = HandOff::decode(&bytes).map_or(Uuid::nil(), |h| h.header.asset_id);
            return refused(inbox, address, asset_id, &error);
        }
    };
    let asset_id = verified.asset_id();
    let report = match upload_hand_off(upload, scheduler, &verified).await {
        Ok(report) => report,
        Err(error) => return refused(inbox, address, asset_id, &error),
    };
    match settle(coordinator, &verified, &report.deferred, inbox).await {
        Ok(Settled::Removed) => Courier::settled(asset_id, address, "removed", None),
        Ok(Settled::Kept(reason)) => {
            Courier::settled(asset_id, address, "kept", Some(kept_because(&reason)))
        }
        Err(error) => refused(inbox, address, asset_id, &error),
    }
}

/// Report a held hand-off that failed with `error`: `quarantined`, its copy kept but no longer
/// retried, when the failure is permanent ([`dispose_failure`]); `failed`, its copy kept for the
/// next run, otherwise.
fn refused(
    inbox: &dyn DelegateInbox,
    address: &str,
    asset_id: Uuid,
    error: &DelegationError,
) -> Courier {
    let kind = delegation_kind(error);
    let (outcome, message) = match dispose_failure(inbox, address, asset_id, kind, error) {
        Ok(HeldFailure::Quarantined) => ("quarantined", error.to_string()),
        Ok(HeldFailure::Kept) => ("failed", error.to_string()),
        // A permanent refusal whose copy could not be quarantined stays held, and is tried
        // again.
        Err(quarantine) => ("failed", format!("{error}; {quarantine}")),
    };
    Courier {
        asset_id,
        hand_off: address.to_owned(),
        outcome,
        kind: Some(kind),
        code: error.error_code().map(str::to_owned),
        message: Some(message),
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

/// One hand-off this device quarantined, as `capsule peer status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarantinedView {
    /// The hand-off's content address: what `capsule peer discard` takes.
    pub hand_off: String,
    /// The asset it uploads; nil when its header could not be read.
    pub asset_id: Uuid,
    /// The file the copy is kept in, byte for byte as it was accepted.
    pub file: String,
    /// The stable name of the refusal that quarantined it (a [`PeerFailure::kind`]).
    pub kind: String,
    /// The server's stable `error.*` code, when it supplied one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
}

/// Every hand-off `inbox` holds in quarantine, each once.
///
/// # Errors
///
/// [`PeerError::State`] when the quarantine cannot be read.
pub fn quarantined(inbox: &FileInbox) -> Result<Vec<QuarantinedView>, PeerError> {
    let listed = inbox
        .quarantined()
        .map_err(state_error("list the quarantine"))?;
    listed
        .into_iter()
        .map(|(address, note)| {
            let file = inbox
                .quarantined_path(&address)
                .map_err(state_error("name a quarantined copy"))?;
            Ok(QuarantinedView {
                file: file.display().to_string(),
                hand_off: address,
                asset_id: note.asset_id,
                kind: note.kind,
                code: note.code,
                message: note.message,
            })
        })
        .collect()
}

/// The `capsule peer status` run, as `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct StatusSummary {
    /// Every delegation the origin ledger holds.
    pub delegations: Vec<DelegationView>,
    /// Acknowledgements that were refused, with why.
    pub refused_acks: Vec<PeerFailure>,
    /// Every hand-off this device quarantined: kept, not retried, until `peer discard`.
    pub quarantined: Vec<QuarantinedView>,
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
    versions: &DbDirectoryVersions,
    clock: &dyn BootClock,
    acks: &[PathBuf],
) -> Result<StatusSummary, PeerError> {
    let session = remote::resume_session(remote, store).await?;
    let outcome = status_with(
        remote,
        workspace,
        session.clone(),
        state,
        versions,
        clock,
        acks,
    )
    .await;
    remote::checkpoint(store, &session).await;
    outcome
}

async fn status_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    state: &PeerState,
    versions: &DbDirectoryVersions,
    clock: &dyn BootClock,
    acks: &[PathBuf],
) -> Result<StatusSummary, PeerError> {
    let trust = pinned_trust(remote, workspace, session.clone(), versions).await?;
    let mut ledger = state.load_ledger()?;
    let mut refused_acks = Vec::new();
    for file in acks {
        let subject = file.display().to_string();
        let taken = fs::read(file)
            .map_err(|error| PeerFailure::new(&subject, "read", error.to_string()))
            .and_then(|bytes| {
                serde_json::from_slice::<HandOffAck>(&bytes)
                    .map_err(|error| PeerFailure::new(&subject, "ack_malformed", error.to_string()))
            })
            .and_then(|ack| {
                ledger
                    .acknowledge(&ack)
                    .map_err(|error| PeerFailure::delegation(&subject, &error))
            });
        if let Err(failure) = taken {
            tracing::warn!(file = %subject, kind = failure.kind, error = %failure.message, "peer status: acknowledgement refused");
            refused_acks.push(failure);
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
        quarantined: quarantined(&state.inbox())?,
    })
}

// ─── peer discard ────────────────────────────────────────────────────────────

/// The `capsule peer discard` run, as `--json` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Discarded {
    /// The hand-off deleted.
    pub hand_off: String,
    /// The asset it would have uploaded.
    pub asset_id: Uuid,
    /// The refusal it had been quarantined for.
    pub kind: String,
}

/// `capsule peer discard`: delete the quarantined hand-off under `address`, on the user's word.
/// Local only: a held hand-off is never deleted this way, and nothing reaches the network.
///
/// # Errors
///
/// [`PeerError::NotQuarantined`] when this device holds no quarantined copy under `address`
/// (including a held one, which is still being uploaded), or [`PeerError::State`] when it could
/// not be deleted.
#[instrument(skip_all, fields(hand_off = %address))]
pub fn discard(state: &PeerState, address: &str) -> Result<Discarded, PeerError> {
    let inbox = state.inbox();
    let not_quarantined = || PeerError::NotQuarantined(address.to_owned());
    // A name that is not a content address names nothing this device holds.
    inbox.path(address).map_err(|_| not_quarantined())?;
    let note = inbox
        .discard(address)
        .map_err(state_error("discard the quarantined copy"))?
        .ok_or_else(not_quarantined)?;
    tracing::warn!(asset_id = %note.asset_id, kind = %note.kind, "peer discard: the quarantined copy is deleted");
    Ok(Discarded {
        hand_off: address.to_owned(),
        asset_id: note.asset_id,
        kind: note.kind,
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
