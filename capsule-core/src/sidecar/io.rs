use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::sidecar::{LibraryConfigCbor, LibraryVersionCbor};
use crate::utils::paths::tmp_path;

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
    use tempfile::TempDir;

    use super::*;
    use crate::sidecar::{LibraryConfigCbor, LibraryVersionCbor};

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
