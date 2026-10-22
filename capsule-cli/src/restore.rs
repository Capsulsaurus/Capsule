//! `capsule restore`: download originals from the server and verify them byte for byte.
//!
//! The network legs are the SDK's: the feed comes from [`crate::remote::restore`]'s fresh drain,
//! and every blob rides [`RangedFetcher`] over whatever [`BlobSource`] the caller hands in
//! ([`capsule_sdk::fetch::HttpBlobSource`] in production, a mock in tests). Verification is the
//! library's: [`Workspace::open_remote_original`] runs each entry through the `verify_asset`
//! chokepoint, decrypts the original under this library's album keys and checks the plaintext
//! against the signed sidecar's digest. What this module owns is the orchestration between
//! them, and the two properties a restore needs on top:
//!
//! - **Resume without re-fetching.** Every ciphertext byte that lands is appended to a part file
//!   under `OUT/.capsule-restore/` before it is handed to the fetcher. A later run serves that
//!   prefix from disk and asks the server only for the bytes after it; the fetcher still hashes
//!   the whole reassembled ciphertext against its content address, so a prefix is trusted only
//!   once the blob it belongs to verifies. A part file whose blob fails that check is removed,
//!   so a bad prefix is fetched again rather than kept.
//! - **Atomic writes.** An original is written to a temporary file in the staging directory,
//!   flushed to disk, and renamed into `OUT`. A file named for the asset in `OUT` is therefore
//!   always a complete, verified original, and a run that finds one skips the asset without
//!   fetching anything.
//!
//! One asset's failure is that asset's: it is recorded with its reason and the run moves on.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use capsule_core::cbor;
use capsule_core::crypto::provenance::ProvenanceRecord;
use capsule_core::lifecycle::{
    QuarantineReason, RemoteEntry, RemoteOriginal, SyncApplyOutcome, Workspace,
};
use capsule_sdk::fetch::{BlobSource, FetchError, RangeOutcome, RangedFetcher};
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::sync::{ChangeKind, FeedEntry};
use indexmap::IndexMap;
use serde::Serialize;
use tracing::instrument;
use uuid::Uuid;

/// The staging directory under the output directory: part files and temporary writes.
pub const STAGING_DIR: &str = ".capsule-restore";

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
    /// `metadata_missing`, `malformed_feed`, `fetch`, `keys_unavailable`, `quarantined`,
    /// `undecryptable`, `digest_mismatch`, `workspace` or `write`.
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
    let fetcher = RangedFetcher::new(options.connection);
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
        let ciphertext = self
            .fetch_original(&original.ciphertext_hash, original.size, outcome)
            .await?;

        // A restore verifies the head the feed serves against the chain position it names: this
        // client holds no earlier record of the asset to place it after. Its signatures, its
        // signer's directory membership and the bytes it commits to are still all checked.
        let local_chain_head = cbor::from_slice::<ProvenanceRecord>(&entry.manifest_cbor)
            .ok()
            .and_then(|record| record.prior_provenance_hash);
        let opened = self
            .workspace
            .open_remote_original(RemoteEntry {
                album_id: head.album,
                manifest_cbor: &entry.manifest_cbor,
                metadata_blob: &metadata,
                original_ciphertext: &ciphertext,
                local_chain_head,
            })
            .map_err(|error| AssetRestoreFailure::new("workspace", error.to_string()))?;
        let (facts, plaintext) = match opened {
            RemoteOriginal::Opened { facts, plaintext } => (facts, plaintext),
            RemoteOriginal::Refused(verdict) => return Err(refusal(&verdict)),
            RemoteOriginal::Undecryptable { .. } => {
                return Err(AssetRestoreFailure::new(
                    "undecryptable",
                    "the original did not open under this library's album keys",
                ));
            }
            RemoteOriginal::DigestMismatch { .. } => {
                return Err(AssetRestoreFailure::new(
                    "digest_mismatch",
                    "the decrypted original does not match the digest its signed metadata records",
                ));
            }
        };
        let ext = facts
            .sidecar
            .as_ref()
            .map_or("bin", |sidecar| extension_for(&sidecar.content_type));
        let target = self.out.join(format!("{}.{ext}", head.asset));
        write_atomically(self.staging, &target, &plaintext)
            .map_err(|error| AssetRestoreFailure::new("write", error.to_string()))?;
        Ok(target)
    }

    /// The sealed metadata blob the head commits to, or nothing for a head that binds none.
    async fn fetch_metadata(&self, entry: &FeedEntry) -> Result<Vec<u8>, AssetRestoreFailure> {
        if entry.metadata_blob.is_empty() {
            return Ok(Vec::new());
        }
        let hash = String::from_utf8_lossy(&entry.metadata_blob).into_owned();
        let Some(size) = entry
            .blobs
            .derivatives
            .iter()
            .find(|blob| blob.ciphertext_hash == hash)
            .map(|blob| blob.size)
        else {
            return Err(AssetRestoreFailure::new(
                "metadata_missing",
                format!("the feed lists no size for metadata blob {hash}"),
            ));
        };
        self.fetcher
            .fetch(self.source, &hash, size)
            .await
            .map_err(|error| AssetRestoreFailure::fetch("metadata blob", &error))
    }

    /// The original's ciphertext, resumed from and persisted to its part file.
    async fn fetch_original(
        &self,
        hash: &str,
        size: u64,
        outcome: &mut AssetRestoreOutcome,
    ) -> Result<Vec<u8>, AssetRestoreFailure> {
        // The address names a file on this machine, so it must be one: a feed that hands over
        // anything but a SHA-256 in lowercase hex never reaches a path.
        if !is_content_address(hash) {
            return Err(AssetRestoreFailure::new(
                "malformed_feed",
                format!("the feed names the original by {hash:?}, which is not a content address"),
            ));
        }
        let part = self.staging.join(format!("{hash}.part"));
        let source = PersistingSource::open(self.source, &part, size)
            .map_err(|error| AssetRestoreFailure::new("write", error.to_string()))?;
        if source.held_len() > 0 {
            tracing::info!(
                hash,
                resumed_from = source.held_len(),
                "restore: resuming the original from its part file"
            );
        }
        let fetched = self.fetcher.fetch(&source, hash, size).await;
        outcome.resumed_bytes = source.held_len();
        outcome.fetched_bytes = source.fetched();
        match fetched {
            Ok(ciphertext) => {
                remove_part(&part);
                Ok(ciphertext)
            }
            Err(error) => {
                // Bytes that do not hash to their address are no prefix to resume from.
                if matches!(error, FetchError::IntegrityFailed { .. }) {
                    remove_part(&part);
                }
                Err(AssetRestoreFailure::fetch("original", &error))
            }
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

/// Write `bytes` to `target` so that `target` is either absent or complete: a temporary file in
/// `staging` (on the same filesystem, so the rename is atomic), flushed, then renamed.
pub fn write_atomically(staging: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = target.file_name().map_or_else(
        || "original".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let tmp = staging.join(format!("{name}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, target)?;
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

/// A [`BlobSource`] that serves a part file's bytes from disk and appends every byte the inner
/// source delivers to it, so an interrupted fetch resumes where it stopped.
///
/// [`RangedFetcher`] asks for consecutive ranges from offset zero and appends what each returns,
/// so the bytes below the part file's length are answered from the file without a request, and
/// the first request that reaches the inner source starts exactly at its end. The fetcher still
/// hashes the whole reassembled blob, so nothing read from the file is trusted on its own.
pub struct PersistingSource<'a, B> {
    inner: &'a B,
    held: Vec<u8>,
    part: Mutex<std::fs::File>,
    fetched: AtomicU64,
}

impl<'a, B> PersistingSource<'a, B> {
    /// Open (or create) the part file at `path` for a blob of `size` bytes. A part file longer
    /// than the blob cannot be a prefix of it and is started over.
    pub fn open(inner: &'a B, path: &Path, size: u64) -> std::io::Result<Self> {
        let mut held = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        if held.len() as u64 > size {
            tracing::warn!(part = %path.display(), held = held.len(), size, "restore: part file is longer than its blob; starting over");
            held.clear();
            std::fs::write(path, b"")?;
        }
        let part = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            inner,
            held,
            part: Mutex::new(part),
            fetched: AtomicU64::new(0),
        })
    }

    /// Bytes the part file held when it was opened.
    pub fn held_len(&self) -> u64 {
        self.held.len() as u64
    }

    /// Bytes the inner source delivered.
    pub fn fetched(&self) -> u64 {
        self.fetched.load(Ordering::Relaxed)
    }

    /// Append freshly delivered bytes to the part file. A failed write costs only resumability:
    /// the bytes still reach the fetcher, and whatever did land is still a prefix of the blob.
    fn persist(&self, bytes: &[u8]) {
        let mut part = self
            .part
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = part.write_all(bytes).and_then(|()| part.sync_data()) {
            tracing::warn!(%error, "restore: the part file could not be extended; this fetch will not resume");
        }
    }
}

impl<B: BlobSource + Sync> BlobSource for PersistingSource<'_, B> {
    async fn get_range(&self, hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        let held = self.held_len();
        if start < held {
            let end = max_len.map_or(held, |window| start.saturating_add(window).min(held));
            // `start < held <= usize::MAX`, so both offsets index the in-memory prefix.
            let bytes = self.held[start as usize..end as usize].to_vec();
            return RangeOutcome::Partial { bytes };
        }
        let outcome = self.inner.get_range(hash, start, max_len).await;
        if let RangeOutcome::Complete { bytes } | RangeOutcome::Partial { bytes } = &outcome
            && !bytes.is_empty()
        {
            self.fetched
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            self.persist(bytes);
        }
        outcome
    }
}

#[cfg(test)]
mod tests;
