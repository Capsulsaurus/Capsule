use std::path::{Path, PathBuf};

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailSize {
    Xs,
    S,
    M,
    L,
    Xl,
    O,
}

impl ThumbnailSize {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThumbnailSize::Xs => "xs",
            ThumbnailSize::S => "s",
            ThumbnailSize::M => "m",
            ThumbnailSize::L => "l",
            ThumbnailSize::Xl => "xl",
            ThumbnailSize::O => "o",
        }
    }
}

/// Returns the first two hex chars and next two hex chars of a UUID string (without hyphens).
pub fn uuid_shard(uuid: &Uuid) -> (String, String) {
    let hex = uuid.simple().to_string(); // 32 hex chars, no hyphens
    (hex[0..2].to_string(), hex[2..4].to_string())
}

fn shard_dirs(uuid: &Uuid) -> (String, String) {
    uuid_shard(uuid)
}

fn date_parts(capture_utc: Option<i64>) -> (u32, u8) {
    match capture_utc {
        Some(ts) => {
            let date = jiff::Timestamp::from_second(ts)
                .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .date();
            (
                u32::try_from(date.year()).unwrap_or(1970),
                u8::try_from(date.month()).unwrap_or(1),
            )
        }
        None => (1970, 1),
    }
}

/// `media/{YYYY}/{YYYY-MM}/`
pub fn media_dir(root: &Path, year: u32, month: u8) -> PathBuf {
    root.join("media")
        .join(format!("{year:04}"))
        .join(format!("{year:04}-{month:02}"))
}

/// `media/{YYYY}/{YYYY-MM}/{uuid}.{ext}` where date comes from capture_utc
pub fn media_path(root: &Path, uuid: &Uuid, ext: &str, capture_utc: Option<i64>) -> PathBuf {
    let (year, month) = date_parts(capture_utc);
    media_dir(root, year, month).join(format!("{}.{}", uuid.simple(), ext))
}

/// `media/{YYYY}/{YYYY-MM}/{uuid}.cbor`
pub fn sidecar_path(root: &Path, uuid: &Uuid, _ext: &str, capture_utc: Option<i64>) -> PathBuf {
    let (year, month) = date_parts(capture_utc);
    media_dir(root, year, month).join(format!("{}.cbor", uuid.simple()))
}

/// `media/{YYYY}/{YYYY-MM}/{uuid}.receipts.cbor` — the client's custody-receipt log for the
/// asset, appended beside the provenance chain (SSoT: Storage Verification — Custody Receipts).
/// Evidentiary, not a cache: it is included verbatim in the backup artifact.
pub fn receipts_path(root: &Path, uuid: &Uuid, capture_utc: Option<i64>) -> PathBuf {
    let (year, month) = date_parts(capture_utc);
    media_dir(root, year, month).join(format!("{}.receipts.cbor", uuid.simple()))
}

/// `media/{YYYY}/{YYYY-MM}/{uuid}.attestations.cbor` — the client's signed storage-attestation
/// log for the asset, kept beside its custody receipts (SSoT: Transfer — Durability: Signed
/// Storage Attestation). Evidence, not a cache: it is included verbatim in the backup artifact.
pub fn attestations_path(root: &Path, uuid: &Uuid, capture_utc: Option<i64>) -> PathBuf {
    let (year, month) = date_parts(capture_utc);
    media_dir(root, year, month).join(format!("{}.attestations.cbor", uuid.simple()))
}

/// The `.cbor` files the library keeps under `media/` that are not an asset's sidecar: the
/// provenance, receipt and attestation logs beside it, and the derivative bundle the lifecycle
/// writes as `derivatives/{uuid}.derivatives.cbor`. They share the sidecar's suffix, so every
/// walk that reads `{uuid}.cbor` sidecars must skip them by name.
pub(crate) const SIBLING_LOG_SUFFIXES: [&str; 4] = [
    ".provenance.cbor",
    ".receipts.cbor",
    ".attestations.cbor",
    ".derivatives.cbor",
];

/// Whether a file name under `media/` names an asset's sidecar: exactly `{uuid}.cbor`, never one
/// of the [sibling logs](SIBLING_LOG_SUFFIXES). The one filter `rebuild_index`, `Workspace::open`
/// and the unanchored-sidecar scan (`lifecycle/unanchored.rs`) share, so a new sibling cannot be
/// added to one walk and parsed as a sidecar by another.
pub(crate) fn is_sidecar_file_name(name: &str) -> bool {
    // Exact-case on purpose: the library writes every one of these names itself, lowercase.
    Path::new(name).extension().is_some_and(|ext| ext == "cbor")
        && !SIBLING_LOG_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

/// `index/thumbnails/{size}/{s1}/{s2}/{uuid}.{format}` where format is "jxl" or "webp"
pub fn thumbnail_path(root: &Path, uuid: &Uuid, size: ThumbnailSize) -> PathBuf {
    let (s1, s2) = shard_dirs(uuid);
    root.join("index")
        .join("thumbnails")
        .join(size.as_str())
        .join(&s1)
        .join(&s2)
        .join(format!("{}", uuid.simple()))
    // Note: caller appends .jxl or .webp as needed
}

/// `index/meta/{s1}/{s2}/{uuid}.meta.cbor`
pub fn meta_cache_path(root: &Path, uuid: &Uuid) -> PathBuf {
    let (s1, s2) = shard_dirs(uuid);
    root.join("index")
        .join("meta")
        .join(&s1)
        .join(&s2)
        .join(format!("{}.meta.cbor", uuid.simple()))
}

/// `index/transcodes/h264/{s1}/{s2}/{uuid}.mp4`
pub fn transcode_h264_path(root: &Path, uuid: &Uuid) -> PathBuf {
    let (s1, s2) = shard_dirs(uuid);
    root.join("index")
        .join("transcodes")
        .join("h264")
        .join(&s1)
        .join(&s2)
        .join(format!("{}.mp4", uuid.simple()))
}

/// `index/transcodes/live/{s1}/{s2}/{uuid}.mov`
pub fn transcode_live_path(root: &Path, uuid: &Uuid) -> PathBuf {
    let (s1, s2) = shard_dirs(uuid);
    root.join("index")
        .join("transcodes")
        .join("live")
        .join(&s1)
        .join(&s2)
        .join(format!("{}.mov", uuid.simple()))
}

/// `.library/trash/{uuid}.{ext}`
pub fn trash_path(root: &Path, uuid: &Uuid, ext: &str) -> PathBuf {
    root.join(".library")
        .join("trash")
        .join(format!("{}.{}", uuid.simple(), ext))
}

// `tmp_path` encodes no library layout, so it lives in `crate::utils::paths` where
// `capsule_core::sidecar` can reach it without depending on `library`. Re-exported here so
// `library::paths::tmp_path` and `library::tmp_path` keep resolving for existing callers.
pub use crate::utils::paths::tmp_path;

#[cfg(test)]
mod tests;
