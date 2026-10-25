//! The signed import path: EXIF scan, encrypt, sign the create manifest, seal the sidecar,
//! self-verify, and write through to the queryable index.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use capsule_core::cbor;
use capsule_core::crypto::encryption::{
    RekeyStreamError, blob_ciphertext_hash, encrypt_asset_rekey_stream, seal_metadata_blob,
};
use capsule_core::crypto::hash;
use capsule_core::crypto::hash::{Hash32, Sha256Hasher};
use capsule_core::crypto::keys::{Amk, AmkVersion};
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use capsule_core::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, ProvenanceChain, ProvenanceRecord,
};
use capsule_core::crypto::verify_asset::{
    MetadataBinding, VerifyOutcome, verify_asset_hashed, verify_metadata_binding,
};
use capsule_core::media::{MAX_DECODE_PIXELS, StillFormat};
use capsule_core::metadata::crdt::{Lww, OrSet};
use capsule_core::sidecar::normalize_label;
use capsule_core::sidecar::sidecar_v1::{
    Gps, GpsSource, SIDECAR_SCHEMA_V1, SidecarV1, StackMembership, StackRole,
};
use jiff::Timestamp;
use uuid::Uuid;

use super::derivatives::{PreparedStill, StillSource};
use super::{
    AssetState, DerivativeStatus, LifecycleError, Result, SidecarEnrichment, SignedImport,
    SignedImportOptions, StackPlacement, StreamedImport, Workspace, asset_is_deleted, media_dir,
    media_file_path, now_rfc3339,
};
use crate::db::{AssetRow, CachedRepresentationRow};
use crate::exif::{ExifExtract, extract_exif, resolve_timezone};
use crate::library::{ReleaseDecision, RetainReason, StorageVerifier, release_move_source};
use crate::utils::paths::{sync_dir, tmp_path};

/// A rename in [`Workspace::write_signed_artifacts`] a test can make fail after its `.tmp`
/// write — the crash the temp-then-rename writes exist to survive (maintenance doc, Atomic
/// Writes).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteFault {
    /// Before the signed sidecar is renamed into place.
    SidecarRename,
    /// Before the provenance chain is renamed into place — the sidecar and blob already are.
    ChainRename,
}

// The armed fault and how many matching renames to let through before it fires.
// `#[cfg(test)]` and thread-local for the same reasons as `derivatives::SealerFault`: absent
// from a release build, and a test seam that stays out of a production signature.
#[cfg(test)]
thread_local! {
    static WRITE_FAULT: std::cell::Cell<Option<(WriteFault, u32)>> =
        const { std::cell::Cell::new(None) };
}

/// Run `body` with the `skip + 1`-th rename at `point` failing after its `.tmp` write (the
/// earlier ones succeed), disarming after.
#[cfg(test)]
pub(super) fn with_write_fault<T>(point: WriteFault, skip: u32, body: impl FnOnce() -> T) -> T {
    WRITE_FAULT.with(|slot| slot.set(Some((point, skip))));
    let out = body();
    WRITE_FAULT.with(|slot| slot.set(None));
    out
}

/// Whether the armed fault fires at `point` now; counts down the renames it lets through.
#[cfg(test)]
fn write_fault_fires(point: WriteFault) -> bool {
    WRITE_FAULT.with(|slot| match slot.get() {
        Some((armed, 0)) if armed == point => {
            slot.set(None);
            true
        }
        Some((armed, n)) if armed == point => {
            slot.set(Some((armed, n - 1)));
            false
        }
        _ => false,
    })
}

/// Fail with an injected-fault error when the armed fault fires at `point`.
#[cfg(test)]
fn inject_write_fault(point: WriteFault) -> Result<()> {
    if write_fault_fires(point) {
        return Err(LifecycleError::Io(format!(
            "injected fault: crashed before the {point:?} rename"
        )));
    }
    Ok(())
}

/// A file written to `{target}.tmp`, waiting to be renamed over `target`.
#[must_use = "a staged file lands only when committed"]
struct Staged {
    tmp: std::path::PathBuf,
    target: std::path::PathBuf,
}

impl Staged {
    /// Rename the staged file into place: atomic on one filesystem, so `target` holds either
    /// its previous bytes or the new ones, never a torn mix.
    fn commit(self) -> Result<()> {
        fs::rename(&self.tmp, &self.target)
            .map_err(|e| LifecycleError::Io(format!("rename into {}: {e}", self.target.display())))
    }
}

/// Write `bytes` to `{target}.tmp` and fsync it, without touching `target`. The data is on disk
/// before the rename can publish it, so a crash after the rename never exposes an empty file.
fn stage(target: &Path, bytes: &[u8]) -> Result<Staged> {
    let tmp = tmp_path(target);
    write_synced(&tmp, bytes)
        .map_err(|e| LifecycleError::Io(format!("stage {}: {e}", tmp.display())))?;
    Ok(Staged {
        tmp,
        target: target.to_path_buf(),
    })
}

/// Write `bytes` to `path` and fsync the file before returning.
fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Create `dir` and every missing ancestor, fsyncing each new directory's entry in its parent,
/// so a crash after an import cannot lose the shard directory its files were written into.
fn create_dir_durable(dir: &Path) -> Result<()> {
    let missing: Vec<&Path> = dir.ancestors().take_while(|p| !p.is_dir()).collect();
    fs::create_dir_all(dir)
        .map_err(|e| LifecycleError::Io(format!("create {}: {e}", dir.display())))?;
    for created in missing {
        if let Some(parent) = created.parent() {
            sync_dir_entries(parent)?;
        }
    }
    Ok(())
}

/// Fsync `dir`, making the entries just written or renamed into it durable.
fn sync_dir_entries(dir: &Path) -> Result<()> {
    sync_dir(dir).map_err(|e| LifecycleError::Io(format!("fsync {}: {e}", dir.display())))
}

/// How much of a source's head is read to identify it: [`StillFormat::detect`] looks at the
/// first 12 bytes, so this is headroom, not a format's requirement.
const SNIFF_LEN: u64 = 64;

/// The largest still an import holds in memory for the decoder: an uncompressed frame at the
/// pixel budget, four 16-bit channels per pixel. No still the decoder would accept is larger
/// (its pixels would exceed [`MAX_DECODE_PIXELS`]), so a file over this that names itself a
/// still by its header is streamed like any other original and gets no derivatives.
pub(super) const MAX_HELD_STILL_BYTES: u64 = MAX_DECODE_PIXELS * 8;

/// The EXIF of the `size`-byte file at `src`, or nothing for a file over
/// [`MAX_HELD_STILL_BYTES`].
///
/// The EXIF reader can buffer a whole file whatever its container: a TIFF always (its IFD
/// offsets may point anywhere), and a HEIF, PNG or WebP whose box or chunk sizes claim more than
/// they hold. So the scan is bounded by the file's size, the same bound the decoder is held to,
/// and every container is treated alike. Nothing real is lost: no camera still is that large, and
/// the reader parses no video container, which is what files that large are.
fn scan_exif(src: &Path, size: u64) -> ExifExtract {
    if size > MAX_HELD_STILL_BYTES {
        tracing::debug!(
            path = %src.display(),
            size,
            limit = MAX_HELD_STILL_BYTES,
            "import: file too large to scan for EXIF; capture time and GPS fall back"
        );
        return ExifExtract::default();
    }
    extract_exif(src).unwrap_or_default()
}

/// Read the first [`SNIFF_LEN`] bytes of `file` (fewer for a shorter file) and rewind it.
pub(super) fn read_head(file: &mut fs::File) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(SNIFF_LEN as usize);
    Read::by_ref(file).take(SNIFF_LEN).read_to_end(&mut head)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(head)
}

/// The plaintext on its way into the encryptor: hashed as it passes and, unless the original
/// already lies at its media path, copied into the staged library file — so one read of the
/// source yields the signed `hash`, the ciphertext address and the library copy, and holds one
/// STREAM chunk at a time.
pub(super) struct Tee<R> {
    pub(super) inner: R,
    pub(super) hasher: Sha256Hasher,
    pub(super) copy: Option<fs::File>,
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let passed = &buf[..n];
        self.hasher.update(passed);
        if let Some(copy) = self.copy.as_mut() {
            copy.write_all(passed)?;
        }
        Ok(n)
    }
}

/// An import's library copy of its original, written to `{media}.tmp` while it is encrypted
/// and renamed into place only once the asset has self-verified. Dropped uncommitted — the
/// import failed after the copy began — it removes the `.tmp`, so a failed import leaves no
/// plaintext behind; a crash leaves it to the startup scrub.
struct StagedOriginal(Option<Staged>);

impl StagedOriginal {
    /// Rename the copy into place.
    fn commit(mut self) -> Result<()> {
        self.0.take().map_or(Ok(()), Staged::commit)
    }
}

impl Drop for StagedOriginal {
    fn drop(&mut self) {
        if let Some(staged) = self.0.take() {
            let _ = fs::remove_file(&staged.tmp);
        }
    }
}

/// Render a Unix-second capture time as the sidecar's RFC 3339 `capture_timestamp`.
fn capture_rfc3339(secs: i64) -> String {
    Timestamp::from_second(secs)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_string()
}

/// The sidecar `stack_membership` register an import starts life with.
///
/// A standalone import returns the **never-written** register, which is wire-absent — the
/// absent-key discipline every signed-struct field carries, and what keeps an unstacked
/// asset's sidecar bytes identical to one written before the register existed. A stacked
/// member returns the register with `membership` stamped `(now, device_id)`.
fn stack_membership_register(
    membership: Option<StackMembership>,
    device_id: Uuid,
) -> Lww<Option<StackMembership>> {
    let mut register = Lww::new();
    if membership.is_some() {
        register.set(membership, now_rfc3339(), device_id);
    }
    register
}

/// Which side the sidecar's `capture_timestamp` was taken from — logged so an import decision
/// can be reconstructed after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureSource {
    /// This file's own EXIF, resolved to a UTC instant at the write site.
    Embedded,
    /// The adapter's folded value: an EXIF time the write site could not resolve on
    /// its own, else the exporter's taken-time. Which of the two is recorded by the executor's
    /// `taken_time_source` log line for the same file.
    Folded,
    /// Neither side carried a capture time — the import's own clock.
    ImportTime,
}

impl CaptureSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded-exif",
            Self::Folded => "adapter-fold",
            Self::ImportTime => "import-time",
        }
    }
}

/// Capture-time [precedence] at the write site: the file's own embedded EXIF wins; the
/// adapter's folded value is the fallback; the import clock is the last resort.
///
/// [precedence]: https://docs/design/import/pipeline/#third-party-importers
fn folded_capture(embedded_utc: Option<i64>, folded: Option<Timestamp>) -> (i64, CaptureSource) {
    match (embedded_utc, folded) {
        (Some(secs), _) => (secs, CaptureSource::Embedded),
        (None, Some(t)) => (t.as_second(), CaptureSource::Folded),
        (None, None) => (Timestamp::now().as_second(), CaptureSource::ImportTime),
    }
}

/// GPS [precedence](folded_capture) at the write site: this file's own EXIF fix wins over the
/// exporter's record, which fills in only where the bytes carried none.
fn folded_gps(embedded: Option<Gps>, folded: Option<&Gps>) -> Option<Gps> {
    embedded.or_else(|| folded.cloned())
}

/// The sidecar `content_type` for a file whose bytes named no still image Capsule models.
///
/// The **fallback only**: [`Workspace::prepare_still`] sniffs the header first, so a still's
/// media type comes from [`StillFormat::mime`](capsule_core::media::StillFormat::mime) and a `.jpg`
/// that is really a HEIC is typed `image/heic`. What is left for this table is the non-still
/// suffixes — video above all, which has no detection path.
fn content_type_for(ext: &str) -> String {
    match ext {
        "mp4" | "m4v" => "video/mp4",
        "mov" | "qt" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// A video's derivatives are **deferred**, not inapplicable: the tier table commits to a
/// first-frame still and an H.264 preview for every video, and no build links the codec for
/// them yet (SSoT: thumbnails § Video Previews). The still path sees a video as
/// [`DerivativeStatus::NotAKnownStill`], so it is reclassified here, where the content type is
/// known, and the import summary counts it with every other deferral. Any other status passes
/// through unchanged.
fn video_derivatives_deferred(status: DerivativeStatus, content_type: &str) -> DerivativeStatus {
    if status == DerivativeStatus::NotAKnownStill && content_type.starts_with("video/") {
        DerivativeStatus::DeferredNoCodec
    } else {
        status
    }
}

fn asset_type_for(content_type: &str) -> String {
    if content_type.starts_with("video/") {
        "video"
    } else {
        "photo"
    }
    .to_string()
}

fn rfc3339_to_secs(s: &str) -> i64 {
    s.parse::<Timestamp>().map_or(0, Timestamp::as_second)
}

/// Map a managed asset's in-memory state to its queryable `assets` index row. Deletion state is
/// derived from the provenance chain's lifecycle actions; media-derived fields (dimensions,
/// duration, chromahash) stay NULL — they are out of scope in this offline core.
fn asset_row_from_state(asset: &AssetState) -> AssetRow {
    let mut is_deleted = false;
    let mut deleted_at = None;
    for rec in asset.chain.records() {
        match rec.manifest.core.action {
            Action::Delete => {
                is_deleted = true;
                deleted_at = Some(rfc3339_to_secs(&rec.manifest.core.timestamp));
            }
            Action::TrashRestore => {
                is_deleted = false;
                deleted_at = None;
            }
            _ => {}
        }
    }
    debug_assert_eq!(is_deleted, asset_is_deleted(asset));
    // Stack columns are a projection of the signed `stack_membership` register — the
    // same projection `library::rebuild::signed_asset_row` applies, so the write path and a
    // rebuild agree on what the views show. A *written* register is authoritative in both arms:
    // `Some(m)` is a placement, a stamped `None` is an explicit departure from a stack. Only a
    // never-written register falls back to `AssetState::stack`, the index-only placement a
    // import from before the register existed left behind.
    let (stack_id, is_stack_hidden) = match asset.sidecar.stack_membership.get() {
        Some(membership) => membership.as_ref().map_or((None, false), |m| {
            (Some(m.stack_id.to_string()), m.role != StackRole::Primary)
        }),
        None => asset
            .stack
            .as_ref()
            .map_or((None, false), |s| (Some(s.stack_id.clone()), s.hidden)),
    };
    // Capture time is projected from the **signed sidecar**, not from `AssetState::capture_utc`
    // — exactly as `library::rebuild::signed_asset_row` projects it, so the write path and a
    // rebuild agree. The two values are equal at import; they part ways after a capture-time
    // correction (`Workspace::set_capture_timestamp`), which by design re-signs the
    // sidecar and leaves the `media/{YYYY}/{YYYY-MM}` shard — and therefore `capture_utc`,
    // which names it — where the files already are. Reading the shard here would keep the
    // timeline on the wrong date until the next rebuild while the rebuild read the right one.
    // The instant projected is the *effective* one: the user's correction when one was written,
    // else the as-imported value.
    let capture = asset.sidecar.effective_capture_timestamp();
    let capture_utc = capture.parse::<Timestamp>().map_or_else(
        |_| {
            tracing::warn!(
                asset_id = %asset.asset_id,
                capture_timestamp = %capture,
                "index: unparseable capture timestamp; indexing it as the epoch"
            );
            0
        },
        Timestamp::as_second,
    );
    AssetRow {
        uuid: asset.asset_id.to_string(),
        asset_type: asset_type_for(&asset.sidecar.content_type),
        capture_timestamp: capture_utc,
        capture_utc: Some(capture_utc),
        capture_tz_source: None,
        import_timestamp: rfc3339_to_secs(&asset.sidecar.import_timestamp),
        hash_sha256: asset.sidecar.hash.to_hex(),
        width: asset.sidecar.dimensions.as_ref().map(|d| d.width as i64),
        height: asset.sidecar.dimensions.as_ref().map(|d| d.height as i64),
        duration_ms: None,
        stack_id,
        is_stack_hidden,
        chromahash: None,
        dominant_color: None,
        album_id: Some(asset.album_id.to_string()),
        rating: asset.sidecar.rating.get().copied().unwrap_or(0) as i64,
        is_deleted,
        deleted_at,
        // Projection of the sidecar `hidden` LWW register: a never-written register
        // means visible, the wire-absent default.
        is_hidden: asset.sidecar.hidden.get().copied().unwrap_or(false),
    }
}

/// Whether `a` and `b` resolve to the same existing file.
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// One signed create, as [`Workspace::commit_signed_create`] takes it: everything a caller
/// decides *before* the sealing order starts.
pub(super) struct CreateRequest<'a> {
    /// The asset id. An import mints `Uuid::now_v7()`.
    pub asset_id: Uuid,
    /// The owning album; must be held with write capability.
    pub album_id: Uuid,
    /// The plaintext to admit. When this already *is* the asset's own media path, the bytes are
    /// left where they are rather than rewritten over themselves.
    pub src: &'a Path,
    /// Move-mode release, stack placement, and exporter enrichment.
    pub opts: &'a SignedImportOptions,
}

impl Workspace {
    /// Write an asset's plaintext and its signed artifacts.
    ///
    /// The plaintext is written only when the media path does not already hold it: an
    /// original that is already correct is never rewritten over itself, which would only open
    /// a crash window in which the one copy is truncated. The decision is made from the
    /// buffer — a `stat` of the media path, a length comparison, and `hash_bytes(plaintext)`
    /// against the sidecar's `hash` — with no second read of the disk, so a metadata edit
    /// costs one read of the original and no write.
    ///
    /// **Caller rule.** `plaintext` must be either the bytes read from the media path
    /// (`append_lifecycle`) or the file about to become it (a restored backup, an adopted drop).
    /// An import does not come through here: `commit_signed_create` streams its original into
    /// the library without a buffer and then calls
    /// [`write_signed_artifacts`](Self::write_signed_artifacts). A caller whose buffer may
    /// legitimately differ from a *same-length* file already at the media path is not covered
    /// by this guard and must decide the overwrite itself; `import_backup` restores into a
    /// workspace where the media path does not exist, so it never meets that case.
    pub(super) fn write_asset_files(&self, asset: &AssetState, plaintext: &[u8]) -> Result<()> {
        let dir = media_dir(&self.root, asset.capture_utc);
        create_dir_durable(&dir)?;
        let media_path = self.media_path(asset);
        let already_correct = fs::metadata(&media_path)
            .is_ok_and(|m| m.is_file() && m.len() == plaintext.len() as u64)
            && hash::hash_bytes(plaintext) == asset.sidecar.hash;
        if already_correct {
            tracing::debug!(
                asset_id = %asset.asset_id,
                "original already on disk with the signed hash; not rewriting it"
            );
        } else {
            // Fsynced here, and its directory entry with the signed artifacts' below: the
            // library copy is what a Move source is checked against before it is deleted, so
            // it must be on disk rather than in the page cache.
            write_synced(&media_path, plaintext)
                .map_err(|e| LifecycleError::Io(format!("write {}: {e}", media_path.display())))?;
        }
        self.write_signed_artifacts(asset)
    }

    /// The signed half of [`write_asset_files`](Self::write_asset_files): the sidecar, the
    /// provenance chain, and the sealed metadata blob — everything but the plaintext. The
    /// right call for a writer whose plaintext is already on disk and unchanged (a metadata
    /// edit), which then needs neither to read nor to write the original.
    ///
    /// Every file is staged to `{name}.tmp` and renamed into place (the single-file atomic
    /// write of the maintenance doc), so a crash mid-write leaves the previous file intact
    /// rather than a torn one. All three are staged before any is renamed, and the renames run
    /// sidecar, then sealed metadata blob, then provenance chain: the chain is what anchors an
    /// asset ([`find_unanchored`](super::unanchored::find_unanchored)), so it lands last, and a
    /// crash before it leaves a chainless signed sidecar that `open` reports rather than a torn
    /// chain that counts as anchored and hides the asset. A stale `.tmp` is the startup
    /// scrub's to remove. The per-asset *bundle* (sidecar, chain, blob renamed as one unit)
    /// remains separate work.
    pub(super) fn write_signed_artifacts(&self, asset: &AssetState) -> Result<()> {
        let dir = media_dir(&self.root, asset.capture_utc);
        create_dir_durable(&dir)?;
        let prov = cbor::to_canonical_vec(&asset.chain.records().to_vec())
            .map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        let sidecar = stage(&self.sidecar_path(asset), &asset.sidecar.to_canonical_vec())?;
        // The sealed metadata blob. Un-regenerable (its nonce is folded into the blob
        // key), AMK ciphertext, and required by `export_backup` and upload — so it is written
        // beside the chain rather than held only in memory. An action that mints no blob
        // (`delete` / `trash-restore`) leaves the previous file in place.
        let blob = if asset.metadata_blob.is_empty() {
            None
        } else {
            Some(stage(
                &self.metadata_blob_path(asset),
                &asset.metadata_blob,
            )?)
        };
        let chain = stage(&self.provenance_path(asset), &prov)?;

        #[cfg(test)]
        inject_write_fault(WriteFault::SidecarRename)?;
        sidecar.commit()?;
        if let Some(blob) = blob {
            blob.commit()?;
        }
        #[cfg(test)]
        inject_write_fault(WriteFault::ChainRename)?;
        chain.commit()?;
        // One directory fsync makes every rename above durable, and with them an original
        // `write_asset_files` wrote into the same directory.
        sync_dir_entries(&dir)?;
        tracing::trace!(
            asset_id = %asset.asset_id,
            "signed artifacts renamed into place and fsynced: sidecar, blob, chain"
        );
        Ok(())
    }

    /// Write the queryable index row + user tags for `asset` into `library.sqlite`. Re-syncs on
    /// every change (import, metadata edit, soft-delete/restore), so the index reflects the
    /// asset's current rating, tags, and deletion state. Upsert keeps it conflict-safe even
    /// though the legacy importer shares the same `assets` table.
    pub(super) fn index_asset_row(&self, asset: &AssetState) -> Result<()> {
        self.library
            .db
            .upsert_asset(&asset_row_from_state(asset))
            .map_err(|e| LifecycleError::Db(e.to_string()))?;
        let tags: Vec<String> = asset.sidecar.tags_user.value().into_iter().collect();
        self.library
            .db
            .replace_asset_tags(&asset.asset_id.to_string(), &tags)
            .map_err(|e| LifecycleError::Db(e.to_string()))
    }

    /// Record the asset's own original as a device-owned cache representation — exempt from the
    /// automatic eviction sweep, and the real lifecycle data that sweep then operates on.
    pub(super) fn index_original_representation(
        &self,
        asset: &AssetState,
        bytes: usize,
    ) -> Result<()> {
        self.library
            .db
            .upsert_representation(&CachedRepresentationRow {
                uuid: asset.asset_id.to_string(),
                tier: "original".to_string(),
                format: Some(asset.ext.clone()),
                bytes: bytes as i64,
                path: self.media_path(asset).to_string_lossy().into_owned(),
                last_accessed_at: Timestamp::now().as_second(),
                pinned: false,
                is_owned_original: true,
            })
            .map_err(|e| LifecycleError::Db(e.to_string()))
    }

    /// Import a file into `album_id`: encrypt, build the signed create manifest + provenance,
    /// write the signed sidecar, and self-verify through `verify_asset` **and** the
    /// metadata↔manifest binding. Follows the [sealing order] so the manifest commits to the
    /// content address of the sidecar it seals, without a cycle. Returns the asset id.
    ///
    /// [sealing order]: https://docs/design/metadata/#provenance-binding-and-sealing-order
    pub fn import_asset(&mut self, album_id: Uuid, src: &Path) -> Result<Uuid> {
        Ok(self
            .import_asset_with(album_id, src, &SignedImportOptions::default())?
            .asset_id)
    }

    /// Import **bytes already in memory** into `album_id` under the file name `file_name`
    /// (consulted for its extension and nothing else), returning the asset id.
    ///
    /// This is the entry point a platform app needs and the CLI does not: PhotoKit, the Android
    /// MediaStore, and a browser upload all hand over a byte buffer rather than a path the
    /// library may read at leisure. It stages those bytes in the library's own scratch
    /// directory and drives the **one** signed write path —
    /// [`import_asset_with`](Self::import_asset_with) in Move mode, so the staged copy is
    /// released by the same durable-commit rule that releases any other moved source. There is
    /// no second sealing implementation here and none is wanted: the file key derivation, STREAM
    /// encryption, manifest, sidecar, provenance chain, and `verify_asset` self-check are
    /// byte-for-byte the path a file import takes.
    ///
    /// EXIF is read back off the staged file, so a JPEG carrying capture time and GPS is scanned
    /// exactly as it would be on a file import.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, file_name, bytes = bytes.len()))]
    pub fn import_bytes(&mut self, album_id: Uuid, file_name: &str, bytes: &[u8]) -> Result<Uuid> {
        let ext = Path::new(file_name)
            .extension()
            .map_or_else(|| "bin".to_string(), |e| e.to_string_lossy().to_lowercase());
        let staging = self.root().join(".library").join("staging");
        fs::create_dir_all(&staging)
            .map_err(|e| LifecycleError::Io(format!("create staging dir: {e}")))?;
        let staged = staging.join(format!("{}.{ext}", Uuid::now_v7().simple()));
        fs::write(&staged, bytes)
            .map_err(|e| LifecycleError::Io(format!("stage {}: {e}", staged.display())))?;

        let opts = SignedImportOptions {
            // The staged copy is scratch, and the caller still holds the buffer it handed over,
            // so releasing on the local durable commit is right: nothing is the only copy.
            move_source: true,
            defer_source_release: false,
            stack: None,
            enrichment: None,
        };
        let imported = self.import_asset_with(album_id, &staged, &opts);
        if imported.is_err() {
            // A failed import must not leave staged plaintext behind.
            let _ = fs::remove_file(&staged);
        }
        let asset_id = imported?.asset_id;
        tracing::info!(asset_id = %asset_id, "imported in-memory bytes as a signed asset");
        Ok(asset_id)
    }

    /// As [`import_asset`](Self::import_asset) but with executor-supplied [`SignedImportOptions`]
    /// (Move-mode source release + stack placement). This is the single signed write path the
    /// import executor drives: every imported member lands as a signed `SidecarV1` +
    /// manifest + append-only provenance, self-verified through
    /// [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset), and — when the
    /// still decodes — with a chromahash `lqip` in the sidecar and signed thumbnail derivatives
    /// on disk (the private `prepare_still`).
    ///
    /// Returns a [`SignedImport`]: the asset id, the
    /// [`DerivativeStatus`] saying whether derivatives were generated
    /// and, if not, why, and the per-format deferral count. A format this build has no
    /// codec for **still imports** — the original is the backup, the thumbnail is a bonus — so
    /// the status is a report, never a rejection.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, src = %src.display()))]
    pub fn import_asset_with(
        &mut self,
        album_id: Uuid,
        src: &Path,
        opts: &SignedImportOptions,
    ) -> Result<SignedImport> {
        self.commit_signed_create(&CreateRequest {
            asset_id: Uuid::now_v7(),
            album_id,
            src,
            opts,
        })
    }

    /// The signed create commit every import goes through: EXIF scan, encrypt, derivatives,
    /// author + sign the sidecar, seal it, build + sign the create manifest, self-verify,
    /// write, index. The [`CreateRequest`] carries the few things a caller decides beforehand.
    #[tracing::instrument(
        skip_all,
        fields(asset_id = %req.asset_id, album_id = %req.album_id, src = %req.src.display())
    )]
    pub(super) fn commit_signed_create(&mut self, req: &CreateRequest<'_>) -> Result<SignedImport> {
        let src = req.src;
        let asset_id = req.asset_id;
        let album_id = req.album_id;
        let opts = req.opts;
        let read_err = |e: io::Error| LifecycleError::Io(format!("read {}: {e}", src.display()));
        let ext = src
            .extension()
            .map_or_else(|| "bin".into(), |e| e.to_string_lossy().to_lowercase());

        // **Bounded memory, whatever the file's size.** The original is never read whole for
        // its own sake: it is hashed, encrypted and copied into the library in one streamed
        // pass below. The one exception is a still this build can decode, which the decoder
        // takes as a buffer — at most `MAX_HELD_STILL_BYTES` — and which is then streamed from
        // that same buffer, so the pixels and the signed hash come from the same bytes. Whether
        // a file is such a still is settled from its head alone, exactly as the decoder's own
        // gate settles it.
        let mut source = fs::File::open(src).map_err(read_err)?;
        let size = source.metadata().map_err(read_err)?.len();
        let head = read_head(&mut source).map_err(read_err)?;
        let decodable = StillFormat::detect(&head, &ext).is_some_and(StillFormat::is_decodable);
        let held = if decodable && size <= MAX_HELD_STILL_BYTES {
            let mut whole = Vec::new();
            source.read_to_end(&mut whole).map_err(read_err)?;
            Some(whole)
        } else {
            None
        };
        tracing::debug!(
            asset_id = %asset_id,
            decodable,
            size,
            held_bytes = held.as_ref().map_or(0, Vec::len),
            "import: source identified; streaming the original"
        );

        // Scan & extract: capture time, dimensions, and GPS from the file's EXIF. Missing values
        // degrade cleanly (capture → now; dimensions/GPS → absent).
        let exif = scan_exif(src, size);
        let tz = resolve_timezone(&exif);
        // A third-party import arrives with the adapter's folded exporter record. Its
        // capture time and GPS are *fallbacks* — the file's own EXIF wins wherever it yields a
        // value, which is the pipeline doc's precedence rule applied at the write site.
        let enrichment = opts.enrichment.as_ref();
        let (capture_utc, capture_source) =
            folded_capture(tz.capture_utc, enrichment.and_then(|e| e.capture_time));
        // EXIF GPS is the near-universal WGS-84 camera datum (metadata doc, Geolocation);
        // stored verbatim, so the wire-absent default datum applies.
        let embedded_gps = exif.gps_lat.zip(exif.gps_lon).map(|(lat, lon)| Gps {
            lat,
            lon,
            source: GpsSource::Exif,
            datum: capsule_core::domain::GpsDatum::Wgs84,
        });
        let gps = folded_gps(embedded_gps, enrichment.and_then(|e| e.gps.as_ref()));

        // Exporter-authoritative registers: the description, favorite flag, and album
        // membership the file bytes never carried. Each is stamped `(now, device_id)` exactly as
        // the `stack_membership` register is, so a later edit on any device converges under the
        // same LWW rule; each album title gets its own `add_id` so it stays individually
        // removable. Nothing folded ⇒ every register stays at its default, and the sidecar
        // encodes byte-identically to a plain filesystem import's.
        let device_id = self.account.device.device_id;
        let stamp = now_rfc3339();
        let mut caption = Lww::new();
        if let Some(text) = enrichment.and_then(|e| e.caption.as_deref()) {
            caption.set(text.to_string(), stamp.clone(), device_id);
        }
        let mut rating = Lww::new();
        if let Some(stars) = enrichment.and_then(|e| e.rating) {
            rating.set(stars, stamp.clone(), device_id);
        }
        // Each exporter tag becomes an asset label, so it passes through the one label rule
        // (`normalize_label`: NFC, trimmed, 1–256 bytes) exactly as `tag_add` does. A title the
        // rule refuses is dropped rather than failing the import: the photo itself is the thing
        // being imported, and an unusable album title must not cost the user the asset.
        let mut tags_user = OrSet::new();
        for tag in enrichment.map_or(&[][..], |e| e.tags.as_slice()) {
            match normalize_label(tag) {
                Ok(label) => {
                    let add_id = self.counter.issue();
                    tags_user.add(label, add_id);
                }
                Err(error) => tracing::warn!(
                    asset_id = %asset_id,
                    tag_bytes = tag.len(),
                    %error,
                    "import: exporter tag refused by the label rule; dropped"
                ),
            }
        }
        // Logged for *every* import, enriched or not: which side each contested field came from
        // is what makes a surprising capture time or location explainable after the fact. User
        // content stays out — sizes and counts are what a decision has to be reconstructed from,
        // not the caption text or the album titles.
        tracing::debug!(
            asset_id = %asset_id,
            enriched = enrichment.is_some(),
            capture_source = capture_source.as_str(),
            capture_utc,
            gps_source = ?gps.as_ref().map(|g| g.source),
            caption_bytes = enrichment.and_then(|e| e.caption.as_ref()).map_or(0, String::len),
            rating = ?enrichment.and_then(|e| e.rating),
            tags = enrichment.map_or(0, |e| e.tags.len()),
            "import: sidecar metadata resolved"
        );

        let album = self.album(&album_id)?;
        let epoch = album.current_epoch;
        let amk = Amk::from_bytes(album.amks[&epoch]);
        // The media bucket every file of this asset resolves to: the resolved capture time.
        let bucket = capture_utc;
        let media_path = media_file_path(&self.root, bucket, asset_id, &ext);
        // The original is copied to `{media}.tmp` in the same pass that encrypts it. The asset
        // id is fresh, so its media path cannot already name the source.
        create_dir_durable(&media_dir(&self.root, bucket))?;
        let tmp = tmp_path(&media_path);
        let staged = StagedOriginal(Some(Staged {
            tmp: tmp.clone(),
            target: media_path.clone(),
        }));
        let copy = Some(
            fs::File::create(&tmp)
                .map_err(|e| LifecycleError::Io(format!("stage {}: {e}", tmp.display())))?,
        );
        let mut held_reader = held.as_deref().unwrap_or_default();
        let reader: &mut dyn Read = if held.is_some() {
            &mut held_reader
        } else {
            &mut source
        };
        let mut tee = Tee {
            inner: reader,
            hasher: Sha256Hasher::new(),
            copy,
        };
        // First write: draw a fresh nonce prefix and derive the folded file key together
        // (nothing to replace on a create).
        //
        // **Before** the derivatives, and that ordering is load-bearing: the `original`
        // sentinel is a signed *reference* to this blob, so it commits to this ciphertext's
        // address and this nonce prefix, neither of which exists until now.
        //
        // The ciphertext goes to a sink: the manifest commits to its address, which the
        // encryptor hashes as the chunks pass, and an upload re-derives it from the library
        // copy and the recorded prefix.
        let (enc, _file_key) =
            encrypt_asset_rekey_stream(&amk, &asset_id, &mut tee, io::sink(), None).map_err(
                |e| match e {
                    RekeyStreamError::Crypto(e) => LifecycleError::Crypto(e),
                    RekeyStreamError::Stream(e) => {
                        LifecycleError::Io(format!("encrypt {}: {e}", src.display()))
                    }
                },
            )?;
        let Tee { hasher, copy, .. } = tee;
        let plaintext_hash = hasher.finalize();
        if let Some(copy) = copy {
            // Fsynced before the rename can publish it: the library copy is what a Move source
            // is checked against before it is deleted, so it must be on disk rather than in the
            // page cache.
            copy.sync_all()
                .map_err(|e| LifecycleError::Io(format!("fsync {}: {e}", media_path.display())))?;
        }
        tracing::debug!(
            asset_id = %asset_id,
            plaintext_size = enc.plaintext_size,
            "import: original hashed, encrypted and staged in one pass"
        );

        // Still-derived sidecar metadata, from one decode pass over the plaintext: the
        // header-derived `content_type`, pixel `dimensions`, the chromahash `lqip`, and the
        // signed thumbnail derivatives to persist once the asset's own files are durable. Each
        // generated derivative is encrypted under its own fresh nonce prefix as it is signed —
        // derivative bytes cross the network encrypted exactly like the original.
        //
        // Never fatal. A still this build cannot decode — or cannot decode *these bytes* of —
        // commits exactly as before: EXIF dimensions, no LQIP, no derivatives, and a
        // `DerivativeStatus` recording which reason applied so the gap is reportable rather
        // than silent.
        let PreparedStill {
            format,
            dimensions,
            lqip,
            derivatives,
            deferred_formats,
            status: derivative_status,
        } = self.prepare_still(
            &StillSource {
                // The whole file for a held still; for anything else its head, which is all
                // the decoder's gate reads before it refuses.
                plaintext: held.as_deref().unwrap_or(&head),
                size,
                ext: &ext,
                src,
                exif: &exif,
            },
            asset_id,
            album_id,
            bucket,
            &amk,
            &enc,
        )?;
        drop(held);
        // Header-derived wherever the bytes name a still Capsule models; the extension table is
        // the fallback for everything else (video, unknown suffixes).
        let content_type = format.map_or_else(|| content_type_for(&ext), |f| f.mime().to_string());
        let derivative_status = video_derivatives_deferred(derivative_status, &content_type);

        // Sealing order (1) the prior head `H` is `None` on a create; (2) author + sign the
        // sidecar with `provenance_chain_hash = H`.
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: asset_id,
            hash: plaintext_hash,
            capture_timestamp: capture_rfc3339(capture_utc),
            // An import records what the file says; a user correction is a later write.
            capture_timestamp_user: Lww::new(),
            import_timestamp: now_rfc3339(),
            content_type,
            dimensions,
            lqip,
            tags_user,
            tags_ai: Default::default(),
            caption,
            rating,
            // An importer-formed stack is written into the signed sidecar exactly as the
            // manual `set_stack_membership` path writes it, stamped with this device id +
            // now so it converges under the same `(ts, device_id)` LWW rule. A standalone
            // import leaves the register at its default, which is wire-absent — a sidecar for
            // an unstacked asset encodes byte-identically to one with no register at all.
            stack_membership: stack_membership_register(
                opts.stack.clone(),
                self.account.device.device_id,
            ),
            regions: Default::default(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: self.account.device.device_id,
            session_id: Uuid::now_v7(),
            gps,
            gps_user: Lww::new(),
            provenance_chain_hash: None,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);

        // (3) Seal the sidecar into the metadata blob (fresh nonce folded into the blob key;
        // nothing to replace on a create); compute its content hash.
        let (metadata_blob, blob_key) =
            seal_metadata_blob(&amk, &asset_id, &sidecar.to_canonical_vec(), None)?;
        let metadata_blob_hash = blob_ciphertext_hash(&metadata_blob);

        // (4) Build + sign the manifest with `prior_provenance_hash = H` (None) and the
        // `metadata_blob_hash` from (3); append it as the new chain head.
        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: asset_id,
            album_id,
            amk_version: AmkVersion(epoch),
            ciphertext_hash: enc.ciphertext_hash,
            plaintext_size: enc.plaintext_size,
            chunk_size: enc.chunk_size,
            nonce_prefix: enc.nonce_prefix,
            key_mode: KeyMode::Derived,
            wrapped_file_key: None,
            metadata_blob_hash: Some(metadata_blob_hash),
            created_by_user: self.account.user_id,
            created_by_device: self.account.device.device_id,
            client_version: self.client_version.clone(),
            timestamp: now_rfc3339(),
            action: Action::Create,
            prior_provenance_hash: None,
            upgraded_from: None,
            unknown: Default::default(),
            retention_until: None,
        };
        let manifest = core.sign(self.device_signer.as_ref(), album.write_tier_signer()?)?;

        let mut chain = ProvenanceChain::new();
        chain
            .append(ProvenanceRecord::new(asset_id, manifest.clone(), None))
            .map_err(|e| LifecycleError::Cbor(format!("chain: {e}")))?;

        // Self-check: the asset must verify through the one chokepoint, and the sealed metadata
        // blob must round-trip to the signed sidecar and the committed hash, before we accept it.
        let authority = self.authority(&album_id)?;
        // Over the address the encryptor computed from every chunk it produced: the ciphertext
        // itself was never held.
        let outcome = verify_asset_hashed(
            &manifest,
            &enc.ciphertext_hash,
            &self.directory,
            authority,
            None,
        );
        if outcome != VerifyOutcome::Accept {
            return Err(LifecycleError::SelfVerify(outcome));
        }
        let binding = verify_metadata_binding(
            &manifest,
            &metadata_blob,
            &blob_key,
            &sidecar.to_canonical_vec(),
        );
        if binding != MetadataBinding::Bound {
            return Err(LifecycleError::MetadataUnbound(binding));
        }

        let asset = AssetState {
            asset_id,
            album_id,
            ext,
            capture_utc: bucket,
            chain,
            sidecar,
            metadata_blob,
            // Kept in step with the register it is projected from; only an asset imported
            // before the register existed reaches `AssetState::stack` by any other route.
            stack: opts.stack.as_ref().map(StackPlacement::from_membership),
        };
        debug_assert_eq!(media_path, self.media_path(&asset));
        // The library copy lands first, then the signed artifacts, whose directory fsync makes
        // the copy's rename durable with theirs.
        staged.commit()?;
        self.write_signed_artifacts(&asset)?;
        // After the asset's own files, and deliberately: a derivative is regenerable, so a
        // failure to write one must never fail an import whose signed original is already
        // durable. `persist_derivatives` logs and continues rather than returning.
        self.persist_derivatives(&asset, &derivatives);
        self.index_asset_row(&asset)?;
        self.index_original_representation(&asset, enc.plaintext_size as usize)?;

        // Move mode: release the source only after the durable, self-verified commit — unless
        // the caller defers release to its server-side verify-before-destroy gate,
        // where the source is the only copy until the *server* durably holds it.
        if opts.move_source && !opts.defer_source_release {
            let _ = fs::remove_file(src);
        }

        self.assets.insert(asset_id, asset);
        Ok(SignedImport {
            asset_id,
            derivatives: derivative_status,
            deferred_formats: deferred_formats as u32,
        })
    }

    /// Import a file on the signed path for a **streaming** import: identical to
    /// [`import_asset_with`](Self::import_asset_with) but with `defer_source_release` forced on,
    /// and returning the [`StreamedImport`] descriptor the streaming window drives its per-asset
    /// upload → verify → release step from. The local original (and any Move-mode source) is left
    /// in place — the [streaming executor](crate::import::execute_streaming) releases it only
    /// after the server's `durable` verdict + custody receipt clear the release gate.
    ///
    /// `enrichment` carries the folded third-party exporter metadata for this file exactly as
    /// [`import_asset_with`](Self::import_asset_with) takes it. It is a parameter and
    /// not a hard-wired `None` because the enrichment is written *inside the signed sidecar*:
    /// dropping it here would make a streamed Takeout import silently lossier than a bulk one,
    /// and the difference would be unrecoverable without re-importing.
    #[tracing::instrument(
        skip_all,
        fields(album_id = %album_id, src = %src.display(), move_source, enriched = enrichment.is_some())
    )]
    pub fn import_asset_streaming(
        &mut self,
        album_id: Uuid,
        src: &Path,
        move_source: bool,
        stack: Option<StackMembership>,
        enrichment: Option<SidecarEnrichment>,
    ) -> Result<StreamedImport> {
        let opts = SignedImportOptions {
            move_source,
            defer_source_release: true,
            stack,
            enrichment,
        };
        let asset_id = self.import_asset_with(album_id, src, &opts)?.asset_id;
        let asset = self
            .assets
            .get(&asset_id)
            .expect("asset just inserted by import_asset_with");
        Ok(StreamedImport {
            asset_id,
            blob_hashes: release_blob_hashes(asset),
            local_original: self.media_path(asset),
            move_source: move_source.then(|| src.to_path_buf()),
        })
    }

    /// Delete a Move-mode `src` that the library holds as `asset_id` — only on server custody
    /// (invariant 7), and only when both copies still read back as the signed original.
    ///
    /// In order, and every step before the delete is a reason to keep the source:
    ///
    /// 1. **Read-back.** The library's copy and `src` are each read back and hashed; both must
    ///    equal the hash the signed sidecar commits to. A library copy that is torn or missing,
    ///    or a source edited since it was imported, is [`RetainReason::ReadBackMismatch`] —
    ///    whatever the server says, deleting `src` could lose bytes nothing else holds.
    /// 2. **The gate.** [`release_move_source`] over
    ///    `verifier` for the asset's required blobs (original ciphertext and sealed metadata
    ///    blob), with a fresh `verify_asset` as the crypto half: a `durable` verdict and a
    ///    verified custody receipt, or the source stays.
    ///
    /// `src` that resolves to the asset's own media file is never deleted.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::NotFound`] when the workspace does not hold `asset_id`.
    #[tracing::instrument(skip(self, verifier), fields(%asset_id, src = %src.display()))]
    pub fn release_move_source<V: StorageVerifier + ?Sized>(
        &self,
        verifier: &V,
        asset_id: Uuid,
        src: &Path,
    ) -> Result<ReleaseDecision> {
        let asset = self
            .assets
            .get(&asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?;
        let media_path = self.media_path(asset);
        if is_same_file(src, &media_path) {
            tracing::warn!("move release: the source is the library copy itself; keeping it");
            return Ok(ReleaseDecision::Retain(RetainReason::ReadBackMismatch));
        }
        let signed = asset.sidecar.hash;
        let reads_back = |path: &Path| fs::read(path).is_ok_and(|b| hash::hash_bytes(&b) == signed);
        if !reads_back(&media_path) {
            tracing::warn!(
                media = %media_path.display(),
                "move release: the library copy does not read back as the signed original; keeping the source"
            );
            return Ok(ReleaseDecision::Retain(RetainReason::ReadBackMismatch));
        }
        if !reads_back(src) {
            tracing::warn!(
                "move release: the source no longer hashes to the signed original; keeping it"
            );
            return Ok(ReleaseDecision::Retain(RetainReason::ReadBackMismatch));
        }
        let verify_asset_accepted = self
            .verify(&asset_id)
            .is_ok_and(|outcome| outcome == VerifyOutcome::Accept);
        let decision = release_move_source(
            verifier,
            asset_id,
            src,
            &release_blob_hashes(asset),
            verify_asset_accepted,
        );
        match &decision {
            ReleaseDecision::Release => {
                tracing::info!("move release: source deleted on server custody");
            }
            ReleaseDecision::Retain(reason) => {
                tracing::info!(?reason, "move release: source kept");
            }
        }
        Ok(decision)
    }
}

/// The blobs a release of `asset`'s original waits on — the original ciphertext and, when the
/// head binds one, the sealed metadata blob — from the provenance chain head.
fn release_blob_hashes(asset: &AssetState) -> Vec<Hash32> {
    let head = &asset
        .chain
        .records()
        .last()
        .expect("provenance chain is never empty")
        .manifest
        .core;
    let mut blob_hashes = vec![head.ciphertext_hash];
    if let Some(h) = head.metadata_blob_hash {
        blob_hashes.push(h);
    }
    blob_hashes
}

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod tests;
