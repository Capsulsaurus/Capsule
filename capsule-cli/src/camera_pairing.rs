//! The CLI's camera pairing state: the PTP/IP initiator identity it presents to every camera.
//!
//! A camera that pairs (Sony's "Remote/PC connection", and others that keep a list of known
//! computers) remembers the initiator's GUID, so the identity must be stable across runs: it is
//! created once, on first use, and persisted in client-local config ([Import — Camera Import]),
//! never in a library, because it names this client to cameras and not any library's contents.
//!
//! The GUID is a random UUIDv4 rather than a v7: it is shown to every camera the client meets,
//! and its creation time is nobody's business. Renaming keeps the GUID, so a camera that already
//! paired still recognises the client under its new name.
//!
//! [Import — Camera Import]: https://docs/design/import/camera-import/

use std::io;
use std::path::{Path, PathBuf};

use ptpip::InitiatorIdentity;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// The file, under the CLI's config directory, that holds the identity.
pub const PAIRING_FILE: &str = "camera_initiator.json";

/// The longest initiator name, in characters. PTP strings carry at most 255 UTF-16 code units;
/// cameras show far fewer, so a name is held to what a camera's pairing list can display.
pub const MAX_NAME_CHARS: usize = 64;

/// What loading or saving the pairing state can fail with. None of them is read as "no
/// identity yet": minting a fresh GUID over an unreadable file would unpair every camera.
#[derive(Debug, Error)]
pub enum PairingError {
    /// The file could not be read or written.
    #[error("camera pairing state at {path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// Why.
        source: io::Error,
    },
    /// The file exists but does not hold an identity.
    #[error(
        "camera pairing state at {path} is corrupt ({reason}); remove it to pair as a new client"
    )]
    Corrupt {
        /// The file.
        path: String,
        /// What was wrong with it.
        reason: String,
    },
    /// A name the identity cannot carry.
    #[error("invalid camera initiator name: {0}")]
    InvalidName(&'static str),
}

/// The identity as the CLI reports it: the GUID as a hyphenated UUID, and the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    /// The initiator GUID.
    pub guid: Uuid,
    /// The name cameras show for this client.
    pub name: String,
}

impl Pairing {
    /// The identity `ptpip` presents in `InitCommandRequest`.
    #[must_use]
    pub fn initiator(&self) -> InitiatorIdentity {
        InitiatorIdentity {
            guid: *self.guid.as_bytes(),
            name: self.name.clone(),
        }
    }
}

/// Check that `name` is one a camera can be shown: not blank, no control characters, at most
/// [`MAX_NAME_CHARS`] characters. Returns it trimmed.
pub fn validate_name(name: &str) -> Result<String, PairingError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(PairingError::InvalidName("it is empty"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(PairingError::InvalidName("it contains a control character"));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(PairingError::InvalidName("it is longer than 64 characters"));
    }
    Ok(trimmed.to_owned())
}

/// The name a new identity takes: `Capsule (<host>)`, cut to [`MAX_NAME_CHARS`], or plain
/// `Capsule` where the host has no usable name.
#[must_use]
pub fn default_name(host: Option<&str>) -> String {
    let host: String = host
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let host = host.trim();
    if host.is_empty() {
        return "Capsule".to_owned();
    }
    let budget = MAX_NAME_CHARS - "Capsule ()".len();
    format!(
        "Capsule ({})",
        host.chars().take(budget).collect::<String>()
    )
}

/// The file-backed pairing state.
#[derive(Debug, Clone)]
pub struct PairingStore {
    path: PathBuf,
}

impl PairingStore {
    /// A store backed by `path` (normally `<config_dir>/camera_initiator.json`).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The backing file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored identity, or `None` when this client has never paired.
    pub fn load(&self) -> Result<Option<Pairing>, PairingError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(self.io_error(source)),
        };
        let pairing: Pairing =
            serde_json::from_slice(&bytes).map_err(|error| self.corrupt(error.to_string()))?;
        if pairing.guid.is_nil() {
            return Err(self.corrupt("the GUID is nil".to_owned()));
        }
        validate_name(&pairing.name).map_err(|error| self.corrupt(error.to_string()))?;
        Ok(Some(pairing))
    }

    /// The stored identity, creating and persisting one named `name()` when there is none.
    /// Returns it and whether it was created now.
    pub fn load_or_create(
        &self,
        name: impl FnOnce() -> String,
    ) -> Result<(Pairing, bool), PairingError> {
        if let Some(pairing) = self.load()? {
            tracing::debug!(guid = %pairing.guid, path = %self.path.display(), "camera pairing: loaded");
            return Ok((pairing, false));
        }
        let pairing = Pairing {
            guid: Uuid::new_v4(),
            name: validate_name(&name())?,
        };
        self.save(&pairing)?;
        tracing::info!(guid = %pairing.guid, name = %pairing.name, path = %self.path.display(), "camera pairing: created a new initiator identity");
        Ok((pairing, true))
    }

    /// Rename the identity, creating it first where there is none. The GUID is kept.
    pub fn rename(&self, name: &str) -> Result<Pairing, PairingError> {
        let name = validate_name(name)?;
        let mut pairing = match self.load()? {
            Some(pairing) => pairing,
            None => Pairing {
                guid: Uuid::new_v4(),
                name: String::new(),
            },
        };
        pairing.name = name;
        self.save(&pairing)?;
        tracing::info!(guid = %pairing.guid, name = %pairing.name, "camera pairing: renamed");
        Ok(pairing)
    }

    /// Write `pairing` beside the target and rename it into place, so a crash leaves the old
    /// identity or the new one and never a torn file that would unpair the client.
    fn save(&self, pairing: &Pairing) -> Result<(), PairingError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| self.io_error(source))?;
        }
        let json =
            serde_json::to_vec_pretty(pairing).map_err(|error| self.corrupt(error.to_string()))?;
        let mut staging = self.path.clone().into_os_string();
        staging.push(".tmp");
        let staging = PathBuf::from(staging);
        let write = || -> io::Result<()> {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&staging)?;
            file.write_all(&json)?;
            file.sync_all()?;
            std::fs::rename(&staging, &self.path)
        };
        write().map_err(|source| self.io_error(source))
    }

    fn io_error(&self, source: io::Error) -> PairingError {
        PairingError::Io {
            path: self.path.display().to_string(),
            source,
        }
    }

    fn corrupt(&self, reason: String) -> PairingError {
        PairingError::Corrupt {
            path: self.path.display().to_string(),
            reason,
        }
    }
}

#[cfg(test)]
mod tests;
