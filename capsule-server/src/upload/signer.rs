//! The signer check: a provenance blob is admitted only as a manifest its signer signed.
//!
//! # Why the write path decodes this one blob
//!
//! The rest of the upload surface is key-free: it validates the envelope projection a client
//! declared ([`super::envelope`]) and stores bytes it never opens. That is enough while the
//! uploader and the signer are the same device, and it stops being enough the moment they are
//! not. [Peering § Delegation] lets one device of an account upload a bundle another device of
//! the same account sealed and signed, and [Transfer — Upload] fixes the rule that makes that
//! safe: *the signature is verified against the signer, never the uploader*.
//!
//! The server cannot tell the two cases apart. A session's `device_id` is advisory — recorded
//! for the devices view, gating nothing — so "this upload comes from the device that signed
//! it" is a claim no request can prove. The check therefore runs on **every** provenance blob,
//! delegated or not: a manifest is admitted on its signature under the key the uploader's
//! published directory holds for the device it names, and never on the uploader's identity
//! alone.
//!
//! # What it checks
//!
//! [`check_signed_manifest`], over the exact bytes finalization hashed:
//!
//! 1. The bytes are a manifest ([`crate::ledger::manifest::parse_valid`], the server's one
//!    manifest parser, bounded and fuzzed): a provenance record of a version this build reads
//!    whose inner manifest agrees with it and keeps the structural presence rules. A record a
//!    newer client wrote cannot be verified here, so it is refused rather than admitted unread;
//!    the client keeps it queued until the server is updated (versioning invariant 5). The
//!    bytes must also be the canonical encoding of what they decode to: the chain head is their
//!    hash, so a re-spelling of the same values is not the manifest the signer produced.
//! 2. The signed core says what the session's envelope said: asset, album, author, device,
//!    action, chain position, epoch and the bundle it commits to. Otherwise a valid signature
//!    over one manifest would carry an envelope describing another.
//! 3. The device it names is in the uploader's published directory, not revoked, added no later
//!    than it claims to have signed, and its key verifies `device_sig`
//!    ([`verify_device_signature`], the same check a rebuild makes).
//!
//! A device of another account fails 2 or 3 (its manifest names another `created_by_user`, or a
//! device this directory does not list); a revoked device fails 3; a delegate that altered a
//! byte of the manifest fails 3, and one that swapped the envelope fails 2.
//!
//! [Peering § Delegation]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/peering.md#delegation
//! [Transfer — Upload]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/transfer/upload.md#quota-and-permissions

use capsule_core::crypto::keys::DeviceDirectory;
use capsule_core::crypto::provenance::{AssetManifest, ProvenanceRecord};
use capsule_core::crypto::verify_asset::verify_device_signature;
use uuid::Uuid;

use super::envelope::{GateReject, ManifestEnvelope};
use crate::ledger::manifest::{ManifestFault, parse_valid};
use crate::store::UserId;

/// Why a provenance blob was not admitted as its signer's manifest.
///
/// Finer than the wire answer, which is one `error.upload.envelope_rejected` for all of them:
/// the variant is the operator's log line ([`Self::as_str`]), and [`Self::gate`] is what the
/// client is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignerReject {
    /// The bytes are not a manifest this build can verify.
    #[error("the provenance blob is not a verifiable manifest: {0}")]
    NotAManifest(ManifestFault),
    /// The bytes decode, but are not the canonical encoding of what they decode to. The signed
    /// values may be intact, yet the chain head is the hash of the bytes, so a re-spelling would
    /// leave the server holding a head the signer's next record does not chain onto.
    #[error("the provenance blob is not canonically encoded")]
    NotCanonical,
    /// The signed manifest disagrees with the envelope the session was opened under. Carries
    /// the field.
    #[error("the signed manifest's {0} is not the envelope's")]
    EnvelopeDisagrees(&'static str),
    /// The uploader has published no device directory, so no key can verify the signer.
    #[error("the uploader has no published device directory")]
    NoDirectory,
    /// The device the manifest names is revoked in the uploader's directory.
    #[error("the signing device is revoked")]
    Revoked,
    /// The signer is not a device of the uploader's directory, was added after it claims to
    /// have signed, or its key does not verify the signature.
    #[error("the device signature does not verify under the uploader's directory")]
    BadSignature,
}

impl SignerReject {
    /// A short stable name, for a log field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAManifest(_) => "not_a_manifest",
            Self::NotCanonical => "not_canonical",
            Self::EnvelopeDisagrees(_) => "envelope_disagrees",
            Self::NoDirectory => "no_directory",
            Self::Revoked => "revoked",
            Self::BadSignature => "bad_signature",
        }
    }

    /// The gate rejection this becomes at finalization.
    #[must_use]
    pub fn gate(self) -> GateReject {
        match self {
            Self::NoDirectory | Self::Revoked => GateReject::DeviceNotAuthorized,
            Self::EnvelopeDisagrees(field) => GateReject::EnvelopeMismatch(field),
            Self::NotAManifest(_) | Self::NotCanonical | Self::BadSignature => {
                GateReject::SignatureUnverified
            }
        }
    }
}

/// Admit `bytes` — a provenance blob's exact bytes — only as a manifest that agrees with
/// `envelope` and that the device it names signed, under the key `uploader`'s published
/// `directory` holds for that device.
///
/// # Errors
///
/// The [`SignerReject`] naming the first check that failed.
pub fn check_signed_manifest(
    bytes: &[u8],
    envelope: &ManifestEnvelope,
    uploader: &UserId,
    directory: Option<&DeviceDirectory>,
) -> Result<AssetManifest, SignerReject> {
    let manifest = parse_valid(bytes).map_err(SignerReject::NotAManifest)?;
    if !is_canonical(bytes, &manifest) {
        return Err(SignerReject::NotCanonical);
    }
    agrees_with_envelope(&manifest, envelope)?;
    if manifest.core.created_by_user.to_string() != uploader.as_str() {
        return Err(SignerReject::EnvelopeDisagrees("created_by_user"));
    }
    let directory = directory.ok_or(SignerReject::NoDirectory)?;
    if directory
        .device(&manifest.core.created_by_device)
        .is_some_and(|entry| entry.revoked_at.is_some())
    {
        return Err(SignerReject::Revoked);
    }
    verify_device_signature(&manifest, directory).map_err(|reason| {
        tracing::debug!(?reason, "the manifest's device signature was refused");
        SignerReject::BadSignature
    })?;
    Ok(manifest)
}

/// Whether the signed core says what the envelope declared, field by field.
///
/// The envelope's `ciphertext_hash` names the session's own blob (the provenance blob here), not
/// the manifest's original, so it is not compared; `original_blob_hash`, which names the
/// original, is — when the envelope carries it.
fn agrees_with_envelope(
    manifest: &AssetManifest,
    envelope: &ManifestEnvelope,
) -> Result<(), SignerReject> {
    let core = &manifest.core;
    let check = |holds: bool, field: &'static str| {
        if holds {
            Ok(())
        } else {
            Err(SignerReject::EnvelopeDisagrees(field))
        }
    };
    check(same_uuid(&envelope.file_id, core.file_id), "file_id")?;
    check(
        envelope
            .album_id
            .as_deref()
            .is_some_and(|album| same_uuid(album, core.album_id)),
        "album_id",
    )?;
    check(
        same_uuid(&envelope.created_by_user, core.created_by_user),
        "created_by_user",
    )?;
    check(
        same_uuid(&envelope.created_by_device, core.created_by_device),
        "created_by_device",
    )?;
    check(wire(&core.action) == envelope.action, "action")?;
    check(wire(&core.key_mode) == envelope.key_mode, "key_mode")?;
    check(
        core.prior_provenance_hash.map(|hash| hash.to_hex()) == envelope.prior_provenance_hash,
        "prior_provenance_hash",
    )?;
    check(
        core.metadata_blob_hash.map(|hash| hash.to_hex()) == envelope.metadata_blob_hash,
        "metadata_blob_hash",
    )?;
    check(
        envelope
            .original_blob_hash
            .as_ref()
            .is_none_or(|original| *original == core.ciphertext_hash.to_hex()),
        "original_blob_hash",
    )?;
    check(core.amk_version.0 == envelope.amk_version, "amk_version")?;
    check(
        core.crypto_suite_id == envelope.crypto_suite_id,
        "crypto_suite_id",
    )?;
    check(
        core.protocol_version == envelope.protocol_version,
        "protocol_version",
    )?;
    check(core.timestamp == envelope.timestamp, "timestamp")?;
    check(
        core.client_version == envelope.client_version,
        "client_version",
    )?;
    check(
        core.plaintext_size == envelope.plaintext_size,
        "plaintext_size",
    )?;
    check(core.chunk_size == envelope.chunk_size, "chunk_size")?;
    check(
        core.retention_until == envelope.retention_until,
        "retention_until",
    )?;
    Ok(())
}

/// Whether `bytes` are the canonical encoding of what they decoded to: the provenance record
/// when they are one, else the bare manifest a record-less blob carries.
fn is_canonical(bytes: &[u8], manifest: &AssetManifest) -> bool {
    let reencoded = match ProvenanceRecord::decode(bytes) {
        Ok(record) => capsule_core::cbor::to_canonical_vec(&record),
        Err(_) => capsule_core::cbor::to_canonical_vec(manifest),
    };
    reencoded.is_ok_and(|reencoded| reencoded == bytes)
}

/// Whether `text` parses as exactly `id`.
fn same_uuid(text: &str, id: Uuid) -> bool {
    Uuid::parse_str(text).is_ok_and(|parsed| parsed == id)
}

/// A wire enum's bare protocol string.
fn wire<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
