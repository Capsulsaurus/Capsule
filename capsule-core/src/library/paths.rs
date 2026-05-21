use chrono::{DateTime, Datelike};
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
            let dt = DateTime::from_timestamp(ts, 0).unwrap_or(DateTime::UNIX_EPOCH);
            (dt.year() as u32, dt.month() as u8)
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

/// Appends `.tmp` to any path
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_uuid() -> Uuid {
        // Use a fixed UUID for deterministic tests
        Uuid::parse_str("01956ef3-1234-7abc-9def-123456789abc").unwrap()
    }

    #[test]
    fn test_uuid_shard() {
        let uuid = test_uuid();
        let (s1, s2) = uuid_shard(&uuid);
        // simple() gives 32 hex chars; first 2 and next 2
        let hex = uuid.simple().to_string();
        assert_eq!(s1, &hex[0..2]);
        assert_eq!(s2, &hex[2..4]);
        assert_eq!(s1.len(), 2);
        assert_eq!(s2.len(), 2);
    }

    #[test]
    fn test_media_dir() {
        let root = Path::new("/lib");
        let path = media_dir(root, 2024, 7);
        assert_eq!(path, PathBuf::from("/lib/media/2024/2024-07"));
    }

    #[test]
    fn test_media_path_with_capture_utc() {
        let root = Path::new("/lib");
        let uuid = test_uuid();
        // 2024-07-15 -> ts = 1721001600 (approx)
        let ts = 1721001600i64;
        let path = media_path(root, &uuid, "jpg", Some(ts));
        let uuid_str = uuid.simple().to_string();
        assert!(path.to_str().unwrap().contains("media/2024/2024-07"));
        assert!(path.to_str().unwrap().contains(&format!("{uuid_str}.jpg")));
    }

    #[test]
    fn test_media_path_no_utc() {
        let root = Path::new("/lib");
        let uuid = test_uuid();
        let path = media_path(root, &uuid, "arw", None);
        assert!(path.to_str().unwrap().contains("media/1970/1970-01"));
    }

    #[test]
    fn test_sidecar_path() {
        let root = Path::new("/lib");
        let uuid = test_uuid();
        let path = sidecar_path(root, &uuid, "jpg", Some(1721001600));
        assert!(path.to_str().unwrap().ends_with(".cbor"));
        assert!(path.to_str().unwrap().contains("media/2024/2024-07"));
    }

    #[test]
    fn test_meta_cache_path() {
        let root = Path::new("/lib");
        let uuid = test_uuid();
        let path = meta_cache_path(root, &uuid);
        assert!(path.to_str().unwrap().contains("index/meta"));
        assert!(path.to_str().unwrap().ends_with(".meta.cbor"));
    }

    #[test]
    fn test_transcode_h264_path() {
        let root = Path::new("/lib");
        let uuid = test_uuid();
        let path = transcode_h264_path(root, &uuid);
        assert!(path.to_str().unwrap().contains("transcodes/h264"));
        assert!(path.to_str().unwrap().ends_with(".mp4"));
    }

    #[test]
    fn test_transcode_live_path() {
        let root = Path::new("/lib");
        let uuid = test_uuid();
        let path = transcode_live_path(root, &uuid);
        assert!(path.to_str().unwrap().contains("transcodes/live"));
        assert!(path.to_str().unwrap().ends_with(".mov"));
    }

    #[test]
    fn test_trash_path() {
        let root = Path::new("/lib");
        let uuid = test_uuid();
        let path = trash_path(root, &uuid, "jpg");
        assert!(path.to_str().unwrap().contains(".library/trash"));
        assert!(path.to_str().unwrap().ends_with(".jpg"));
    }

    #[test]
    fn test_tmp_path() {
        let p = PathBuf::from("/lib/media/2024/2024-07/abc.jpg");
        let tmp = tmp_path(&p);
        assert_eq!(tmp, PathBuf::from("/lib/media/2024/2024-07/abc.jpg.tmp"));
    }

    #[test]
    fn test_thumbnail_size_str() {
        assert_eq!(ThumbnailSize::Xs.as_str(), "xs");
        assert_eq!(ThumbnailSize::O.as_str(), "o");
    }
}
