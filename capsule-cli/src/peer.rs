//! `capsule peer` — upload delegation between two devices of one account
//! ([Peering § Delegation]).
//!
//! The rules are `capsule_sdk::peering::delegation`'s; this module supplies their local state and
//! network legs:
//!
//! - **`peer delegate`** (the origin): seals each named asset's upload bundle for another device
//!   of the account, writes the hand-off to a file or (`--peer`) serves it over the LAN peering
//!   channel, and records it in the origin ledger with a monotonic deadline. The original stays
//!   pinned.
//! - **`peer accept`** (the delegate): verifies a hand-off against the account's published
//!   directory, persists it in the delegate inbox (synced) and only then acknowledges it — in a
//!   file beside it, or (`--listen`) over the channel it was pulled on; then uploads every
//!   hand-off in the inbox under its own session and policy, and deletes each copy once the
//!   server holds it. A hand-off that can never succeed is quarantined, its bytes kept, and is
//!   not retried.
//! - **`peer status`** (the origin): takes the delegate's acknowledgements, re-queues what is
//!   overdue or whose delegate was revoked, and releases what the server holds by the origin's
//!   own custody check of its own hashes and a read-back of its own copy, and lists every
//!   hand-off this device quarantined.
//! - **`peer discard`** (the delegate): deletes one quarantined hand-off, on the user's word.
//!
//! The hand-off rides a file or the peering channel: the courier is opaque and the hand-off
//! self-verifying, so neither carries trust for its contents
//! ([`capsule_sdk::peering::delegation`]). Over the channel, `--peer` dials and `--listen`
//! admits only the local network; the origin hands off only to the device the handshake
//! verified (which must be `--to` when given) and authorizes each of its requests against the
//! directory the server publishes at that moment, and the delegate takes only hand-offs that
//! peer sealed itself. The account's directory is the one the server publishes, verified under
//! the library's user identity key and held to the anti-rollback floor this device persists in
//! the CLI database ([`crate::peering_state::DbDirectoryVersions`]).
//!
//! A delegated asset the origin deletes after the hand-off keeps its delete on the origin until
//! the create's custody exists: `peer status` routes the library's signed delete through
//! [`OriginLedger::hold_delete`], and sends the deletes [`OriginLedger::release`] hands back
//! once its own custody check passes, so the server never sees a delete for an asset it has not
//! indexed. `capsule push` skips an asset whose hand-off is still open and inside its deadline
//! ([`open_delegations`]); the library browser shows each one with its ledger line
//! ([`delegation_lines`]).
//!
//! State lives under the CLI data directory, per device: the origin ledger (`origin.json`), the
//! released deletes not yet applied by the server (`deletes.json`) and the delegate inbox
//! (`inbox/`). A delegate copy is never a library asset.
//!
//! [Peering § Delegation]: https://docs/design/peering/#delegation

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::future::Future;
use std::io::{self, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::HybridVerifyingKey;
use capsule_library::import::UploadPolicy;
use capsule_library::library::RetainReason;
use capsule_library::lifecycle::Workspace;
use capsule_sdk::auth::Session;
use capsule_sdk::directory::DirectoryClient;
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::peering::delegation::DEFAULT_HAND_OFF_DEADLINE_MS;
use capsule_sdk::peering::{
    DelegateInbox, DelegationError, DelegationState, DeleteDisposition, DirectoryVersions, HandOff,
    HandOffAck, HandshakeLimiter, HeldFailure, LocalPeer, OriginLedger, PeerChannel, PeerClient,
    PeeringError, PinnedTrust, Quarantine, ReleaseOutcome, RequeueReason, SealedHandOff, Settled,
    TrustSource, accept_hand_off, dispose_failure, is_lan, offer_hand_offs, seal, settle,
    take_hand_offs, upload_hand_off, verify_hand_off,
};
use capsule_sdk::staged::StagedScheduler;
use capsule_sdk::upload::{UploadClient, UploadTransport};
use capsule_sdk::verify::{HeldRelease, ReleaseCoordinator, StorageVerifyClient, VerifyTransport};
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use serde::Serialize;
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
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
    /// A hand-off written to files names no delegate (`--to`).
    #[error("a hand-off written to files must name the device it is for")]
    NoDelegate,
    /// The `--listen` address could not be bound.
    #[error("listen on {address}: {error}")]
    Listen {
        /// The address asked for.
        address: String,
        /// Why it could not be bound.
        error: String,
    },
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

    fn outbox_path(&self) -> PathBuf {
        self.root.join("deletes.json")
    }

    /// The deletes the origin ledger released and the server has not applied yet, in the order
    /// they are sent; empty when none was written yet.
    ///
    /// # Errors
    ///
    /// [`PeerError::State`] when it cannot be read or does not parse.
    pub fn load_outbox(&self) -> Result<Vec<PendingDelete>, PeerError> {
        match fs::read(self.outbox_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| PeerError::State(format!("the delete outbox: {error}"))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(state_error("read the delete outbox")(error)),
        }
    }

    /// Persist the delete outbox durably.
    ///
    /// # Errors
    ///
    /// [`PeerError::State`] when it cannot be written.
    pub fn save_outbox(&self, outbox: &[PendingDelete]) -> Result<(), PeerError> {
        let bytes = serde_json::to_vec_pretty(outbox)
            .map_err(|error| PeerError::State(format!("the delete outbox: {error}")))?;
        write_durably(&self.outbox_path(), &bytes).map_err(state_error("write the delete outbox"))
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

/// The bound on resolving and connecting to a `--peer`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many times `peer delegate --peer` dials again after the channel is severed mid-session.
/// Each new session resumes the delegate's partial pulls.
pub const MAX_REDIALS: u32 = 3;

/// How long `peer accept --listen` waits, by default, for a peer to connect and verify. A
/// verified session is not cut by it.
pub const DEFAULT_LISTEN_TIMEOUT: Duration = Duration::from_secs(300);

/// Where `capsule peer delegate` sends its hand-offs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandOffRoute {
    /// Write each hand-off to a file in this directory, for any courier to carry.
    Files(PathBuf),
    /// Serve them over the LAN peering channel to the device listening at this `HOST:PORT`
    /// (`capsule peer accept --listen`), which pulls and acknowledges them over it.
    Peer(String),
}

/// What `capsule peer delegate` hands off.
#[derive(Debug, Clone)]
pub struct DelegateOptions {
    /// The device to hand the assets to. Required for files; over the channel the delegate is
    /// the device the handshake verifies, which must be this one when it is given.
    pub to: Option<Uuid>,
    /// The assets.
    pub assets: Vec<Uuid>,
    /// Where the hand-offs go.
    pub route: HandOffRoute,
    /// How long to wait for the server to hold them before re-queuing, in milliseconds.
    pub deadline_ms: u64,
}

impl DelegateOptions {
    /// Hand `assets` to `to` through files in `out`, with the default deadline.
    #[must_use]
    pub fn new(to: Uuid, assets: Vec<Uuid>, out: PathBuf) -> Self {
        Self {
            to: Some(to),
            assets,
            route: HandOffRoute::Files(out),
            deadline_ms: DEFAULT_HAND_OFF_DEADLINE_MS,
        }
    }

    /// Hand `assets` over the peering channel to the device listening at `peer` (`HOST:PORT`),
    /// which must be `to` when given, with the default deadline.
    #[must_use]
    pub fn over_peer(peer: impl Into<String>, to: Option<Uuid>, assets: Vec<Uuid>) -> Self {
        Self {
            to,
            assets,
            route: HandOffRoute::Peer(peer.into()),
            deadline_ms: DEFAULT_HAND_OFF_DEADLINE_MS,
        }
    }
}

/// One asset handed off.
#[derive(Debug, Clone, Serialize)]
pub struct HandedOff {
    /// The asset.
    pub asset_id: Uuid,
    /// The hand-off file, when it was written to one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Its content address.
    pub hand_off: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Whether the delegate acknowledged it over the peering channel. Always `false` for a file,
    /// whose acknowledgement comes back through `capsule peer status --ack`.
    pub acknowledged: bool,
}

/// One asset, hand-off file or acknowledgement that failed, and why, in the shape `--json`
/// reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerFailure {
    /// The asset, or the file, it concerns.
    pub subject: String,
    /// A stable name for why: one per [`DelegationError`] and [`PeeringError`] variant
    /// ([`delegation_kind`]), or `bundle` (the library could not build the asset's upload
    /// bundle), `read` (the file could not be read), `ack_malformed` (it is not an
    /// acknowledgement), `peer_mismatch` (the device at `--peer` is not the `--to` device) or
    /// `not_acknowledged` (the peering session ended without the delegate's acknowledgement),
    /// or `refusals_elided` (`--listen` refused more distinct sources than it itemizes).
    pub kind: &'static str,
    /// The server's stable `error.*` code, when it supplied one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
    /// How many times this happened, when more than once: `--listen` reports the connections
    /// it refused once per source address and kind, with their count. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrences: Option<u32>,
}

impl PeerFailure {
    fn new(subject: impl Into<String>, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            kind,
            code: None,
            message: message.into(),
            occurrences: None,
        }
    }

    fn delegation(subject: impl Into<String>, error: &DelegationError) -> Self {
        Self {
            subject: subject.into(),
            kind: delegation_kind(error),
            code: error.error_code().map(str::to_owned),
            message: error.to_string(),
            occurrences: None,
        }
    }
}

/// The stable name `--json` reports a [`DelegationError`] under ([`DelegationError::kind`]). A
/// refused device or directory is named by its [`PeeringError`] ([`peering_kind`]); every other
/// variant has its own name.
#[must_use]
pub fn delegation_kind(error: &DelegationError) -> &'static str {
    error.kind()
}

/// The stable name `--json` reports a [`PeeringError`] under ([`PeeringError::kind`]): the same
/// name a peering channel carries a refusal under.
#[must_use]
pub fn peering_kind(error: &PeeringError) -> &'static str {
    error.kind()
}

/// The `capsule peer delegate` run, as `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct DelegateSummary {
    /// The delegate: the `--to` device for files, the device the channel verified for `--peer`
    /// (`null` when no channel was established).
    pub delegate: Option<Uuid>,
    /// The `--peer` address dialed, for a hand-off over the peering channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
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
#[instrument(skip_all, fields(to = ?options.to, route = ?options.route, assets = options.assets.len()))]
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
    let trust = pinned_trust(remote, workspace, session.clone(), versions).await?;
    let summary = match &options.route {
        HandOffRoute::Files(out) => {
            let to = options.to.ok_or(PeerError::NoDelegate)?;
            delegate_to_files(workspace, state, clock, options, to, out, &trust)?
        }
        HandOffRoute::Peer(peer) => {
            let published = PublishedTrust {
                directory: DirectoryClient::new(session, &remote.auth_endpoint),
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
            let sessions = OverPeer {
                peer,
                local,
                published: &published,
            };
            delegate_over_peer(workspace, state, versions, clock, options, &sessions).await?
        }
    };
    tracing::info!(
        handed_off = summary.handed_off.len(),
        failed = summary.failed.len(),
        "peer delegate: done"
    );
    Ok(summary)
}

/// Seal each asset for `delegate`, reporting each that cannot be sealed in `failed`.
fn seal_each(
    workspace: &Workspace,
    delegate: Uuid,
    trust: &PinnedTrust,
    assets: &[Uuid],
    failed: &mut Vec<PeerFailure>,
) -> Vec<SealedHandOff> {
    let mut sealed = Vec::new();
    for asset_id in assets {
        let outcome = workspace
            .upload_bundle(asset_id)
            .map_err(|error| PeerFailure::new(asset_id.to_string(), "bundle", error.to_string()))
            .and_then(|bundle| {
                seal(&bundle, delegate, trust)
                    .map_err(|error| PeerFailure::delegation(asset_id.to_string(), &error))
            });
        match outcome {
            Ok(hand_off) => sealed.push(hand_off),
            Err(failure) => {
                // A delegate the directory does not admit fails every asset alike; it is still
                // reported per asset so the run's report names what was not handed off.
                tracing::warn!(%asset_id, kind = failure.kind, error = %failure.message, "peer delegate: not handed off");
                failed.push(failure);
            }
        }
    }
    sealed
}

fn delegate_to_files(
    workspace: &Workspace,
    state: &PeerState,
    clock: &dyn BootClock,
    options: &DelegateOptions,
    to: Uuid,
    out: &Path,
    trust: &PinnedTrust,
) -> Result<DelegateSummary, PeerError> {
    let mut ledger = state.load_ledger()?;
    let mut summary = DelegateSummary {
        delegate: Some(to),
        peer: None,
        handed_off: Vec::new(),
        failed: Vec::new(),
    };
    for sealed in seal_each(workspace, to, trust, &options.assets, &mut summary.failed) {
        let file = out.join(format!("{}.{HAND_OFF_SUFFIX}", sealed.asset_id.simple()));
        write_durably(&file, &sealed.bytes).map_err(state_error("write the hand-off"))?;
        ledger.hand_off(&sealed, &clock.epoch(), clock.now_ms(), options.deadline_ms);
        state.save_ledger(&ledger)?;
        summary.handed_off.push(HandedOff {
            asset_id: sealed.asset_id,
            file: Some(file.display().to_string()),
            hand_off: sealed.address,
            bytes: sealed.bytes.len() as u64,
            acknowledged: false,
        });
    }
    Ok(summary)
}

/// The account's directory as the server publishes it **now**, verified under the library's
/// user identity key and admitted against this device's floor: what the origin authorizes each
/// request of a peering session against, so a delegate revoked mid-session is refused at its
/// next request. A directory that cannot be fetched refuses the request (fail closed).
struct PublishedTrust<'a> {
    directory: DirectoryClient,
    user_id: Uuid,
    user_ik: HybridVerifyingKey,
    versions: &'a DbDirectoryVersions,
}

impl TrustSource for PublishedTrust<'_> {
    async fn current(&self) -> Result<PinnedTrust, PeeringError> {
        let directory = self
            .directory
            .fetch(self.user_id, &self.user_ik)
            .await
            .map_err(|error| PeeringError::Directory(error.to_string()))?;
        let trust = PinnedTrust {
            user_ik: self.user_ik.clone(),
            directory,
        };
        self.versions.admit(trust.directory_version()).await?;
        Ok(trust)
    }
}

/// How `peer delegate --peer` opens its sessions: the address to dial, what this device proves
/// itself with, and the trust each request is authorized against.
struct OverPeer<'a> {
    peer: &'a str,
    local: LocalPeer<'a>,
    published: &'a PublishedTrust<'a>,
}

/// Resolve `peer` (`HOST:PORT`) to a local-network address, connect within
/// [`CONNECT_TIMEOUT`], and complete the peering handshake.
///
/// # Errors
///
/// [`PeeringError::NotLan`] when `peer` resolves to no local-network address (nothing is
/// sent), a timeout or I/O failure, or the handshake's refusal.
pub async fn dial(peer: &str, local: LocalPeer<'_>) -> Result<PeerChannel, PeeringError> {
    let resolved: Vec<SocketAddr> =
        tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::lookup_host(peer))
            .await
            .map_err(|_| PeeringError::Timeout("resolving the peer"))?
            .map_err(|error| PeeringError::Io(format!("resolve {peer}: {error}")))?
            .collect();
    let address = match resolved.iter().find(|address| is_lan(address.ip())) {
        Some(address) => *address,
        None => {
            return Err(resolved.first().map_or_else(
                || PeeringError::Io(format!("{peer} resolves to no address")),
                |address| PeeringError::NotLan(address.ip()),
            ));
        }
    };
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| PeeringError::Timeout("connecting to the peer"))?
        .map_err(|error| PeeringError::Io(format!("connect to {address}: {error}")))?;
    tracing::debug!(%address, "peer delegate: connected; handshaking");
    capsule_sdk::peering::connect(tcp, local).await
}

/// Whether a session ended because the channel itself failed, so dialing again may finish it.
fn severed(error: &DelegationError) -> bool {
    matches!(
        error,
        DelegationError::Peering(PeeringError::Io(_) | PeeringError::Timeout(_))
    )
}

/// `peer delegate --peer`: dial the delegate, seal each asset for the device the handshake
/// verified, record each in the ledger before a byte of it leaves, then serve the session,
/// dialing again (up to [`MAX_REDIALS`] times) when the channel is severed. An asset the
/// delegate did not acknowledge is reported failed; the ledger keeps it pinned and re-queues it
/// at its deadline.
async fn delegate_over_peer(
    workspace: &Workspace,
    state: &PeerState,
    versions: &DbDirectoryVersions,
    clock: &dyn BootClock,
    options: &DelegateOptions,
    sessions: &OverPeer<'_>,
) -> Result<DelegateSummary, PeerError> {
    let mut summary = DelegateSummary {
        delegate: None,
        peer: Some(sessions.peer.to_owned()),
        handed_off: Vec::new(),
        failed: Vec::new(),
    };
    let fail_all = |summary: &mut DelegateSummary, kind: &'static str, message: &str| {
        for asset_id in &options.assets {
            summary
                .failed
                .push(PeerFailure::new(asset_id.to_string(), kind, message));
        }
    };
    let mut channel = match dial(sessions.peer, sessions.local).await {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(peer = sessions.peer, %error, "peer delegate: no verified channel");
            fail_all(&mut summary, error.kind(), &error.to_string());
            return Ok(summary);
        }
    };
    let delegate = channel.peer().device_id;
    summary.delegate = Some(delegate);
    if let Some(to) = options.to
        && to != delegate
    {
        let message = format!("the device at {} is {delegate}, not {to}", sessions.peer);
        tracing::warn!(%message, "peer delegate: refusing to hand off");
        fail_all(&mut summary, "peer_mismatch", &message);
        return Ok(summary);
    }

    let sealed = seal_each(
        workspace,
        delegate,
        sessions.local.trust,
        &options.assets,
        &mut summary.failed,
    );
    let mut ledger = state.load_ledger()?;
    for hand_off in &sealed {
        ledger.hand_off(
            hand_off,
            &clock.epoch(),
            clock.now_ms(),
            options.deadline_ms,
        );
    }
    state.save_ledger(&ledger)?;

    let mut acknowledged: BTreeSet<Uuid> = BTreeSet::new();
    let mut redials = 0;
    let ended: Option<(&'static str, String)> = loop {
        // Served even when empty (nothing sealed, or everything acknowledged before a sever),
        // so the delegate's session ends cleanly instead of waiting out its timeout. A clone
        // shares the sealed bytes; only the small per-blob records are copied.
        let pending: Vec<SealedHandOff> = sealed
            .iter()
            .filter(|hand_off| !acknowledged.contains(&hand_off.asset_id))
            .cloned()
            .collect();
        let served = offer_hand_offs(
            &mut channel,
            &pending,
            sessions.published,
            versions,
            |ack| {
                ledger.acknowledge(ack)?;
                state
                    .save_ledger(&ledger)
                    .map_err(|error| DelegationError::Inbox(error.to_string()))?;
                acknowledged.insert(ack.asset_id);
                Ok(())
            },
        )
        .await;
        match served {
            Ok(_) => break None,
            Err(error) if severed(&error) && redials < MAX_REDIALS => {
                redials += 1;
                tracing::warn!(%error, redials, "peer delegate: the channel was severed; dialing again");
                match dial(sessions.peer, sessions.local).await {
                    Ok(next) if next.peer().device_id == delegate => channel = next,
                    Ok(next) => {
                        break Some((
                            "peer_mismatch",
                            format!(
                                "the device at {} is now {}, not {delegate}",
                                sessions.peer,
                                next.peer().device_id
                            ),
                        ));
                    }
                    Err(error) => break Some((error.kind(), error.to_string())),
                }
            }
            Err(error) => break Some((error.kind(), error.to_string())),
        }
    };

    for hand_off in sealed {
        if acknowledged.contains(&hand_off.asset_id) {
            summary.handed_off.push(HandedOff {
                asset_id: hand_off.asset_id,
                file: None,
                hand_off: hand_off.address,
                bytes: hand_off.bytes.len() as u64,
                acknowledged: true,
            });
        } else {
            let (kind, why) = ended.clone().unwrap_or((
                "not_acknowledged",
                "the delegate ended the session without acknowledging it".to_owned(),
            ));
            summary.failed.push(PeerFailure::new(
                hand_off.asset_id.to_string(),
                kind,
                format!(
                    "{why}; this device keeps the original pinned and takes the upload back at \
                     its deadline unless the server holds it by then"
                ),
            ));
        }
    }
    Ok(summary)
}

// ─── peer accept ─────────────────────────────────────────────────────────────

/// A bound socket `capsule peer accept --listen` takes hand-offs on over the LAN peering channel,
/// and how long it waits for a peer to complete a session on it.
#[derive(Debug)]
pub struct PeerListener {
    listener: TcpListener,
    timeout: Duration,
}

impl PeerListener {
    /// Bind `address`. Binding any address is allowed; a connection from off the local network
    /// is refused before its handshake.
    ///
    /// # Errors
    ///
    /// [`PeerError::Listen`] when the address cannot be bound.
    pub async fn bind(address: SocketAddr, timeout: Duration) -> Result<Self, PeerError> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|error| PeerError::Listen {
                address: address.to_string(),
                error: error.to_string(),
            })?;
        Ok(Self { listener, timeout })
    }

    /// The address actually bound (port `0` resolved).
    ///
    /// # Errors
    ///
    /// [`PeerError::Listen`] when the socket cannot say.
    pub fn local_addr(&self) -> Result<SocketAddr, PeerError> {
        self.listener
            .local_addr()
            .map_err(|error| PeerError::Listen {
                address: "the listener".into(),
                error: error.to_string(),
            })
    }
}

/// What `capsule peer accept` takes and how it uploads.
#[derive(Debug)]
pub struct AcceptOptions {
    /// Hand-off files to take before uploading what the inbox holds.
    pub from: Vec<PathBuf>,
    /// This device's upload policy.
    pub policy: UploadPolicy,
    /// This device's connection class now.
    pub connection: ConnectionClass,
    /// Also take hand-offs a peer offers over the LAN peering channel on this listener, before
    /// uploading.
    pub listen: Option<PeerListener>,
}

/// One hand-off taken.
#[derive(Debug, Clone, Serialize)]
pub struct Accepted {
    /// The file it came from, when it came from one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The device it was pulled from over the peering channel (the device the handshake
    /// verified, and the hand-off's origin), when it came over the channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<Uuid>,
    /// The acknowledgement: written beside a file, sent over the channel to a peer.
    pub ack: HandOffAck,
    /// Where the acknowledgement was written, for a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_file: Option<String>,
    /// For a hand-off over the channel, whether the origin took the acknowledgement. Either
    /// way this device holds the hand-off and uploads it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged: Option<bool>,
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
                    file: Some(file.display().to_string()),
                    peer: None,
                    ack,
                    ack_file: Some(ack_file.display().to_string()),
                    acknowledged: None,
                });
            }
            Err(failure) => {
                tracing::warn!(file = %subject, kind = failure.kind, error = %failure.message, "peer accept: refused");
                summary.refused.push(failure);
            }
        }
    }

    if let Some(listener) = &options.listen {
        let local = LocalPeer {
            device_id: device,
            signer: workspace.device_signer(),
            trust: &trust,
            versions,
        };
        listen(
            listener,
            local,
            &remote.protocol_version,
            &inbox,
            &mut summary,
        )
        .await;
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

/// `peer accept --listen`: take hand-offs over the LAN peering channel until a session completes
/// or the listener's timeout passes.
///
/// A connection from off the local network is dropped before its handshake; a handshake that
/// fails (rate-limited, revoked, foreign, timed out, …) is reported and the listener keeps
/// waiting. Handshakes run concurrently ([`Admission`]), so one that stalls never holds the
/// listener, and the listen timeout bounds the wait for a connection and every handshake. A
/// verified session is not cut by it: it runs to its end, each of its frames bounded by the
/// channel's I/O timeout and each ranged pull by the channel's minimum pull rate, so a slow but
/// live pull is never thrown away and a trickling one is. Each verified session pulls every
/// hand-off the peer offers, verifies it was sealed by that peer for this device, persists it durably and acknowledges it over the same channel. A
/// session the channel severs keeps its partial pulls, and the listener waits for the origin to
/// dial again, resuming them; a session that ends cleanly ends the listening.
async fn listen(
    listener: &PeerListener,
    local: LocalPeer<'_>,
    max_protocol: &str,
    inbox: &FileInbox,
    summary: &mut AcceptSummary,
) {
    let subject = listener
        .local_addr()
        .map_or_else(|_| "the listener".to_owned(), |address| address.to_string());
    tracing::info!(listening = %subject, timeout = ?listener.timeout, "peer accept: listening for a peer");
    let limiter = HandshakeLimiter::default();
    let limiter = &limiter;
    let deadline = tokio::time::Instant::now() + listener.timeout;
    let handshake = |tcp: TcpStream, source: SocketAddr| async move {
        let client = match capsule_sdk::peering::accept(tcp, local, limiter).await {
            Ok(channel) => PeerClient::open(channel).await,
            Err(error) => Err(error),
        };
        (source, client)
    };
    let mut admission = Admission::new();
    let mut partials: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    // A pull the channel lost waits here for a later session to finish it.
    let mut interrupted: BTreeMap<String, PeerFailure> = BTreeMap::new();
    let mut sessions = 0_u32;
    loop {
        let client = match admission
            .next_session(&listener.listener, &subject, deadline, &handshake)
            .await
        {
            Admitted::Session(source, client) => {
                tracing::info!(%source, peer = %client.peer().device_id, "peer accept: verified session");
                client
            }
            Admitted::Failed => break,
            Admitted::TimedOut => {
                if sessions == 0 {
                    summary.refused.push(PeerFailure::new(
                        &subject,
                        "peering_timeout",
                        "no peer connected and verified before the listen timeout",
                    ));
                }
                break;
            }
        };
        sessions += 1;
        let peer = client.peer().device_id;
        // Not bounded by the listen deadline: a verified session runs to its end, each of its
        // frames bounded by the channel's I/O timeout and each range by its minimum pull rate.
        let taken = take_hand_offs(
            &client,
            local.trust,
            local.versions,
            local.device_id,
            max_protocol,
            inbox,
            &mut partials,
        )
        .await;
        for item in taken.taken {
            match (item.ack, item.error) {
                (Some(ack), error) => {
                    interrupted.remove(&item.address);
                    if let Some(error) = &error {
                        tracing::warn!(hand_off = %item.address, %error, "peer accept: held, but the origin did not take the acknowledgement");
                    }
                    summary.accepted.push(Accepted {
                        file: None,
                        peer: Some(peer),
                        ack,
                        ack_file: None,
                        acknowledged: Some(error.is_none()),
                    });
                }
                (None, Some(error)) => {
                    let failure = PeerFailure::delegation(&item.address, &error);
                    if matches!(failure.kind, "peering_io" | "peering_timeout") {
                        interrupted.insert(item.address, failure);
                    } else {
                        tracing::warn!(hand_off = %item.address, kind = failure.kind, "peer accept: refused");
                        summary.refused.push(failure);
                    }
                }
                (None, None) => {}
            }
        }
        if !taken.severed {
            break;
        }
        tracing::info!(
            partial = partials.len(),
            "peer accept: the session was severed; waiting for the origin to dial again"
        );
    }
    summary.refused.extend(admission.into_refusals());
    summary.refused.extend(interrupted.into_values());
}

/// The most handshakes `peer accept --listen` runs at once. While this many are in flight the
/// listener stops accepting, so the kernel's backlog holds further connections until one ends.
const MAX_PENDING_HANDSHAKES: usize = 16;

/// The most handshakes one source address has in flight at once. A further connection from it
/// is refused before its handshake, so no one host can take more than its share of
/// [`MAX_PENDING_HANDSHAKES`] and starve another.
const MAX_HANDSHAKES_PER_SOURCE: usize = 2;

/// The most distinct (source, kind) refusals `--listen` itemizes; the rest are counted in one
/// `refusals_elided` entry, so a connection flood bounds both memory and the `--json` report.
const MAX_ITEMIZED_REFUSALS: usize = 64;

/// The connections `--listen` refused, one entry per source address and kind with its count.
#[derive(Debug, Default)]
struct Refusals {
    entries: Vec<PeerFailure>,
    index: BTreeMap<(String, &'static str), usize>,
    elided: u32,
}

impl Refusals {
    /// Record one refusal of `subject` for `kind`.
    fn record(&mut self, subject: String, kind: &'static str, message: String) {
        if let Some(entry) = self
            .index
            .get(&(subject.clone(), kind))
            .and_then(|&at| self.entries.get_mut(at))
        {
            entry.occurrences = Some(entry.occurrences.unwrap_or(1).saturating_add(1));
            return;
        }
        if self.entries.len() >= MAX_ITEMIZED_REFUSALS {
            self.elided = self.elided.saturating_add(1);
            return;
        }
        self.index
            .insert((subject.clone(), kind), self.entries.len());
        self.entries.push(PeerFailure::new(subject, kind, message));
    }

    /// The refusals as `--json` reports them, in the order each was first seen.
    fn into_failures(self) -> Vec<PeerFailure> {
        let mut failures = self.entries;
        if self.elided > 0 {
            failures.push(PeerFailure {
                occurrences: Some(self.elided),
                ..PeerFailure::new(
                    "other sources",
                    "refusals_elided",
                    format!(
                        "{} more refused connections from other sources were not itemized",
                        self.elided
                    ),
                )
            });
        }
        failures
    }
}

/// The handshakes `peer accept --listen` has in flight, how many each source holds, and the
/// connections it refused.
struct Admission<Fut> {
    pending: FuturesUnordered<Fut>,
    per_source: BTreeMap<IpAddr, usize>,
    refusals: Refusals,
}

impl<Fut> Admission<Fut> {
    fn new() -> Self {
        Self {
            pending: FuturesUnordered::new(),
            per_source: BTreeMap::new(),
            refusals: Refusals::default(),
        }
    }

    /// The refusals recorded, as `--json` reports them.
    fn into_refusals(self) -> Vec<PeerFailure> {
        self.refusals.into_failures()
    }

    fn refuse(&mut self, source: IpAddr, error: &PeeringError) {
        self.refusals
            .record(source.to_string(), error.kind(), error.to_string());
    }

    fn release(&mut self, source: IpAddr) {
        if let Some(held) = self.per_source.get_mut(&source) {
            *held = held.saturating_sub(1);
            if *held == 0 {
                self.per_source.remove(&source);
            }
        }
    }

    /// Accept connections on `listener` and run their handshakes concurrently, up to
    /// [`MAX_PENDING_HANDSHAKES`] at once and [`MAX_HANDSHAKES_PER_SOURCE`] per source address,
    /// until one completes or `deadline` passes. A connection from off the local network, or
    /// from a source already at its share, is refused before its handshake; a handshake that
    /// fails is recorded while the others go on. So a peer that stalls its handshakes holds
    /// only its own slots and never the listener. Handshakes still in flight stay pending for
    /// the next call; every one of them is bounded by `deadline`, which ends the wait.
    async fn next_session<T, F>(
        &mut self,
        listener: &TcpListener,
        subject: &str,
        deadline: tokio::time::Instant,
        handshake: &F,
    ) -> Admitted<T>
    where
        F: Fn(TcpStream, SocketAddr) -> Fut,
        Fut: Future<Output = (SocketAddr, Result<T, PeeringError>)>,
    {
        loop {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => return Admitted::TimedOut,
                Some((source, result)) = self.pending.next(), if !self.pending.is_empty() => {
                    self.release(source.ip());
                    match result {
                        Ok(session) => return Admitted::Session(source, session),
                        Err(error) => {
                            tracing::warn!(%source, %error, "peer accept: no verified session");
                            self.refuse(source.ip(), &error);
                        }
                    }
                }
                accepted = listener.accept(), if self.pending.len() < MAX_PENDING_HANDSHAKES => {
                    let (tcp, source) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => {
                            self.refusals.record(subject.to_owned(), "peering_io", error.to_string());
                            return Admitted::Failed;
                        }
                    };
                    let ip = source.ip();
                    if !is_lan(ip) {
                        tracing::warn!(%source, "peer accept: refusing a connection from off the local network");
                        self.refuse(ip, &PeeringError::NotLan(ip));
                        continue;
                    }
                    let held = self.per_source.entry(ip).or_insert(0);
                    if *held >= MAX_HANDSHAKES_PER_SOURCE {
                        tracing::warn!(%source, held = *held, "peer accept: refusing a connection from a source at its handshake share");
                        self.refuse(ip, &PeeringError::RateLimited(ip));
                        continue;
                    }
                    *held += 1;
                    tracing::debug!(%source, pending = self.pending.len(), "peer accept: handshake started");
                    self.pending.push(handshake(tcp, source));
                }
            }
        }
    }
}

/// What [`Admission::next_session`] got.
#[derive(Debug)]
enum Admitted<T> {
    /// A handshake from this source completed.
    Session(SocketAddr, T),
    /// The listen deadline passed first.
    TimedOut,
    /// The listener itself failed; the failure is recorded.
    Failed,
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
    /// Every delete made after a hand-off: held until the create's custody, sent, superseded,
    /// or failed and kept for the next run.
    pub deletes: Vec<DeleteView>,
}

// ─── Delegated assets: push, the library browser, and deletes after hand-off ─

/// The name a delegate is shown under in a delegation's status line.
#[must_use]
pub fn delegate_name(delegate: Uuid) -> String {
    format!("device {delegate}")
}

/// The assets `ledger` holds an open delegation for (handed off or acknowledged) that is still
/// inside its deadline on the boot clock `epoch` names at `now_ms`: the ones `capsule push`
/// leaves to their delegate.
///
/// A deadline that passed, or one set under another boot, is not open here even before
/// `peer status` re-queues it, so a push never waits on a hand-off the ledger would take back.
/// A revoked delegate is only known once `peer status` checks the account's directory; until
/// then its asset is skipped, and the next `peer status` re-queues it.
#[must_use]
pub fn open_delegations(ledger: &OriginLedger, epoch: &str, now_ms: u64) -> BTreeSet<Uuid> {
    ledger
        .delegations()
        .filter(|d| {
            matches!(
                d.state,
                DelegationState::HandedOff | DelegationState::Acknowledged
            ) && d.clock_epoch == epoch
                && now_ms < d.deadline_ms
        })
        .map(|d| d.asset_id)
        .collect()
}

/// [`open_delegations`] of this device's persisted ledger, at `clock`'s now.
///
/// # Errors
///
/// [`PeerError::State`] when the ledger cannot be read.
pub fn delegated_assets(
    state: &PeerState,
    clock: &dyn BootClock,
) -> Result<BTreeSet<Uuid>, PeerError> {
    let ledger = state.load_ledger()?;
    let open = open_delegations(&ledger, &clock.epoch(), clock.now_ms());
    tracing::debug!(open = open.len(), "peer: open delegations read for a push");
    Ok(open)
}

/// The assets `capsule push` (and the TUI's push screen) leaves to their delegate for `device`:
/// [`delegated_assets`] of its persisted ledger at the system boot clock. A ledger that cannot
/// be read skips nothing, which is logged: the push then uploads those assets as well, and
/// deduplication makes that safe, only not cheap.
#[must_use]
pub fn delegated_for_push(device: Uuid) -> BTreeSet<Uuid> {
    PeerState::for_device(device)
        .and_then(|state| delegated_assets(&state, &SystemBootClock::default()))
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "push: the origin ledger could not be read; skipping no delegated asset");
            BTreeSet::new()
        })
}

/// [`delegation_lines`] of `device`'s persisted ledger, for the library browser. A ledger that
/// cannot be read shows no line, which is logged: the browser still opens.
#[must_use]
pub fn delegation_lines_for(device: Uuid) -> BTreeMap<Uuid, String> {
    PeerState::for_device(device)
        .and_then(|state| state.load_ledger())
        .map_or_else(
            |error| {
                tracing::warn!(%error, "library: the origin ledger could not be read; no delegation is shown");
                BTreeMap::new()
            },
            |ledger| delegation_lines(&ledger, short_delegate_name),
        )
}

/// The name a delegate is shown under where space is short, as the library browser shows every
/// device: its id's last eight hex digits.
#[must_use]
pub fn short_delegate_name(delegate: Uuid) -> String {
    format!("device {}", crate::utils::ids::short_id(delegate))
}

/// The line the library browser shows beside each delegated asset whose custody is not
/// confirmed yet, by asset, each delegate named by `name`: "handed to {device}, awaiting
/// server" while the hand-off is open, the re-queued line once the origin took it back. A
/// released delegation has none.
#[must_use]
pub fn delegation_lines(
    ledger: &OriginLedger,
    name: impl Fn(Uuid) -> String,
) -> BTreeMap<Uuid, String> {
    ledger
        .delegations()
        .filter(|d| d.state != DelegationState::Released)
        .map(|d| (d.asset_id, d.status_line(&name(d.delegate))))
        .collect()
}

/// A delete the origin ledger released once the create's custody was confirmed, kept until the
/// server applies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct PendingDelete {
    /// The asset it deletes.
    pub asset_id: Uuid,
    /// The lifecycle op, as [`remote::delete_op`] encoded it.
    pub op: Vec<u8>,
}

/// What happened to one delete made after a hand-off, as `capsule peer status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeleteView {
    /// The asset.
    pub asset_id: Uuid,
    /// `held` (the create has no confirmed custody yet), `sent` (the server applied it),
    /// `superseded` (the asset was restored since, so the delete was dropped unsent) or
    /// `failed` (kept, and sent again by the next run).
    pub outcome: &'static str,
    /// The server's stable `error.*` code, for a failed one that carried it.
    pub code: Option<String>,
    /// The English detail, for a failed one.
    pub message: Option<String>,
}

/// Route every delete the library made after a hand-off through the ledger: for each
/// delegation whose custody is not confirmed and whose library chain head is a `delete`, hold
/// that signed delete ([`OriginLedger::hold_delete`]) unless it is held already. Returns the
/// assets whose delete was newly held.
///
/// # Errors
///
/// [`PeerError::State`] when a delete record does not encode, which is a corrupt library.
pub fn hold_deletes(
    ledger: &mut OriginLedger,
    workspace: &Workspace,
) -> Result<Vec<Uuid>, PeerError> {
    let mut candidates = Vec::new();
    for delegation in ledger
        .delegations()
        .filter(|d| d.state != DelegationState::Released)
    {
        let op = remote::delete_op(workspace, &delegation.asset_id)
            .map_err(|error| PeerError::State(format!("a delete record: {error}")))?;
        if let Some(op) = op.filter(|op| !delegation.held_deletes.contains(op)) {
            candidates.push((delegation.asset_id, op));
        }
    }
    let mut held = Vec::new();
    for (asset_id, op) in candidates {
        match ledger.hold_delete(asset_id, op) {
            DeleteDisposition::Held => held.push(asset_id),
            // Only a released delegation sends now, and those were not routed here.
            DeleteDisposition::SendNow(_) => {}
        }
    }
    Ok(held)
}

/// Send every delete in `outbox`, in order, under `session`, and keep the ones the server did
/// not apply. A delete whose asset the library no longer holds in trash was undone by a restore
/// after it was held, so it is dropped unsent rather than deleting what the user brought back.
async fn send_deletes(
    remote: &RemoteConfig,
    session: &Session,
    workspace: &Workspace,
    outbox: Vec<PendingDelete>,
) -> (Vec<PendingDelete>, Vec<DeleteView>) {
    let mut kept = Vec::new();
    let mut views = Vec::new();
    for pending in outbox {
        let asset_id = pending.asset_id;
        if !workspace.is_trashed(&asset_id) {
            tracing::warn!(%asset_id, "peer status: a held delete was undone by a restore; dropped unsent");
            views.push(DeleteView {
                asset_id,
                outcome: "superseded",
                code: None,
                message: None,
            });
            continue;
        }
        match remote::send_op(remote, session.clone(), &pending.op).await {
            Ok(applied) => {
                tracing::info!(%asset_id, sync_seq = applied.sync_seq, replayed = applied.replayed, "peer status: held delete sent after custody");
                views.push(DeleteView {
                    asset_id,
                    outcome: "sent",
                    code: None,
                    message: None,
                });
            }
            Err(failure) => {
                tracing::warn!(%asset_id, code = ?failure.code, error = %failure.message, "peer status: held delete not applied; kept for the next run");
                views.push(DeleteView {
                    asset_id,
                    outcome: "failed",
                    code: failure.code,
                    message: Some(failure.message),
                });
                kept.push(pending);
            }
        }
    }
    (kept, views)
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
    // A delete made since the hand-off waits in the ledger for the create's custody.
    let newly_held = hold_deletes(&mut ledger, workspace)?;
    if !newly_held.is_empty() {
        tracing::info!(
            held = newly_held.len(),
            "peer status: deletes held until custody"
        );
    }
    state.save_ledger(&ledger)?;

    // The origin's own custody check of its own hashes, with its own session: nothing the
    // delegate relays is consulted.
    let verify = StorageVerifyClient::new(VerifyTransport::with_session(
        session.clone(),
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
    let mut outbox = state.load_outbox()?;
    for asset_id in pending {
        let read_back = reads_back(workspace, &asset_id);
        let accepted = verify_asset_accepts(workspace, &asset_id);
        match ledger.release(asset_id, &verifier, accepted, read_back)? {
            ReleaseOutcome::Released { deletes } => {
                tracing::info!(%asset_id, held_deletes = deletes.len(), "peer status: released");
                outbox.extend(deletes.into_iter().map(|op| PendingDelete { asset_id, op }));
            }
            ReleaseOutcome::Retained(reason) => {
                kept.insert(asset_id, kept_because(&reason));
            }
        }
    }
    // The released deletes are in the outbox before the ledger forgets them, so a run that
    // stops here sends them next time; a delete sent twice is a replay the server answers alike.
    state.save_outbox(&outbox)?;
    state.save_ledger(&ledger)?;
    let (unsent, mut deletes) = send_deletes(remote, &session, workspace, outbox).await;
    state.save_outbox(&unsent)?;
    deletes.extend(
        ledger
            .delegations()
            .filter(|d| d.state != DelegationState::Released && !d.held_deletes.is_empty())
            .map(|d| DeleteView {
                asset_id: d.asset_id,
                outcome: "held",
                code: None,
                message: None,
            }),
    );

    let delegations = ledger
        .delegations()
        .map(|d| DelegationView {
            asset_id: d.asset_id,
            delegate: d.delegate,
            state: d.state,
            status: d.status_line(&delegate_name(d.delegate)),
            requeued_because: d.requeued_because,
            kept_because: kept.get(&d.asset_id).copied(),
        })
        .collect();
    Ok(StatusSummary {
        delegations,
        refused_acks,
        quarantined: quarantined(&state.inbox())?,
        deletes,
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
