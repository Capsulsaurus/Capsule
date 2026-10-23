//! The key-less structural envelope checks a server runs before persisting any write
//! (SSoT: [Threat Model — Server-Side Validation Invariants]). The server holds no keys,
//! so it cannot verify signatures — but it validates *structure* refuse-by-default. These
//! are pure predicates over the manifest core and server-known state; the client mirrors
//! them via [`verify_asset`](crate::crypto::verify_asset::verify_asset).
//!
//! [Threat Model — Server-Side Validation Invariants]: https://docs/design/threat-model/validation/#server-side-validation-invariants

use crate::crypto::encryption::blob_ciphertext_hash;
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
    let a = added_at.parse::<jiff::Timestamp>().ok()?;
    let t = timestamp.parse::<jiff::Timestamp>().ok()?;
    Some(a <= t)
}

/// Invariant 8: the self-asserted `timestamp` is within `±drift_days` of the server clock.
/// A non-security gross-drift guard. `None` on an unparseable timestamp.
pub fn timestamp_within_drift(
    timestamp: &str,
    server_clock: &str,
    drift_days: i64,
) -> Option<bool> {
    let t = timestamp.parse::<jiff::Timestamp>().ok()?;
    let now = server_clock.parse::<jiff::Timestamp>().ok()?;
    // Absolute-time days: 24 h, matching the drift guard's UTC semantics.
    let delta = (t.as_second() - now.as_second()).abs() / 86_400;
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
/// MLS is the authority on the ceiling — see [`verify_asset`](crate::crypto::verify_asset::verify_asset)).
pub fn amk_version_monotonic(new: u32, stored: Option<u32>) -> bool {
    match stored {
        None => true,
        Some(prev) => new >= prev,
    }
}

/// Invariant 25 (key-free): the metadata blob in the bundle has a content hash equal to the
/// manifest's committed `metadata_blob_hash`. The server holds no key, but it hashes the
/// ciphertext object it stores and compares it against the signed manifest field, so a client
/// cannot present a metadata blob different from the one its manifest is signed over. `false`
/// when the manifest commits to no hash — a bundle carrying a blob must be committed to.
///
/// This is the value half of the client-side round-trip check
/// ([`verify_metadata_binding`](crate::crypto::verify_asset::verify_metadata_binding)); the server never
/// decrypts, so it checks only the content address.
pub fn metadata_blob_hash_matches(committed: Option<Hash32>, metadata_blob: &[u8]) -> bool {
    committed.is_some_and(|h| blob_ciphertext_hash(metadata_blob) == h)
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
    /// The bundled metadata blob's content hash does not match the manifest's committed
    /// `metadata_blob_hash` (invariant 25).
    MetadataBlobHashMismatch,
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

/// Invariant 25 as a refuse-by-default decision: run on any write whose bundle carries a
/// metadata blob (a `create` bundle, at finalization, or a non-upload `metadata-update`). The
/// blob's content hash must equal the manifest's committed `metadata_blob_hash`. Key-free — the
/// server never decrypts (SSoT: [Threat Model — invariant 25], [Metadata — Local and Server
/// Metadata Equivalence]).
///
/// [Threat Model — invariant 25]: https://docs/design/threat-model/validation/#server-side-validation-invariants
/// [Metadata — Local and Server Metadata Equivalence]: https://docs/design/metadata/#local-and-server-metadata-equivalence
pub fn check_metadata_blob_envelope(
    core: &ManifestCore,
    metadata_blob: &[u8],
) -> Result<(), EnvelopeReject> {
    if metadata_blob_hash_matches(core.metadata_blob_hash, metadata_blob) {
        Ok(())
    } else {
        Err(EnvelopeReject::MetadataBlobHashMismatch)
    }
}

#[cfg(test)]
mod tests;
