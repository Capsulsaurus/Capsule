//! Client-side device-cohort emission and the devices-grouping view model
//! (slice `S-D11`; SSoT: [Authentication — Device Cohorts]).
//!
//! This module owns the **client half** of the cohort story whose server half
//! (advisory storage + the `{devices, cohorts}` listing) landed in slice `S-C13`,
//! and whose pure hash lives in [`capsule_core::cohort`]. Three cohesive pieces:
//!
//! - **[`PrimaryIdentifierReader`]** — the per-platform seam. Each platform reads
//!   its single primary identifier (Keychain seed / SSAID / IOPlatformUUID /
//!   MachineGuid / hashed `machine-id`) behind this trait; the SDK never bakes in a
//!   platform's identifier source. [`compute_cohort_hash`] turns a reader plus the
//!   account `user_id` into the advisory hash that [`crate::auth::AuthClient`] rides.
//! - **[`devices`]** — the devices-grouping view model that consumes the server's
//!   `GET /devices` `{devices, cohorts}` body into groups the UI renders, carrying
//!   the **assert-don't-litigate** copy as `locales/` catalog keys.
//! - **[`devices::SupportBundle`]** — the one-tap dispute payload
//!   (`cohort_hash` + the device/session map), serializable and round-trippable.
//!
//! ## Why the hash needs the `user_id`
//!
//! The cohort hash folds the account `user_id` in so the same physical device under
//! two accounts yields unlinkable hashes (the cross-account correlation surface is
//! removed at the source — see the owner doc). A client therefore emits a cohort
//! only once it knows which account it is authenticating: on a returning device the
//! app has the cached `user_id`; on a first-ever registration it does not, so it
//! simply emits nothing (**absent stays legal**) and starts emitting on the next
//! login. This module never guesses a `user_id`.
//!
//! ## Never log the raw identifier
//!
//! A [`PrimaryIdentifierReader`] handles a stable device fingerprint. Implementations
//! and this module **must never** emit the raw identifier to logs, errors, or the
//! `Debug` surface — only the derived hash (already one-way) ever leaves. The
//! adapters here uphold that: the Linux reader returns a *hashed* value, never the
//! raw `machine-id`, and no error variant embeds the identifier.
//!
//! [Authentication — Device Cohorts]: https://docs/design/authentication/#device-cohorts

pub mod devices;

pub use capsule_core::cohort::PlatformTag;
use capsule_core::cohort::cohort_hash;
use uuid::Uuid;

/// Domain-separation label for the Linux `machine-id` pre-hash. Hashing the raw
/// `machine-id` before it is ever used is systemd's explicit guidance ("never used
/// raw") and keeps the stable machine secret from leaving the device even inside the
/// cohort input.
const MACHINE_ID_PREHASH_V1: &str = "capsule-machine-id-prehash/v1";

/// Everything a [`PrimaryIdentifierReader`] can fail with. **No variant embeds the
/// raw identifier** — the whole point of the reader is that the fingerprint never
/// leaks, so the errors carry only the *reason*, never the value.
#[derive(Debug, thiserror::Error)]
pub enum IdentifierError {
    /// The platform exposes no primary identifier in this environment (e.g. the
    /// source file/service is absent). The caller emits no cohort — legal.
    #[error("no primary identifier available on this platform/environment")]
    Unavailable,
    /// The identifier source was reachable but returned an empty value.
    #[error("primary identifier source returned an empty value")]
    Empty,
    /// Reading the identifier source failed for the given reason (path/kind only —
    /// never the value).
    #[error("reading the primary identifier failed: {reason}")]
    Read {
        /// Why the read failed (I/O kind, a missing tool, etc.) — never the value.
        reason: String,
    },
}

/// The per-platform seam: read this platform's single primary identifier.
///
/// One identifier per platform, chosen for reinstall-stability, never a concatenated
/// fingerprint (owner doc). The returned string is the `primary_id` fed verbatim into
/// [`capsule_core::cohort::cohort_hash`]. Implementations **must not** log or otherwise
/// surface the raw identifier; where the raw value is itself a stable device secret
/// (Linux `machine-id`), the implementation returns a one-way-derived value instead.
pub trait PrimaryIdentifierReader {
    /// The platform this reader speaks for — becomes the closed `platform_tag` in the
    /// hash construction.
    fn platform(&self) -> PlatformTag;

    /// Read the platform's primary identifier (already safe to hash: raw device
    /// secrets are pre-derived by the implementation).
    fn read_primary_id(&self) -> Result<String, IdentifierError>;
}

/// Compute the advisory cohort hash (lowercase-hex SHA-256) for an account on this
/// device: read the platform's primary identifier and fold it with `user_id` through
/// [`capsule_core::cohort::cohort_hash`].
///
/// The result is exactly what [`crate::auth::AuthClient::with_cohort_hash`] wants.
/// Returns [`IdentifierError`] when no identifier is available — the caller then emits
/// nothing, which is legal.
pub fn compute_cohort_hash(
    reader: &dyn PrimaryIdentifierReader,
    user_id: Uuid,
) -> Result<String, IdentifierError> {
    let primary_id = reader.read_primary_id()?;
    // Traceable without leaking: log that we computed one, never the input.
    tracing::debug!(
        platform = reader.platform().as_str(),
        "computed device-cohort hash"
    );
    Ok(cohort_hash(user_id, reader.platform(), &primary_id).to_hex())
}

/// Derive the non-reversible `primary_id` for a Linux `machine-id`: a domain-separated
/// SHA-256 over the raw value, so the stable machine secret never leaves the device
/// even as a cohort input (systemd's "never used raw" guidance).
///
/// The input is **length-delimited** (each field prefixed by its byte length), not
/// naively concatenated, so the fixed domain label and the value can never blur into
/// each other.
fn prehash_machine_id(raw: &str) -> String {
    use capsule_core::crypto::hash::hash_bytes;

    let label = MACHINE_ID_PREHASH_V1.as_bytes();
    let raw = raw.as_bytes();
    let mut buf = Vec::with_capacity(8 + label.len() + raw.len());
    buf.extend_from_slice(&(label.len() as u32).to_le_bytes());
    buf.extend_from_slice(label);
    buf.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    buf.extend_from_slice(raw);
    hash_bytes(&buf).to_hex()
}

/// Linux primary-identifier reader: the systemd `machine-id`, **hashed** before use.
///
/// Reads `/etc/machine-id` (falling back to `/var/lib/dbus/machine-id`), then returns
/// the domain-separated hash — never the raw value. The source path is configurable
/// ([`from_path`](LinuxMachineIdReader::from_path)) so the derivation is host-testable
/// off a fixture on any OS.
#[derive(Debug, Clone)]
pub struct LinuxMachineIdReader {
    paths: Vec<std::path::PathBuf>,
}

impl LinuxMachineIdReader {
    /// The standard reader: `/etc/machine-id` then the dbus fallback.
    pub fn new() -> Self {
        Self {
            paths: vec![
                std::path::PathBuf::from("/etc/machine-id"),
                std::path::PathBuf::from("/var/lib/dbus/machine-id"),
            ],
        }
    }

    /// A reader over an explicit source path (for tests/fixtures).
    pub fn from_path(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            paths: vec![path.into()],
        }
    }
}

impl Default for LinuxMachineIdReader {
    fn default() -> Self {
        Self::new()
    }
}

impl PrimaryIdentifierReader for LinuxMachineIdReader {
    fn platform(&self) -> PlatformTag {
        PlatformTag::Linux
    }

    fn read_primary_id(&self) -> Result<String, IdentifierError> {
        for path in &self.paths {
            match std::fs::read_to_string(path) {
                Ok(contents) => {
                    let raw = contents.trim();
                    if raw.is_empty() {
                        return Err(IdentifierError::Empty);
                    }
                    return Ok(prehash_machine_id(raw));
                }
                // Try the next candidate on absence; surface other I/O errors by kind.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(IdentifierError::Read {
                        reason: e.kind().to_string(),
                    });
                }
            }
        }
        Err(IdentifierError::Unavailable)
    }
}

/// macOS primary-identifier reader: `IOPlatformUUID`, used directly (macOS is the one
/// platform where the identifier is reset-stable, per the owner doc).
///
/// Resolves the UUID via `ioreg -rd1 -c IOPlatformExpertDevice`. Compiled only on
/// macOS; other platforms use their own reader.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Default)]
pub struct MacosPlatformUuidReader;

#[cfg(target_os = "macos")]
impl MacosPlatformUuidReader {
    /// A new reader.
    pub fn new() -> Self {
        Self
    }

    /// Extract the `IOPlatformUUID` value from `ioreg` output.
    fn parse_uuid(ioreg_output: &str) -> Option<String> {
        // Lines look like: `    "IOPlatformUUID" = "XXXXXXXX-XXXX-..."`
        for line in ioreg_output.lines() {
            if let Some(rest) = line.split_once("\"IOPlatformUUID\"")
                && let Some(value) = rest.1.split('=').nth(1)
            {
                let uuid = value.trim().trim_matches('"').trim();
                if !uuid.is_empty() {
                    return Some(uuid.to_string());
                }
            }
        }
        None
    }
}

#[cfg(target_os = "macos")]
impl PrimaryIdentifierReader for MacosPlatformUuidReader {
    fn platform(&self) -> PlatformTag {
        PlatformTag::Macos
    }

    fn read_primary_id(&self) -> Result<String, IdentifierError> {
        let output = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .map_err(|e| IdentifierError::Read {
                reason: e.kind().to_string(),
            })?;
        if !output.status.success() {
            return Err(IdentifierError::Unavailable);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Self::parse_uuid(&text).ok_or(IdentifierError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_reader_hashes_machine_id_never_returns_raw() {
        let dir = std::env::temp_dir().join(format!("capsule-cohort-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("machine-id");
        let raw = "abcdef0123456789abcdef0123456789";
        std::fs::write(&path, format!("{raw}\n")).unwrap();

        let reader = LinuxMachineIdReader::from_path(&path);
        assert_eq!(reader.platform(), PlatformTag::Linux);
        let id = reader.read_primary_id().unwrap();

        // The returned primary_id is the derived hash (64 hex chars), never the raw
        // machine-id — the stable device secret must not leave the device.
        assert_ne!(id, raw);
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic derivation.
        assert_eq!(
            id,
            LinuxMachineIdReader::from_path(&path)
                .read_primary_id()
                .unwrap()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn linux_reader_reports_empty_and_unavailable_cleanly() {
        let dir = std::env::temp_dir().join(format!("capsule-cohort-e-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("machine-id");
        std::fs::write(&empty, "\n").unwrap();
        assert!(matches!(
            LinuxMachineIdReader::from_path(&empty).read_primary_id(),
            Err(IdentifierError::Empty)
        ));
        assert!(matches!(
            LinuxMachineIdReader::from_path(dir.join("does-not-exist")).read_primary_id(),
            Err(IdentifierError::Unavailable)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compute_cohort_hash_matches_core_and_is_deterministic() {
        let dir = std::env::temp_dir().join(format!("capsule-cohort-c-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("machine-id");
        std::fs::write(&path, "0011223344556677\n").unwrap();
        let reader = LinuxMachineIdReader::from_path(&path);
        let user = Uuid::from_u128(0xABCD);

        let a = compute_cohort_hash(&reader, user).unwrap();
        let b = compute_cohort_hash(&reader, user).unwrap();
        assert_eq!(a, b, "same device + account ⇒ same cohort");

        // Byte-identical to computing through the core hash directly.
        let primary = reader.read_primary_id().unwrap();
        let expected = cohort_hash(user, PlatformTag::Linux, &primary).to_hex();
        assert_eq!(a, expected);

        // A different account on the same device ⇒ a different (unlinkable) hash.
        let other = compute_cohort_hash(&reader, Uuid::from_u128(0x1234)).unwrap();
        assert_ne!(a, other);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reader_reads_a_uuid_shaped_platform_id() {
        let reader = MacosPlatformUuidReader::new();
        assert_eq!(reader.platform(), PlatformTag::Macos);
        let id = reader
            .read_primary_id()
            .expect("IOPlatformUUID on macOS host");
        // IOPlatformUUID is a canonical UUID string; assert it parses as one.
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "IOPlatformUUID parses as a UUID"
        );
        // Folds into a stable cohort hash.
        let user = Uuid::from_u128(7);
        assert_eq!(
            compute_cohort_hash(&reader, user).unwrap(),
            cohort_hash(user, PlatformTag::Macos, &id).to_hex()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_parse_uuid_extracts_the_value() {
        let sample = "    \"IOPlatformUUID\" = \"12345678-90AB-CDEF-1234-567890ABCDEF\"\n";
        assert_eq!(
            MacosPlatformUuidReader::parse_uuid(sample).as_deref(),
            Some("12345678-90AB-CDEF-1234-567890ABCDEF")
        );
        assert_eq!(MacosPlatformUuidReader::parse_uuid("no uuid here"), None);
    }
}
