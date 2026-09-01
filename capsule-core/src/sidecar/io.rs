use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::sidecar::{AssetSidecar, LibraryConfigCbor, LibraryVersionCbor};
use crate::utils::paths::tmp_path;

pub fn read_sidecar(path: &Path) -> Result<AssetSidecar, Box<dyn std::error::Error + Send + Sync>> {
    let file = fs::File::open(path)?;
    let sidecar = ciborium::de::from_reader(BufReader::new(file))?;
    Ok(sidecar)
}

/// Write an unsigned [`AssetSidecar`] to disk (atomic tmp-then-rename).
///
/// The legacy unsigned sidecar **write path is retired** (`S-G4`): no production code writes
/// unsigned sidecars anymore — imports land on the signed [`SidecarV1`](crate::sidecar::SidecarV1)
/// path via [`Workspace::import_asset_with`](crate::lifecycle::Workspace::import_asset_with)
/// (`S-B2`), and soft-delete/retention ride the signed lifecycle. This writer survives only to
/// build on-disk fixtures for the retained *read* path — the recovery-first index rebuild
/// ([`rebuild_index`](crate::library::rebuild::rebuild_index)) that still ingests unsigned
/// `.cbor` sidecars from pre-signed-path libraries — so it is compiled under `cfg(test)` only.
#[cfg(test)]
pub fn write_sidecar(
    path: &Path,
    sidecar: &AssetSidecar,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tmp = tmp_path(path);
    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        ciborium::ser::into_writer(sidecar, BufWriter::new(file))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_library_version(
    path: &Path,
) -> Result<LibraryVersionCbor, Box<dyn std::error::Error + Send + Sync>> {
    let file = fs::File::open(path)?;
    let v = ciborium::de::from_reader(BufReader::new(file))?;
    Ok(v)
}

pub fn write_library_version(
    path: &Path,
    v: &LibraryVersionCbor,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tmp = tmp_path(path);
    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        ciborium::ser::into_writer(v, BufWriter::new(file))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_library_config(
    path: &Path,
) -> Result<LibraryConfigCbor, Box<dyn std::error::Error + Send + Sync>> {
    let file = fs::File::open(path)?;
    let cfg = ciborium::de::from_reader(BufReader::new(file))?;
    Ok(cfg)
}

pub fn write_library_config(
    path: &Path,
    cfg: &LibraryConfigCbor,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tmp = tmp_path(path);
    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        ciborium::ser::into_writer(cfg, BufWriter::new(file))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use super::*;
    use crate::domain::ImportMode;
    use crate::metadata::AssetType;
    use crate::sidecar::{AssetSidecar, LibraryConfigCbor, LibraryVersionCbor};

    fn minimal_sidecar() -> AssetSidecar {
        AssetSidecar {
            version: 1,
            uuid: "01956ef3-0000-7000-8000-000000000001".to_string(),
            asset_type: AssetType::Photo,
            original_filename: "IMG_1234.jpg".to_string(),
            import_timestamp: 1720000000,
            modified_timestamp: 1720000000,
            hash_sha256: "a".repeat(64),
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
    fn test_write_read_sidecar() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.cbor");
        let s = minimal_sidecar();
        write_sidecar(&path, &s).unwrap();
        assert!(path.exists(), "sidecar file should exist after write");
        assert!(
            !dir.path().join("test.cbor.tmp").exists(),
            "temp file should be removed after atomic rename"
        );
        let read_back = read_sidecar(&path).unwrap();
        assert_eq!(s, read_back);
    }

    #[test]
    fn test_write_read_library_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("version.cbor");
        let v = LibraryVersionCbor { version: 1 };
        write_library_version(&path, &v).unwrap();
        assert!(path.exists());
        let read_back = read_library_version(&path).unwrap();
        assert_eq!(v, read_back);
    }

    #[test]
    fn test_write_read_library_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.cbor");
        let cfg = LibraryConfigCbor {
            schema_version: 1,
            library_name: "Test".to_string(),
            last_opened_at: 1720000000,
            last_scrubbed_at: None,
        };
        write_library_config(&path, &cfg).unwrap();
        let read_back = read_library_config(&path).unwrap();
        assert_eq!(cfg, read_back);
    }

    #[test]
    fn test_write_read_library_config_with_scrubbed() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config2.cbor");
        let cfg = LibraryConfigCbor {
            schema_version: 1,
            library_name: "My Library".to_string(),
            last_opened_at: 1720000000,
            last_scrubbed_at: Some(1719990000),
        };
        write_library_config(&path, &cfg).unwrap();
        let read_back = read_library_config(&path).unwrap();
        assert_eq!(cfg, read_back);
    }
}
