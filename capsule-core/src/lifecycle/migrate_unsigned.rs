//! Migrate the unsigned pre-signed-path sidecars into signed assets (slice `S-D24`).
//!
//! A library written before the signed path existed holds, beside each original, a flat
//! unsigned CBOR map (`version: 1`, text keys) with no provenance chain, no sealed metadata
//! blob, and no album key material. [`Workspace::open`] anchors on provenance chains, so such
//! an asset is invisible to every signed operation — it cannot be verified, exported, or
//! uploaded — and a keyless [`rebuild_index`](crate::library::rebuild_index) cannot admit it
//! either: it holds no album write capability and cannot sign a sidecar or a manifest.
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

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use ciborium::value::Value;
use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;
use walkdir::WalkDir;

use super::import::CreateRequest;
use super::open::{month_dir_timestamp, original_extension};
use super::{LifecycleError, Result, SidecarEnrichment, SignedImportOptions, Workspace};
use crate::crypto::hash;
use crate::crypto::provenance::action::Action;
use crate::domain::{GpsDatum, StackType};
use crate::sidecar::shape::{self, SidecarShape};
use crate::sidecar::sidecar_v1::{
    Gps, GpsSource, SIDECAR_SCHEMA_V1, SidecarV1, StackMembership, StackRole,
};

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
    /// names an album this workspace holds no write capability for. Must already exist and
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
    /// The subset of [`migrated`](Self::migrated) whose legacy record said `is_deleted`, now
    /// carrying a signed `delete` record.
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
/// `.provenance.cbor` / `.receipts.cbor` logs. The same filter `rebuild_index` and the add-id
/// sweep apply.
fn sidecar_files(root: &Path) -> Vec<PathBuf> {
    let media = root.join("media");
    if !media.exists() {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = WalkDir::new(&media)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().is_file())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.ends_with(".cbor")
                && !name.ends_with(".provenance.cbor")
                && !name.ends_with(".receipts.cbor")
        })
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
    /// rebuild the index so stack rows are reconstructed uniformly (slice `S-D24`).
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
    /// of two or more admitted members gets a deterministic stack id — an RFC 9562 v8 (custom)
    /// UUID over `SHA-256(domain ‖ user_id ‖ "{method}:{key}")` — written into each member's
    /// signed `stack_membership` register at create. A singleton gets no stack; its hint
    /// survives in the fold.
    ///
    /// The album is the legacy `album_id` when this workspace holds write capability for it,
    /// else `opts.fallback_album`, which must already exist and be writable — the verb never
    /// mints an album, and a read-only fallback is a typed
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

        let mut report = UnsignedMigrationReport {
            // Assets a previous run admitted but whose `delete` record never landed.
            trashed: self.reconcile_legacy_trash(opts.trash_retain_days)?,
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
    /// left exactly as they left it.
    fn reconcile_legacy_trash(&mut self, retain_days: i64) -> Result<Vec<Uuid>> {
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
        for id in &owed {
            tracing::info!(
                asset_id = %id,
                "unsigned migration: applying the delete record an interrupted run owed"
            );
            self.soft_delete(id, retain_days)?;
        }
        Ok(owed)
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

    /// The legacy `album_id` when it parses and this workspace can write into it, else the
    /// fallback.
    fn resolve_legacy_album(&self, legacy: Option<&str>, opts: &UnsignedMigrationOptions) -> Uuid {
        let held = legacy
            .and_then(|s| Uuid::parse_str(s).ok())
            .filter(|id| self.albums.get(id).is_some_and(|a| a.write_tier.is_some()));
        if let Some(id) = held {
            return id;
        }
        if let Some(legacy) = legacy {
            tracing::debug!(
                legacy_album = legacy,
                fallback_album = %opts.fallback_album,
                "unsigned migration: legacy album not held or not writable; using the fallback"
            );
        }
        opts.fallback_album
    }

    /// Group the admitted members by `(detection_method, detection_key)` and derive one
    /// deterministic stack id per group of two or more (`legacy_stack_id`), so an
    /// interrupted or repeated run lands every member under the same id.
    fn legacy_stack_memberships(
        &self,
        candidates: &[Candidate],
    ) -> BTreeMap<Uuid, StackMembership> {
        let mut groups: BTreeMap<(String, String), Vec<(Uuid, &LegacyStackHint)>> = BTreeMap::new();
        for c in candidates {
            if let Some(hint) = &c.record.stack_hint {
                groups
                    .entry((hint.detection_method.clone(), hint.detection_key.clone()))
                    .or_default()
                    .push((c.asset_id, hint));
            }
        }
        let user_id = self.account.user_id;
        let mut out = BTreeMap::new();
        for ((method, key), mut members) in groups {
            if members.len() < 2 {
                continue;
            }
            members.sort_by_key(|(id, _)| *id);
            let stack_id = legacy_stack_id(&user_id, &method, &key);
            let stack_type = members[0].1.stack_type;
            for (index, (asset_id, hint)) in members.iter().enumerate() {
                let role = match hint.member_role.as_str() {
                    "primary" => StackRole::Primary,
                    "proxy" => StackRole::Proxy,
                    _ => StackRole::Member,
                };
                out.insert(
                    *asset_id,
                    StackMembership {
                        stack_id,
                        stack_type,
                        role,
                        member_index: Some(index as u32),
                    },
                );
            }
            tracing::debug!(
                stack_id = %stack_id,
                method = %method,
                key = %key,
                members = members.len(),
                "unsigned migration: stack derived from legacy hints"
            );
        }
        out
    }

    /// Copy the legacy bytes to `.library/quarantine/{uuid}.cbor` and write the sibling
    /// `.reason.json`, before any signed write. A resumed candidate's copy is already there.
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
            gps: record.gps.map(|(lat, lon)| Gps {
                lat,
                lon,
                source: GpsSource::Exif,
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
mod tests {
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::cbor;
    use crate::crypto::primitives::Argon2Params;
    use crate::crypto::verify_asset::VerifyOutcome;
    use crate::library::rebuild_index;
    use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

    fn fast_params() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    fn int(i: i64) -> Value {
        Value::Integer(i.into())
    }

    /// A legacy unsigned record exactly as the retired serializer wrote it: text keys, the
    /// required fields, `version: 1`, plus `extra` entries appended.
    fn legacy_map(asset_id: Uuid, original: &[u8], extra: Vec<(&str, Value)>) -> Value {
        let mut entries = vec![
            ("version", int(1)),
            ("uuid", text(&asset_id.to_string())),
            ("asset_type", text("photo")),
            ("original_filename", text("IMG_0001.JPG")),
            ("import_timestamp", int(1_720_000_000)),
            ("modified_timestamp", int(1_720_000_000)),
            ("hash_sha256", text(&hash::hash_bytes(original).to_hex())),
            ("file_size", int(original.len() as i64)),
            ("is_deleted", Value::Bool(false)),
            ("rating", int(0)),
            ("tags", Value::Array(vec![])),
            ("import_mode", text("copy")),
            ("importer_version", text("0.1.0")),
            ("rawshift_version", text("0.1.0")),
        ];
        entries.extend(extra);
        Value::Map(entries.into_iter().map(|(k, v)| (text(k), v)).collect())
    }

    fn stack_hint(key: &str, role: &str) -> Value {
        Value::Map(vec![
            (text("detection_key"), text(key)),
            (text("detection_method"), text("filename_stem")),
            (text("member_role"), text(role)),
            (text("stack_type"), text("raw_jpeg")),
        ])
    }

    /// Write a legacy asset — the original `{uuid}.jpg` and its unsigned `{uuid}.cbor` —
    /// into `media/1970/1970-01` (where a `None` capture time landed). Returns the sidecar
    /// path and the legacy bytes.
    fn write_legacy(
        root: &Path,
        asset_id: Uuid,
        original: &[u8],
        extra: Vec<(&str, Value)>,
    ) -> (PathBuf, Vec<u8>) {
        let dir = root.join("media/1970/1970-01");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{}.jpg", asset_id.simple())), original).unwrap();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&legacy_map(asset_id, original, extra), &mut bytes).unwrap();
        let path = dir.join(format!("{}.cbor", asset_id.simple()));
        fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }

    fn opts(fallback: Uuid) -> UnsignedMigrationOptions {
        UnsignedMigrationOptions {
            fallback_album: fallback,
            trash_retain_days: 30,
        }
    }

    /// Three legacy assets, one of them the "rich" one (rating, tags, GPS) and one carrying a
    /// key no build ever defined. Returns `(rich, future, plain)`.
    fn three_legacy_assets(root: &Path) -> (Uuid, Uuid, Uuid) {
        let rich = Uuid::from_u128(0xA1);
        let future = Uuid::from_u128(0xA2);
        let plain = Uuid::from_u128(0xA3);
        write_legacy(
            root,
            rich,
            b"\xFF\xD8\xFF rich legacy asset",
            vec![
                ("rating", int(4)),
                ("tags", Value::Array(vec![text("trip"), text("2024")])),
                ("gps_lat", Value::Float(48.8584)),
                ("gps_lon", Value::Float(2.2945)),
                ("capture_utc", int(1_700_000_000)),
            ],
        );
        write_legacy(
            root,
            future,
            b"\xFF\xD8\xFF legacy asset from the future",
            vec![("future_field", text("kept verbatim"))],
        );
        write_legacy(root, plain, b"\xFF\xD8\xFF plain legacy asset", vec![]);
        (rich, future, plain)
    }

    fn read_signed(ws: &Workspace, id: &Uuid) -> SidecarV1 {
        let bytes = fs::read(ws.sidecar_path(ws.asset(id).unwrap())).unwrap();
        SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap()
    }

    // ── T1: the acceptance case ─────────────────────────────────────────────

    /// **The `S-D24` acceptance case.** After the verb, every legacy asset is a signed asset:
    /// `verify_asset` accepts it, its sidecar verifies under the user IK, its chain, sealed
    /// metadata blob, and index row exist, and the legacy rating, tags, GPS, and capture time
    /// are carried into their signed homes.
    #[test]
    fn migrated_assets_verify_and_carry_their_legacy_metadata() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let (rich, future, plain) = three_legacy_assets(lib.path());

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.migrated, vec![rich, future, plain]);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert!(report.trashed.is_empty());
        assert!(report.stacks.is_empty());

        let ik = ws.user_ik_public();
        for id in [rich, future, plain] {
            assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept, "{id}");
            let asset = ws.asset(&id).unwrap();
            assert_eq!(asset.album_id, album);
            assert!(
                asset.sidecar.verify(&ik),
                "sidecar signed under the user IK"
            );
            assert!(ws.provenance_path(asset).exists());
            assert!(ws.metadata_blob_path(asset).exists());
            assert!(!asset.metadata_blob.is_empty());
            assert_eq!(asset.chain.records().len(), 1, "one create record");
            assert!(
                ws.db().find_by_uuid(&id.to_string()).unwrap().is_some(),
                "index row written"
            );
            // The files stayed in their legacy bucket.
            assert_eq!(asset.capture_utc, 0, "1970-01 bucket");
            assert!(ws.media_path(asset).exists());
            // The sidecar's import time is the legacy import time, not now.
            assert_eq!(asset.sidecar.import_timestamp, "2024-07-03T09:46:40Z");
        }

        // The rich record's metadata landed in the signed registers.
        let sidecar = read_signed(&ws, &rich);
        assert_eq!(sidecar.rating.get(), Some(&4));
        let tags = sidecar.tags_user.value();
        assert!(tags.contains("trip") && tags.contains("2024"));
        let gps = sidecar.gps.expect("legacy GPS carried");
        assert_eq!((gps.lat, gps.lon), (48.8584, 2.2945));
        // The legacy capture time is the sidecar's capture timestamp (these bytes carry no
        // EXIF, so the fold wins over the import clock).
        assert_eq!(sidecar.capture_timestamp, "2023-11-14T22:13:20Z");
        assert_eq!(
            ws.db()
                .find_by_uuid(&rich.to_string())
                .unwrap()
                .unwrap()
                .rating,
            4
        );
        assert_eq!(ws.db().tags_for(&rich.to_string()).unwrap().len(), 2);
        // A record with no capture time falls back to its legacy import time, never `now`.
        assert_eq!(
            read_signed(&ws, &plain).capture_timestamp,
            "2024-07-03T09:46:40Z"
        );
    }

    // ── T2: the never-strip tripwire ────────────────────────────────────────

    /// **The never-strip tripwire.** The whole legacy map — including a key no build has ever
    /// defined — rides in the signed sidecar's `_unknown` under [`LEGACY_FOLD_KEY`], and the
    /// signature covers it: the fold is byte-equal to the canonicalised legacy map, and
    /// removing the fold (or one key inside it) fails `verify`. A fold that dropped
    /// `future_field` would fail the equality assertion below.
    #[test]
    fn the_legacy_map_is_folded_verbatim_and_covered_by_the_signature() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let (_, future, _) = three_legacy_assets(lib.path());
        let legacy_bytes = fs::read(
            lib.path()
                .join("media/1970/1970-01")
                .join(format!("{}.cbor", future.simple())),
        )
        .unwrap();

        ws.migrate_unsigned_sidecars(&opts(album)).unwrap();

        let sidecar = read_signed(&ws, &future);
        let fold = sidecar
            .unknown
            .get(LEGACY_FOLD_KEY)
            .expect("the legacy record is folded into _unknown");
        // Byte-equal to the legacy map, canonically re-encoded.
        assert_eq!(
            cbor::value_to_canonical_vec(fold),
            cbor::canonicalize(&legacy_bytes).unwrap(),
            "the fold is the entire legacy map, verbatim"
        );
        let Value::Map(entries) = fold else {
            panic!("the fold is a map");
        };
        assert!(
            entries.contains(&(text("future_field"), text("kept verbatim"))),
            "a key no build defined survives inside the fold"
        );
        assert!(
            entries.iter().any(|(k, _)| *k == text("original_filename")),
            "fields with no signed home of their own survive inside the fold"
        );

        // The signature covers the fold: stripping it, or a key inside it, invalidates it.
        let ik = ws.user_ik_public();
        assert!(sidecar.verify(&ik));
        let mut stripped = sidecar.clone();
        stripped.unknown.remove(LEGACY_FOLD_KEY);
        assert!(
            !stripped.verify(&ik),
            "stripping the fold breaks the signature"
        );
        let mut trimmed = sidecar.clone();
        let Some(Value::Map(inner)) = trimmed.unknown.get_mut(LEGACY_FOLD_KEY) else {
            panic!("fold present")
        };
        inner.retain(|(k, _)| *k != text("future_field"));
        assert!(
            !trimmed.verify(&ik),
            "dropping one legacy key from the fold breaks the signature"
        );
    }

    // ── T3: bytes preserved ─────────────────────────────────────────────────

    /// The legacy bytes survive verbatim in quarantine with a reason file, and are still there
    /// after a reopen (the startup scrub deletes only `.tmp` files). The original is signed
    /// where it lies: its file is not rewritten, which its unchanged mtime proves.
    #[test]
    fn legacy_bytes_are_preserved_verbatim_in_quarantine() {
        use std::time::{Duration, SystemTime};

        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let id = Uuid::from_u128(0xB1);
        let (sidecar_path, legacy_bytes) =
            write_legacy(lib.path(), id, b"\xFF\xD8\xFF preserved", vec![]);
        let original = lib
            .path()
            .join("media/1970/1970-01")
            .join(format!("{}.jpg", id.simple()));
        let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(&original)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        ws.migrate_unsigned_sidecars(&opts(album)).unwrap();

        assert_eq!(
            fs::metadata(&original).unwrap().modified().unwrap(),
            long_ago,
            "the original is signed in place, never rewritten over itself"
        );
        assert_eq!(fs::read(&original).unwrap(), b"\xFF\xD8\xFF preserved");

        let quarantine = lib.path().join(".library/quarantine");
        let twin = quarantine.join(format!("{}.cbor", id.simple()));
        assert_eq!(fs::read(&twin).unwrap(), legacy_bytes, "byte-equal copy");
        let reason: serde_json::Value = serde_json::from_slice(
            &fs::read(quarantine.join(format!("{}.reason.json", id.simple()))).unwrap(),
        )
        .unwrap();
        assert_eq!(reason["reason"], QUARANTINE_REASON);
        assert_eq!(
            reason["sha256_of_legacy_bytes"],
            hash::hash_bytes(&legacy_bytes).to_hex()
        );
        assert!(reason["migrated_at"].is_string());
        // The signed sidecar was written over the legacy one, in place.
        assert_ne!(fs::read(&sidecar_path).unwrap(), legacy_bytes);
        assert!(SidecarV1::from_canonical_slice(&fs::read(&sidecar_path).unwrap(), 1).is_ok());

        drop(ws);
        let _ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert_eq!(
            fs::read(&twin).unwrap(),
            legacy_bytes,
            "a reopen leaves it alone"
        );
    }

    // ── T4: idempotent ──────────────────────────────────────────────────────

    /// A second run migrates nothing and changes no bytes; a second rebuild yields the same
    /// rows and no duplicate stack members.
    #[test]
    fn a_second_run_is_a_no_op() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let primary = Uuid::from_u128(0xC1);
        let raw = Uuid::from_u128(0xC2);
        write_legacy(
            lib.path(),
            primary,
            b"\xFF\xD8\xFF stack primary",
            vec![("stack_hint", stack_hint("img_0042", "primary"))],
        );
        write_legacy(
            lib.path(),
            raw,
            b"\xFF\xD8\xFF stack raw",
            vec![("stack_hint", stack_hint("img_0042", "raw"))],
        );

        let first = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(first.migrated.len(), 2);
        assert_eq!(first.stacks.len(), 1);
        let snapshot = |ws: &Workspace| {
            [primary, raw].map(|id| {
                let asset = ws.asset(&id).unwrap();
                (
                    fs::read(ws.sidecar_path(asset)).unwrap(),
                    fs::read(ws.provenance_path(asset)).unwrap(),
                    asset.chain.records().len(),
                )
            })
        };
        let before = snapshot(&ws);
        let rows_before = ws.db().query_timeline(0, 100).unwrap();

        let second = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(
            second,
            UnsignedMigrationReport::default(),
            "nothing left to do"
        );
        assert_eq!(snapshot(&ws), before, "no bytes changed");
        assert!(ws.unmigrated_sidecars().is_empty());

        rebuild_index(&ws.library).unwrap();
        assert_eq!(ws.db().query_timeline(0, 100).unwrap(), rows_before);
        let stack_id = first.stacks[0];
        assert_eq!(
            ws.db()
                .list_stack_members(&stack_id.to_string())
                .unwrap()
                .len(),
            2,
            "no duplicate stack members after repeated rebuilds"
        );
    }

    // ── T5: the open outcome, before and after ──────────────────────────────

    /// Before the verb, `Workspace::open` still succeeds on a library holding unsigned
    /// sidecars — the signed asset is restored and verifies — and reports the legacy files
    /// through `unmigrated_sidecars`. After the verb and a reopen, nothing is left to report
    /// and every asset verifies.
    #[test]
    fn open_reports_unmigrated_sidecars_until_the_verb_runs() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("signed.jpg");
        fs::write(&img, b"\xFF\xD8\xFF a signed asset").unwrap();

        let (album, signed) = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Imports").unwrap();
            (album, ws.import_asset(album, &img).unwrap())
        };
        let (rich, future, plain) = three_legacy_assets(lib.path());

        let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        let unmigrated = ws.unmigrated_sidecars();
        assert_eq!(unmigrated.len(), 3);
        let mut ids: Vec<Uuid> = unmigrated.iter().filter_map(|u| u.asset_id).collect();
        ids.sort();
        assert_eq!(ids, vec![rich, future, plain]);
        assert!(
            unmigrated
                .iter()
                .all(|u| u.shape == UnmigratedShape::LegacyUnsigned)
        );
        assert_eq!(
            ws.asset_ids(),
            vec![signed],
            "only the signed asset is restored"
        );
        assert_eq!(ws.verify(&signed).unwrap(), VerifyOutcome::Accept);

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.migrated.len(), 3);
        assert!(ws.unmigrated_sidecars().is_empty());
        drop(ws);

        let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert!(ws.unmigrated_sidecars().is_empty());
        let mut all = ws.asset_ids();
        all.sort();
        let mut expected = vec![signed, rich, future, plain];
        expected.sort();
        assert_eq!(all, expected);
        for id in all {
            assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept, "{id}");
        }
    }

    // ── T6: trash carry-over ────────────────────────────────────────────────

    /// A legacy `is_deleted: true` becomes a signed `delete` record with a retention window:
    /// the chain is Create then Delete, the asset is in Recently Deleted and out of the
    /// timeline.
    #[test]
    fn a_deleted_legacy_asset_lands_in_trash() {
        use crate::crypto::provenance::action::Action;

        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let gone = Uuid::from_u128(0xD1);
        let kept = Uuid::from_u128(0xD2);
        write_legacy(
            lib.path(),
            gone,
            b"\xFF\xD8\xFF trashed legacy asset",
            vec![
                ("is_deleted", Value::Bool(true)),
                ("deleted_at", int(1_720_100_000)),
            ],
        );
        write_legacy(lib.path(), kept, b"\xFF\xD8\xFF kept legacy asset", vec![]);

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.trashed, vec![gone]);

        let actions: Vec<Action> = ws
            .asset(&gone)
            .unwrap()
            .chain
            .records()
            .iter()
            .map(|r| r.manifest.core.action)
            .collect();
        assert_eq!(actions, vec![Action::Create, Action::Delete]);
        let delete = &ws.asset(&gone).unwrap().chain.records()[1].manifest.core;
        assert!(
            delete.retention_until.is_some(),
            "the delete carries retention"
        );
        assert_eq!(ws.verify(&gone).unwrap(), VerifyOutcome::Accept);

        let timeline = ws.db().query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].uuid, kept.to_string());
        let trash = ws.db().query_trash(0, 100).unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].uuid, gone.to_string());
    }

    // ── T7: stack carry-over ────────────────────────────────────────────────

    /// Two legacy members sharing a `(filename_stem, img_0042)` hint land under one
    /// deterministic stack id, written into each signed `stack_membership` register at
    /// create: the timeline collapses to the primary, both are in `stack_members`, and the id
    /// is a pure function of the user id and the group key.
    #[test]
    fn legacy_stack_hints_become_one_signed_stack() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let primary = Uuid::from_u128(0xE1);
        let raw = Uuid::from_u128(0xE2);
        let loner = Uuid::from_u128(0xE3);
        write_legacy(
            lib.path(),
            primary,
            b"\xFF\xD8\xFF stack primary",
            vec![("stack_hint", stack_hint("img_0042", "primary"))],
        );
        write_legacy(
            lib.path(),
            raw,
            b"\xFF\xD8\xFF stack raw",
            vec![("stack_hint", stack_hint("img_0042", "raw"))],
        );
        // A hint with no partner: no stack, the hint survives only in the fold.
        write_legacy(
            lib.path(),
            loner,
            b"\xFF\xD8\xFF lonely hint",
            vec![("stack_hint", stack_hint("img_0099", "primary"))],
        );

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        let expected = legacy_stack_id(&ws.user_id(), "filename_stem", "img_0042");
        assert_eq!(
            report.stacks,
            vec![expected],
            "a pure function of user id + key"
        );
        assert_eq!(expected.get_version_num(), 8, "an RFC 9562 custom (v8) id");

        for (id, role, index) in [
            (primary, StackRole::Primary, 0),
            (raw, StackRole::Member, 1),
        ] {
            let sidecar = read_signed(&ws, &id);
            let membership = sidecar
                .stack_membership
                .get()
                .and_then(Option::as_ref)
                .expect("the register is written at create");
            assert_eq!(membership.stack_id, expected);
            assert_eq!(membership.role, role);
            assert_eq!(membership.member_index, Some(index));
            assert_eq!(membership.stack_type, StackType::RawJpeg);
            assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
        }
        assert_eq!(read_signed(&ws, &loner).stack_membership.get(), None);

        let timeline = ws.db().query_timeline(0, 100).unwrap();
        let mut shown: Vec<String> = timeline.iter().map(|r| r.uuid.clone()).collect();
        shown.sort();
        let mut want = vec![primary.to_string(), loner.to_string()];
        want.sort();
        assert_eq!(shown, want, "the raw member is collapsed under the primary");
        assert_eq!(
            ws.db()
                .list_stack_members(&expected.to_string())
                .unwrap()
                .len(),
            2
        );

        // Determinism across libraries: the same user migrating a copy derives the same id.
        let copy = TempDir::new().unwrap();
        let mut ws2 = fast_workspace(copy.path());
        let album2 = ws2.create_album("Imports").unwrap();
        write_legacy(
            copy.path(),
            primary,
            b"\xFF\xD8\xFF stack primary",
            vec![("stack_hint", stack_hint("img_0042", "primary"))],
        );
        write_legacy(
            copy.path(),
            raw,
            b"\xFF\xD8\xFF stack raw",
            vec![("stack_hint", stack_hint("img_0042", "raw"))],
        );
        let report2 = ws2.migrate_unsigned_sidecars(&opts(album2)).unwrap();
        assert_eq!(
            report2.stacks,
            vec![legacy_stack_id(&ws2.user_id(), "filename_stem", "img_0042")]
        );
        assert_ne!(
            report2.stacks, report.stacks,
            "a different user derives a different id"
        );
    }

    // ── T8: refusals ────────────────────────────────────────────────────────

    /// A missing original or a hash mismatch is refused with nothing written — the sidecar
    /// bytes, the quarantine directory, and the workspace's assets are all untouched — while
    /// an unknown legacy `album_id` lands the asset in the fallback album.
    #[test]
    fn refusals_write_nothing_and_unknown_albums_fall_back() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();

        let orphan = Uuid::from_u128(0xF1);
        let (orphan_path, orphan_bytes) =
            write_legacy(lib.path(), orphan, b"\xFF\xD8\xFF orphaned", vec![]);
        fs::remove_file(
            lib.path()
                .join("media/1970/1970-01")
                .join(format!("{}.jpg", orphan.simple())),
        )
        .unwrap();

        let corrupt = Uuid::from_u128(0xF2);
        let (corrupt_path, corrupt_bytes) =
            write_legacy(lib.path(), corrupt, b"\xFF\xD8\xFF as recorded", vec![]);
        fs::write(
            lib.path()
                .join("media/1970/1970-01")
                .join(format!("{}.jpg", corrupt.simple())),
            b"\xFF\xD8\xFF silently altered",
        )
        .unwrap();

        let stray = Uuid::from_u128(0xF3);
        write_legacy(
            lib.path(),
            stray,
            b"\xFF\xD8\xFF unknown album",
            vec![("album_id", text(&Uuid::from_u128(0xBAD).to_string()))],
        );

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.migrated, vec![stray]);
        assert_eq!(ws.asset(&stray).unwrap().album_id, album, "fallback album");

        let mut skips: Vec<(PathBuf, MigrationSkip)> = report.skipped.clone();
        skips.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(skips.len(), 2);
        assert_eq!(
            skips[0],
            (orphan_path.clone(), MigrationSkip::OriginalMissing(orphan))
        );
        assert!(matches!(
            &skips[1].1,
            MigrationSkip::HashMismatch { asset_id, .. } if *asset_id == corrupt
        ));
        assert_eq!(skips[1].0, corrupt_path);

        // Nothing was written for either refusal.
        assert_eq!(fs::read(&orphan_path).unwrap(), orphan_bytes);
        assert_eq!(fs::read(&corrupt_path).unwrap(), corrupt_bytes);
        assert!(ws.asset(&orphan).is_none());
        assert!(ws.asset(&corrupt).is_none());
        let quarantine = lib.path().join(".library/quarantine");
        assert!(
            !quarantine
                .join(format!("{}.cbor", orphan.simple()))
                .exists()
        );
        assert!(
            !quarantine
                .join(format!("{}.cbor", corrupt.simple()))
                .exists()
        );
        // They are still reported as unmigrated.
        assert_eq!(ws.unmigrated_sidecars().len(), 2);
    }

    /// A read-only fallback album (recovered from a backup: content keys, no write
    /// capability) is a typed refusal before anything is written.
    #[test]
    fn a_read_only_fallback_album_is_refused_before_any_write() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        // Strip the write capability, exactly the shape `import_backup` restores.
        ws.albums.get_mut(&album).unwrap().write_tier = None;
        let id = Uuid::from_u128(0xF4);
        let (path, bytes) = write_legacy(lib.path(), id, b"\xFF\xD8\xFF read-only", vec![]);

        assert!(matches!(
            ws.migrate_unsigned_sidecars(&opts(album)),
            Err(LifecycleError::AlbumReadOnly(a)) if a == album
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!lib.path().join(".library/quarantine").exists());
        assert!(ws.asset(&id).is_none());

        // An album the workspace does not hold at all is `NotFound`, not a minted album.
        assert!(matches!(
            ws.migrate_unsigned_sidecars(&opts(Uuid::from_u128(0x404))),
            Err(LifecycleError::NotFound(_))
        ));
        assert_eq!(ws.albums().len(), 1);
    }

    /// A signed asset with the legacy id already restored is refused: migrating over it would
    /// replace a signed record with a re-derived one.
    #[test]
    fn a_colliding_signed_asset_is_refused() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("signed.jpg");
        fs::write(&img, b"\xFF\xD8\xFF a signed asset").unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let signed = ws.import_asset(album, &img).unwrap();
        // A legacy sidecar claiming the same id, in a different bucket.
        let (path, bytes) = write_legacy(lib.path(), signed, b"\xFF\xD8\xFF impostor", vec![]);

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert!(report.migrated.is_empty());
        assert_eq!(
            report.skipped,
            vec![(path.clone(), MigrationSkip::IdCollision(signed))]
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(ws.verify(&signed).unwrap(), VerifyOutcome::Accept);
    }

    // ── resumability ────────────────────────────────────────────────────────

    /// An interrupted run that wrote the signed sidecar but died before the chain leaves a
    /// signed sidecar with no `.provenance.cbor` and a quarantine twin. `open` reports it as
    /// `SignedWithoutChain`; the next run resumes from the quarantine copy and lands a
    /// verifying asset.
    #[test]
    fn an_interrupted_migration_resumes_from_quarantine() {
        let lib = TempDir::new().unwrap();
        let id = Uuid::from_u128(0x1A);
        let album = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Imports").unwrap();
            write_legacy(lib.path(), id, b"\xFF\xD8\xFF interrupted", vec![]);
            ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
            // Simulate the crash window: the chain never made it to disk.
            fs::remove_file(ws.provenance_path(ws.asset(&id).unwrap())).unwrap();
            album
        };

        let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert!(ws.asset(&id).is_none(), "no chain, not restored");
        assert_eq!(ws.unmigrated_sidecars().len(), 1);
        assert_eq!(
            ws.unmigrated_sidecars()[0].shape,
            UnmigratedShape::SignedWithoutChain { schema: 1 }
        );

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.migrated, vec![id]);
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
        assert!(ws.unmigrated_sidecars().is_empty());
        let sidecar = read_signed(&ws, &id);
        assert!(sidecar.unknown.contains_key(LEGACY_FOLD_KEY));
    }

    /// A migrated asset that was *edited* after migration and then lost its chain is not an
    /// interrupted create: redoing the create would discard the signed edit. It is refused as
    /// stranded and its sidecar bytes are left exactly as found.
    #[test]
    fn a_chainless_sidecar_carrying_a_later_write_is_not_resumed() {
        let lib = TempDir::new().unwrap();
        let id = Uuid::from_u128(0x1B);
        let (album, sidecar_path, edited_bytes) = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Imports").unwrap();
            write_legacy(lib.path(), id, b"\xFF\xD8\xFF edited later", vec![]);
            ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
            ws.tag_add(&id, "kept").unwrap();
            let path = ws.sidecar_path(ws.asset(&id).unwrap());
            fs::remove_file(ws.provenance_path(ws.asset(&id).unwrap())).unwrap();
            let bytes = fs::read(&path).unwrap();
            (album, path, bytes)
        };

        let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert!(report.migrated.is_empty());
        assert_eq!(
            report.skipped,
            vec![(sidecar_path.clone(), MigrationSkip::Stranded(id))]
        );
        assert_eq!(
            fs::read(&sidecar_path).unwrap(),
            edited_bytes,
            "left as found"
        );
        assert!(
            lib.path()
                .join(".library/quarantine")
                .join(format!("{}.cbor", id.simple()))
                .exists()
        );
    }

    /// A torn write of the signed sidecar leaves bytes that are neither shape. With the
    /// quarantine copy present the run resumes from it; without one, the file is reported as
    /// unknown and left alone.
    #[test]
    fn a_torn_sidecar_resumes_from_quarantine_or_is_reported_unknown() {
        let lib = TempDir::new().unwrap();
        let torn = Uuid::from_u128(0x1C);
        let album = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Imports").unwrap();
            write_legacy(lib.path(), torn, b"\xFF\xD8\xFF torn write", vec![]);
            ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
            let asset = ws.asset(&torn).unwrap();
            let sidecar = ws.sidecar_path(asset);
            let bytes = fs::read(&sidecar).unwrap();
            fs::write(&sidecar, &bytes[..bytes.len() / 2]).unwrap();
            fs::remove_file(ws.provenance_path(asset)).unwrap();
            album
        };
        // And a garbage sidecar with no twin at all.
        let garbage = Uuid::from_u128(0x1D);
        let garbage_path = lib
            .path()
            .join("media/1970/1970-01")
            .join(format!("{}.cbor", garbage.simple()));
        fs::write(&garbage_path, b"\xFF\x00 not cbor").unwrap();

        let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert!(
            ws.unmigrated_sidecars()
                .iter()
                .all(|u| u.shape == UnmigratedShape::Unknown)
        );
        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.migrated, vec![torn]);
        assert_eq!(ws.verify(&torn).unwrap(), VerifyOutcome::Accept);
        assert_eq!(
            report.skipped,
            vec![(garbage_path.clone(), MigrationSkip::UnknownShape)]
        );
        assert_eq!(fs::read(&garbage_path).unwrap(), b"\xFF\x00 not cbor");
    }

    /// A crash between the create and the `delete` record leaves an admitted asset whose fold
    /// says `is_deleted` but whose chain does not. The next run applies the delete it owes —
    /// and never re-deletes an asset the user has since restored from trash by hand.
    #[test]
    fn an_owed_delete_record_is_applied_on_the_next_run_but_a_restore_is_respected() {
        let lib = TempDir::new().unwrap();
        let owed = Uuid::from_u128(0x1E);
        let restored = Uuid::from_u128(0x1F);
        let album = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Imports").unwrap();
            for id in [owed, restored] {
                write_legacy(
                    lib.path(),
                    id,
                    &[b"\xFF\xD8\xFF deleted ".as_slice(), id.as_bytes()].concat(),
                    vec![("is_deleted", Value::Bool(true))],
                );
            }
            let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
            assert_eq!(report.trashed, vec![owed, restored]);
            // Simulate the crash window for `owed`: the chain holds the create only.
            let asset = ws.asset(&owed).unwrap();
            let create_only =
                cbor::to_canonical_vec(&vec![asset.chain.records()[0].clone()]).unwrap();
            fs::write(ws.provenance_path(asset), create_only).unwrap();
            // The user restores `restored` by hand.
            ws.restore(&restored).unwrap();
            album
        };

        let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert!(ws.unmigrated_sidecars().is_empty(), "both are anchored");
        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert!(report.migrated.is_empty());
        assert_eq!(report.trashed, vec![owed], "the owed delete, and only that");
        let actions = |ws: &Workspace, id: &Uuid| -> Vec<Action> {
            ws.asset(id)
                .unwrap()
                .chain
                .records()
                .iter()
                .map(|r| r.manifest.core.action)
                .collect()
        };
        assert_eq!(actions(&ws, &owed), vec![Action::Create, Action::Delete]);
        assert_eq!(
            actions(&ws, &restored),
            vec![Action::Create, Action::Delete, Action::TrashRestore],
            "a hand restore is left alone"
        );
        let trash: Vec<String> = ws
            .db()
            .query_trash(0, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.uuid)
            .collect();
        assert_eq!(trash, vec![owed.to_string()]);
        // Idempotent from here.
        assert_eq!(
            ws.migrate_unsigned_sidecars(&opts(album)).unwrap(),
            UnsignedMigrationReport::default()
        );
    }

    /// Ids and names are checked before anything is written: the same id in two month
    /// buckets admits one and refuses the other; a record whose `uuid` field does not name
    /// its file is refused; a stem that is not a UUID is refused; and a quarantine twin that
    /// holds *different* bytes is a conflict, not something to overwrite.
    #[test]
    fn duplicate_ids_mismatched_records_and_conflicting_twins_are_refused() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();

        // The same id in two buckets.
        let twice = Uuid::from_u128(0x2B);
        write_legacy(lib.path(), twice, b"\xFF\xD8\xFF first bucket", vec![]);
        let other_dir = lib.path().join("media/2024/2024-07");
        fs::create_dir_all(&other_dir).unwrap();
        let second_original = b"\xFF\xD8\xFF second bucket";
        fs::write(
            other_dir.join(format!("{}.jpg", twice.simple())),
            second_original,
        )
        .unwrap();
        let mut second = Vec::new();
        ciborium::ser::into_writer(&legacy_map(twice, second_original, vec![]), &mut second)
            .unwrap();
        let second_path = other_dir.join(format!("{}.cbor", twice.simple()));
        fs::write(&second_path, &second).unwrap();

        // A record naming a different uuid than its file.
        let misnamed = Uuid::from_u128(0x2C);
        let (misnamed_path, _) = write_legacy(
            lib.path(),
            misnamed,
            b"\xFF\xD8\xFF misnamed",
            vec![("uuid", text(&Uuid::from_u128(0x2CC).to_string()))],
        );

        // A stem that is not a uuid.
        let not_an_id = lib.path().join("media/1970/1970-01/not-an-asset-id.cbor");
        let mut junk = Vec::new();
        ciborium::ser::into_writer(&legacy_map(Uuid::from_u128(0x2D), b"x", vec![]), &mut junk)
            .unwrap();
        fs::write(&not_an_id, &junk).unwrap();

        // A quarantine twin holding different bytes.
        let conflicted = Uuid::from_u128(0x2E);
        let (conflicted_path, conflicted_bytes) =
            write_legacy(lib.path(), conflicted, b"\xFF\xD8\xFF conflicted", vec![]);
        let quarantine = lib.path().join(".library/quarantine");
        fs::create_dir_all(&quarantine).unwrap();
        fs::write(
            quarantine.join(format!("{}.cbor", conflicted.simple())),
            b"\xA1\x67version\x01",
        )
        .unwrap();

        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.migrated, vec![twice]);
        assert_eq!(
            ws.read_plaintext(&twice).unwrap(),
            b"\xFF\xD8\xFF first bucket",
            "the 1970 bucket sorts first"
        );
        let mut skips = report.skipped.clone();
        skips.sort_by(|a, b| a.0.cmp(&b.0));
        let mut want = vec![
            (second_path.clone(), MigrationSkip::IdCollision(twice)),
            (
                conflicted_path.clone(),
                MigrationSkip::QuarantineConflict(conflicted),
            ),
            (
                not_an_id.clone(),
                MigrationSkip::InvalidAssetId("not-an-asset-id".to_string()),
            ),
        ];
        want.sort_by(|a, b| a.0.cmp(&b.0));
        let misnamed_skip = skips
            .iter()
            .position(|(p, _)| *p == misnamed_path)
            .expect("the misnamed record is refused");
        assert!(
            matches!(&skips[misnamed_skip].1, MigrationSkip::Undecodable(m) if m.contains("does not name"))
        );
        skips.remove(misnamed_skip);
        assert_eq!(skips, want);
        assert_eq!(fs::read(&second_path).unwrap(), second, "untouched");
        assert_eq!(
            fs::read(&conflicted_path).unwrap(),
            conflicted_bytes,
            "untouched"
        );
        assert!(ws.asset(&misnamed).is_none());
        assert!(ws.asset(&conflicted).is_none());
    }

    /// A signed sidecar with no chain and no quarantine copy is nothing this verb can rebuild:
    /// reported as stranded, never touched.
    #[test]
    fn a_stranded_signed_sidecar_is_reported_not_touched() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("signed.jpg");
        fs::write(&img, b"\xFF\xD8\xFF loses its chain").unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        let chain = ws.provenance_path(ws.asset(&id).unwrap());
        let sidecar_path = ws.sidecar_path(ws.asset(&id).unwrap());
        fs::remove_file(&chain).unwrap();
        let bytes = fs::read(&sidecar_path).unwrap();
        drop(ws);

        let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(
            report.skipped,
            vec![(sidecar_path.clone(), MigrationSkip::Stranded(id))]
        );
        assert_eq!(fs::read(&sidecar_path).unwrap(), bytes);
    }

    // ── the private decoder ─────────────────────────────────────────────────

    #[test]
    fn the_legacy_decoder_projects_its_fields_and_keeps_the_map() {
        let id = Uuid::from_u128(0x2A);
        let map = legacy_map(
            id,
            b"bytes",
            vec![
                ("rating", int(3)),
                ("tags", Value::Array(vec![text("a")])),
                ("capture_timestamp", int(1_600_000_000)),
                ("capture_utc", Value::Null),
                ("gps_lat", Value::Float(1.5)),
                ("gps_lon", Value::Float(-2.5)),
                ("stack_hint", stack_hint("k", "raw")),
                ("album_id", text("not-a-uuid")),
                ("mystery", Value::Bytes(vec![1, 2, 3])),
            ],
        );
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&map, &mut bytes).unwrap();
        let record = LegacyRecord::decode(&bytes).unwrap();
        assert_eq!(record.uuid, id.to_string());
        assert_eq!(record.import_timestamp, 1_720_000_000);
        assert_eq!(record.rating, 3);
        assert_eq!(record.tags, vec!["a".to_string()]);
        assert_eq!(record.capture_utc, None, "null counts as absent");
        assert_eq!(record.capture_timestamp, Some(1_600_000_000));
        assert_eq!(record.gps, Some((1.5, -2.5)));
        assert_eq!(record.album_id.as_deref(), Some("not-a-uuid"));
        assert!(!record.is_deleted);
        let hint = record.stack_hint.as_ref().unwrap();
        assert_eq!(
            (hint.detection_key.as_str(), hint.member_role.as_str()),
            ("k", "raw")
        );
        assert_eq!(hint.detection_method, "filename_stem");
        assert_eq!(hint.stack_type, StackType::RawJpeg);
        assert_eq!(
            record.map, map,
            "the whole map is kept, unknown keys included"
        );
        // Precedence inside the fallback: capture_utc, then capture_timestamp, then import.
        assert_eq!(record.capture_fallback().as_second(), 1_600_000_000);

        // Required fields are required; a wrong type is an error, not a default.
        let mut missing = Vec::new();
        ciborium::ser::into_writer(
            &Value::Map(vec![(text("version"), int(1)), (text("uuid"), text("x"))]),
            &mut missing,
        )
        .unwrap();
        assert!(
            LegacyRecord::decode(&missing)
                .unwrap_err()
                .contains("hash_sha256")
        );
        let mut wrong = Vec::new();
        ciborium::ser::into_writer(
            &legacy_map(id, b"bytes", vec![("rating", text("four"))]),
            &mut wrong,
        )
        .unwrap();
        assert!(LegacyRecord::decode(&wrong).unwrap_err().contains("rating"));
        assert!(LegacyRecord::decode(b"garbage").is_err());
    }
}
