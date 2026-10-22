//! `capsule restore`: download originals from the server and verify them byte for byte.
//!
//! The network legs are the SDK's: the feed comes from [`crate::remote::restore`]'s fresh drain,
//! and every blob rides [`RangedFetcher`] over whatever [`BlobSource`] the caller hands in
//! ([`capsule_sdk::fetch::HttpBlobSource`] in production, a mock in tests). Verification is the
//! library's: [`Workspace::open_remote_original_from`] runs each entry through the
//! `verify_asset` chokepoint, decrypts the original under this library's album keys and checks
//! the plaintext against the signed sidecar's digest. What this module owns is the orchestration
//! between them, and the three properties a restore needs on top:
//!
//! - **Bounded memory.** An original never sits in memory whole, whatever its size: the fetch
//!   streams it one `Range` window at a time into its part file on disk
//!   ([`RangedFetcher::fetch_into`]), and the library decrypts that file into the staging file
//!   one STREAM chunk at a time. A restore holds a window and a few chunks.
//! - **Resume without re-fetching.** The part file under `OUT/.capsule-restore/` is where the
//!   ciphertext lands, each window appended as it arrives. A later run serves that prefix from
//!   disk and asks the server only for the bytes after it; the fetcher still hashes the whole
//!   ciphertext against its content address, so a prefix is trusted only once the blob it
//!   belongs to verifies. A part file whose blob fails that check is removed, so a bad prefix is
//!   fetched again rather than kept. The production source reads each `Range` whole and hands
//!   over nothing from a response whose body drops, so a restore always asks for bounded
//!   windows ([`RESTORE_RANGE_WINDOW`], or the connection class's smaller one): a run that is
//!   cut off loses at most the window in flight, whatever the connection class.
//! - **Atomic writes.** An original is decrypted into a temporary file in the staging
//!   directory, and only once the library has verified it end to end is that file flushed to
//!   disk and renamed into `OUT`. A file named for the asset in `OUT` is therefore always a
//!   complete, verified original, and a run that finds one skips the asset without fetching
//!   anything.
//!
//! One asset's failure is that asset's: it is recorded with its reason and the run moves on.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::provenance::ProvenanceRecord;
use capsule_core::lifecycle::{
    LifecycleError, QuarantineReason, RemoteHead, RemoteOriginal, SyncApplyOutcome, Workspace,
};
use capsule_sdk::fetch::{BlobSource, FetchError, RangeOutcome, RangedFetcher};
use capsule_sdk::net::{ConnectionClass, StallConfig, SystemClock, TokioSleeper};
use capsule_sdk::sync::{ChangeKind, FeedEntry};
use indexmap::IndexMap;
use serde::Serialize;
use tracing::instrument;
use uuid::Uuid;

/// The staging directory under the output directory: part files and temporary writes.
pub const STAGING_DIR: &str = ".capsule-restore";

/// The largest `Range` window a restore asks for (4 MiB): the most a cut-off run re-fetches.
pub const RESTORE_RANGE_WINDOW: u64 = 4 * 1024 * 1024;

/// What a restore covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreScope {
    /// Every live asset the feed lists.
    Library,
    /// Every live asset of one album.
    Album(Uuid),
    /// One asset.
    Asset(Uuid),
}

impl RestoreScope {
    fn covers(self, album: Uuid, asset: Uuid) -> bool {
        match self {
            Self::Library => true,
            Self::Album(id) => id == album,
            Self::Asset(id) => id == asset,
        }
    }
}

/// How a `capsule restore` run is configured.
#[derive(Debug, Clone)]
pub struct RestoreOptions {
    /// What to restore.
    pub scope: RestoreScope,
    /// The directory the originals are written to.
    pub out: PathBuf,
    /// The connection class the ranged fetch sizes its windows for.
    pub connection: ConnectionClass,
    /// The largest `Range` window to ask for, normally [`RESTORE_RANGE_WINDOW`]. A class with a
    /// smaller window of its own (`adverse`) keeps it.
    pub range_window: u64,
}

impl RestoreOptions {
    /// The window every blob request of this run asks for. Never open-ended: the production
    /// source keeps nothing from a response whose body drops, so only whole windows persist.
    fn window(&self) -> u64 {
        let window = self.range_window.max(1);
        self.connection
            .range_window()
            .map_or(window, |class| class.min(window))
    }
}

/// Where one asset ended up after a restore run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetRestoreStatus {
    /// Fetched, verified, decrypted and written.
    Restored,
    /// A file named for the asset was already in the output directory; nothing was fetched.
    AlreadyPresent,
    /// The asset was not restored; `error` says why.
    Failed,
}

/// Why an asset failed, in the shape `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetRestoreFailure {
    /// A stable name for where the asset stopped: `no_original`, `awaiting_original`,
    /// `malformed_feed`, `fetch`, `keys_unavailable`, `quarantined`, `undecryptable`,
    /// `digest_mismatch`, `workspace` or `write`.
    pub kind: &'static str,
    /// The server's stable `error.*` code, when it supplied one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
}

impl AssetRestoreFailure {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: None,
            message: message.into(),
        }
    }

    fn fetch(what: &str, error: &FetchError) -> Self {
        Self {
            kind: "fetch",
            code: error.error_code().map(str::to_owned),
            message: format!("fetching the {what} failed: {error}"),
        }
    }
}

/// What a restore run did for one asset — one line of the `--json` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetRestoreOutcome {
    /// The asset id.
    pub asset_id: String,
    /// The album the feed lists it in.
    pub album_id: String,
    /// Where the asset ended up.
    pub status: AssetRestoreStatus,
    /// The original's path, for a restored or already present asset.
    pub path: Option<String>,
    /// Ciphertext bytes requested from the server this run.
    pub fetched_bytes: u64,
    /// Ciphertext bytes a part file from an earlier run supplied instead.
    pub resumed_bytes: u64,
    /// Why the asset failed, for a [`AssetRestoreStatus::Failed`] asset only.
    pub error: Option<AssetRestoreFailure>,
}

/// The outcome of a `capsule restore` run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RestoreSummary {
    /// Live assets in scope.
    pub assets: usize,
    /// Assets restored this run.
    pub restored: usize,
    /// Assets whose original was already in the output directory.
    pub already_present: usize,
    /// Assets that were not restored; each is in `outcomes` with its error.
    pub failed: usize,
    /// Assets in scope whose head is a deletion, which a restore leaves out.
    pub skipped_deleted: usize,
    /// Ciphertext bytes requested from the server.
    pub fetched_bytes: u64,
    /// Ciphertext bytes served from part files left by an earlier run.
    pub resumed_bytes: u64,
    /// The output directory.
    pub out: String,
    /// One entry per live asset in scope, in feed order.
    pub outcomes: Vec<AssetRestoreOutcome>,
}

impl RestoreSummary {
    /// The assets that were not restored.
    pub fn failures(&self) -> impl Iterator<Item = &AssetRestoreOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status == AssetRestoreStatus::Failed)
    }

    fn record(&mut self, outcome: AssetRestoreOutcome) {
        self.fetched_bytes += outcome.fetched_bytes;
        self.resumed_bytes += outcome.resumed_bytes;
        match outcome.status {
            AssetRestoreStatus::Restored => self.restored += 1,
            AssetRestoreStatus::AlreadyPresent => self.already_present += 1,
            AssetRestoreStatus::Failed => self.failed += 1,
        }
        self.outcomes.push(outcome);
    }
}

/// The summary line after a restore. English inline, as every CLI line is going to be.
#[must_use]
pub fn summary_line(summary: &RestoreSummary) -> String {
    format!(
        "Restored {} of {} assets to {} ({} already present, {} failed, {} deleted skipped).",
        summary.restored,
        summary.assets,
        summary.out,
        summary.already_present,
        summary.failed,
        summary.skipped_deleted
    )
}

/// One failed asset, as the line the human output prints for it.
#[must_use]
pub fn failure_line(outcome: &AssetRestoreOutcome) -> String {
    let detail = outcome
        .error
        .as_ref()
        .map_or("no detail", |error| error.message.as_str());
    format!("asset {} failed: {detail}", outcome.asset_id)
}

/// Why a finished run exits non-zero, or `None` when every asset in scope is in place.
#[must_use]
pub fn failed_run_reason(summary: &RestoreSummary) -> Option<String> {
    (summary.failed > 0).then(|| {
        format!(
            "{} of {} assets were not restored",
            summary.failed, summary.assets
        )
    })
}

/// One asset's current head, as the feed last described it.
struct Head {
    album: Uuid,
    asset: Uuid,
    entry: FeedEntry,
}

/// The heads in scope, one per asset, in the order the feed first named each asset.
///
/// The feed carries every change, so an asset appears once per change; the last entry for an
/// asset is its head. An entry whose ids are not UUIDs names nothing this client can restore
/// into and is dropped with a warning.
fn heads_in_scope(entries: Vec<FeedEntry>, scope: RestoreScope) -> Vec<Head> {
    let mut heads: IndexMap<Uuid, Head> = IndexMap::new();
    for entry in entries {
        let parse = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| Uuid::parse_str(text).ok())
        };
        let (Some(album), Some(asset)) = (parse(&entry.album_id), parse(&entry.asset_id)) else {
            tracing::warn!(
                album = %String::from_utf8_lossy(&entry.album_id),
                asset = %String::from_utf8_lossy(&entry.asset_id),
                "restore: a feed entry's ids are not UUIDs; skipped"
            );
            continue;
        };
        if scope.covers(album, asset) {
            heads.insert(
                asset,
                Head {
                    album,
                    asset,
                    entry,
                },
            );
        }
    }
    heads.into_values().collect()
}

/// Restore every asset `entries` holds in `options.scope` into `options.out`.
///
/// `entries` is a drain of the sync feed; `source` serves blobs by content address.
///
/// # Errors
///
/// Only when the output or staging directory cannot be created. Everything that goes wrong for
/// one asset is recorded in its outcome.
#[instrument(skip_all, fields(out = %options.out.display(), scope = ?options.scope))]
pub async fn restore_entries<B: BlobSource + Sync>(
    workspace: &Workspace,
    source: &B,
    entries: Vec<FeedEntry>,
    options: &RestoreOptions,
) -> std::io::Result<RestoreSummary> {
    let staging = options.out.join(STAGING_DIR);
    std::fs::create_dir_all(&staging)?;
    let present = present_stems(&options.out)?;
    let fetcher = RangedFetcher::from_parts(
        options.connection,
        StallConfig::default(),
        Some(options.window()),
        TokioSleeper,
        SystemClock::new(),
    );
    let mut summary = RestoreSummary {
        out: options.out.display().to_string(),
        ..Default::default()
    };

    for head in heads_in_scope(entries, options.scope) {
        if head.entry.kind == ChangeKind::Deleted {
            tracing::debug!(asset = %head.asset, "restore: the head is a deletion; skipped");
            summary.skipped_deleted += 1;
            continue;
        }
        summary.assets += 1;
        let mut outcome = AssetRestoreOutcome {
            asset_id: head.asset.to_string(),
            album_id: head.album.to_string(),
            status: AssetRestoreStatus::Restored,
            path: None,
            fetched_bytes: 0,
            resumed_bytes: 0,
            error: None,
        };
        if let Some(existing) = present.iter().find(|(stem, _)| *stem == head.asset) {
            tracing::info!(asset = %head.asset, path = %existing.1.display(), "restore: already present");
            outcome.status = AssetRestoreStatus::AlreadyPresent;
            outcome.path = Some(existing.1.display().to_string());
            summary.record(outcome);
            continue;
        }
        let run = AssetRestore {
            workspace,
            source,
            fetcher: &fetcher,
            staging: &staging,
            out: &options.out,
        };
        match run.restore(&head, &mut outcome).await {
            Ok(path) => {
                tracing::info!(asset = %head.asset, path = %path.display(), "restore: original restored");
                outcome.path = Some(path.display().to_string());
            }
            Err(failure) => {
                tracing::warn!(
                    asset = %head.asset,
                    kind = failure.kind,
                    error = %failure.message,
                    "restore: asset failed; continuing with the next"
                );
                outcome.status = AssetRestoreStatus::Failed;
                outcome.error = Some(failure);
            }
        }
        summary.record(outcome);
    }
    tracing::info!(
        assets = summary.assets,
        restored = summary.restored,
        already_present = summary.already_present,
        failed = summary.failed,
        skipped_deleted = summary.skipped_deleted,
        fetched_bytes = summary.fetched_bytes,
        resumed_bytes = summary.resumed_bytes,
        "restore complete"
    );
    Ok(summary)
}

/// The files directly in `out` whose stem is an asset id — originals an earlier run wrote.
fn present_stems(out: &Path) -> std::io::Result<Vec<(Uuid, PathBuf)>> {
    let mut present = Vec::new();
    for entry in std::fs::read_dir(out)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        if let Some(id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| Uuid::parse_str(stem).ok())
        {
            present.push((id, path));
        }
    }
    Ok(present)
}

/// Everything restoring one asset borrows from the run.
struct AssetRestore<'a, B> {
    workspace: &'a Workspace,
    source: &'a B,
    fetcher: &'a RangedFetcher,
    staging: &'a Path,
    out: &'a Path,
}

impl<B: BlobSource + Sync> AssetRestore<'_, B> {
    /// Fetch, verify, decrypt and write one asset, returning the path written.
    async fn restore(
        &self,
        head: &Head,
        outcome: &mut AssetRestoreOutcome,
    ) -> Result<PathBuf, AssetRestoreFailure> {
        let entry = &head.entry;
        let Some(original) = entry.blobs.original.as_ref() else {
            return Err(AssetRestoreFailure::new(
                "no_original",
                "the feed lists no original for this asset",
            ));
        };
        if !entry.original_held {
            return Err(AssetRestoreFailure::new(
                "awaiting_original",
                "the server does not hold the original yet; push it from the device that has it",
            ));
        }

        let metadata = self.fetch_metadata(entry).await?;
        let (part, ciphertext_hash) = self
            .fetch_original(&original.ciphertext_hash, original.size, outcome)
            .await?;

        // A restore verifies the head the feed serves against the chain position it names: this
        // client holds no earlier record of the asset to place it after. Its signatures, its
        // signer's directory membership and the bytes it commits to are still all checked.
        let local_chain_head = ProvenanceRecord::decode(&entry.manifest_cbor)
            .ok()
            .and_then(|record| record.prior_provenance_hash);
        let remote = RemoteHead {
            album_id: head.album,
            manifest_cbor: &entry.manifest_cbor,
            metadata_blob: &metadata,
            original_ciphertext_hash: ciphertext_hash,
            local_chain_head,
        };
        let written = self.open_into_out(head.asset, remote, &part);
        // The part file has served its purpose whatever the verdict: its bytes hashed to their
        // address, so a later run would only be told the same thing again.
        remove_part(&part);
        written
    }

    /// Decrypt the verified part file through the library into a staging file, and move it into
    /// `OUT` only once the library vouches for it. Returns the path written.
    fn open_into_out(
        &self,
        asset: Uuid,
        remote: RemoteHead<'_>,
        part: &Path,
    ) -> Result<PathBuf, AssetRestoreFailure> {
        let write_failure =
            |error: std::io::Error| AssetRestoreFailure::new("write", error.to_string());
        let ciphertext = std::fs::File::open(part).map_err(write_failure)?;
        let staged = self.staging.join(format!("{asset}.tmp"));
        let sink = BufWriter::new(std::fs::File::create(&staged).map_err(write_failure)?);
        let opened = match self
            .workspace
            .open_remote_original_from(remote, ciphertext, sink)
        {
            Ok(opened) => opened,
            Err(error) => {
                discard_staged(&staged);
                let kind = if matches!(error, LifecycleError::Io(_)) {
                    "write"
                } else {
                    "workspace"
                };
                return Err(AssetRestoreFailure::new(kind, error.to_string()));
            }
        };
        let (facts, sink) = match opened {
            RemoteOriginal::Opened { facts, plaintext } => (facts, plaintext),
            other => {
                discard_staged(&staged);
                return Err(not_opened(other));
            }
        };
        let ext = facts
            .sidecar
            .as_ref()
            .map_or("bin", |sidecar| extension_for(&sidecar.content_type));
        let target = self.out.join(format!("{asset}.{ext}"));
        commit_staged(sink, &staged, &target).map_err(|error| {
            discard_staged(&staged);
            write_failure(error)
        })?;
        Ok(target)
    }

    /// The sealed metadata blob the head commits to, or nothing for a head that binds none.
    async fn fetch_metadata(&self, entry: &FeedEntry) -> Result<Vec<u8>, AssetRestoreFailure> {
        if entry.metadata_blob.is_empty() {
            return Ok(Vec::new());
        }
        let hash = String::from_utf8_lossy(&entry.metadata_blob).into_owned();
        // The feed names the metadata blob by address only: its blob list carries the original
        // and the derivatives, so a size is there only when a server chose to list it.
        let listed = entry
            .blobs
            .derivatives
            .iter()
            .find(|blob| blob.ciphertext_hash == hash)
            .map(|blob| blob.size);
        let fetched = match listed {
            Some(size) => self.fetcher.fetch(self.source, &hash, size).await,
            None => {
                self.fetcher
                    .fetch_unsized(self.source, &hash, MAX_METADATA_BLOB_BYTES)
                    .await
            }
        };
        fetched.map_err(|error| AssetRestoreFailure::fetch("metadata blob", &error))
    }

    /// Fetch the original's ciphertext into its part file, resuming from whatever an earlier
    /// run left there, and return the part file's path with the content address every byte in
    /// it was verified against. Holds one `Range` window of the original at a time.
    async fn fetch_original(
        &self,
        hash: &str,
        size: u64,
        outcome: &mut AssetRestoreOutcome,
    ) -> Result<(PathBuf, Hash32), AssetRestoreFailure> {
        // The address names a file on this machine, so it must be one: a feed that hands over
        // anything but a SHA-256 in lowercase hex never reaches a path.
        let address = is_content_address(hash)
            .then(|| Hash32::from_hex(hash).ok())
            .flatten()
            .ok_or_else(|| {
                AssetRestoreFailure::new(
                    "malformed_feed",
                    format!(
                        "the feed names the original by {hash:?}, which is not a content address"
                    ),
                )
            })?;
        let part = self.staging.join(format!("{hash}.part"));
        let write_failure =
            |error: std::io::Error| AssetRestoreFailure::new("write", error.to_string());
        let source = ResumingSource::open(self.source, &part, size).map_err(write_failure)?;
        let mut sink = PartSink::open(&part, source.held_len()).map_err(write_failure)?;
        if source.held_len() > 0 {
            tracing::info!(
                hash,
                resumed_from = source.held_len(),
                "restore: resuming the original from its part file"
            );
        }
        let fetched = self
            .fetcher
            .fetch_into(&source, hash, size, &mut sink)
            .await;
        outcome.resumed_bytes = source.held_len();
        outcome.fetched_bytes = source.fetched();
        match fetched {
            Ok(_signals) => {
                tracing::debug!(
                    hash,
                    size,
                    "restore: the original's part file is complete and verified"
                );
                Ok((part, address))
            }
            Err(error) => {
                // Bytes that do not hash to their address are no prefix to resume from.
                if matches!(error, FetchError::IntegrityFailed { .. }) {
                    remove_part(&part);
                }
                if let FetchError::Sink(io) = &error {
                    return Err(AssetRestoreFailure::new(
                        "write",
                        format!("extending the original's part file failed: {io}"),
                    ));
                }
                Err(AssetRestoreFailure::fetch("original", &error))
            }
        }
    }
}

/// The failure for an original the library did not open.
fn not_opened<W>(verdict: RemoteOriginal<W>) -> AssetRestoreFailure {
    match verdict {
        RemoteOriginal::Refused(verdict) => refusal(&verdict),
        RemoteOriginal::Undecryptable { .. } => AssetRestoreFailure::new(
            "undecryptable",
            "the original did not open under this library's album keys",
        ),
        RemoteOriginal::DigestMismatch { .. } => AssetRestoreFailure::new(
            "digest_mismatch",
            "the decrypted original does not match the digest its signed metadata records",
        ),
        RemoteOriginal::Opened { .. } => {
            AssetRestoreFailure::new("workspace", "the original opened but was not kept")
        }
    }
}

/// Whether `hash` is a content address: 64 lowercase hex characters.
fn is_content_address(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The failure for an entry the library refused to open.
fn refusal(verdict: &SyncApplyOutcome) -> AssetRestoreFailure {
    match verdict {
        SyncApplyOutcome::Quarantined(QuarantineReason::UnknownAlbum(album)) => {
            AssetRestoreFailure::new(
                "keys_unavailable",
                format!("this library holds no keys for album {album}"),
            )
        }
        SyncApplyOutcome::Pending(reason) => AssetRestoreFailure::new(
            "keys_unavailable",
            format!("the album key for this asset's epoch is not held here ({reason:?})"),
        ),
        SyncApplyOutcome::Quarantined(reason) => AssetRestoreFailure::new(
            "quarantined",
            format!("the asset did not verify: {reason:?}"),
        ),
        SyncApplyOutcome::Applied(_) => {
            AssetRestoreFailure::new("quarantined", "the asset applied but was not opened")
        }
    }
}

/// The most a restore reads of a metadata blob whose size the feed does not list (16 MiB, the
/// upload protocol's largest chunk). A sealed sidecar is a few KiB; the cap only stops a
/// server that keeps answering full windows from growing the buffer without bound.
pub const MAX_METADATA_BLOB_BYTES: u64 = capsule_sdk::upload::PROTOCOL_MAX_CHUNK;

/// A file extension for a media type, for naming a restored original. Cosmetic: the bytes are
/// the original's whatever the name says, and an unlisted type is written as `.bin`.
#[must_use]
pub fn extension_for(content_type: &str) -> &'static str {
    match content_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/jxl" => "jxl",
        "image/tiff" => "tiff",
        "image/gif" => "gif",
        "image/avif" => "avif",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/x-portable-anymap" => "pnm",
        "image/x-adobe-dng" | "image/dng" => "dng",
        "image/x-sony-arw" => "arw",
        "image/x-canon-cr2" => "cr2",
        "image/x-canon-cr3" => "cr3",
        "image/x-nikon-nef" => "nef",
        "image/x-fuji-raf" => "raf",
        "image/x-olympus-orf" => "orf",
        "image/x-panasonic-rw2" => "rw2",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        _ => "bin",
    }
}

/// Move a fully written staging file into place as `target`, so that `target` is either absent
/// or complete: flushed and synced to disk, then renamed (`staging` is under `OUT`, on the same
/// filesystem, so the rename is atomic).
fn commit_staged(
    sink: BufWriter<std::fs::File>,
    staged: &Path,
    target: &Path,
) -> std::io::Result<()> {
    let file = sink
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(staged, target)?;
    // Persist the rename itself. Best effort: not every platform opens a directory for sync.
    if let Some(parent) = target.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn remove_part(part: &Path) {
    match std::fs::remove_file(part) {
        Ok(()) => tracing::debug!(part = %part.display(), "restore: part file removed"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(part = %part.display(), %error, "restore: part file could not be removed");
        }
    }
}

/// A [`BlobSource`] that serves a part file's bytes from disk and the rest from the inner
/// source, so an interrupted fetch resumes where it stopped. [`PartSink`] is its other half:
/// the writer that appends the inner source's bytes to the same file.
///
/// [`RangedFetcher`] asks for consecutive ranges from offset zero and appends what each returns,
/// so the bytes below the part file's length are answered from the file without a request, and
/// the first request that reaches the inner source starts exactly at its end. The fetcher still
/// hashes the whole blob, so nothing read from the file is trusted on its own. The held prefix
/// is read from the file one window at a time rather than kept in memory.
pub struct ResumingSource<'a, B> {
    inner: &'a B,
    held: u64,
    /// A read handle on the part file, for serving the held prefix.
    reader: Mutex<std::fs::File>,
    fetched: AtomicU64,
}

impl<'a, B> ResumingSource<'a, B> {
    /// Open (or create) the part file at `path` for a blob of `size` bytes. A part file longer
    /// than the blob cannot be a prefix of it and is started over.
    pub fn open(inner: &'a B, path: &Path, size: u64) -> std::io::Result<Self> {
        let part = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let mut held = part.metadata()?.len();
        if held > size {
            tracing::warn!(part = %path.display(), held, size, "restore: part file is longer than its blob; starting over");
            part.set_len(0)?;
            held = 0;
        }
        let reader = std::fs::File::open(path)?;
        Ok(Self {
            inner,
            held,
            reader: Mutex::new(reader),
            fetched: AtomicU64::new(0),
        })
    }

    /// Bytes the part file held when it was opened.
    pub fn held_len(&self) -> u64 {
        self.held
    }

    /// The held bytes `start..end`, read from the part file.
    fn read_held(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut reader = self
            .reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reader.seek(SeekFrom::Start(start))?;
        // `end - start` is at most one window, which fits in memory by construction.
        let mut bytes = vec![0; (end - start) as usize];
        reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Bytes the inner source delivered.
    pub fn fetched(&self) -> u64 {
        self.fetched.load(Ordering::Relaxed)
    }
}

impl<B: BlobSource + Sync> BlobSource for ResumingSource<'_, B> {
    async fn get_range(&self, hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        let held = self.held_len();
        if start < held {
            let end = max_len.map_or(held, |window| start.saturating_add(window).min(held));
            return match self.read_held(start, end) {
                Ok(bytes) => RangeOutcome::Partial { bytes },
                Err(error) => {
                    // The part file vanished or shrank under us: fail this fetch as a transport
                    // error would. The next run reopens whatever is left of the file.
                    tracing::warn!(%error, start, end, "restore: the part file could not be read");
                    RangeOutcome::Status {
                        status: 0,
                        code: None,
                    }
                }
            };
        }
        let outcome = self.inner.get_range(hash, start, max_len).await;
        if let RangeOutcome::Complete { bytes } | RangeOutcome::Partial { bytes } = &outcome
            && !bytes.is_empty()
        {
            self.fetched
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
        outcome
    }
}

/// The streamed fetch's destination: the part file, extended by every byte the fetch receives
/// beyond the prefix it already held. Each write is synced before the next window is asked
/// for, so a run that is cut off keeps every window that landed.
///
/// The fetch hands over the whole blob from offset zero, the held prefix included (read back
/// from this same file by [`ResumingSource`]), so the first `skip` bytes are already on disk
/// and are passed over. A write that fails fails the fetch, rather than leaving a part file
/// that is not the blob it is named for.
pub struct PartSink {
    file: std::fs::File,
    skip: u64,
}

impl PartSink {
    /// Append to the part file at `path`, which already holds the blob's first `held` bytes.
    pub fn open(path: &Path, held: u64) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new().append(true).open(path)?;
        Ok(Self { file, skip: held })
    }
}

impl Write for PartSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let skipped = usize::try_from(self.skip).map_or(buf.len(), |skip| skip.min(buf.len()));
        self.skip -= skipped as u64;
        let fresh = &buf[skipped..];
        if !fresh.is_empty() {
            self.file.write_all(fresh)?;
            self.file.sync_data()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Remove a staging file that will not be renamed into place.
fn discard_staged(staged: &Path) {
    match std::fs::remove_file(staged) {
        Ok(()) => {
            tracing::debug!(staged = %staged.display(), "restore: unverified staging file removed");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(staged = %staged.display(), %error, "restore: staging file could not be removed");
        }
    }
}

#[cfg(test)]
mod tests;
