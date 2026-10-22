//! The pinned attestation-key history of each server this library has pushed to (SSoT:
//! [Transfer — Durability: Custody Receipts](https://docs/design/transfer/durability/#custody-receipts)
//! — the client checks a receipt's signature **under the pinned key**).
//!
//! A server publishes its attestation keys at `/.well-known/capsule/attestation-keys` with an
//! append-only history, so a receipt signed before a rotation still verifies. A client that
//! trusted whatever each fetch returned would accept a receipt from anyone able to swap that
//! record — the server itself, or anything on the path to it. So the first fetch is pinned
//! (trust on first use) and every later fetch is checked against the pin:
//!
//! - **First contact** — nothing is pinned for the origin: the published history is pinned.
//! - **Unchanged** — every published key is already pinned: nothing is written.
//! - **Extended** — every pinned key is still published, and new ones were added: the new keys
//!   are appended to the pin, in the order the server published them.
//! - **Rewritten** — a pinned key is no longer published (dropped, or replaced by another):
//!   [`KeyPinError::HistoryRewritten`], and the pin is left exactly as it was. There is no
//!   silent re-pin; a server that rotates *out* a key breaks the append-only contract.
//!
//! The comparison is by key, not by position: a server that republishes the same history in
//! another order has dropped nothing, and the pin keeps its own order.
//!
//! # The file
//!
//! `.library/attestation-keys.cbor` ([`attestation_key_pins_path`]) is canonical CBOR of a
//! versioned map from a server's canonical origin (`scheme://host[:port]`, as the caller
//! derives it) to the pinned keys' raw bytes. It is rewritten through a temporary sibling and a
//! rename, so a crash mid-write leaves the previous pin whole.
//!
//! # Backup and restore
//!
//! The pin rides the backup artifact verbatim ([`read_attestation_key_pins`]), so a restored
//! library keeps checking receipts under the keys it pinned rather than trusting whatever the
//! server publishes first after the restore. [`restore_attestation_key_pins`] folds a carried
//! pin into the library by the same append-only rule a fetch obeys, per origin: the union is
//! kept only where one history extends the other, and two histories that each hold a key the
//! other lacks are [`KeyPinError::HistoriesDiverge`], with neither overwritten.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::cbor::{self, CanonicalError};
use capsule_core::crypto::keys::HybridVerifyingKey;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::utils::paths::tmp_path;

/// The pin file's schema version. A file stamped with another one is refused, never rewritten.
const PIN_FILE_VERSION: u8 = 1;

/// `.library/attestation-keys.cbor` — the pinned attestation-key history of every server this
/// library has contacted.
pub fn attestation_key_pins_path(root: &Path) -> PathBuf {
    root.join(".library").join("attestation-keys.cbor")
}

/// The on-disk shape of the pin file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PinFile {
    version: u8,
    /// Canonical origin → the pinned keys' raw bytes, in pinned order.
    servers: BTreeMap<String, Vec<ByteBuf>>,
}

/// What pinning a fetched history did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPinOutcome {
    /// Nothing was pinned for the origin; the published history now is.
    FirstContact {
        /// How many keys were pinned.
        keys: usize,
    },
    /// The published history holds exactly the pinned keys; nothing was written.
    Unchanged,
    /// The published history extends the pin; the new keys were appended to it.
    Extended {
        /// How many keys were appended.
        added: usize,
    },
}

/// The pinned history a run verifies under, and how it got there.
#[derive(Debug, Clone)]
pub struct PinnedKeys {
    /// What this call did to the pin.
    pub outcome: KeyPinOutcome,
    /// Every pinned key for the origin, in pinned order, after this call.
    pub keys: Vec<HybridVerifyingKey>,
}

/// Errors pinning or reading a server's attestation-key history.
#[derive(Debug, thiserror::Error)]
pub enum KeyPinError {
    /// The server no longer publishes a key this library pinned for it: its history was
    /// rewritten, not extended. The pin is unchanged and nothing the server signs is trusted.
    #[error(
        "the server at {origin} no longer publishes {missing} of the {pinned} attestation key(s) \
         pinned for it; its key history was rewritten, not extended, so its receipts are not trusted"
    )]
    HistoryRewritten {
        /// The canonical origin whose pin was contradicted.
        origin: String,
        /// How many pinned keys the server no longer publishes.
        missing: usize,
        /// How many keys were pinned.
        pinned: usize,
    },
    /// A backup carries a pinned history for an origin that neither extends nor is extended by
    /// the one this library pins: each holds a key the other lacks. Neither pin is trusted over
    /// the other, so the restore is refused and the library's pin is left exactly as it was.
    #[error(
        "the backup pins an attestation-key history for {origin} that diverges from this \
         library's: the backup holds {only_carried} key(s) this library does not pin, and this \
         library pins {only_local} key(s) the backup lacks"
    )]
    HistoriesDiverge {
        /// The canonical origin whose two pinned histories diverge.
        origin: String,
        /// How many keys only the backup's pin holds.
        only_carried: usize,
        /// How many keys only this library's pin holds.
        only_local: usize,
    },
    /// The server published no attestation key at all, so there is nothing to pin and nothing
    /// a receipt could verify under.
    #[error("the server at {origin} publishes no attestation key")]
    EmptyHistory {
        /// The canonical origin that published nothing.
        origin: String,
    },
    /// The pin file was written by a schema this build does not know.
    #[error("the attestation-key pin file has schema v{found}; this build reads v{supported}")]
    UnsupportedVersion {
        /// The version the file carries.
        found: u8,
        /// The version this build reads and writes.
        supported: u8,
    },
    /// A pinned key's bytes no longer decode as a key.
    #[error("a pinned attestation key for {origin} does not decode: {reason}")]
    CorruptPin {
        /// The canonical origin whose pin is corrupt.
        origin: String,
        /// Why the key did not decode.
        reason: String,
    },
    /// The pin file could not be decoded or encoded.
    #[error("attestation-key pin codec: {0}")]
    Codec(#[from] CanonicalError),
    /// A filesystem error reading or writing the pin file.
    #[error("attestation-key pin I/O: {0}")]
    Io(String),
}

/// A pin failure during a backup restore, as the backup error the restore reports: a diverged
/// history keeps its counts, everything else its message.
impl From<KeyPinError> for capsule_core::backup::BackupError {
    fn from(error: KeyPinError) -> Self {
        match error {
            KeyPinError::HistoriesDiverge {
                origin,
                only_carried,
                only_local,
            } => Self::AttestationKeyPinsDiverge {
                origin,
                only_carried,
                only_local,
            },
            other => Self::AttestationKeyPins(other.to_string()),
        }
    }
}

/// The pin file's raw bytes, or `None` when it is absent or empty.
fn read_pin_bytes(root: &Path) -> Result<Option<Vec<u8>>, KeyPinError> {
    match fs::read(attestation_key_pins_path(root)) {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(KeyPinError::Io(e.to_string())),
    }
}

fn load_pin_file(root: &Path) -> Result<PinFile, KeyPinError> {
    match read_pin_bytes(root)? {
        Some(bytes) => decode_pin_file(&bytes),
        None => Ok(empty_pin_file()),
    }
}

/// Decode pin-file bytes, refusing another schema version and any key that does not decode.
fn decode_pin_file(bytes: &[u8]) -> Result<PinFile, KeyPinError> {
    let file: PinFile = cbor::from_slice(bytes)?;
    if file.version != PIN_FILE_VERSION {
        return Err(KeyPinError::UnsupportedVersion {
            found: file.version,
            supported: PIN_FILE_VERSION,
        });
    }
    Ok(file)
}

fn empty_pin_file() -> PinFile {
    PinFile {
        version: PIN_FILE_VERSION,
        servers: BTreeMap::new(),
    }
}

fn store_pin_file(root: &Path, file: &PinFile) -> Result<(), KeyPinError> {
    store_pin_bytes(root, &cbor::to_canonical_vec(file)?)
}

/// Write `bytes` as the pin file through a temporary sibling and a rename.
fn store_pin_bytes(root: &Path, bytes: &[u8]) -> Result<(), KeyPinError> {
    let path = attestation_key_pins_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| KeyPinError::Io(e.to_string()))?;
    }
    let tmp = tmp_path(&path);
    fs::write(&tmp, bytes).map_err(|e| KeyPinError::Io(e.to_string()))?;
    fs::rename(&tmp, &path).map_err(|e| KeyPinError::Io(e.to_string()))?;
    Ok(())
}

fn decode_pin(origin: &str, pinned: &[ByteBuf]) -> Result<Vec<HybridVerifyingKey>, KeyPinError> {
    pinned
        .iter()
        .map(|bytes| {
            HybridVerifyingKey::from_bytes(bytes).map_err(|e| KeyPinError::CorruptPin {
                origin: origin.to_owned(),
                reason: e.to_string(),
            })
        })
        .collect()
}

/// The history pinned for `origin`, or `None` when this library has never contacted it.
pub fn load_pinned_attestation_keys(
    root: &Path,
    origin: &str,
) -> Result<Option<Vec<HybridVerifyingKey>>, KeyPinError> {
    let file = load_pin_file(root)?;
    file.servers
        .get(origin)
        .map(|pinned| decode_pin(origin, pinned))
        .transpose()
}

/// Check the history `origin` just `published` against the pin, pin or extend it, and return
/// the pinned history every receipt from that server is verified under.
///
/// Nothing pinned for `origin` pins `published` ([`KeyPinOutcome::FirstContact`]); a history
/// holding exactly the pinned keys writes nothing ([`KeyPinOutcome::Unchanged`]); one holding
/// every pinned key and more appends the new keys in published order
/// ([`KeyPinOutcome::Extended`]). Keys are compared by value, not position. A history missing a
/// pinned key is [`KeyPinError::HistoryRewritten`], and an empty one
/// [`KeyPinError::EmptyHistory`]; a refusal writes nothing.
#[tracing::instrument(skip(root, published), fields(published = published.len()))]
pub fn pin_attestation_keys(
    root: &Path,
    origin: &str,
    published: &[HybridVerifyingKey],
) -> Result<PinnedKeys, KeyPinError> {
    if published.is_empty() {
        tracing::warn!("attestation-key pin: the server publishes no key");
        return Err(KeyPinError::EmptyHistory {
            origin: origin.to_owned(),
        });
    }
    let published_bytes: Vec<Vec<u8>> =
        published.iter().map(HybridVerifyingKey::to_bytes).collect();

    let mut file = load_pin_file(root)?;
    let Some(pinned) = file.servers.get(origin) else {
        let keys = published.len();
        file.servers.insert(
            origin.to_owned(),
            published_bytes.into_iter().map(ByteBuf::from).collect(),
        );
        store_pin_file(root, &file)?;
        tracing::info!(keys, "attestation-key pin: first contact, history pinned");
        return Ok(PinnedKeys {
            outcome: KeyPinOutcome::FirstContact { keys },
            keys: published.to_vec(),
        });
    };

    let missing = pinned
        .iter()
        .filter(|key| !published_bytes.iter().any(|p| p == key.as_slice()))
        .count();
    if missing > 0 {
        tracing::warn!(
            missing,
            pinned = pinned.len(),
            "attestation-key pin: a pinned key is no longer published; refusing"
        );
        return Err(KeyPinError::HistoryRewritten {
            origin: origin.to_owned(),
            missing,
            pinned: pinned.len(),
        });
    }

    let added: Vec<ByteBuf> = published_bytes
        .into_iter()
        .filter(|key| !pinned.iter().any(|p| p.as_slice() == key.as_slice()))
        .map(ByteBuf::from)
        .collect();
    if added.is_empty() {
        tracing::debug!(keys = pinned.len(), "attestation-key pin: unchanged");
        let keys = decode_pin(origin, pinned)?;
        return Ok(PinnedKeys {
            outcome: KeyPinOutcome::Unchanged,
            keys,
        });
    }

    let added_count = added.len();
    let mut extended = pinned.clone();
    extended.extend(added);
    let keys = decode_pin(origin, &extended)?;
    file.servers.insert(origin.to_owned(), extended);
    store_pin_file(root, &file)?;
    tracing::info!(
        added = added_count,
        keys = keys.len(),
        "attestation-key pin: history extended and re-pinned"
    );
    Ok(PinnedKeys {
        outcome: KeyPinOutcome::Extended { added: added_count },
        keys,
    })
}

/// What folding a backup's pinned histories into this library did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPinRestoreOutcome {
    /// This library pinned nothing; the backup's pin file was written byte for byte.
    Restored {
        /// How many origins the restored pin covers.
        origins: usize,
    },
    /// Every history the backup carries is already pinned here, or extended by the pin here:
    /// nothing was written.
    Unchanged,
    /// This library's pin was extended with what the backup carries.
    Merged {
        /// Origins the backup pins that this library had never contacted.
        origins_added: usize,
        /// Keys appended to histories this library already pinned.
        keys_added: usize,
    },
    /// The backup's pin file has a schema newer than this build reads, so it was left out and
    /// nothing was written. A newer artifact still restores its assets on an older build; the
    /// library re-pins each server on its next contact, as one that never pinned it would.
    SkippedNewerSchema {
        /// The schema version the carried pin file declares.
        found: u8,
    },
}

/// Just the schema version of a pin file, read before the rest so a newer schema whose shape
/// this build cannot decode is recognised as newer rather than as corrupt.
#[derive(Deserialize)]
struct PinFileVersion {
    version: u8,
}

/// Refuse a pin file holding a key that does not decode.
fn check_pin_keys(file: &PinFile) -> Result<(), KeyPinError> {
    for (origin, pinned) in &file.servers {
        decode_pin(origin, pinned)?;
    }
    Ok(())
}

/// The pin file's bytes exactly as they are on disk, for the backup artifact to carry, or `None`
/// when this library has pinned nothing.
///
/// The bytes are checked first — this build's schema, and every key decodes — so an artifact
/// never carries a pin a restore would refuse.
pub fn read_attestation_key_pins(root: &Path) -> Result<Option<Vec<u8>>, KeyPinError> {
    let Some(bytes) = read_pin_bytes(root)? else {
        return Ok(None);
    };
    check_pin_keys(&decode_pin_file(&bytes)?)?;
    Ok(Some(bytes))
}

/// Fold the pin file a backup `carried` into this library's pin.
///
/// A library with no pin takes the carried file byte for byte
/// ([`KeyPinRestoreOutcome::Restored`]). Otherwise each origin is merged by the rule a fetch
/// obeys: an origin this library never contacted is pinned as carried; one whose pinned history
/// holds every carried key is kept as it is; one whose pinned keys are all carried, plus more, is
/// extended by the carried keys it lacks, in carried order. An origin where each side holds a key
/// the other lacks is [`KeyPinError::HistoriesDiverge`]. Every origin is checked before anything
/// is written, so a refusal — that one, or a carried file that does not decode — leaves the
/// library's pin exactly as it was.
///
/// A carried file stamped with a schema **newer** than this build reads is not refused: it is
/// skipped with a warning ([`KeyPinRestoreOutcome::SkippedNewerSchema`]), so a backup from a
/// newer build still restores here. A pin is never worth more than the assets it travels with.
/// Any other version, and anything that does not decode, is refused.
#[tracing::instrument(skip_all, fields(carried = carried.len()))]
pub fn restore_attestation_key_pins(
    root: &Path,
    carried: &[u8],
) -> Result<KeyPinRestoreOutcome, KeyPinError> {
    use std::collections::btree_map::Entry;

    let PinFileVersion { version } = cbor::from_slice(carried)?;
    if version > PIN_FILE_VERSION {
        tracing::warn!(
            found = version,
            supported = PIN_FILE_VERSION,
            "attestation-key pin restore: the backup's pin has a newer schema; skipping it"
        );
        return Ok(KeyPinRestoreOutcome::SkippedNewerSchema { found: version });
    }
    let carried_file = decode_pin_file(carried)?;
    check_pin_keys(&carried_file)?;
    if carried_file.servers.is_empty() {
        tracing::debug!("attestation-key pin restore: the backup pins no origin");
        return Ok(KeyPinRestoreOutcome::Unchanged);
    }
    let Some(local_bytes) = read_pin_bytes(root)? else {
        store_pin_bytes(root, carried)?;
        let origins = carried_file.servers.len();
        tracing::info!(
            origins,
            "attestation-key pin restore: pin restored from the backup"
        );
        return Ok(KeyPinRestoreOutcome::Restored { origins });
    };

    let mut local = decode_pin_file(&local_bytes)?;
    let (mut origins_added, mut keys_added) = (0usize, 0usize);
    for (origin, keys) in carried_file.servers {
        match local.servers.entry(origin) {
            Entry::Vacant(slot) => {
                slot.insert(keys);
                origins_added += 1;
            }
            Entry::Occupied(mut slot) => {
                let pinned = slot.get_mut();
                let only_local = pinned.iter().filter(|key| !keys.contains(key)).count();
                let only_carried: Vec<ByteBuf> = keys
                    .into_iter()
                    .filter(|key| !pinned.contains(key))
                    .collect();
                if only_local > 0 && !only_carried.is_empty() {
                    tracing::warn!(
                        origin = %slot.key(),
                        only_local,
                        only_carried = only_carried.len(),
                        "attestation-key pin restore: the histories diverge; refusing"
                    );
                    return Err(KeyPinError::HistoriesDiverge {
                        origin: slot.key().clone(),
                        only_carried: only_carried.len(),
                        only_local,
                    });
                }
                keys_added += only_carried.len();
                pinned.extend(only_carried);
            }
        }
    }

    if origins_added == 0 && keys_added == 0 {
        tracing::debug!("attestation-key pin restore: the backup's pin is already held");
        return Ok(KeyPinRestoreOutcome::Unchanged);
    }
    store_pin_file(root, &local)?;
    tracing::info!(
        origins_added,
        keys_added,
        "attestation-key pin restore: the backup's pin merged into this library's"
    );
    Ok(KeyPinRestoreOutcome::Merged {
        origins_added,
        keys_added,
    })
}

#[cfg(test)]
mod tests;
