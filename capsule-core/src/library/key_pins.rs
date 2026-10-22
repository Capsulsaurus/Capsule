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

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::cbor::{self, CanonicalError};
use crate::crypto::keys::HybridVerifyingKey;
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

fn load_pin_file(root: &Path) -> Result<PinFile, KeyPinError> {
    let path = attestation_key_pins_path(root);
    let file: PinFile = match fs::read(&path) {
        Ok(bytes) if bytes.is_empty() => return Ok(empty_pin_file()),
        Ok(bytes) => cbor::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(empty_pin_file()),
        Err(e) => return Err(KeyPinError::Io(e.to_string())),
    };
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
    let path = attestation_key_pins_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| KeyPinError::Io(e.to_string()))?;
    }
    let bytes = cbor::to_canonical_vec(file)?;
    let tmp = tmp_path(&path);
    fs::write(&tmp, &bytes).map_err(|e| KeyPinError::Io(e.to_string()))?;
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

#[cfg(test)]
mod tests;
