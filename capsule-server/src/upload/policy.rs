//! [`UploadPolicy`] — the deployment-tunable half of the upload contract.
//!
//! # What belongs here and what does not
//!
//! The [Upload Protocol](../../../capsule-docs/src/content/docs/design/import/upload-protocol.md)
//! draws one line down the middle of this surface, and this module is that line made into a
//! type:
//!
//! - **Protocol surface** — the 4 KiB alignment, the `[4 KiB, 16 MiB]` chunk range, the
//!   offset semantics — is *not* here. It is fixed for a protocol version, so it lives as
//!   constants in [`super::chunk`] where no deployment can move it.
//! - **Server-tunable** — the accepted protocol window, the per-file ceiling, the closed
//!   `content_type` enum, the timestamp-drift bound, and the suggested chunk-size tiers — is
//!   here, because a self-hosted deployment legitimately sets them differently.
//!
//! Every value carries the Salvo deployment's default, so the rebuild starts from the
//! behaviour clients already see rather than from a fresh set of numbers.

use jiff::Timestamp;

/// The closed `content_type` enum for the current protocol version (invariant 5).
///
/// Frozen for a given `protocol_version` and server-tunable across versions. Metadata,
/// provenance and backup blobs are opaque CBOR or ciphertext and declare
/// `application/octet-stream`.
pub const DEFAULT_CONTENT_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/heic",
    "image/heif",
    "image/webp",
    "image/avif",
    "image/gif",
    "image/tiff",
    "video/mp4",
    "video/quicktime",
    "video/webm",
    "application/octet-stream",
];

/// Lowest protocol date this server accepts (`X-Capsule-Protocol-Min`).
pub const DEFAULT_PROTOCOL_MIN: &str = "2026-01-01";

/// Highest protocol date this server accepts (`X-Capsule-Protocol-Max`).
pub const DEFAULT_PROTOCOL_MAX: &str = "2026-12-31";

/// Gross-drift sanity bound for the envelope timestamp, in days (invariant 8).
pub const DEFAULT_DRIFT_DAYS: i64 = 30;

/// The per-blob ceiling a deployment defaults to, in bytes.
///
/// 4 GiB, the Salvo deployment's `max_file_size`. It bounds one blob; the total in-flight
/// bytes are bounded by the discard window instead, which is `S-C1`'s pressure-eviction half
/// and is not this type's business.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// The tunable half of the upload contract.
///
/// Cheap to clone (one `Vec` of short strings), because a Kynos provider hands the context out
/// per request.
#[derive(Debug, Clone)]
pub struct UploadPolicy {
    /// Lowest accepted protocol date (`YYYY-MM-DD`).
    protocol_min: String,
    /// Highest accepted protocol date (`YYYY-MM-DD`).
    protocol_max: String,
    /// The closed `content_type` allow-list (invariant 5).
    content_types: Vec<String>,
    /// Gross-drift sanity bound in days for the envelope timestamp (invariant 8).
    drift_days: i64,
    /// The per-blob ceiling in bytes (invariant 4's upper half).
    max_file_bytes: u64,
}

impl Default for UploadPolicy {
    fn default() -> Self {
        Self {
            protocol_min: DEFAULT_PROTOCOL_MIN.to_owned(),
            protocol_max: DEFAULT_PROTOCOL_MAX.to_owned(),
            content_types: DEFAULT_CONTENT_TYPES
                .iter()
                .map(|kind| (*kind).to_owned())
                .collect(),
            drift_days: DEFAULT_DRIFT_DAYS,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }
}

impl UploadPolicy {
    /// The lowest protocol date this server accepts.
    pub fn protocol_min(&self) -> &str {
        &self.protocol_min
    }

    /// The highest protocol date this server accepts.
    pub fn protocol_max(&self) -> &str {
        &self.protocol_max
    }

    /// The closed `content_type` allow-list, as the shared predicate wants it.
    pub fn content_types(&self) -> Vec<&str> {
        self.content_types.iter().map(String::as_str).collect()
    }

    /// The gross-drift bound in days.
    pub fn drift_days(&self) -> i64 {
        self.drift_days
    }

    /// The per-blob ceiling in bytes.
    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
    }

    /// Narrow the accepted protocol window.
    #[must_use]
    pub fn with_protocol_window(mut self, min: impl Into<String>, max: impl Into<String>) -> Self {
        self.protocol_min = min.into();
        self.protocol_max = max.into();
        self
    }

    /// Replace the closed `content_type` enum.
    #[must_use]
    pub fn with_content_types<I, S>(mut self, kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.content_types = kinds.into_iter().map(Into::into).collect();
        self
    }

    /// Replace the per-blob ceiling.
    #[must_use]
    pub fn with_max_file_bytes(mut self, bytes: u64) -> Self {
        self.max_file_bytes = bytes;
        self
    }

    /// Replace the gross-drift bound.
    #[must_use]
    pub fn with_drift_days(mut self, days: i64) -> Self {
        self.drift_days = days;
        self
    }
}

/// The server's clock as the RFC3339 string the shared validation predicates read.
///
/// `capsule_core::validation` takes timestamps as text because the values it compares arrive
/// as text on the wire; this is the one place the server's own `jiff` reading is rendered into
/// that form, so no call site formats a clock by hand.
pub fn as_rfc3339(at: Timestamp) -> String {
    at.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_window_admits_the_protocol_version_core_speaks() {
        let policy = UploadPolicy::default();
        let current = capsule_core::crypto::primitives::PROTOCOL_VERSION;
        assert!(
            policy.protocol_min() <= current && current <= policy.protocol_max(),
            "a server that refused the protocol its own core speaks could accept no upload"
        );
    }

    #[test]
    fn the_allow_list_carries_the_opaque_blob_type() {
        // Metadata, provenance and backup blobs all declare `application/octet-stream`; an
        // allow-list without it would refuse every blob but the original.
        assert!(
            UploadPolicy::default()
                .content_types()
                .contains(&"application/octet-stream")
        );
    }

    #[test]
    fn a_deployment_can_narrow_every_tunable() {
        let policy = UploadPolicy::default()
            .with_protocol_window("2026-06-01", "2026-06-30")
            .with_content_types(["image/jpeg"])
            .with_max_file_bytes(1024)
            .with_drift_days(1);

        assert_eq!(policy.protocol_min(), "2026-06-01");
        assert_eq!(policy.protocol_max(), "2026-06-30");
        assert_eq!(policy.content_types(), vec!["image/jpeg"]);
        assert_eq!(policy.max_file_bytes(), 1024);
        assert_eq!(policy.drift_days(), 1);
    }

    #[test]
    fn the_server_clock_renders_as_the_predicates_read_it() {
        let rendered = as_rfc3339(Timestamp::UNIX_EPOCH);
        assert_eq!(
            rendered.parse::<Timestamp>().expect("round trips"),
            Timestamp::UNIX_EPOCH
        );
    }
}
