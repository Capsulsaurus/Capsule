use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use capsule_core::sidecar::{LibraryConfigCbor, LibraryVersionCbor};

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
mod tests;
