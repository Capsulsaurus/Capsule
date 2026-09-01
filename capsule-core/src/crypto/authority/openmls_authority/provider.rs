//! The OpenMLS provider Capsule drives every album group through — the libcrux-backed
//! crypto/rand halves paired with an **owned, serializable** storage half.
//!
//! S-X1 used `openmls_libcrux_crypto::Provider`, whose bundled `MemoryStorage` is a private
//! field: durable group state could not be exported, so S-X1 left MLS state in RAM. S-X2 swaps
//! in this thin composition — the same formally-verified libcrux [`CryptoProvider`] (crypto +
//! rand) as before, but over a [`MemoryStorage`] this crate *holds*, so the whole storage keyspace
//! (ratchet tree, epoch secrets, queued proposals, the leaf signer, key-package bundles) round-trips
//! through [`export_bytes`](CapsuleMlsProvider::export_bytes) /
//! [`import_bytes`](CapsuleMlsProvider::import_bytes). That is the durable-persistence surface the
//! [`OpenMlsAuthority`](super::OpenMlsAuthority) exposes; *where* those bytes live (a `library.sqlite`
//! row, a file) is the caller's lifecycle concern — the same split every other core state object uses.

use std::collections::HashMap;
use std::sync::RwLock;

use openmls_libcrux_crypto::CryptoProvider;
use openmls_memory_storage::MemoryStorage;
use openmls_traits::OpenMlsProvider;
use serde_bytes::ByteBuf;

use super::OpenMlsAuthorityError;

/// The Capsule OpenMLS provider: libcrux crypto + rand, over an owned [`MemoryStorage`].
///
/// Behaviourally identical to `openmls_libcrux_crypto::Provider` — it delegates crypto and
/// randomness to the same [`CryptoProvider`] — but the storage is a field this crate owns, which
/// is what makes group state serializable (the upstream provider hides it).
pub(crate) struct CapsuleMlsProvider {
    crypto: CryptoProvider,
    storage: MemoryStorage,
}

impl CapsuleMlsProvider {
    /// Instantiate a fresh provider with empty storage. Fails only if the libcrux crypto
    /// backend cannot initialise (mirrors `LibcruxProvider::new`).
    pub(crate) fn new() -> Result<Self, OpenMlsAuthorityError> {
        let crypto =
            CryptoProvider::new().map_err(|e| OpenMlsAuthorityError::Provider(format!("{e:?}")))?;
        Ok(Self {
            crypto,
            storage: MemoryStorage::default(),
        })
    }

    /// Serialize the entire storage keyspace to bytes (the durable group-state blob). A fresh
    /// libcrux crypto backend is minted on [`import_bytes`](Self::import_bytes) — only the storage
    /// carries state — so this captures everything OpenMLS needs to reload the group and its keys.
    ///
    /// Reads the storage's public `values` map directly (a `key → value` byte keyspace) rather than
    /// the upstream `MemoryStorage::serialize`, which is gated behind the crate's `test-utils`
    /// feature; keeping to the public field avoids pulling a test-only feature into a shipping build.
    pub(crate) fn export_bytes(&self) -> Result<Vec<u8>, OpenMlsAuthorityError> {
        let guard = self
            .storage
            .values
            .read()
            .map_err(|_| OpenMlsAuthorityError::Persist("storage lock poisoned".into()))?;
        // Deterministic order (sorted keys) for reproducible export blobs.
        let mut pairs: Vec<(ByteBuf, ByteBuf)> = guard
            .iter()
            .map(|(k, v)| (ByteBuf::from(k.clone()), ByteBuf::from(v.clone())))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        crate::cbor::to_canonical_vec(&pairs)
            .map_err(|e| OpenMlsAuthorityError::Persist(format!("storage serialize: {e}")))
    }

    /// Reconstruct a provider from an [`export_bytes`](Self::export_bytes) blob: a fresh libcrux
    /// crypto backend plus the deserialized storage keyspace.
    pub(crate) fn import_bytes(bytes: &[u8]) -> Result<Self, OpenMlsAuthorityError> {
        let crypto =
            CryptoProvider::new().map_err(|e| OpenMlsAuthorityError::Provider(format!("{e:?}")))?;
        let pairs: Vec<(ByteBuf, ByteBuf)> = crate::cbor::from_slice(bytes)
            .map_err(|e| OpenMlsAuthorityError::Persist(format!("storage deserialize: {e}")))?;
        let map: HashMap<Vec<u8>, Vec<u8>> = pairs
            .into_iter()
            .map(|(k, v)| (k.into_vec(), v.into_vec()))
            .collect();
        let storage = MemoryStorage {
            values: RwLock::new(map),
        };
        Ok(Self { crypto, storage })
    }
}

impl OpenMlsProvider for CapsuleMlsProvider {
    type CryptoProvider = CryptoProvider;
    type RandProvider = CryptoProvider;
    type StorageProvider = MemoryStorage;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}
