//! The durable album-key store: every album's AMK ledger, write-tier + admin signing seeds,
//! and attested authority state, sealed at rest under the account master key (slice `S-A10`).
//!
//! Album keys used to be **session-scoped** — minted per `Workspace` and dropped on close — so a
//! reopened library could neither decrypt nor write its own prior assets. This module is the
//! durable half of the [key chain](https://docs/design/cryptography/keys/#key-chain) the
//! [keystore](super::keystore) already provides for device identity keys: `AccountFile` persists
//! the master key (passphrase-wrapped) and the device private keys (master-sealed);
//! [`AlbumStore`] persists everything below the master key that an album needs.
//!
//! ## On-disk format
//!
//! One file, `{root}/.library/albums.cbor`:
//!
//! ```text
//! MasterKey::seal( canonical_cbor(AlbumStore) )     # nonce(12) ‖ AES-256-GCM(ct+tag)
//! ```
//!
//! - **Sealed under [`MasterKey::seal`]** — the same AES-256-GCM **random-nonce** primitive
//!   `AccountFile` uses for the sealed device seeds. This is deliberately **not**
//!   `backup::artifact::seal_ledger`, whose nonce is *derived deterministically* from the wrap
//!   key so that re-exporting a backup is byte-identical. That property is right for a
//!   write-once artifact and catastrophic here: the store is rewritten on every `create_album`
//!   and `rotate_epoch`, so a fixed nonce under a fixed key would repeat `(key, nonce)` across
//!   different plaintexts and leak the keystream.
//! - **Canonical CBOR, sorted by album id** (and each album's AMK rows by epoch), so the
//!   plaintext is byte-stable for a given logical state.
//! - Written **temp-then-rename** ([`tmp_path`]), so a crash mid-write never leaves a
//!   half-written keystore where the previous good one was.
//! - The AMK rows reuse the backup artifact's row shape `(album_id, epoch, amk)`, so
//!   keystore ↔ backup-ledger conversion is a `map`, not a translation table.
//!
//! ## Recovered (read-only) albums
//!
//! A backup artifact escrows **AMKs** — read access — but never the write-tier or admin signing
//! keys, which are MLS-distributed *capabilities*, nor the admin-signed epoch ledger. An album
//! restored from a backup into a library that never held it therefore lands here with
//! `write_tier_seed64 = None`, `admin_seed64 = None`, `authority = None`: its photos decrypt and
//! re-export, but it cannot author new writes until its authority is re-established. That is the
//! honest state, and the lifecycle surfaces it as a typed error rather than minting a fresh admin
//! key that attests nothing and no peer trusts.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use thiserror::Error;
use uuid::Uuid;

use super::hybrid_sig::HybridSigningKey;
use super::master::MasterKey;
use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::authority::reference::SignedEpochLedger;
use crate::crypto::authority::{AlbumAuthority, Authority, ReferenceAuthority};
use crate::utils::paths::tmp_path;

/// The album store's on-disk format version. Bumped only on a breaking layout change; a store
/// declaring a higher version is refused rather than misread.
pub const ALBUM_STORE_VERSION: u16 = 1;

/// The store's filename beneath `{root}/.library/`.
pub const ALBUM_STORE_FILE: &str = "albums.cbor";

/// One escrowed album content key: `(album_id, epoch, amk)`.
///
/// Identical to the row shape of the backup artifact's AMK ledger, so moving keys between the
/// keystore and a backup is a `map` over rows.
pub type AmkRow = (Uuid, u32, [u8; 32]);

/// Errors from reading, writing, or decoding the album store.
#[derive(Debug, Error)]
pub enum AlbumStoreError {
    /// Filesystem error reading or writing `albums.cbor`.
    #[error("album store io: {0}")]
    Io(String),
    /// The sealed plaintext was not decodable CBOR.
    #[error("album store cbor: {0}")]
    Cbor(String),
    /// The master key did not open the sealed store (wrong key, or tampering).
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// The store declares a format version this build does not implement.
    #[error("album store format version {0} is newer than this build supports")]
    UnsupportedVersion(u16),
    /// The store holds an [`OpenMlsAuthority`](crate::crypto::authority::OpenMlsAuthority) state
    /// blob but this build has no `mls` feature. A typed error, never a panic: the library still
    /// opens, and the affected album is reported rather than silently dropped.
    #[error("album {0} has MLS authority state, but this build has no `mls` support")]
    MlsUnavailable(Uuid),
    /// Exporting or importing MLS group state failed.
    #[error("album {album}: mls authority state: {detail}")]
    MlsState {
        /// The album whose authority state failed.
        album: Uuid,
        /// The backend's own message.
        detail: String,
    },
    /// A persisted reference ledger did not re-verify its admin signature chain — a tampered,
    /// forged, or rewound ledger. Never repaired silently.
    #[error("album {0}: persisted authority ledger failed admin-chain verification")]
    LedgerRejected(Uuid),
    /// A persisted signing seed was not the expected 64 bytes.
    #[error("album {0}: persisted signing seed has the wrong length")]
    MalformedSeed(Uuid),
}

type Result<T> = std::result::Result<T, AlbumStoreError>;

/// One album's persisted authority state, behind the
/// [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority) seam.
///
/// Both variants exist on **every** build, `mls` or not: a non-mls build must be able to *decode*
/// an MLS-authority album and report it as [`AlbumStoreError::MlsUnavailable`], which it could not
/// do if the variant were `cfg`-gated away (the decode itself would fail, indistinguishable from
/// corruption).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PersistedAuthority {
    /// The offline reference backend's admin-signed epoch ledger. Re-verified on load. Boxed:
    /// the ledger dwarfs the MLS variant's byte buffer, and this enum is stored per album.
    Reference(Box<SignedEpochLedger>),
    /// [`OpenMlsAuthority::export_state`](crate::crypto::authority::OpenMlsAuthority::export_state)
    /// bytes (its own CBOR envelope, opaque here).
    OpenMls(ByteBuf),
}

impl PersistedAuthority {
    /// Capture an in-memory [`Authority`] for persistence.
    pub fn capture(authority: &Authority) -> Result<Self> {
        match authority {
            Authority::Reference(a) => Ok(Self::Reference(Box::new(a.to_ledger()))),
            #[cfg(feature = "mls")]
            Authority::OpenMls(a) => a
                .export_state()
                .map(|bytes| Self::OpenMls(ByteBuf::from(bytes)))
                .map_err(|e| AlbumStoreError::MlsState {
                    album: authority.album_id(),
                    detail: e.to_string(),
                }),
        }
    }

    /// Rebuild the live [`Authority`]. `held_epochs` are the epochs whose AMK this device
    /// actually holds — the reference backend's `amk_present` flags are local-only state and are
    /// restored from the AMK rows, never from the signed ledger.
    ///
    /// A reference ledger whose admin chain does not re-verify is [`LedgerRejected`], and an MLS
    /// state blob on a build without the `mls` feature is [`MlsUnavailable`]. Neither panics.
    ///
    /// [`LedgerRejected`]: AlbumStoreError::LedgerRejected
    /// [`MlsUnavailable`]: AlbumStoreError::MlsUnavailable
    pub fn restore(&self, album_id: Uuid, held_epochs: &BTreeSet<u32>) -> Result<Authority> {
        match self {
            Self::Reference(ledger) => ReferenceAuthority::from_ledger(ledger, held_epochs)
                .map(|a| Authority::Reference(Box::new(a)))
                .ok_or(AlbumStoreError::LedgerRejected(album_id)),
            #[cfg(feature = "mls")]
            Self::OpenMls(bytes) => {
                crate::crypto::authority::OpenMlsAuthority::import_state(bytes.as_ref())
                    .map(|a| Authority::OpenMls(Box::new(a)))
                    .map_err(|e| AlbumStoreError::MlsState {
                        album: album_id,
                        detail: e.to_string(),
                    })
            }
            #[cfg(not(feature = "mls"))]
            Self::OpenMls(_) => Err(AlbumStoreError::MlsUnavailable(album_id)),
        }
    }
}

/// One album's persisted key material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedAlbum {
    /// Album id.
    pub album_id: Uuid,
    /// Display name.
    pub name: String,
    /// The AMK ledger: one `(album_id, epoch, amk)` row per epoch, kept sorted by epoch.
    pub amks: Vec<AmkRow>,
    /// The current (highest) epoch — the one new writes are authored under.
    pub current_epoch: u32,
    /// The per-album write-tier signing key's 64-byte hybrid seed (Ed25519 ‖ ML-DSA-65).
    /// `None` for an album recovered from a backup artifact, which escrows no write capability.
    pub write_tier_seed64: Option<ByteBuf>,
    /// The per-album admin signing key's 64-byte hybrid seed. `None` as above.
    pub admin_seed64: Option<ByteBuf>,
    /// The attested authority state. `None` as above.
    pub authority: Option<PersistedAuthority>,
}

impl PersistedAlbum {
    /// A read-only album entry: escrowed AMKs, no write capability, no authority. This is what a
    /// backup restore can honestly reconstruct.
    pub fn recovered(album_id: Uuid, name: &str, amks: Vec<AmkRow>) -> Self {
        let current_epoch = amks.iter().map(|(_, epoch, _)| *epoch).max().unwrap_or(0);
        let mut album = Self {
            album_id,
            name: name.to_string(),
            amks,
            current_epoch,
            write_tier_seed64: None,
            admin_seed64: None,
            authority: None,
        };
        album.normalize();
        album
    }

    /// The epochs whose AMK content key this device holds — the input that restores a reference
    /// authority's local-only `amk_present` flags.
    pub fn held_epochs(&self) -> BTreeSet<u32> {
        self.amks.iter().map(|(_, epoch, _)| *epoch).collect()
    }

    /// Decode the write-tier signing key, if this album carries write capability.
    pub fn write_tier_key(&self) -> Result<Option<HybridSigningKey>> {
        self.decode_seed(self.write_tier_seed64.as_ref())
    }

    /// Decode the admin signing key, if this album carries admin capability.
    pub fn admin_key(&self) -> Result<Option<HybridSigningKey>> {
        self.decode_seed(self.admin_seed64.as_ref())
    }

    fn decode_seed(&self, seed: Option<&ByteBuf>) -> Result<Option<HybridSigningKey>> {
        seed.map(|bytes| {
            let seed64: [u8; 64] = bytes
                .as_ref()
                .try_into()
                .map_err(|_| AlbumStoreError::MalformedSeed(self.album_id))?;
            Ok(HybridSigningKey::from_seed64(&seed64))
        })
        .transpose()
    }

    /// Sort the AMK rows by epoch and re-derive `current_epoch` as their maximum, so the encoded
    /// form is byte-stable and the epoch pointer can never drift below a held key.
    fn normalize(&mut self) {
        self.amks.sort_unstable_by_key(|(_, epoch, _)| *epoch);
        self.amks.dedup_by_key(|(_, epoch, _)| *epoch);
        if let Some(max) = self.amks.iter().map(|(_, epoch, _)| *epoch).max() {
            self.current_epoch = self.current_epoch.max(max);
        }
    }
}

/// Every album this library holds key material for, sealed at rest under the account master key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumStore {
    /// On-disk format version.
    pub version: u16,
    /// The albums, **sorted by `album_id`** so the canonical encoding is byte-stable.
    albums: Vec<PersistedAlbum>,
}

impl Default for AlbumStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AlbumStore {
    /// An empty store at the current format version.
    pub fn new() -> Self {
        Self {
            version: ALBUM_STORE_VERSION,
            albums: Vec::new(),
        }
    }

    /// `{root}/.library/albums.cbor`.
    pub fn path(root: &Path) -> PathBuf {
        root.join(".library").join(ALBUM_STORE_FILE)
    }

    /// The persisted albums, sorted by id.
    pub fn albums(&self) -> &[PersistedAlbum] {
        &self.albums
    }

    /// One album's persisted material.
    pub fn get(&self, album_id: &Uuid) -> Option<&PersistedAlbum> {
        self.albums
            .binary_search_by_key(album_id, |a| a.album_id)
            .ok()
            .map(|i| &self.albums[i])
    }

    /// Insert or replace one album's entry wholesale (the `create_album` / `rotate_epoch` path,
    /// where the workspace holds the complete, authoritative material).
    pub fn upsert(&mut self, mut album: PersistedAlbum) {
        album.normalize();
        match self
            .albums
            .binary_search_by_key(&album.album_id, |a| a.album_id)
        {
            Ok(i) => self.albums[i] = album,
            Err(i) => self.albums.insert(i, album),
        }
    }

    /// Merge escrowed `(album_id, epoch, amk)` rows into the store — the backup-restore path.
    ///
    /// An album already present keeps its write capability, admin key, and authority and simply
    /// gains any epochs it was missing; an unknown album is added as a
    /// [read-only recovered entry](PersistedAlbum::recovered). An epoch already held is **never
    /// overwritten**: the local copy is the one the local authority attests, and silently
    /// swapping a content key underneath it would be a downgrade vector. Returns the number of
    /// epochs actually added.
    pub fn merge_amks(
        &mut self,
        album_id: Uuid,
        name: &str,
        rows: impl IntoIterator<Item = AmkRow>,
    ) -> usize {
        let rows: Vec<AmkRow> = rows.into_iter().collect();
        match self.albums.binary_search_by_key(&album_id, |a| a.album_id) {
            Ok(i) => {
                let existing = &mut self.albums[i];
                let held = existing.held_epochs();
                let mut added = 0;
                for row in rows {
                    if !held.contains(&row.1) {
                        existing.amks.push(row);
                        added += 1;
                    }
                }
                existing.normalize();
                added
            }
            Err(i) => {
                let album = PersistedAlbum::recovered(album_id, name, rows);
                let added = album.amks.len();
                self.albums.insert(i, album);
                added
            }
        }
    }

    /// Load and unseal the store at `{root}/.library/albums.cbor`.
    ///
    /// `Ok(None)` means the file does not exist — a **pre-`S-A10` library**, whose album keys
    /// were session-scoped and were therefore never persisted at all. Such a library opens with
    /// zero albums; its assets are recoverable only through a backup artifact (see
    /// [`Workspace::import_backup`](crate::lifecycle::Workspace::import_backup)). This is not a
    /// regression — those assets were already undecryptable across a restart before this module
    /// existed — but it is a real, permanent limitation of libraries created before it.
    #[tracing::instrument(skip_all, fields(root = %root.display()))]
    pub fn load(root: &Path, master: &MasterKey) -> Result<Option<Self>> {
        let path = Self::path(root);
        if !path.exists() {
            tracing::debug!(path = %path.display(), "album store: absent");
            return Ok(None);
        }
        let sealed = fs::read(&path).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        let plaintext = master.open(&sealed)?;
        let store: Self =
            cbor::from_slice(&plaintext).map_err(|e| AlbumStoreError::Cbor(e.to_string()))?;
        if store.version > ALBUM_STORE_VERSION {
            return Err(AlbumStoreError::UnsupportedVersion(store.version));
        }
        tracing::info!(
            albums = store.albums.len(),
            epochs = store.albums.iter().map(|a| a.amks.len()).sum::<usize>(),
            sealed_bytes = sealed.len(),
            "album store: loaded"
        );
        for album in &store.albums {
            tracing::debug!(
                album_id = %album.album_id,
                current_epoch = album.current_epoch,
                epochs = album.amks.len(),
                writable = album.write_tier_seed64.is_some(),
                has_authority = album.authority.is_some(),
                "album store: album restored"
            );
        }
        Ok(Some(store))
    }

    /// Seal and durably replace the store. Temp-then-rename, so a crash mid-write leaves the
    /// previous good keystore intact rather than a truncated one.
    #[tracing::instrument(skip_all, fields(root = %root.display(), albums = self.albums.len()))]
    pub fn save(&self, root: &Path, master: &MasterKey) -> Result<()> {
        let mut canonical = self.clone();
        canonical.version = ALBUM_STORE_VERSION;
        canonical
            .albums
            .sort_unstable_by_key(|album| album.album_id);
        for album in &mut canonical.albums {
            album.normalize();
        }
        let plaintext =
            cbor::to_canonical_vec(&canonical).map_err(|e| AlbumStoreError::Cbor(e.to_string()))?;
        let sealed = master.seal(&plaintext);

        let path = Self::path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        }
        let tmp = tmp_path(&path);
        fs::write(&tmp, &sealed).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        fs::rename(&tmp, &path).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        tracing::info!(
            path = %path.display(),
            sealed_bytes = sealed.len(),
            epochs = canonical.albums.iter().map(|a| a.amks.len()).sum::<usize>(),
            "album store: persisted"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::crypto::keys::AmkVersion;

    fn signing_key(a: u8, b: u8) -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[a; 32], &[b; 32])
    }

    /// A store holding one fully-capable album (keys + attested reference authority) and one
    /// read-only recovered album.
    fn fixture() -> (AlbumStore, Uuid, Uuid) {
        let writable = Uuid::from_u128(0xAA);
        let recovered = Uuid::from_u128(0x11);

        let admin = signing_key(1, 2);
        let write_tier = signing_key(3, 4);
        let authority = ReferenceAuthority::new(writable, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &write_tier.verifying_key(),
            true,
        );

        let mut store = AlbumStore::new();
        store.upsert(PersistedAlbum {
            album_id: writable,
            name: "Trip".into(),
            amks: vec![(writable, 1, [7; 32])],
            current_epoch: 1,
            write_tier_seed64: Some(ByteBuf::from(write_tier.to_seed_bytes().to_vec())),
            admin_seed64: Some(ByteBuf::from(admin.to_seed_bytes().to_vec())),
            authority: Some(
                PersistedAuthority::capture(&Authority::Reference(Box::new(authority))).unwrap(),
            ),
        });
        store.upsert(PersistedAlbum::recovered(
            recovered,
            "Restored",
            vec![(recovered, 2, [9; 32]), (recovered, 1, [8; 32])],
        ));
        (store, writable, recovered)
    }

    #[test]
    fn album_store_round_trips_under_master_key() {
        let dir = TempDir::new().unwrap();
        let master = MasterKey::generate();
        let (store, writable, recovered) = fixture();
        store.save(dir.path(), &master).unwrap();

        // The sealed file is not the plaintext.
        let on_disk = fs::read(AlbumStore::path(dir.path())).unwrap();
        assert!(
            !on_disk.windows(32).any(|w| w == [7u8; 32]),
            "a raw AMK must never appear in the sealed file"
        );

        let back = AlbumStore::load(dir.path(), &master).unwrap().unwrap();
        assert_eq!(back.version, ALBUM_STORE_VERSION);
        assert_eq!(back.albums().len(), 2);

        // The writable album keeps its AMK, its signing keys, and a re-verifying authority.
        let a = back.get(&writable).unwrap();
        assert_eq!(a.amks, vec![(writable, 1, [7; 32])]);
        assert_eq!(a.current_epoch, 1);
        assert_eq!(
            a.write_tier_key().unwrap().unwrap().verifying_key(),
            signing_key(3, 4).verifying_key()
        );
        assert_eq!(
            a.admin_key().unwrap().unwrap().verifying_key(),
            signing_key(1, 2).verifying_key()
        );
        let authority = a
            .authority
            .as_ref()
            .unwrap()
            .restore(writable, &a.held_epochs())
            .unwrap();
        assert!(authority.admin_chain_verifies());
        assert_eq!(authority.epoch_ceiling(), AmkVersion(1));
        assert!(
            authority.has_amk(AmkVersion(1)),
            "amk_present is restored from the epochs actually held"
        );

        // The recovered album is honest about holding no write capability.
        let r = back.get(&recovered).unwrap();
        assert_eq!(r.current_epoch, 2, "current epoch is the max recovered one");
        assert_eq!(
            r.amks,
            vec![(recovered, 1, [8; 32]), (recovered, 2, [9; 32])],
            "rows are normalized into epoch order"
        );
        assert!(r.write_tier_key().unwrap().is_none());
        assert!(r.admin_key().unwrap().is_none());
        assert!(r.authority.is_none());
    }

    #[test]
    fn album_store_rejects_wrong_master_key() {
        let dir = TempDir::new().unwrap();
        let master = MasterKey::generate();
        fixture().0.save(dir.path(), &master).unwrap();

        let err = AlbumStore::load(dir.path(), &MasterKey::generate()).unwrap_err();
        assert!(
            matches!(err, AlbumStoreError::Crypto(CryptoError::Auth(_))),
            "wrong master key must authenticate-fail, got {err:?}"
        );

        // Tampering with a sealed byte is equally rejected (AES-GCM tag).
        let path = AlbumStore::path(dir.path());
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 0x01;
        fs::write(&path, &bytes).unwrap();
        assert!(AlbumStore::load(dir.path(), &master).is_err());
    }

    /// The sealed bytes differ every write (fresh nonce — the whole reason this does not reuse
    /// the backup artifact's derived-nonce sealer), but the **plaintext** is byte-stable for a
    /// given logical state regardless of the order albums or epochs were inserted in.
    #[test]
    fn album_store_is_canonical_cbor_stable() {
        let (ordered, writable, recovered) = fixture();

        // Build the same logical store with the albums inserted in the opposite order and each
        // album's epochs shuffled.
        let mut shuffled = AlbumStore::new();
        shuffled.upsert(PersistedAlbum::recovered(
            recovered,
            "Restored",
            vec![(recovered, 1, [8; 32]), (recovered, 2, [9; 32])],
        ));
        for album in ordered.albums() {
            if album.album_id == writable {
                shuffled.upsert(album.clone());
            }
        }

        let encode = |s: &AlbumStore| {
            let mut c = s.clone();
            c.albums.sort_unstable_by_key(|a| a.album_id);
            for a in &mut c.albums {
                a.normalize();
            }
            cbor::to_canonical_vec(&c).unwrap()
        };
        assert_eq!(
            encode(&ordered),
            encode(&shuffled),
            "insertion order must not change the canonical plaintext"
        );

        // And the sealed file *is* fresh-nonced: two saves of identical state differ.
        let dir = TempDir::new().unwrap();
        let master = MasterKey::generate();
        ordered.save(dir.path(), &master).unwrap();
        let first = fs::read(AlbumStore::path(dir.path())).unwrap();
        ordered.save(dir.path(), &master).unwrap();
        let second = fs::read(AlbumStore::path(dir.path())).unwrap();
        assert_ne!(
            first, second,
            "the store is rewritten on every album mutation, so the nonce MUST be fresh per seal"
        );
    }

    #[test]
    fn album_store_load_returns_none_for_a_library_that_never_had_one() {
        let dir = TempDir::new().unwrap();
        assert!(
            AlbumStore::load(dir.path(), &MasterKey::generate())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn merge_amks_adds_missing_epochs_and_never_overwrites_a_held_one() {
        let (mut store, writable, _) = fixture();

        // Epoch 1 is already held with a different value; epoch 2 is new.
        let added = store.merge_amks(
            writable,
            "Trip",
            vec![(writable, 1, [0xEE; 32]), (writable, 2, [0xAB; 32])],
        );
        assert_eq!(added, 1, "only the unheld epoch is added");
        let a = store.get(&writable).unwrap();
        assert_eq!(
            a.amks,
            vec![(writable, 1, [7; 32]), (writable, 2, [0xAB; 32])],
            "the locally held epoch-1 key is not swapped out from underneath the authority"
        );
        assert_eq!(a.current_epoch, 2);
        // The album keeps its write capability across the merge.
        assert!(a.write_tier_key().unwrap().is_some());

        // An unknown album lands as a read-only recovered entry.
        let fresh = Uuid::from_u128(0xFF);
        assert_eq!(
            store.merge_amks(fresh, "Restored", vec![(fresh, 5, [1; 32])]),
            1
        );
        let f = store.get(&fresh).unwrap();
        assert!(f.write_tier_key().unwrap().is_none());
        assert_eq!(f.current_epoch, 5);
    }

    #[test]
    fn a_tampered_reference_ledger_is_rejected_on_restore() {
        let (store, writable, _) = fixture();
        let album = store.get(&writable).unwrap();
        let PersistedAuthority::Reference(ledger) = album.authority.as_ref().unwrap() else {
            panic!("fixture builds a reference authority");
        };
        // Swap the attested write-tier key with no matching admin re-signature.
        let mut forged = ledger.as_ref().clone();
        forged.entries[0].write_tier_pub = signing_key(9, 9).verifying_key();
        match PersistedAuthority::Reference(Box::new(forged))
            .restore(writable, &album.held_epochs())
        {
            Err(AlbumStoreError::LedgerRejected(id)) => assert_eq!(id, writable),
            other => panic!("a forged ledger must be rejected, got {:?}", other.err()),
        }
    }

    /// A build without `mls` must *decode* an MLS-authority album and report it as a typed
    /// error — never panic, and never fail the whole store's decode as if it were corrupt.
    #[cfg(not(feature = "mls"))]
    #[test]
    fn mls_authority_state_on_a_non_mls_build_is_a_typed_error() {
        let dir = TempDir::new().unwrap();
        let master = MasterKey::generate();
        let album_id = Uuid::from_u128(0xC0FFEE);

        let mut store = AlbumStore::new();
        let mut album =
            PersistedAlbum::recovered(album_id, "MLS album", vec![(album_id, 1, [3; 32])]);
        album.authority = Some(PersistedAuthority::OpenMls(ByteBuf::from(vec![1, 2, 3, 4])));
        store.upsert(album);
        store.save(dir.path(), &master).unwrap();

        // The store still loads — the album is present, only its authority is unusable here.
        let back = AlbumStore::load(dir.path(), &master).unwrap().unwrap();
        let album = back.get(&album_id).unwrap();
        match album
            .authority
            .as_ref()
            .unwrap()
            .restore(album_id, &album.held_epochs())
        {
            Err(AlbumStoreError::MlsUnavailable(id)) => assert_eq!(id, album_id),
            other => panic!(
                "an mls authority on a non-mls build must be typed, got {:?}",
                other.err()
            ),
        }
    }

    #[test]
    fn a_store_from_a_newer_format_version_is_refused_not_misread() {
        let dir = TempDir::new().unwrap();
        let master = MasterKey::generate();
        let mut store = AlbumStore::new();
        store.version = ALBUM_STORE_VERSION + 1;
        // `save` normalizes the version, so write the sealed bytes by hand.
        let plaintext = cbor::to_canonical_vec(&store).unwrap();
        fs::create_dir_all(dir.path().join(".library")).unwrap();
        fs::write(AlbumStore::path(dir.path()), master.seal(&plaintext)).unwrap();

        assert!(matches!(
            AlbumStore::load(dir.path(), &master).unwrap_err(),
            AlbumStoreError::UnsupportedVersion(v) if v == ALBUM_STORE_VERSION + 1
        ));
    }
}
