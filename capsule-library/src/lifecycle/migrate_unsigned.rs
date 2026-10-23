//! Migrate the unsigned pre-signed-path sidecars into signed assets.
//!
//! A library written before the signed path existed holds, beside each original, a flat
//! unsigned CBOR map (`version: 1`, text keys) with no provenance chain, no sealed metadata
//! blob, and no album key material. [`Workspace::open`] anchors on provenance chains, so such
//! an asset is invisible to every signed operation — it cannot be verified, exported, or
//! uploaded — and a keyless [`rebuild_index`](crate::library::rebuild_index) cannot admit it
//! either: it holds no album write capability and cannot sign a sidecar or a manifest, and
//! the unsigned reader it once fell back to is gone.
//!
//! The migration is an **explicit verb**, never automatic: it authors signed records, which an
//! open must not do unasked, and it needs the album write capability a keyless rebuild does
//! not hold. Each legacy record is *admitted* as a signed `create` authored by this device now,
//! attesting exactly what any import attests — the content hash of the bytes on disk (checked
//! against the legacy record first), this device, this album, and now — through the one signed
//! write path every import takes. The legacy bytes are preserved verbatim under
//! `.library/quarantine/` before anything is written, and the whole decoded legacy map rides
//! inside the signed sidecar's `_unknown` under [`LEGACY_FOLD_KEY`], where the signature covers
//! it and the never-strip rule protects it.
//!
//! This module owns the only decoder of the legacy shape, and it is private: the verb reads
//! the handful of fields it projects and carries the rest as an opaque map.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::crypto::hash;
use capsule_core::crypto::provenance::Action;
use capsule_core::domain::{GpsDatum, StackType};
use capsule_core::sidecar::shape::{self, SidecarShape};
use capsule_core::sidecar::sidecar_v1::{
    Gps, GpsSource, SIDECAR_SCHEMA_V1, SidecarV1, StackMembership, StackRole,
};
use ciborium::value::Value;
use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;
use walkdir::WalkDir;

use super::import::CreateRequest;
use super::open::{month_dir_timestamp, original_extension};
use super::{LifecycleError, Result, SidecarEnrichment, SignedImportOptions, Workspace, media_dir};
use crate::utils::paths::sync_dir;

/// The `_unknown` key under which a migrated asset's signed sidecar carries its whole legacy
/// record. Hyphenated on purpose: no snake_case schema field can ever collide with it.
pub const LEGACY_FOLD_KEY: &str = "legacy-unsigned-sidecar";

/// The `reason` recorded in a quarantined legacy sidecar's `.reason.json`.
const QUARANTINE_REASON: &str = "unsigned-sidecar-migrated";

/// Domain separator for `legacy_stack_id`.
const LEGACY_STACK_ID_DOMAIN: &[u8] = b"capsule-legacy-stack-id-v1";

/// The deterministic stack id for a legacy `stack_hint` group: an RFC 9562 v8 (custom) UUID
/// over `SHA-256(domain ‖ user_id ‖ "{method}:{key}")`, the same construction the master key
/// uses for the default album id. A pure function of the user and the group key, so an
/// interrupted or repeated run — or the same user migrating a copy — lands every member under
/// the same id; carries no creation time.
fn legacy_stack_id(user_id: &Uuid, detection_method: &str, detection_key: &str) -> Uuid {
    let mut input = Vec::with_capacity(64 + detection_method.len() + detection_key.len());
    input.extend_from_slice(LEGACY_STACK_ID_DOMAIN);
    input.extend_from_slice(user_id.as_bytes());
    input.extend_from_slice(format!("{detection_method}:{detection_key}").as_bytes());
    let digest = hash::hash_bytes(&input);
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest.0[..16]);
    uuid::Builder::from_custom_bytes(b).into_uuid()
}

/// What [`Workspace::migrate_unsigned_sidecars`] needs beyond the workspace itself.
#[derive(Debug, Clone)]
pub struct UnsignedMigrationOptions {
    /// The album a legacy asset lands in when its own `album_id` is absent, unparseable, or
    /// names an album this workspace does not own with write capability (a shared album is
    /// never honoured: the unsigned claim is untrusted). Must already exist and
    /// be writable: the verb never mints an album.
    pub fallback_album: Uuid,
    /// The retention window, in days, stamped on the `delete` record of a legacy asset whose
    /// record says `is_deleted: true` — the same argument [`Workspace::soft_delete`] takes.
    pub trash_retain_days: i64,
}

/// What one run of [`Workspace::migrate_unsigned_sidecars`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UnsignedMigrationReport {
    /// Every asset admitted as a signed create this run, in the order it was written.
    pub migrated: Vec<Uuid>,
    /// Every asset that received its signed `delete` record this run: the subset of
    /// [`migrated`](Self::migrated) whose legacy record said `is_deleted`, plus any asset an
    /// earlier run admitted whose `delete` never landed and was applied now.
    pub trashed: Vec<Uuid>,
    /// The stack ids derived from legacy `stack_hint` groups of two or more members.
    pub stacks: Vec<Uuid>,
    /// Sidecar files the run refused, each with why. Nothing was written for any of them.
    pub skipped: Vec<(PathBuf, MigrationSkip)>,
}

/// Why the migration refused one sidecar file without writing anything for it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MigrationSkip {
    /// The file's stem does not parse as a UUID, so it cannot be an asset's sidecar.
    #[error("sidecar file name is not an asset id: {0}")]
    InvalidAssetId(String),
    /// The file is neither the signed shape nor the legacy unsigned one.
    #[error("sidecar is neither the signed nor the legacy unsigned shape")]
    UnknownShape,
    /// The legacy map is missing a required field or carries one of the wrong type.
    #[error("legacy record does not decode: {0}")]
    Undecodable(String),
    /// A signed asset with this id is already restored; migrating over it would replace a
    /// signed record with a re-derived one.
    #[error("asset {0} is already a signed asset in this library")]
    IdCollision(Uuid),
    /// No `{uuid}.{ext}` original sits beside the sidecar. An orphaned sidecar is the
    /// maintenance scrub's finding, not this verb's.
    #[error("asset {0}: no original media file beside the sidecar")]
    OriginalMissing(Uuid),
    /// The bytes on disk do not hash to what the legacy record says. A corrupt original is
    /// surfaced, never laundered into a fresh signature.
    #[error("asset {asset_id}: original hashes to {actual}, but the legacy record says {recorded}")]
    HashMismatch {
        /// The asset whose original disagrees with its record.
        asset_id: Uuid,
        /// The `hash_sha256` the legacy record carries.
        recorded: String,
        /// The SHA-256 of the bytes actually on disk.
        actual: String,
    },
    /// `.library/quarantine/` already holds a copy of this sidecar with different bytes, so
    /// the run cannot tell which is the legacy record.
    #[error("asset {0}: quarantine already holds different bytes for this sidecar")]
    QuarantineConflict(Uuid),
    /// The sidecar does not sit in a `media/{YYYY}/{YYYY-MM}` bucket, so its files cannot be
    /// addressed by the lifecycle's bucket-derived paths without moving them — and the
    /// migration never moves a file.
    #[error("asset {asset_id}: {dir} is not a media/{{YYYY}}/{{YYYY-MM}} bucket")]
    OutsideMonthBucket {
        /// The asset whose sidecar sits outside a bucket.
        asset_id: Uuid,
        /// The directory it was found in.
        dir: PathBuf,
    },
    /// The original's extension is not a lowercase single segment (`jpg`, `dng`, `mp4`), the
    /// form every lifecycle path derives; a `JPG` or `tar.gz` original would resolve to a
    /// different path than the file that exists.
    #[error("asset {asset_id}: original extension {ext:?} is not lowercase single-segment")]
    UnusualExtension {
        /// The asset whose original has the extension.
        asset_id: Uuid,
        /// The extension as found on disk.
        ext: String,
    },
    /// An asset an earlier run admitted still owes its `delete` record, but its album holds
    /// no write capability now (recovered from a backup); the record cannot be authored.
    #[error(
        "asset {asset_id}: album {album_id} is read-only; its owed delete record was not written"
    )]
    AlbumReadOnly {
        /// The asset that still owes a `delete`.
        asset_id: Uuid,
        /// The album without write capability.
        album_id: Uuid,
    },
    /// A signed sidecar with no provenance chain that is not an interrupted migration
    /// create: it carries a later write (`provenance_chain_hash` set), it carries no legacy
    /// fold, or there is no quarantine copy to resume from. Nothing this verb can rebuild
    /// without discarding signed state, so it is reported and left alone.
    #[error("asset {0}: signed sidecar without a provenance chain is not a resumable migration")]
    Stranded(Uuid),
}

/// The shape of a `{uuid}.cbor` under `media/` that no `.provenance.cbor` anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmigratedShape {
    /// The retired unsigned pre-signed-path record: what the migration verb rewrites.
    LegacyUnsigned,
    /// A signed sidecar whose chain is missing — an interrupted migration, resumable from its
    /// quarantine copy — with the `sidecar_schema` it carries.
    SignedWithoutChain {
        /// The value at the sidecar's integer key `0`.
        schema: u16,
    },
    /// Neither shape — not CBOR, not a map, or a torn write. Resumed from its quarantine copy
    /// when one exists; otherwise reported so it is never silently skipped, and never touched.
    Unknown,
}

/// One sidecar file [`Workspace::open`] found that no provenance chain anchors — an asset the
/// workspace cannot see until [`Workspace::migrate_unsigned_sidecars`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmigratedSidecar {
    /// The `{uuid}.cbor` file.
    pub path: PathBuf,
    /// The asset id its stem names, when the stem parses as one.
    pub asset_id: Option<Uuid>,
    /// Which shape the bytes have.
    pub shape: UnmigratedShape,
}

/// Every `{uuid}.cbor` under `root/media` — the sidecars, never the sibling
/// `.provenance.cbor` / `.receipts.cbor` / `.attestations.cbor` logs or the
/// `.derivatives.cbor` bundle. The same filter `rebuild_index` and the add-id sweep apply.
fn sidecar_files(root: &Path) -> Vec<PathBuf> {
    let media = root.join("media");
    if !media.exists() {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = WalkDir::new(&media)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().is_file())
        .filter(|e| crate::library::paths::is_sidecar_file_name(&e.file_name().to_string_lossy()))
        .map(|e| e.path().to_path_buf())
        .collect();
    // Deterministic order regardless of directory-walk order.
    out.sort();
    out
}

/// The sidecars under `root/media` with no `.provenance.cbor` sibling, each probed for its
/// shape. Used by [`Workspace::open`] to report them and by the migration verb to find its
/// candidates.
pub(super) fn find_unanchored(root: &Path) -> Vec<UnmigratedSidecar> {
    let mut out = Vec::new();
    for path in sidecar_files(root) {
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".cbor"))
        else {
            continue;
        };
        let dir = path.parent().unwrap_or(root);
        if dir.join(format!("{stem}.provenance.cbor")).exists() {
            continue;
        }
        let asset_id = Uuid::parse_str(stem).ok();
        let shape = match fs::read(&path) {
            Ok(bytes) => match shape::probe(&bytes) {
                SidecarShape::Signed { schema } => UnmigratedShape::SignedWithoutChain { schema },
                SidecarShape::LegacyUnsigned => UnmigratedShape::LegacyUnsigned,
                SidecarShape::Unknown => UnmigratedShape::Unknown,
            },
            Err(e) => {
                tracing::warn!(sidecar = %path.display(), error = %e, "unreadable sidecar file");
                UnmigratedShape::Unknown
            }
        };
        out.push(UnmigratedSidecar {
            path,
            asset_id,
            shape,
        });
    }
    out
}

/// The sidecars under the library at `root` that no provenance chain anchors — the set
/// [`Workspace::open`] reports through [`Workspace::unmigrated_sidecars`], found without a
/// passphrase or any key material.
///
/// The scan only walks `root/media` and probes each unanchored file's shape, so a keyless
/// caller (`capsule library info`, which opens the library without unlocking it) can report
/// what [`Workspace::migrate_unsigned_sidecars`] would pick up. A missing `media/` directory
/// is an empty library, not an error; an unreadable sidecar is reported as
/// [`UnmigratedShape::Unknown`], never skipped.
#[must_use]
pub fn unmigrated_sidecars(root: &Path) -> Vec<UnmigratedSidecar> {
    let found = find_unanchored(root);
    tracing::debug!(
        library = %root.display(),
        count = found.len(),
        "keyless unmigrated-sidecar scan"
    );
    found
}

// ── the legacy shape, decoded once and privately ────────────────────────────

/// Re-encode a CBOR [`Value`] and deserialize it as `T`.
fn from_value<T: serde::de::DeserializeOwned>(v: &Value) -> std::result::Result<T, String> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(v, &mut buf).map_err(|e| e.to_string())?;
    ciborium::de::from_reader(buf.as_slice()).map_err(|e| e.to_string())
}

/// [`from_value`] with the field name in the error.
fn typed<T: serde::de::DeserializeOwned>(key: &str, v: &Value) -> std::result::Result<T, String> {
    from_value(v).map_err(|e| format!("field {key}: {e}"))
}

/// The legacy `stack_hint`, with the two fields the grouping keys on kept as the strings the
/// legacy serializer wrote (`snake_case` enum names).
#[derive(Debug, Clone, PartialEq)]
struct LegacyStackHint {
    detection_key: String,
    detection_method: String,
    member_role: String,
    stack_type: StackType,
}

/// The fields of a legacy unsigned record the migration projects, plus the whole map.
///
/// Everything not named here — `original_filename`, `import_mode`, `importer_version`,
/// `rawshift_version`, `capture_tz*`, `tz_db_version`, `file_size`, `duration_ms`,
/// `modified_timestamp`, `camera_make`/`model`, and any key a later build added — has no
/// signed home of its own and survives only inside [`map`](Self::map).
#[derive(Debug, Clone, PartialEq)]
struct LegacyRecord {
    uuid: String,
    hash_sha256: String,
    import_timestamp: i64,
    capture_utc: Option<i64>,
    capture_timestamp: Option<i64>,
    rating: u8,
    tags: Vec<String>,
    stack_hint: Option<LegacyStackHint>,
    album_id: Option<String>,
    is_deleted: bool,
    gps: Option<(f64, f64)>,
    /// The entire legacy map, verbatim (every key, projected ones included).
    map: Value,
}

impl LegacyRecord {
    /// Decode a legacy unsigned sidecar. Required: `uuid`, `hash_sha256`, `import_timestamp`.
    /// Every other projected field defaults when absent and fails when present with the wrong
    /// type; a `null` counts as absent, as the legacy reader treated it.
    fn decode(bytes: &[u8]) -> std::result::Result<Self, String> {
        let value: Value =
            ciborium::de::from_reader(bytes).map_err(|e| format!("cbor decode: {e}"))?;
        Self::from_value(value)
    }

    /// [`decode`](Self::decode) over a map already parsed — the bytes' own value, or the fold a
    /// migrated asset's signed sidecar carries under [`LEGACY_FOLD_KEY`].
    fn from_value(value: Value) -> std::result::Result<Self, String> {
        let Value::Map(entries) = &value else {
            return Err("legacy sidecar must be a CBOR map".into());
        };
        let mut fields: BTreeMap<&str, &Value> = BTreeMap::new();
        for (k, v) in entries {
            if let Value::Text(key) = k
                && !matches!(v, Value::Null)
            {
                fields.insert(key.as_str(), v);
            }
        }
        let req = |key: &str| -> std::result::Result<&Value, String> {
            fields
                .get(key)
                .copied()
                .ok_or_else(|| format!("missing required field: {key}"))
        };
        let opt = |key: &str| fields.get(key).copied();

        let uuid: String = typed("uuid", req("uuid")?)?;
        let hash_sha256: String = typed("hash_sha256", req("hash_sha256")?)?;
        let import_timestamp: i64 = typed("import_timestamp", req("import_timestamp")?)?;
        let capture_utc = opt("capture_utc")
            .map(|v| typed("capture_utc", v))
            .transpose()?;
        let capture_timestamp = opt("capture_timestamp")
            .map(|v| typed("capture_timestamp", v))
            .transpose()?;
        let rating: u8 = opt("rating").map_or(Ok(0), |v| typed("rating", v))?;
        let tags: Vec<String> = opt("tags").map_or(Ok(Vec::new()), |v| typed("tags", v))?;
        let album_id = opt("album_id").map(|v| typed("album_id", v)).transpose()?;
        let is_deleted: bool = opt("is_deleted").map_or(Ok(false), |v| typed("is_deleted", v))?;
        let gps_lat: Option<f64> = opt("gps_lat").map(|v| typed("gps_lat", v)).transpose()?;
        let gps_lon: Option<f64> = opt("gps_lon").map(|v| typed("gps_lon", v)).transpose()?;
        let stack_hint = match opt("stack_hint") {
            None => None,
            Some(Value::Map(hint)) => {
                let mut h: BTreeMap<&str, &Value> = BTreeMap::new();
                for (k, v) in hint {
                    if let Value::Text(key) = k {
                        h.insert(key.as_str(), v);
                    }
                }
                let field = |key: &str| -> std::result::Result<&Value, String> {
                    h.get(key)
                        .copied()
                        .ok_or_else(|| format!("stack_hint missing field: {key}"))
                };
                Some(LegacyStackHint {
                    detection_key: typed("stack_hint.detection_key", field("detection_key")?)?,
                    detection_method: typed(
                        "stack_hint.detection_method",
                        field("detection_method")?,
                    )?,
                    member_role: typed("stack_hint.member_role", field("member_role")?)?,
                    stack_type: typed("stack_hint.stack_type", field("stack_type")?)?,
                })
            }
            Some(_) => return Err("field stack_hint: expected a map".into()),
        };

        Ok(Self {
            uuid,
            hash_sha256,
            import_timestamp,
            capture_utc,
            capture_timestamp,
            rating,
            tags,
            stack_hint,
            album_id,
            is_deleted,
            gps: gps_lat.zip(gps_lon),
            map: value,
        })
    }

    /// The legacy capture instant the sidecar's `capture_timestamp` falls back to when the
    /// file's own EXIF resolves none: `capture_utc`, else `capture_timestamp`, else the legacy
    /// import time — never the migration's own clock.
    fn capture_fallback(&self) -> Timestamp {
        [
            self.capture_utc,
            self.capture_timestamp,
            Some(self.import_timestamp),
        ]
        .into_iter()
        .flatten()
        .find_map(|secs| Timestamp::from_second(secs).ok())
        .unwrap_or(Timestamp::UNIX_EPOCH)
    }
}

/// One member of a legacy `stack_hint` group, as [`Workspace::legacy_stack_memberships`] counts
/// it.
struct StackMember {
    asset_id: Uuid,
    hint: LegacyStackHint,
    /// A candidate of this run, which needs a placement — as opposed to an asset an earlier
    /// run already placed, which only counts toward the group and its numbering.
    this_run: bool,
    /// The `member_index` an earlier run's placement holds, which no candidate of this run
    /// may be given. Always `None` for this run's candidates.
    held_index: Option<u32>,
}

// ── candidates ──────────────────────────────────────────────────────────────

/// A legacy sidecar the run has admitted: decoded, its original present and hash-checked.
struct Candidate {
    /// The `{uuid}.cbor` file the signed sidecar will be written over.
    path: PathBuf,
    /// Its media directory (`media/{YYYY}/{YYYY-MM}`).
    dir: PathBuf,
    asset_id: Uuid,
    /// The legacy bytes — from the file itself, or from quarantine when resuming.
    bytes: Vec<u8>,
    record: LegacyRecord,
    /// The original's extension (`{uuid}.{ext}` beside the sidecar).
    ext: String,
    /// Whether the legacy bytes came from quarantine (an interrupted run being resumed), in
    /// which case the quarantine copy is already in place.
    resumed: bool,
}

impl Workspace {
    /// The sidecar files [`open`](Self::open) found under `media/` that no provenance chain
    /// anchors — assets this workspace cannot see, verify, export, or upload until
    /// [`migrate_unsigned_sidecars`](Self::migrate_unsigned_sidecars) runs. Empty for a library
    /// written entirely on the signed path.
    pub fn unmigrated_sidecars(&self) -> &[UnmigratedSidecar] {
        &self.unmigrated
    }

    /// Where a legacy sidecar's verbatim bytes are preserved: `.library/quarantine/{uuid}.cbor`.
    fn quarantine_sidecar_path(&self, asset_id: &Uuid) -> PathBuf {
        self.root
            .join(".library")
            .join("quarantine")
            .join(format!("{}.cbor", asset_id.simple()))
    }

    /// Migrate every unsigned pre-signed-path sidecar under `media/` into a signed asset, then
    /// rebuild the index so stack rows are reconstructed uniformly.
    ///
    /// For each legacy sidecar, in asset-id order:
    ///
    /// 1. **Refuse without writing** when the file name is not a UUID, a signed asset with that
    ///    id is already restored, the original beside it is missing, or the original's SHA-256
    ///    disagrees with the record's `hash_sha256`. Each refusal is a [`MigrationSkip`] in the
    ///    report; the file is untouched.
    /// 2. **Preserve** the legacy bytes verbatim at `.library/quarantine/{uuid}.cbor` with a
    ///    sibling `{uuid}.reason.json`, before any signed write.
    /// 3. **Admit** the asset through the one signed write path (`import_asset_with`'s commit),
    ///    keeping its id, its bytes where they are, and its media bucket: the signed sidecar is
    ///    written over the legacy one, the chain, sealed metadata blob, and index row beside it.
    ///    Capture time follows the import precedence — the file's own EXIF, else the legacy
    ///    record's, else the legacy import time. Rating, tags, and GPS are carried into their
    ///    signed registers; the whole legacy map rides in `_unknown` under [`LEGACY_FOLD_KEY`].
    /// 4. **Carry trash state**: a legacy `is_deleted` becomes a signed `delete` record with
    ///    `opts.trash_retain_days` of retention.
    ///
    /// Legacy `stack_hint`s are grouped by `(detection_method, detection_key)` first; a group
    /// of two or more members gets a deterministic stack id — an RFC 9562 v8 (custom)
    /// UUID over `SHA-256(domain ‖ user_id ‖ "{method}:{key}")` — written into each member's
    /// signed `stack_membership` register at create. The members counted are this run's
    /// admitted candidates plus any an interrupted earlier run already placed in that stack.
    /// A singleton gets no stack; its hint survives in the fold.
    ///
    /// The album is the legacy `album_id` when this workspace owns it (holds its admin and
    /// write capabilities; a shared album is not honoured), else `opts.fallback_album`, which
    /// must already exist and be writable — the verb never mints an album, and a read-only
    /// fallback is a typed
    /// [`AlbumReadOnly`](LifecycleError::AlbumReadOnly) refusal before anything is written.
    ///
    /// Idempotent and resumable: a second run finds signed sidecars with chains and does
    /// nothing; a sidecar left without a chain (or torn) by an interrupted run is re-migrated
    /// from its quarantine copy, provided the on-disk sidecar is still the migration's own
    /// create — one that carries a later write is [`Stranded`](MigrationSkip::Stranded), never
    /// overwritten; and a legacy `is_deleted` whose `delete` record never landed is applied on
    /// the next run, unless the asset has since been trashed and restored by hand. A write
    /// failure mid-run is returned as the error; the assets already migrated stay migrated,
    /// and a rerun picks up the rest.
    #[tracing::instrument(
        skip_all,
        fields(root = %self.root.display(), fallback_album = %opts.fallback_album)
    )]
    pub fn migrate_unsigned_sidecars(
        &mut self,
        opts: &UnsignedMigrationOptions,
    ) -> Result<UnsignedMigrationReport> {
        // The fallback album must exist and be writable before a single byte is written.
        self.album(&opts.fallback_album)?.write_tier_signer()?;

        // Assets a previous run admitted but whose `delete` record never landed.
        let (trashed, unwritable) = self.reconcile_legacy_trash(opts.trash_retain_days)?;
        let mut report = UnsignedMigrationReport {
            trashed,
            skipped: unwritable,
            ..UnsignedMigrationReport::default()
        };
        let mut candidates: Vec<Candidate> = Vec::new();

        // Pass 1: find, decode, and check every candidate; refuse what cannot be admitted.
        for found in find_unanchored(&self.root) {
            match self.admit(&found) {
                Ok(candidate) => candidates.push(candidate),
                Err(skip) => {
                    tracing::warn!(
                        sidecar = %found.path.display(),
                        reason = %skip,
                        "unsigned migration: refusing this sidecar; nothing written for it"
                    );
                    report.skipped.push((found.path, skip));
                }
            }
        }
        candidates.sort_by_key(|c| c.asset_id);
        // One id, one asset: a second legacy sidecar claiming an id already admitted this run
        // (the same id in two month buckets) would overwrite the first's signed record.
        candidates.dedup_by(|later, first| {
            let dup = later.asset_id == first.asset_id;
            if dup {
                tracing::warn!(
                    sidecar = %later.path.display(),
                    asset_id = %later.asset_id,
                    "unsigned migration: a second sidecar claims an id admitted this run; refusing it"
                );
                report
                    .skipped
                    .push((later.path.clone(), MigrationSkip::IdCollision(later.asset_id)));
            }
            dup
        });

        // Pass 2: derive the stack placements from the admitted members' hints.
        let memberships = self.legacy_stack_memberships(&candidates);
        report.stacks = memberships.values().map(|m| m.stack_id).collect();
        report.stacks.sort();
        report.stacks.dedup();

        // Pass 3: quarantine, then admit, then carry trash state — one asset at a time.
        for candidate in candidates {
            let asset_id = candidate.asset_id;
            let album_id = self.resolve_legacy_album(candidate.record.album_id.as_deref(), opts);
            self.quarantine_legacy(&candidate)?;
            let stack = memberships.get(&asset_id).cloned();
            let stacked = stack.is_some();
            self.commit_legacy_create(&candidate, album_id, stack)?;
            report.migrated.push(asset_id);
            if candidate.record.is_deleted {
                self.soft_delete(&asset_id, opts.trash_retain_days)?;
                report.trashed.push(asset_id);
            }
            tracing::info!(
                asset_id = %asset_id,
                album_id = %album_id,
                trashed = candidate.record.is_deleted,
                stacked,
                resumed = candidate.resumed,
                "unsigned migration: asset admitted as a signed create"
            );
        }

        // The "on rebuild" half: stack rows are reconstructed from the registers uniformly, and
        // every migrated row is re-projected from its signed artifacts.
        crate::library::rebuild_index(&self.library)?;
        self.unmigrated = find_unanchored(&self.root);

        tracing::info!(
            migrated = report.migrated.len(),
            trashed = report.trashed.len(),
            stacks = report.stacks.len(),
            skipped = report.skipped.len(),
            remaining = self.unmigrated.len(),
            "unsigned migration: run complete"
        );
        Ok(report)
    }

    /// The legacy bytes `.library/quarantine/{uuid}.cbor` holds for `asset_id`, when it holds
    /// a legacy record at all.
    fn quarantine_twin(&self, asset_id: &Uuid) -> std::result::Result<Vec<u8>, MigrationSkip> {
        let twin = self.quarantine_sidecar_path(asset_id);
        let bytes = fs::read(&twin).map_err(|_| MigrationSkip::Stranded(*asset_id))?;
        if shape::probe(&bytes) != SidecarShape::LegacyUnsigned {
            return Err(MigrationSkip::Stranded(*asset_id));
        }
        Ok(bytes)
    }

    /// Apply the `delete` record a previous run owed: an asset whose signed sidecar carries a
    /// legacy fold saying `is_deleted: true` but whose chain has **never** carried a `delete`.
    /// An asset the user has since trashed and restored has a `delete` in its chain and is
    /// left exactly as they left it. An owed delete whose album is read-only now is reported
    /// ([`MigrationSkip::AlbumReadOnly`]) rather than aborting the run.
    #[allow(clippy::type_complexity)]
    fn reconcile_legacy_trash(
        &mut self,
        retain_days: i64,
    ) -> Result<(Vec<Uuid>, Vec<(PathBuf, MigrationSkip)>)> {
        let is_deleted_key = Value::Text("is_deleted".to_string());
        let mut owed: Vec<Uuid> = self
            .assets
            .values()
            .filter(|asset| {
                let Some(Value::Map(fold)) = asset.sidecar.unknown.get(LEGACY_FOLD_KEY) else {
                    return false;
                };
                let says_deleted = fold
                    .iter()
                    .any(|(k, v)| *k == is_deleted_key && matches!(v, Value::Bool(true)));
                let ever_trashed = asset
                    .chain
                    .records()
                    .iter()
                    .any(|r| r.manifest.core.action == Action::Delete);
                says_deleted && !ever_trashed
            })
            .map(|asset| asset.asset_id)
            .collect();
        owed.sort();
        let mut trashed = Vec::new();
        let mut unwritable = Vec::new();
        for id in owed {
            let asset = &self.assets[&id];
            let album_id = asset.album_id;
            let writable = self
                .albums
                .get(&album_id)
                .is_some_and(|a| a.write_tier.is_some());
            if !writable {
                let path = self.sidecar_path(asset);
                tracing::warn!(
                    asset_id = %id,
                    album_id = %album_id,
                    "unsigned migration: an owed delete record cannot be written into a read-only album"
                );
                unwritable.push((
                    path,
                    MigrationSkip::AlbumReadOnly {
                        asset_id: id,
                        album_id,
                    },
                ));
                continue;
            }
            tracing::info!(
                asset_id = %id,
                "unsigned migration: applying the delete record an interrupted run owed"
            );
            self.soft_delete(&id, retain_days)?;
            trashed.push(id);
        }
        Ok((trashed, unwritable))
    }

    /// Decode one unanchored sidecar and check everything that must hold before it is
    /// admitted. Reads the original once, streaming, for the hash check.
    fn admit(&self, found: &UnmigratedSidecar) -> std::result::Result<Candidate, MigrationSkip> {
        let stem = found
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let asset_id = found.asset_id.ok_or(MigrationSkip::InvalidAssetId(stem))?;
        let dir = found
            .path
            .parent()
            .map_or_else(|| self.root.clone(), Path::to_path_buf);
        // Every later path for this asset is derived from its bucket, so the bucket must round
        // trip: parse the directory's month, and require that the lifecycle maps it back to
        // exactly this directory. Nothing is ever moved to make it fit.
        if media_dir(&self.root, month_dir_timestamp(&dir)) != dir {
            return Err(MigrationSkip::OutsideMonthBucket { asset_id, dir });
        }

        let (bytes, resumed) = match found.shape {
            UnmigratedShape::LegacyUnsigned => (
                fs::read(&found.path).map_err(|e| MigrationSkip::Undecodable(e.to_string()))?,
                false,
            ),
            // A signed sidecar with no chain: an interrupted run wrote the sidecar and died
            // before the chain. Its quarantine copy is the legacy record; resume from it — but
            // only if the sidecar on disk is still that run's own create. A later write sets
            // `provenance_chain_hash`, and a signed asset that was never migrated has no fold;
            // redoing the create over either would discard signed state.
            UnmigratedShape::SignedWithoutChain { .. } => {
                let on_disk =
                    fs::read(&found.path).map_err(|_| MigrationSkip::Stranded(asset_id))?;
                let signed = SidecarV1::from_canonical_slice(&on_disk, SIDECAR_SCHEMA_V1)
                    .map_err(|_| MigrationSkip::Stranded(asset_id))?;
                if signed.provenance_chain_hash.is_some()
                    || !signed.unknown.contains_key(LEGACY_FOLD_KEY)
                {
                    return Err(MigrationSkip::Stranded(asset_id));
                }
                (self.quarantine_twin(&asset_id)?, true)
            }
            // A torn write of the signed sidecar, most likely: the quarantine copy, if there is
            // one, is the legacy record and the run resumes from it.
            UnmigratedShape::Unknown => (
                self.quarantine_twin(&asset_id)
                    .map_err(|_| MigrationSkip::UnknownShape)?,
                true,
            ),
        };
        let record = LegacyRecord::decode(&bytes).map_err(MigrationSkip::Undecodable)?;
        if Uuid::parse_str(&record.uuid).ok() != Some(asset_id) {
            return Err(MigrationSkip::Undecodable(format!(
                "record uuid {:?} does not name the file's asset id {asset_id}",
                record.uuid
            )));
        }
        if self.assets.contains_key(&asset_id) {
            return Err(MigrationSkip::IdCollision(asset_id));
        }
        if !resumed {
            let twin = self.quarantine_sidecar_path(&asset_id);
            if let Ok(existing) = fs::read(&twin)
                && existing != bytes
            {
                return Err(MigrationSkip::QuarantineConflict(asset_id));
            }
        }
        let ext =
            original_extension(&dir, &asset_id).ok_or(MigrationSkip::OriginalMissing(asset_id))?;
        if ext.is_empty()
            || !ext
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(MigrationSkip::UnusualExtension { asset_id, ext });
        }
        let original = dir.join(format!("{}.{ext}", asset_id.simple()));
        let actual = fs::File::open(&original)
            .and_then(hash::hash_reader)
            .map_err(|_| MigrationSkip::OriginalMissing(asset_id))?
            .to_hex();
        if !actual.eq_ignore_ascii_case(&record.hash_sha256) {
            return Err(MigrationSkip::HashMismatch {
                asset_id,
                recorded: record.hash_sha256.clone(),
                actual,
            });
        }
        tracing::debug!(
            asset_id = %asset_id,
            resumed,
            has_stack_hint = record.stack_hint.is_some(),
            is_deleted = record.is_deleted,
            legacy_album = ?record.album_id,
            "unsigned migration: candidate admitted"
        );
        Ok(Candidate {
            path: found.path.clone(),
            dir,
            asset_id,
            bytes,
            record,
            ext,
            resumed,
        })
    }

    /// The legacy `album_id` when it parses and names an album this workspace **owns** (holds
    /// its admin capability) and can write into, else the fallback.
    ///
    /// The legacy sidecar is unsigned, so its `album_id` is a claim anyone who can plant a file
    /// under `media/` can make. Write access alone would let such a file be signed into an
    /// album shared with this device; only the user's own albums are honoured, and everything
    /// else lands in the fallback album the caller chose.
    fn resolve_legacy_album(&self, legacy: Option<&str>, opts: &UnsignedMigrationOptions) -> Uuid {
        let owned = legacy.and_then(|s| Uuid::parse_str(s).ok()).filter(|id| {
            self.albums
                .get(id)
                .is_some_and(|a| a.admin.is_some() && a.write_tier.is_some())
        });
        if let Some(id) = owned {
            return id;
        }
        if let Some(legacy) = legacy {
            tracing::debug!(
                legacy_album = legacy,
                fallback_album = %opts.fallback_album,
                "unsigned migration: legacy album not held, not owned, or not writable; using the fallback"
            );
        }
        opts.fallback_album
    }

    /// Group the members by `(detection_method, detection_key)` and derive one deterministic
    /// stack id per group of two or more (`legacy_stack_id`), so an interrupted or repeated
    /// run lands every member under the same id. Returns a placement for each candidate.
    ///
    /// The members are this run's candidates **and** the assets an earlier run already
    /// admitted into the same stack — those whose signed sidecar carries the legacy fold with
    /// this hint and whose `stack_membership` register still names the derived id. Without
    /// them, a run that crashed mid-stack would resume the remaining members as a group of
    /// one and leave the admitted member alone in its stack forever. An earlier member placed
    /// in no stack (its group was a singleton then) or since moved by the user is not counted,
    /// so its register is never contradicted.
    ///
    /// An earlier member keeps the `member_index` its register holds. This run's candidates,
    /// in asset-id order, take the lowest indices no earlier member holds. After a crash
    /// mid-stack that reproduces the interrupted run's own numbering (the free indices are
    /// exactly the unplaced members' positions in the sorted union); a member that joins a
    /// stack an earlier run completed without it is numbered after the placed members, so no
    /// two members of a stack share an index.
    fn legacy_stack_memberships(
        &self,
        candidates: &[Candidate],
    ) -> BTreeMap<Uuid, StackMembership> {
        let user_id = self.account.user_id;
        let mut groups: BTreeMap<(String, String), Vec<StackMember>> = BTreeMap::new();
        for c in candidates {
            if let Some(hint) = &c.record.stack_hint {
                groups
                    .entry((hint.detection_method.clone(), hint.detection_key.clone()))
                    .or_default()
                    .push(StackMember {
                        asset_id: c.asset_id,
                        hint: hint.clone(),
                        this_run: true,
                        held_index: None,
                    });
            }
        }
        for asset in self.assets.values() {
            let Some(fold) = asset.sidecar.unknown.get(LEGACY_FOLD_KEY) else {
                continue;
            };
            let Some(hint) = LegacyRecord::from_value(fold.clone())
                .ok()
                .and_then(|r| r.stack_hint)
            else {
                continue;
            };
            let group_key = (hint.detection_method.clone(), hint.detection_key.clone());
            // Only a group this run is completing needs its earlier members.
            let Some(members) = groups.get_mut(&group_key) else {
                continue;
            };
            let stack_id = legacy_stack_id(&user_id, &group_key.0, &group_key.1);
            let placed = asset
                .sidecar
                .stack_membership
                .get()
                .and_then(Option::as_ref)
                .filter(|m| m.stack_id == stack_id);
            if let Some(placed) = placed {
                tracing::debug!(
                    asset_id = %asset.asset_id,
                    stack_id = %stack_id,
                    member_index = ?placed.member_index,
                    "unsigned migration: an earlier run's stack member joins its group"
                );
                members.push(StackMember {
                    asset_id: asset.asset_id,
                    hint,
                    this_run: false,
                    held_index: placed.member_index,
                });
            } else {
                tracing::debug!(
                    asset_id = %asset.asset_id,
                    stack_id = %stack_id,
                    "unsigned migration: an earlier asset sharing the hint is outside the \
                     derived stack; not counted"
                );
            }
        }
        let mut out = BTreeMap::new();
        for ((method, key), mut members) in groups {
            if members.len() < 2 {
                continue;
            }
            members.sort_by_key(|m| m.asset_id);
            let stack_id = legacy_stack_id(&user_id, &method, &key);
            let stack_type = members[0].hint.stack_type;
            let earlier = members.iter().filter(|m| !m.this_run).count();
            let held: BTreeSet<u32> = members.iter().filter_map(|m| m.held_index).collect();
            let mut free = (0u32..).filter(|i| !held.contains(i));
            for member in members.iter().filter(|m| m.this_run) {
                let index = free
                    .next()
                    .expect("an unbounded range always has a free index");
                let role = match member.hint.member_role.as_str() {
                    "primary" => StackRole::Primary,
                    "proxy" => StackRole::Proxy,
                    _ => StackRole::Member,
                };
                out.insert(
                    member.asset_id,
                    StackMembership {
                        stack_id,
                        stack_type,
                        role,
                        member_index: Some(index),
                    },
                );
            }
            tracing::debug!(
                stack_id = %stack_id,
                method = %method,
                key = %key,
                members = members.len(),
                earlier,
                "unsigned migration: stack derived from legacy hints"
            );
        }
        out
    }

    /// Copy the legacy bytes to `.library/quarantine/{uuid}.cbor` and write the sibling
    /// `.reason.json`, before any signed write. The copy is `fsync`ed and its directory entry
    /// made durable ([`sync_dir`], a no-op where the platform has no directory fsync), so the
    /// legacy record is on disk before the signed sidecar can replace it (the replacement
    /// itself is a single-file atomic rename). A resumed candidate's copy is already there.
    fn quarantine_legacy(&self, candidate: &Candidate) -> Result<()> {
        let twin = self.quarantine_sidecar_path(&candidate.asset_id);
        let dir = twin
            .parent()
            .ok_or_else(|| LifecycleError::Io("quarantine path has no parent".into()))?;
        fs::create_dir_all(dir).map_err(|e| LifecycleError::Io(format!("quarantine dir: {e}")))?;
        if !candidate.resumed {
            fs::copy(&candidate.path, &twin).map_err(|e| {
                LifecycleError::Io(format!("quarantine {}: {e}", candidate.path.display()))
            })?;
            // A write handle: flushing a file's buffers needs one on every platform.
            fs::File::options()
                .write(true)
                .open(&twin)
                .and_then(|f| f.sync_all())
                .map_err(|e| LifecycleError::Io(format!("fsync {}: {e}", twin.display())))?;
            sync_dir(dir)
                .map_err(|e| LifecycleError::Io(format!("fsync {}: {e}", dir.display())))?;
        }
        let reason = serde_json::json!({
            "reason": QUARANTINE_REASON,
            "migrated_at": super::now_rfc3339(),
            "sha256_of_legacy_bytes": hash::hash_bytes(&candidate.bytes).to_hex(),
            "source": candidate.path.display().to_string(),
        });
        let reason_path = dir.join(format!("{}.reason.json", candidate.asset_id.simple()));
        let body = serde_json::to_vec_pretty(&reason)
            .map_err(|e| LifecycleError::Io(format!("reason json: {e}")))?;
        fs::write(&reason_path, body)
            .map_err(|e| LifecycleError::Io(format!("write {}: {e}", reason_path.display())))?;
        tracing::debug!(
            asset_id = %candidate.asset_id,
            quarantine = %twin.display(),
            "unsigned migration: legacy bytes preserved"
        );
        Ok(())
    }

    /// Admit one candidate through the signed create commit, with its id, its bytes in place,
    /// and its media bucket kept.
    fn commit_legacy_create(
        &mut self,
        candidate: &Candidate,
        album_id: Uuid,
        stack: Option<StackMembership>,
    ) -> Result<()> {
        let record = &candidate.record;
        let enrichment = SidecarEnrichment {
            capture_time: Some(record.capture_fallback()),
            // Not `Exif`: the fix was read out of the legacy record, not out of these file
            // bytes, and the sidecar is signed — `Manual` is the honest provenance for a
            // record-held coordinate, exactly as the Takeout enrichment tags its own
            // (`import::enrichment::sidecar_enrichment`). A file whose EXIF carries a fix
            // wins at the write site and is tagged `Exif` there.
            gps: record.gps.map(|(lat, lon)| Gps {
                lat,
                lon,
                source: GpsSource::Manual,
                datum: GpsDatum::Wgs84,
            }),
            caption: None,
            rating: (record.rating > 0).then_some(record.rating),
            tags: record.tags.clone(),
        };
        let opts = SignedImportOptions {
            move_source: false,
            defer_source_release: false,
            stack,
            enrichment: Some(enrichment),
        };
        let mut extra_unknown = BTreeMap::new();
        extra_unknown.insert(LEGACY_FOLD_KEY.to_string(), record.map.clone());
        let import_timestamp = Timestamp::from_second(record.import_timestamp)
            .unwrap_or(Timestamp::UNIX_EPOCH)
            .to_string();
        let original =
            candidate
                .dir
                .join(format!("{}.{}", candidate.asset_id.simple(), candidate.ext));
        self.commit_signed_create(&CreateRequest {
            asset_id: candidate.asset_id,
            album_id,
            src: &original,
            import_timestamp: Some(import_timestamp),
            media_bucket: Some(month_dir_timestamp(&candidate.dir)),
            extra_unknown,
            opts: &opts,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
