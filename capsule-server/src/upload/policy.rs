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

/// The still-derivative content types the thumbnail ladder uploads.
///
/// **Source of truth:
/// [Thumbnails and Previews](../../../capsule-docs/src/content/docs/design/thumbnails.md).** Its
/// tier table is the closed set: a JXL master with AVIF and WebP as the delivery variants. The
/// doc is explicit that "every receiver (and every federated peer) compares the
/// `DerivativeManifest.format` value against this list, and an unknown value is a structural
/// rejection" — and this server is such a receiver, so its accept-list has to carry every value
/// that list admits *and no more*.
///
/// The `original` sentinel is deliberately **absent**. It is a recognised `format` value, not a
/// content type: a tier that references the original carries no bytes of its own, so it opens no
/// upload session and never presents a `content_type` at all. Admitting it here would widen the
/// closed enum for a blob that cannot exist.
///
/// # Why this is a list here and not the enum
///
/// It should be `capsule_core::derivative_format::DerivativeFormat::STILL_DELIVERY_ORDER` mapped
/// through `mime()` — one set, evaluated by producer and receiver alike. That module is not
/// reachable from this crate yet: it exists only on the branch of #436, is on neither `master`
/// nor this branch's base, and `capsule-core` is not this change's to edit. So the set is
/// restated once, in one place, with the swap named — and the test below fails the moment this
/// list and the accept-list disagree, which is the failure that produced #470.
pub const DERIVATIVE_CONTENT_TYPES: &[&str] = &["image/jxl", "image/avif", "image/webp"];

/// The closed `content_type` enum for the current protocol version (invariant 5).
///
/// Frozen for a given `protocol_version` and server-tunable across versions. Metadata,
/// provenance and backup blobs are opaque CBOR or ciphertext and declare
/// `application/octet-stream`.
///
/// It carries two disjoint things: the **originals** a client imports, and the **derivatives**
/// its ladder generates ([`DERIVATIVE_CONTENT_TYPES`], plus `video/mp4` for the H.264 baseline
/// video preview, which the stills-only derivative set does not model). `image/jxl` is here for
/// the second reason only — nothing imports a JXL original today — and its absence is #470:
/// every still larger than the 256 px thumbnail cap failed its T1 upload with
/// `400 error.upload.unsupported_content_type`, because the ladder encodes that tier as JXL and
/// the server had never been told the format existed.
pub const DEFAULT_CONTENT_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/heic",
    "image/heif",
    "image/webp",
    "image/avif",
    "image/jxl",
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
    fn every_committed_derivative_format_is_accepted() {
        // #470: the ladder encodes the thumbnail tier as JXL, the SDK uploads each derivative
        // with `content_type = derivative.format`, and the server answered
        // `400 error.upload.unsupported_content_type` — so every still larger than the 256 px
        // cap failed. The defect was not the missing string, it was that nothing tied the
        // accept-list to the closed set it is supposed to mirror. This is that tie.
        let policy = UploadPolicy::default();
        let accepted = policy.content_types();
        for format in DERIVATIVE_CONTENT_TYPES {
            assert!(
                accepted.contains(format),
                "{format} is a committed derivative format the ladder uploads, and the closed \
                 content-type enum refuses it"
            );
        }
    }

    #[test]
    fn the_original_sentinel_is_not_a_content_type() {
        // It is a recognised `DerivativeManifest.format` value and nothing more: a tier that
        // references the original carries no bytes, opens no upload session, and presents no
        // `content_type`. Admitting it would widen the closed enum for a blob that cannot exist.
        assert!(!DERIVATIVE_CONTENT_TYPES.contains(&"original"));
        assert!(
            !UploadPolicy::default()
                .content_types()
                .contains(&"original")
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
