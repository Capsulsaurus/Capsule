use std::fs;
use std::path::Path;

use crate::library::error::LibraryError;
use crate::library::library::Library;
use crate::sidecar::io::{read_sidecar, write_sidecar};

/// Soft-delete an asset.
///
/// 1. Update the DB row (`is_deleted = 1`, `deleted_at = now`).
/// 2. Update the CBOR sidecar (`is_deleted = true`, `deleted_at = now`).
/// 3. Move the media file to `.library/trash/{uuid}.{ext}`.
///
/// `media_path` must be the current absolute path of the media file.
/// `sidecar_cbor_path` must be the current absolute path of the `.cbor` sidecar.
pub fn soft_delete(
    uuid: &str,
    media_path: &Path,
    sidecar_cbor_path: &Path,
    library: &Library,
) -> Result<(), LibraryError> {
    let now = now_secs();

    // 1. DB
    library.db.soft_delete(uuid, now)?;

    // 2. Sidecar
    if sidecar_cbor_path.exists() {
        let mut sidecar =
            read_sidecar(sidecar_cbor_path).map_err(|e| LibraryError::Cbor(e.to_string()))?;
        sidecar.is_deleted = true;
        sidecar.deleted_at = Some(now);
        write_sidecar(sidecar_cbor_path, &sidecar)
            .map_err(|e| LibraryError::Cbor(e.to_string()))?;
    }

    // 3. Move media file to trash (use uuid without hyphens, matching paths::trash_path)
    if media_path.exists() {
        let ext = media_path.extension().unwrap_or_default().to_string_lossy();
        let uuid_plain = uuid.replace('-', "");
        let trash_file = library
            .root
            .join(".library/trash")
            .join(format!("{uuid_plain}.{ext}"));
        fs::create_dir_all(trash_file.parent().unwrap()).map_err(LibraryError::Io)?;
        fs::rename(media_path, &trash_file).map_err(LibraryError::Io)?;
    }

    Ok(())
}

/// Permanently delete all assets that have been soft-deleted longer than
/// `older_than_secs` seconds ago.
///
/// For each expired asset the sidecar is deleted first, then the trash media file.
pub fn purge_expired_trash(library: &Library, older_than_secs: i64) -> Result<(), LibraryError> {
    let expired = library.db.query_expired_trash(older_than_secs)?;

    for row in &expired {
        let uuid = &row.uuid;

        // Delete sidecar(s) from media/ — walk media to find matching .cbor
        let media_dir = library.root.join("media");
        if media_dir.exists() {
            for entry in walkdir::WalkDir::new(&media_dir)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                // Sidecar file names: {uuid_no_hyphens}.cbor or {uuid}.cbor
                let uuid_plain = uuid.replace('-', "");
                if name == format!("{uuid_plain}.cbor") || name == format!("{uuid}.cbor") {
                    let _ = fs::remove_file(path);
                }
            }
        }

        // Delete trash media file (any extension).
        let trash_dir = library.root.join(".library/trash");
        if trash_dir.exists() {
            for entry in fs::read_dir(&trash_dir)
                .map_err(LibraryError::Io)?
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                let stem = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let uuid_plain = uuid.replace('-', "");
                if stem == *uuid || stem == uuid_plain {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }

    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ImportMode;
    use crate::library::init::init_library;
    use crate::metadata::AssetType;
    use crate::sidecar::AssetSidecar;
    use crate::sidecar::io::write_sidecar;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn make_sidecar(uuid: &str, hash: &str) -> AssetSidecar {
        AssetSidecar {
            version: 1,
            uuid: uuid.to_string(),
            asset_type: AssetType::Photo,
            original_filename: format!("{uuid}.jpg"),
            import_timestamp: 1720000000,
            modified_timestamp: 1720000000,
            hash_blake3: hash.to_string(),
            file_size: 1024,
            is_deleted: false,
            rating: 0,
            tags: vec![],
            import_mode: ImportMode::Copy,
            importer_version: "0.1.0".to_string(),
            rawshift_version: "0.1.0".to_string(),
            capture_timestamp: None,
            capture_utc: None,
            capture_tz: None,
            capture_tz_source: None,
            tz_db_version: None,
            width: None,
            height: None,
            duration_ms: None,
            stack_hint: None,
            album_id: None,
            deleted_at: None,
            camera_make: None,
            camera_model: None,
            gps_lat: None,
            gps_lon: None,
            unknown_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn test_soft_delete_updates_db_and_sidecar() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();

        let media_dir = root.join("media/1970/1970-01");
        fs::create_dir_all(&media_dir).unwrap();

        let uuid = "dddd0000-0000-0000-0000-000000000001";
        let uuid_plain = uuid.replace('-', "");
        let hash = "d".repeat(64);

        // Write media file and sidecar
        let media_file = media_dir.join(format!("{uuid_plain}.jpg"));
        let cbor_file = media_dir.join(format!("{uuid_plain}.cbor"));
        fs::write(&media_file, b"photo").unwrap();
        let sidecar = make_sidecar(uuid, &hash);
        write_sidecar(&cbor_file, &sidecar).unwrap();

        // Insert into DB
        let row = crate::db::rows::AssetRow {
            uuid: uuid.to_string(),
            asset_type: "photo".to_string(),
            capture_timestamp: 1720000000,
            capture_utc: None,
            capture_tz_source: None,
            import_timestamp: 1720000000,
            hash_blake3: hash.clone(),
            width: None,
            height: None,
            duration_ms: None,
            stack_id: None,
            is_stack_hidden: false,
            chromahash: None,
            dominant_color: None,
            album_id: None,
            rating: 0,
            is_deleted: false,
            deleted_at: None,
        };
        lib.db.insert_asset(&row).unwrap();

        soft_delete(uuid, &media_file, &cbor_file, &lib).unwrap();

        // DB should mark as deleted
        let found = lib.db.find_by_hash(&hash).unwrap().unwrap();
        assert!(found.is_deleted);
        assert!(found.deleted_at.is_some());

        // Media file should be in trash
        assert!(!media_file.exists());
        let trash = root.join(format!(".library/trash/{uuid_plain}.jpg"));
        assert!(trash.exists(), "media file should be in trash");

        // Sidecar should be updated
        let updated = read_sidecar(&cbor_file).unwrap();
        assert!(updated.is_deleted);
        assert!(updated.deleted_at.is_some());
    }

    #[test]
    fn test_purge_expired_removes_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();

        let uuid = "eeee0000-0000-0000-0000-000000000002";
        let uuid_plain = uuid.replace('-', "");

        // Insert deleted asset with very old deleted_at
        let row = crate::db::rows::AssetRow {
            uuid: uuid.to_string(),
            asset_type: "photo".to_string(),
            capture_timestamp: 1000,
            capture_utc: None,
            capture_tz_source: None,
            import_timestamp: 1000,
            hash_blake3: "e".repeat(64),
            width: None,
            height: None,
            duration_ms: None,
            stack_id: None,
            is_stack_hidden: false,
            chromahash: None,
            dominant_color: None,
            album_id: None,
            rating: 0,
            is_deleted: true,
            deleted_at: Some(100), // far in the past
        };
        lib.db.insert_asset(&row).unwrap();

        // Create a trash file
        let trash_dir = root.join(".library/trash");
        let trash_file = trash_dir.join(format!("{uuid_plain}.jpg"));
        fs::write(&trash_file, b"trash").unwrap();

        purge_expired_trash(&lib, 30 * 86400).unwrap();

        assert!(!trash_file.exists(), "trash file should be purged");
    }
}
