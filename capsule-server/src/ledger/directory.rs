//! The device-directory versions a rebuild verifies manifests against, from the blob root alone.
//!
//! [Server Filesystem § Rebuild] step 2 checks each replayed manifest's `device_sig` against the
//! directory version the ledger recorded as current when the manifest was accepted. The ledger
//! entry ([`LedgerEvent::Directory`](super::LedgerEvent::Directory)) names the signed document
//! only by hash, and [`crate::directory::DeviceDirectoryStore`] keeps only the version currently
//! in force, in the database a rebuild may be running without. So a publish **attaches** a
//! [`DirectoryCopy`] — the signed document verbatim and the identity key it verifies under —
//! beside the ledger under the document's hash ([`super::OwnerLedger::attach`]) before it
//! appends the entry, and a rebuild reads every version back from there.
//!
//! # Re-running the store's checks
//!
//! The entry is written before the store commits, so the ledger records **offers**, and an offer
//! the store refused leaves its entry behind. [`DirectoryHistory::record`] replays each owner's
//! offers in ledger order through the two checks the store makes: the version must strictly
//! advance over the last accepted one, and the identity key must be the account's anchor, the key
//! of the first accepted offer. The copy is trusted only once its document hashes to what the
//! signed entry recorded and verifies, as a directory for that account, under the key beside it
//! ([`crate::directory::project_version`]), so a copy cannot be swapped for another document, and
//! the key beside it can only be one the document verifies under.
//!
//! An offer whose copy is missing or unusable is still in force from its position if its version
//! advances (the store may have accepted it), but nothing verifies against it: a manifest that
//! falls under it is refused as [`Refusal::DirectoryUnavailable`], and the copy is reported.
//!
//! # Which version is in force for a manifest
//!
//! A user's offers are in that user's own ledger. For a manifest in the same ledger, the version
//! in force is the last accepted offer **before its entry**, which is exact. A manifest a shared
//! album's member signed sits in the album owner's ledger, and two ledgers have no common
//! order, so it is the last accepted offer whose entry the server's clock wrote **no later than**
//! the manifest's entry.
//!
//! [Server Filesystem § Rebuild]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/filesystem/server.md#rebuild

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::DeviceDirectory;
use capsule_core::crypto::provenance::AssetManifest;
use capsule_core::crypto::verify_asset::{RejectReason, verify_device_signature};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::replay::{self, DirectoryOffer};
use super::{LedgerEntry, LedgerError, OwnerLedger};
use crate::store::{OwnerId, UserId};

/// The version of the [`DirectoryCopy`] encoding this server writes and reads.
pub const DIRECTORY_COPY_VERSION: u64 = 1;

/// What a device-directory publish attaches beside its ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryCopy {
    /// The identity public key the document verifies under, as the publish presented it.
    pub identity_key: Vec<u8>,
    /// The client-signed document, exactly as it arrived.
    pub document: Vec<u8>,
}

/// A byte string that encodes as a CBOR byte string, not an array of integers.
#[derive(Debug)]
struct Bytes(Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Bytes;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a byte string")
            }

            fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Bytes, E> {
                Ok(Bytes(value.to_vec()))
            }

            fn visit_byte_buf<E: serde::de::Error>(self, value: Vec<u8>) -> Result<Bytes, E> {
                Ok(Bytes(value))
            }
        }
        deserializer.deserialize_byte_buf(Visitor)
    }
}

/// The attachment's wire shape.
#[derive(Debug, Serialize, Deserialize)]
struct Wire {
    version: u64,
    identity_key: Bytes,
    document: Bytes,
}

impl DirectoryCopy {
    /// The attachment's bytes: a canonical CBOR map of the version, the key and the document.
    pub fn encode(&self) -> Vec<u8> {
        capsule_core::cbor::to_canonical_vec(&Wire {
            version: DIRECTORY_COPY_VERSION,
            identity_key: Bytes(self.identity_key.clone()),
            document: Bytes(self.document.clone()),
        })
        .expect("a directory copy encodes")
    }

    /// Read an attachment back.
    ///
    /// # Errors
    ///
    /// [`DirectoryFault::Undecodable`] for bytes that are not a copy, and
    /// [`DirectoryFault::NewerVersion`] for a copy a newer server wrote.
    pub fn decode(bytes: &[u8]) -> Result<Self, DirectoryFault> {
        let wire: Wire =
            capsule_core::cbor::from_slice(bytes).map_err(|_| DirectoryFault::Undecodable)?;
        if wire.version != DIRECTORY_COPY_VERSION {
            return Err(DirectoryFault::NewerVersion {
                found: wire.version,
            });
        }
        Ok(Self {
            identity_key: wire.identity_key.0,
            document: wire.document.0,
        })
    }
}

/// Why a ledgered directory offer's copy cannot be verified against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryFault {
    /// No attachment under the document's hash.
    #[error("the directory copy is missing")]
    Missing,
    /// The attachment is past [`super::MAX_ATTACHMENT_BYTES`], so it was not read.
    #[error("the directory copy is too large to be one")]
    TooLarge,
    /// The attachment is not a directory copy.
    #[error("the directory copy does not decode")]
    Undecodable,
    /// A copy of a version this build does not read: a newer server wrote it.
    #[error("the directory copy is version {found}, newer than this build reads")]
    NewerVersion {
        /// The version it declares.
        found: u64,
    },
    /// The copy's document is not the one the entry recorded.
    #[error("the directory copy's document is not the one the ledger recorded")]
    HashMismatch,
    /// The document is not this account's directory, or does not verify under the key beside it.
    #[error("the directory copy does not verify as this account's directory")]
    Unverifiable,
    /// The document's version is not the one the entry recorded.
    #[error("the directory copy's version is not the one the ledger recorded")]
    VersionMismatch,
}

/// Why a replayed manifest is not restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The signer's account had no accepted directory at the manifest's position.
    NoDirectory,
    /// The directory in force there has no usable copy, so nothing can be verified against it.
    DirectoryUnavailable {
        /// The ledger position of that directory's entry, in the signer's ledger.
        directory_seq: u64,
    },
    /// The directory in force there lists the signing device as revoked.
    Revoked,
    /// The device check against the directory in force failed
    /// ([`verify_device_signature`]).
    Device(RejectReason),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDirectory => f.write_str("the signer had no device directory in force"),
            Self::DirectoryUnavailable { directory_seq } => write!(
                f,
                "the device directory in force (ledger entry {directory_seq}) has no usable copy"
            ),
            Self::Revoked => {
                f.write_str("the signing device was revoked in the directory in force")
            }
            Self::Device(RejectReason::UnknownDevice) => {
                f.write_str("the signing device is not in the directory in force")
            }
            Self::Device(RejectReason::DeviceAddedAfter) => {
                f.write_str("the signing device was added after the manifest's timestamp")
            }
            Self::Device(RejectReason::BadTimestamp) => {
                f.write_str("a timestamp the device check compares does not parse")
            }
            Self::Device(RejectReason::BadDeviceSig) => {
                f.write_str("the device signature does not verify")
            }
            Self::Device(other) => write!(f, "the device check refused it ({other:?})"),
        }
    }
}

/// Where a manifest sits: the ledger it was replayed from, its entry's position and its time.
#[derive(Debug, Clone, Copy)]
pub struct Position<'a> {
    /// The ledger the manifest's entry is in.
    pub owner: &'a OwnerId,
    /// The entry's position in it.
    pub seq: u64,
    /// The server's clock when the entry was written, when it parses.
    pub recorded_at: Option<Timestamp>,
}

/// One accepted offer.
#[derive(Debug)]
struct Point {
    seq: u64,
    recorded_at: Option<Timestamp>,
    /// `None` when its copy could not be used.
    directory: Option<Arc<DeviceDirectory>>,
}

/// Every account's accepted directory versions, read back from the owner ledgers.
#[derive(Debug, Default)]
pub struct DirectoryHistory {
    /// By account, in ledger order.
    users: BTreeMap<String, Vec<Point>>,
    /// By ledger: the offers whose copy could not be used, with their positions.
    faults: BTreeMap<OwnerId, Vec<(u64, DirectoryFault)>>,
}

impl DirectoryHistory {
    /// An empty history: every manifest is [`Refusal::NoDirectory`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Replay `owner`'s directory offers from `verified`, the prefix of its ledger that verified.
    ///
    /// # Errors
    ///
    /// The ledger's own failure reading an attachment. A missing or unusable copy is a fault
    /// recorded here, not an error.
    pub async fn record(
        &mut self,
        ledger: &dyn OwnerLedger,
        owner: &OwnerId,
        verified: &[LedgerEntry],
    ) -> Result<(), LedgerError> {
        let mut anchor: Option<Vec<u8>> = None;
        let mut stored: Option<u64> = None;
        let mut points = Vec::new();
        let mut faults = Vec::new();
        for offer in replay::directory(verified) {
            if offer.user_id != owner.as_str() {
                // A publish appends under the publishing account; anything else was not written
                // by a publish.
                tracing::warn!(%owner, seq = offer.ledger_seq, user = %offer.user_id, "a directory entry names another account than its ledger's; ignored");
                continue;
            }
            if stored.is_some_and(|version| offer.directory_version <= version) {
                tracing::debug!(%owner, seq = offer.ledger_seq, version = offer.directory_version, "a ledgered directory offer was stale; the store refused it");
                continue;
            }
            let recorded_at = offer.recorded_at.parse::<Timestamp>().ok();
            match copy(ledger, owner, &offer).await? {
                Ok((identity_key, directory)) => {
                    if anchor.as_ref().is_some_and(|held| *held != identity_key) {
                        tracing::debug!(%owner, seq = offer.ledger_seq, "a ledgered directory offer was under another identity key; the store refused it");
                        continue;
                    }
                    anchor.get_or_insert(identity_key);
                    points.push(Point {
                        seq: offer.ledger_seq,
                        recorded_at,
                        directory: Some(Arc::new(directory)),
                    });
                }
                Err(fault) => {
                    tracing::warn!(%owner, seq = offer.ledger_seq, version = offer.directory_version, %fault, "a ledgered directory version cannot be verified against");
                    faults.push((offer.ledger_seq, fault));
                    points.push(Point {
                        seq: offer.ledger_seq,
                        recorded_at,
                        directory: None,
                    });
                }
            }
            stored = Some(offer.directory_version);
        }
        tracing::debug!(%owner, versions = points.len(), faults = faults.len(), "read an account's device-directory history from its ledger");
        if !faults.is_empty() {
            self.faults.insert(owner.clone(), faults);
        }
        if !points.is_empty() {
            self.users.insert(owner.as_str().to_owned(), points);
        }
        Ok(())
    }

    /// The offers in `owner`'s ledger whose copy could not be used, with their positions.
    pub fn faults(&self, owner: &OwnerId) -> &[(u64, DirectoryFault)] {
        self.faults.get(owner).map_or(&[], Vec::as_slice)
    }

    /// Check `manifest`'s device signature against the directory in force at `at`.
    ///
    /// # Errors
    ///
    /// The [`Refusal`] that keeps the manifest from being restored.
    pub fn verify(&self, manifest: &AssetManifest, at: Position<'_>) -> Result<(), Refusal> {
        let user = manifest.core.created_by_user.to_string();
        let same_ledger = user == at.owner.as_str();
        let in_force = self.users.get(&user).and_then(|points| {
            points.iter().rev().find(|point| {
                if same_ledger {
                    point.seq < at.seq
                } else {
                    matches!((point.recorded_at, at.recorded_at), (Some(published), Some(accepted)) if published <= accepted)
                }
            })
        });
        let Some(point) = in_force else {
            return Err(Refusal::NoDirectory);
        };
        let Some(directory) = &point.directory else {
            return Err(Refusal::DirectoryUnavailable {
                directory_seq: point.seq,
            });
        };
        // The server refuses a revoked device outright when it accepts a write
        // (`crate::album::authority`), so one revoked in the version in force did not sign
        // anything the server accepted then. A later revocation is not in force here.
        if directory
            .device(&manifest.core.created_by_device)
            .is_some_and(|entry| entry.revoked_at.is_some())
        {
            return Err(Refusal::Revoked);
        }
        verify_device_signature(manifest, directory).map_err(Refusal::Device)
    }
}

/// The copy `offer` names, checked against the entry: its identity key and decoded directory.
async fn copy(
    ledger: &dyn OwnerLedger,
    owner: &OwnerId,
    offer: &DirectoryOffer,
) -> Result<Result<(Vec<u8>, DeviceDirectory), DirectoryFault>, LedgerError> {
    let bytes = match ledger.attachment(owner, &offer.document_hash).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(Err(DirectoryFault::Missing)),
        // Damage found, not a store that could not answer: reported, and the walk goes on.
        Err(LedgerError::AttachmentTooLarge { .. }) => return Ok(Err(DirectoryFault::TooLarge)),
        Err(error) => return Err(error),
    };
    Ok(check_copy(owner, offer, &bytes))
}

/// [`copy`]'s checks over the attachment's bytes.
fn check_copy(
    owner: &OwnerId,
    offer: &DirectoryOffer,
    bytes: &[u8],
) -> Result<(Vec<u8>, DeviceDirectory), DirectoryFault> {
    let copy = DirectoryCopy::decode(bytes)?;
    if hash_bytes(&copy.document) != offer.document_hash {
        return Err(DirectoryFault::HashMismatch);
    }
    let version = crate::directory::project_version(
        &copy.document,
        &UserId::new(owner.as_str()),
        &copy.identity_key,
    )
    .map_err(|_| DirectoryFault::Unverifiable)?;
    if version != offer.directory_version {
        return Err(DirectoryFault::VersionMismatch);
    }
    let directory = capsule_core::cbor::from_slice::<DeviceDirectory>(&copy.document)
        .map_err(|_| DirectoryFault::Undecodable)?;
    Ok((copy.identity_key, directory))
}

#[cfg(test)]
pub(crate) mod tests;
