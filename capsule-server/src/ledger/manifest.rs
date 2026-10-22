//! The server's one manifest parser: the only place it decodes signed CBOR a client wrote.
//!
//! # Why one module
//!
//! [Server Filesystem § Rebuild] confines manifest decoding to a single module, shared by the
//! index rebuild and the [integrity scrub](crate::scrub), and fuzzed. The
//! write path never decodes a manifest: it validates the envelope projection a client declared
//! and stores the manifest's bytes verbatim. Decoding is a recovery and audit act, off the hot
//! path, so a parser bug can cost a rebuild a row or a scrub a finding, and never an upload.
//!
//! Every byte this module reads came from a client, so it is bounded before it is decoded
//! ([`MAX_MANIFEST_BYTES`]), and it decodes through `capsule_core`'s own types rather than a
//! shape defined here: a second implementation of what a manifest says would be a second answer
//! to a question the threat model allows one answer to.
//!
//! # What a provenance blob holds
//!
//! The canonical CBOR of the chain head's [`ProvenanceRecord`]: the signed manifest, the asset
//! it belongs to and the hash of the record before it (`UploadBundle::provenance_blob`). The
//! record and not the bare manifest, because a client's next `prior_provenance_hash` is the
//! record's hash. [`parse`] decodes the record and hands back the manifest inside it, after
//! checking the two agree about the asset and the link. A bare [`AssetManifest`] is accepted
//! too, for blobs written before the record wrapped it. A record a newer client wrote is
//! [`ManifestFault::NewerVersion`]: unreadable here, and never to be moved or rewritten
//! (versioning invariants 3 and 5).
//!
//! # Decoding is not validating
//!
//! [`parse`] answers "is this a manifest", and [`check_structure`] answers "is it a well-formed
//! one" ([`AssetManifest::structural_ok`]). They are separate because their callers want
//! different things from a malformed one: the scrub classifies a non-`create` with no
//! predecessor as a broken chain, which needs the decoded manifest, while a rebuild refuses to
//! replay it and quarantines the bytes. [`parse_valid`] is the two together.
//!
//! Signatures are not checked here. That needs the uploading device's published key, which is
//! the client's `verify_asset` and, for a rebuild, the device directory the ledger recorded.
//!
//! [Server Filesystem § Rebuild]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/filesystem/server.md#rebuild

use capsule_core::crypto::provenance::{AssetManifest, ProvenanceRecord, RecordDecodeError};

use crate::blob::{BlobError, BlobStore, ContentAddress};

/// The largest manifest the parser will decode.
///
/// A signed manifest is a few kilobytes: two hybrid signatures and a handful of hashes. The
/// ceiling is the one the sync feed already applies before it inlines a manifest, so a blob the
/// feed would refuse to serve is a blob this refuses to decode.
pub const MAX_MANIFEST_BYTES: u64 = crate::sync::MAX_MANIFEST_BYTES;

/// Why bytes are not a manifest the server can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ManifestFault {
    /// Longer than [`MAX_MANIFEST_BYTES`]. Refused before a byte is decoded.
    #[error("the manifest is {size} bytes, over the {MAX_MANIFEST_BYTES}-byte ceiling")]
    TooLarge {
        /// Its length.
        size: u64,
    },
    /// Not the CBOR encoding of a provenance record or an asset manifest.
    #[error("the bytes do not decode as a provenance record or an asset manifest")]
    Undecodable,
    /// A provenance record of a version this build does not read: a newer client wrote it.
    ///
    /// Not damage. Kept verbatim and never quarantined: it becomes readable when the server is
    /// updated.
    #[error("the provenance record is version {found}, newer than this build reads")]
    NewerVersion {
        /// The version it declares.
        found: u64,
    },
    /// A record whose manifest names another asset, or another predecessor, than the record.
    #[error("the provenance record and the manifest inside it disagree")]
    RecordMismatch,
    /// A manifest whose fields break a presence rule ([`AssetManifest::structural_ok`]).
    #[error("the manifest breaks a structural presence rule")]
    Structural,
}

impl ManifestFault {
    /// A short stable name, for a log field and a report line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "too_large",
            Self::Undecodable => "undecodable",
            Self::NewerVersion { .. } => "newer_version",
            Self::RecordMismatch => "record_mismatch",
            Self::Structural => "structural",
        }
    }
}

/// Decode `bytes` as an asset manifest.
///
/// # Errors
///
/// [`ManifestFault::TooLarge`] past the ceiling, [`ManifestFault::Undecodable`] for anything
/// that is not a manifest. Never panics, whatever the bytes: that is what the fuzz target holds
/// it to.
pub fn parse(bytes: &[u8]) -> Result<AssetManifest, ManifestFault> {
    let size = bytes.len() as u64;
    if size > MAX_MANIFEST_BYTES {
        tracing::debug!(size, "a manifest over the ceiling was not decoded");
        return Err(ManifestFault::TooLarge { size });
    }
    match ProvenanceRecord::decode(bytes) {
        Ok(record) => {
            if record.manifest.core.file_id != record.asset_id
                || record.manifest.core.prior_provenance_hash != record.prior_provenance_hash
            {
                tracing::debug!(size, "a provenance record disagrees with its manifest");
                return Err(ManifestFault::RecordMismatch);
            }
            Ok(record.manifest)
        }
        Err(RecordDecodeError::UnsupportedVersion { found, .. }) => {
            tracing::debug!(
                size,
                found,
                "a provenance record is newer than this build reads"
            );
            Err(ManifestFault::NewerVersion { found })
        }
        // Not a record: a bare manifest, from before the record wrapped it, or nothing at all.
        Err(RecordDecodeError::Malformed(_)) => {
            capsule_core::cbor::from_slice::<AssetManifest>(bytes).map_err(|error| {
                tracing::debug!(size, %error, "bytes did not decode as a record or a manifest");
                ManifestFault::Undecodable
            })
        }
    }
}

/// Whether a decoded manifest keeps the key-free presence rules.
///
/// # Errors
///
/// [`ManifestFault::Structural`] when it does not.
pub fn check_structure(manifest: &AssetManifest) -> Result<(), ManifestFault> {
    if manifest.structural_ok() {
        Ok(())
    } else {
        Err(ManifestFault::Structural)
    }
}

/// [`parse`], then [`check_structure`]: a manifest a rebuild may replay.
///
/// # Errors
///
/// Whichever of the two refused.
pub fn parse_valid(bytes: &[u8]) -> Result<AssetManifest, ManifestFault> {
    let manifest = parse(bytes)?;
    check_structure(&manifest)?;
    Ok(manifest)
}

/// Read the manifest stored at `address` and [`parse`] it.
///
/// `None` when the store does not hold the blob. A blob over the ceiling is refused from its
/// `stat` without reading it, so a corrupted store cannot make a recovery walk load an
/// arbitrary object into memory.
///
/// # Errors
///
/// The store's own failure. A blob that is not a manifest is `Some(Err(_))`, not an error: the
/// callers walk a damaged store and must keep walking.
pub async fn read(
    blobs: &dyn BlobStore,
    address: &ContentAddress,
) -> Result<Option<Result<AssetManifest, ManifestFault>>, BlobError> {
    let Some(stat) = blobs.stat(address).await? else {
        return Ok(None);
    };
    if stat.size > MAX_MANIFEST_BYTES {
        return Ok(Some(Err(ManifestFault::TooLarge { size: stat.size })));
    }
    let span = usize::try_from(stat.size).unwrap_or(usize::MAX);
    let Some(bytes) = blobs.read_at(address, 0, span).await? else {
        return Ok(None);
    };
    Ok(Some(parse(&bytes)))
}

#[cfg(test)]
mod tests;
