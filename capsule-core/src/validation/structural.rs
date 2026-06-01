//! The key-less structural envelope checks a server runs before persisting any write
//! (SSoT: [Threat Model — Server-Side Validation Invariants]). The server holds no keys,
//! so it cannot verify signatures — but it validates *structure* refuse-by-default. These
//! are pure predicates over the manifest core and server-known state; the client mirrors
//! them via [`verify_asset`](crate::crypto::verify_asset).
//!
//! [Threat Model — Server-Side Validation Invariants]: https://docs/design/threat-model/validation/#server-side-validation-invariants

use crate::crypto::hash::Hash32;
use crate::crypto::primitives::SuiteId;
use crate::crypto::provenance::ManifestCore;

/// Invariant 3: the declared content hash length matches the digest size for the suite.
/// (Type-level true for [`Hash32`], but checked explicitly for wire-decoded lengths.)
pub fn hash_length_ok(suite_id: u16, hash_len: usize) -> bool {
    SuiteId::from_u16(suite_id).is_some_and(|s| s.hash_len() == hash_len)
}

/// Invariant 4: declared size is in `(0, max_file_size]`.
pub fn size_in_bounds(size: u64, max_file_size: u64) -> bool {
    size > 0 && size <= max_file_size
}

/// Invariant 5: `content_type` is in the closed allow-list for this protocol version.
pub fn content_type_allowed(content_type: &str, allowed: &[&str]) -> bool {
    allowed.contains(&content_type)
}

/// Part of invariant 6: the album's pinned `protocol_version` equals the request's.
pub fn album_pin_matches(request_protocol: &str, album_pin: &str) -> bool {
    request_protocol == album_pin
}

/// Invariant 7 (time half): the device's directory `added_at` precedes the manifest
/// `timestamp`. `None` on an unparseable timestamp (the caller rejects with `400`).
pub fn device_added_before(added_at: &str, timestamp: &str) -> Option<bool> {
    let a = chrono::DateTime::parse_from_rfc3339(added_at).ok()?;
    let t = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    Some(a <= t)
}

/// Invariant 8: the self-asserted `timestamp` is within `±drift_days` of the server clock.
/// A non-security gross-drift guard. `None` on an unparseable timestamp.
pub fn timestamp_within_drift(
    timestamp: &str,
    server_clock: &str,
    drift_days: i64,
) -> Option<bool> {
    let t = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let now = chrono::DateTime::parse_from_rfc3339(server_clock).ok()?;
    let delta = (t - now).num_days().abs();
    Some(delta <= drift_days)
}

/// Invariant 17: `prior_provenance_hash` equals the last accepted manifest's hash for this
/// asset. `stored_head` is `None` for a never-seen asset (only a `create` is valid then).
pub fn prior_hash_matches(
    prior: Option<Hash32>,
    stored_head: Option<Hash32>,
    is_create: bool,
) -> bool {
    if is_create {
        prior.is_none() && stored_head.is_none()
    } else {
        prior.is_some() && prior == stored_head
    }
}

/// Invariant 18: `amk_version` never regresses for an album (server's structural backstop;
/// MLS is the authority on the ceiling — see [`verify_asset`](crate::crypto::verify_asset)).
pub fn amk_version_monotonic(new: u32, stored: Option<u32>) -> bool {
    match stored {
        None => true,
        Some(prev) => new >= prev,
    }
}

/// A keyless envelope decision over a manifest core plus the server-known context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeReject {
    /// `crypto_suite_id` unknown (invariant 2).
    UnknownSuite,
    /// Album pin mismatch (invariant 6).
    AlbumPinMismatch,
    /// Device `added_at` postdates the manifest timestamp (invariant 7).
    DeviceAddedAfter,
    /// Timestamp unparseable or beyond the drift bound (invariant 8).
    TimestampUnsane,
    /// `prior_provenance_hash` does not match the stored chain head (invariant 17).
    StaleChain,
    /// `amk_version` regressed (invariant 18).
    AmkRegressed,
}

/// Context a key-less server holds when validating a non-upload lifecycle manifest.
pub struct EnvelopeContext<'a> {
    /// The album's immutable protocol pin.
    pub album_pin: &'a str,
    /// The device's `added_at` from the published directory.
    pub device_added_at: &'a str,
    /// The server's trusted clock (RFC3339).
    pub server_clock: &'a str,
    /// Allowed timestamp drift in days.
    pub drift_days: i64,
    /// The last accepted provenance head for this asset.
    pub stored_chain_head: Option<Hash32>,
    /// The last accepted `amk_version` for this album.
    pub stored_amk_version: Option<u32>,
}

/// Run the keyless envelope checks (2, 6, 7, 8, 17, 18) over a lifecycle manifest core.
/// Returns the first invariant violated, or `Ok(())`.
pub fn check_manifest_envelope(
    core: &ManifestCore,
    ctx: &EnvelopeContext<'_>,
) -> Result<(), EnvelopeReject> {
    if SuiteId::from_u16(core.crypto_suite_id).is_none() {
        return Err(EnvelopeReject::UnknownSuite);
    }
    if !album_pin_matches(&core.protocol_version, ctx.album_pin) {
        return Err(EnvelopeReject::AlbumPinMismatch);
    }
    match device_added_before(ctx.device_added_at, &core.timestamp) {
        None => return Err(EnvelopeReject::TimestampUnsane),
        Some(false) => return Err(EnvelopeReject::DeviceAddedAfter),
        Some(true) => {}
    }
    match timestamp_within_drift(&core.timestamp, ctx.server_clock, ctx.drift_days) {
        None | Some(false) => return Err(EnvelopeReject::TimestampUnsane),
        Some(true) => {}
    }
    if !prior_hash_matches(
        core.prior_provenance_hash,
        ctx.stored_chain_head,
        core.action.is_create(),
    ) {
        return Err(EnvelopeReject::StaleChain);
    }
    if !amk_version_monotonic(core.amk_version.0, ctx.stored_amk_version) {
        return Err(EnvelopeReject::AmkRegressed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::CRYPTO_SUITE_ID;
    use crate::crypto::keys::{AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::PROTOCOL_VERSION;
    use crate::crypto::provenance::action::Action;
    use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, ManifestCore};
    use uuid::Uuid;

    #[test]
    fn hash_length_and_size_and_content_type() {
        assert!(hash_length_ok(CRYPTO_SUITE_ID, 32));
        assert!(!hash_length_ok(CRYPTO_SUITE_ID, 31));
        assert!(!hash_length_ok(0x9999, 32));

        assert!(size_in_bounds(1, 100));
        assert!(size_in_bounds(100, 100));
        assert!(!size_in_bounds(0, 100));
        assert!(!size_in_bounds(101, 100));

        let allowed = ["image/jpeg", "image/heic"];
        assert!(content_type_allowed("image/jpeg", &allowed));
        assert!(!content_type_allowed("application/x-evil", &allowed));
    }

    #[test]
    fn timestamp_and_added_at_rules() {
        assert_eq!(
            device_added_before("2026-05-30T00:00:00Z", "2026-05-31T00:00:00Z"),
            Some(true)
        );
        assert_eq!(
            device_added_before("2026-06-01T00:00:00Z", "2026-05-31T00:00:00Z"),
            Some(false)
        );
        assert_eq!(device_added_before("bogus", "2026-05-31T00:00:00Z"), None);

        assert_eq!(
            timestamp_within_drift("2026-05-31T00:00:00Z", "2026-05-31T00:00:00Z", 30),
            Some(true)
        );
        assert_eq!(
            timestamp_within_drift("2026-01-01T00:00:00Z", "2026-05-31T00:00:00Z", 30),
            Some(false)
        );
    }

    #[test]
    fn prior_hash_and_amk_monotonic() {
        let h = Hash32([1; 32]);
        // create: prior None, no stored head.
        assert!(prior_hash_matches(None, None, true));
        assert!(!prior_hash_matches(Some(h), None, true));
        // non-create: prior must equal stored head.
        assert!(prior_hash_matches(Some(h), Some(h), false));
        assert!(!prior_hash_matches(Some(h), Some(Hash32([2; 32])), false));
        assert!(!prior_hash_matches(None, Some(h), false));

        assert!(amk_version_monotonic(1, None));
        assert!(amk_version_monotonic(3, Some(2)));
        assert!(amk_version_monotonic(2, Some(2)));
        assert!(!amk_version_monotonic(1, Some(2)));
    }

    fn core(action: Action, prior: Option<Hash32>, amk: u32) -> ManifestCore {
        let dev = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let wt = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let c = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: Uuid::from_u128(1),
            album_id: Uuid::from_u128(2),
            amk_version: AmkVersion(amk),
            ciphertext_hash: Hash32([0; 32]),
            plaintext_size: 10,
            chunk_size: 65_520,
            nonce_prefix: [0; 7],
            created_by_user: Uuid::from_u128(3),
            created_by_device: Uuid::from_u128(4),
            client_version: "t".into(),
            timestamp: "2026-05-31T00:00:00Z".into(),
            action,
            prior_provenance_hash: prior,
            retention_until: None,
        };
        let _ = c.clone().sign(&dev, &wt); // ensure it's a well-formed signable core
        c
    }

    fn ctx<'a>(head: Option<Hash32>, amk: Option<u32>) -> EnvelopeContext<'a> {
        EnvelopeContext {
            album_pin: PROTOCOL_VERSION,
            device_added_at: "2026-05-30T00:00:00Z",
            server_clock: "2026-05-31T01:00:00Z",
            drift_days: 30,
            stored_chain_head: head,
            stored_amk_version: amk,
        }
    }

    #[test]
    fn envelope_accepts_valid_create_and_update() {
        let c = core(Action::Create, None, 1);
        assert_eq!(check_manifest_envelope(&c, &ctx(None, None)), Ok(()));

        let head = Hash32([9; 32]);
        let u = core(Action::MetadataUpdate, Some(head), 1);
        assert_eq!(
            check_manifest_envelope(&u, &ctx(Some(head), Some(1))),
            Ok(())
        );
    }

    #[test]
    fn envelope_rejects_each_invariant() {
        // Album pin mismatch.
        let mut c = core(Action::Create, None, 1);
        c.protocol_version = "1999-01-01".into();
        assert_eq!(
            check_manifest_envelope(&c, &ctx(None, None)),
            Err(EnvelopeReject::AlbumPinMismatch)
        );

        // Unknown suite.
        let mut c = core(Action::Create, None, 1);
        c.crypto_suite_id = 0x9999;
        assert_eq!(
            check_manifest_envelope(&c, &ctx(None, None)),
            Err(EnvelopeReject::UnknownSuite)
        );

        // Stale chain (update whose prior != stored head).
        let u = core(Action::MetadataUpdate, Some(Hash32([1; 32])), 1);
        assert_eq!(
            check_manifest_envelope(&u, &ctx(Some(Hash32([2; 32])), Some(1))),
            Err(EnvelopeReject::StaleChain)
        );

        // AMK regression.
        let u = core(Action::MetadataUpdate, Some(Hash32([2; 32])), 1);
        assert_eq!(
            check_manifest_envelope(&u, &ctx(Some(Hash32([2; 32])), Some(5))),
            Err(EnvelopeReject::AmkRegressed)
        );

        // Device added after the manifest timestamp.
        let c = core(Action::Create, None, 1);
        let mut bad = ctx(None, None);
        bad.device_added_at = "2027-01-01T00:00:00Z";
        assert_eq!(
            check_manifest_envelope(&c, &bad),
            Err(EnvelopeReject::DeviceAddedAfter)
        );
    }
}
