//! The durable album-key store: every album's AMK ledger, write-tier + admin signing seeds,
//! and attested authority state, sealed at rest under the account master key (slice `S-A10`).
//!
//! Album keys used to be **session-scoped** — minted per `Workspace` and dropped on close — so a
//! reopened library could neither decrypt nor write its own prior assets. This module is the
//! durable half of the [key chain](https://docs/design/cryptography/keys/#key-chain) the
//! keystore (`Account`) already provides for device identity keys: `AccountFile` persists
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
//! - Written **temp-then-rename** by `capsule-library`'s `AlbumStoreFile::save`, so a crash
//!   mid-write never leaves a half-written keystore where the previous good one was.
//! - The AMK rows reuse the backup artifact's row shape `(album_id, epoch, amk)`, so
//!   keystore ↔ backup-ledger conversion is a `map`, not a translation table.
//!
//! ## Recovered (read-only) albums
//!
//! A backup artifact escrows **AMKs** — read access — but never the write-tier or admin signing
//! keys, which are MLS-distributed *capabilities*. It does carry each album's **public**
//! authority, the admin-signed epoch ledger (#468). An album restored from a backup into a
//! library that never held it therefore lands here with `write_tier_seed64 = None`,
//! `admin_seed64 = None`, and `authority = Some(Reference(ledger))` (or `None` from a format-1
//! artifact, which carried no ledger): its photos decrypt, re-export, and verify, but it cannot
//! author new writes until its signing capability is re-established. That is the honest state,
//! and the lifecycle surfaces it as a typed error rather than minting a fresh admin key that
//! attests nothing and no peer trusts. The ledger re-verifies on every load, like any other.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use thiserror::Error;
use uuid::Uuid;

use super::hybrid_sig::HybridSigningKey;
use super::master::MasterKey;
use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::authority::{AlbumAuthority, Authority, ReferenceAuthority, SignedEpochLedger};

/// The album store's on-disk format version. Bumped only on a breaking layout change; a store
/// declaring a higher version is refused rather than misread.
pub(crate) const ALBUM_STORE_VERSION: u16 = 1;

/// The store's filename beneath `{root}/.library/`.
pub const ALBUM_STORE_FILE: &str = "albums.cbor";

/// The domain separator an album's server-side key escrow carries ahead of its canonical CBOR,
/// inside the master-key seal. It keeps an escrowed album from ever opening as any other blob the
/// master key seals (the keystore file, the sealed identity), and names the format version.
const ALBUM_KEY_ESCROW_DOMAIN: &[u8] = b"capsule-album-key-escrow/v1\0";

/// How an album key escrow the server holds stands against the album a device holds
/// ([`PersistedAlbum::escrow_coverage`]). Only [`Superseded`](Self::Superseded) is a state the
/// device may overwrite: every other one either needs nothing or would lose an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowCoverage {
    /// The escrow is this album's state exactly.
    Current,
    /// The escrow holds every epoch this album holds, with the same keys, and more: another
    /// device rotated further, or the escrow holds a write-tier seed, admin seed or authority
    /// this album lacks (a read-only device). Every local epoch is covered; replacing it would
    /// drop the newer epochs or the capability, so it is left alone.
    Behind,
    /// This album holds every epoch the escrow holds, with the same keys, and more (or the same
    /// epochs with other details changed), and every seed and authority the escrow holds:
    /// replacing the escrow only adds to it.
    Superseded,
    /// The two disagree: a shared epoch under different keys, each holds an epoch the other
    /// lacks, or this album holds an epoch the escrow lacks while the escrow holds a seed or
    /// authority this album lacks. Neither may replace the other without losing an epoch or a
    /// capability, so it is left alone and this album's own epochs stay uncovered.
    Divergent,
    /// The escrow does not open under this master key as this album — sealed under another
    /// master key, or not an escrow of it. Left alone: replacing what cannot be read could
    /// destroy epochs this device cannot see.
    Unreadable,
}

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
    /// An album key escrow opened under the master key but is not one: it lacks the escrow's
    /// domain separator, so it is some other blob the master key seals.
    #[error("the blob is not an album key escrow")]
    NotAnAlbumKeyEscrow,
    /// An album key escrow was served under one album id and holds another — a server that
    /// swapped two of an account's escrows. Refused rather than filed under either id.
    #[error("the album key escrow served for album {expected} holds album {found}")]
    EscrowAlbumMismatch {
        /// The album id the escrow was served under.
        expected: Uuid,
        /// The album id the escrow holds.
        found: Uuid,
    },
}

type Result<T> = std::result::Result<T, AlbumStoreError>;

/// One album's persisted authority state, behind the
/// [`AlbumAuthority`] seam.
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
    /// The attested authority state. For a backup-recovered album, the public ledger the artifact
    /// carried, or `None` when it carried none.
    pub authority: Option<PersistedAuthority>,
}

impl PersistedAlbum {
    /// A read-only album entry: escrowed AMKs and, when the artifact carried one, the album's
    /// public authority — but no write or admin capability. This is what a backup restore can
    /// honestly reconstruct.
    pub fn recovered(
        album_id: Uuid,
        name: &str,
        amks: Vec<AmkRow>,
        authority: Option<PersistedAuthority>,
    ) -> Self {
        let current_epoch = amks.iter().map(|(_, epoch, _)| *epoch).max().unwrap_or(0);
        let mut album = Self {
            album_id,
            name: name.to_string(),
            amks,
            current_epoch,
            write_tier_seed64: None,
            admin_seed64: None,
            authority,
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

    /// This album's **server-side key escrow**: its canonical CBOR behind
    /// a versioned domain separator, sealed under the account master key with
    /// [`MasterKey::seal`].
    ///
    /// Everything the album holds travels — every epoch's AMK, the write-tier and admin seeds,
    /// and the authority — so a device recovered from the master key alone holds the album as
    /// the device that escrowed it did. It adds no root: only the master key opens it, and the
    /// master key already opens the keystore file this is a slice of. The seal draws a fresh
    /// nonce, so two escrows of one state differ in their bytes;
    /// [`escrow_matches`](Self::escrow_matches) compares what they open to.
    ///
    /// # Errors
    ///
    /// [`AlbumStoreError::Cbor`] when the album does not encode.
    pub fn seal_escrow(&self, master: &MasterKey) -> Result<Vec<u8>> {
        Ok(master.seal(&self.escrow_plaintext()?))
    }

    /// Open an album key escrow [`seal_escrow`](Self::seal_escrow) produced, served under
    /// `album_id`.
    ///
    /// # Errors
    ///
    /// [`AlbumStoreError::Crypto`] when the master key does not open it (another account's, or
    /// tampered), [`AlbumStoreError::NotAnAlbumKeyEscrow`] when it opens but is another kind of
    /// sealed blob, [`AlbumStoreError::Cbor`] when it does not decode, and
    /// [`AlbumStoreError::EscrowAlbumMismatch`] when it holds an album other than `album_id`.
    pub fn open_escrow(blob: &[u8], master: &MasterKey, album_id: Uuid) -> Result<Self> {
        let plaintext = master.open(blob)?;
        let body = plaintext
            .strip_prefix(ALBUM_KEY_ESCROW_DOMAIN)
            .ok_or(AlbumStoreError::NotAnAlbumKeyEscrow)?;
        let mut album: Self =
            cbor::from_slice(body).map_err(|e| AlbumStoreError::Cbor(e.to_string()))?;
        if album.album_id != album_id {
            return Err(AlbumStoreError::EscrowAlbumMismatch {
                expected: album_id,
                found: album.album_id,
            });
        }
        album.normalize();
        Ok(album)
    }

    /// Whether `blob` is an escrow of exactly this album's current state: it opens under
    /// `master` as this album, and what it holds encodes to the same canonical bytes. Any
    /// failure to open is `false`.
    pub fn escrow_matches(&self, blob: &[u8], master: &MasterKey) -> bool {
        self.escrow_coverage(blob, master) == EscrowCoverage::Current
    }

    /// How the escrow `blob` the server holds for this album stands against this album's
    /// state — and so whether replacing it with this album's escrow can only add to it.
    ///
    /// The comparison is per epoch: an escrow is never replaced by one that lacks an epoch it
    /// holds, or that holds a different key for an epoch they share, because a recovery from the
    /// replacement would lose that epoch's assets. Nor is it replaced by one that lacks a
    /// write-tier seed, admin seed or authority it holds: a read-only device (one restored from a
    /// backup) would otherwise strip the capability every later recovery depends on. See
    /// [`EscrowCoverage`].
    pub fn escrow_coverage(&self, blob: &[u8], master: &MasterKey) -> EscrowCoverage {
        let Ok(escrowed) = Self::open_escrow(blob, master, self.album_id) else {
            return EscrowCoverage::Unreadable;
        };
        let local: std::collections::BTreeMap<u32, [u8; 32]> = self
            .amks
            .iter()
            .map(|(_, epoch, amk)| (*epoch, *amk))
            .collect();
        let held: std::collections::BTreeMap<u32, [u8; 32]> = escrowed
            .amks
            .iter()
            .map(|(_, epoch, amk)| (*epoch, *amk))
            .collect();
        if local
            .iter()
            .any(|(epoch, amk)| held.get(epoch).is_some_and(|other| other != amk))
        {
            return EscrowCoverage::Divergent;
        }
        let local_only = local.keys().any(|epoch| !held.contains_key(epoch));
        let held_only = held.keys().any(|epoch| !local.contains_key(epoch));
        let lacks_held_capability = self.lacks_capability_of(&escrowed);
        match (local_only, held_only) {
            (true, true) => EscrowCoverage::Divergent,
            (false, true) => EscrowCoverage::Behind,
            (true, false) if lacks_held_capability => EscrowCoverage::Divergent,
            (true, false) => EscrowCoverage::Superseded,
            (false, false) if lacks_held_capability => EscrowCoverage::Behind,
            (false, false) => match (escrowed.escrow_plaintext(), self.escrow_plaintext()) {
                (Ok(escrowed), Ok(local)) if escrowed == local => EscrowCoverage::Current,
                _ => EscrowCoverage::Superseded,
            },
        }
    }

    /// Whether `other` holds a write-tier seed, an admin seed or an authority this album does
    /// not, so that replacing `other` with this album would drop a capability.
    fn lacks_capability_of(&self, other: &Self) -> bool {
        (other.write_tier_seed64.is_some() && self.write_tier_seed64.is_none())
            || (other.admin_seed64.is_some() && self.admin_seed64.is_none())
            || (other.authority.is_some() && self.authority.is_none())
    }

    /// The bytes [`seal_escrow`](Self::seal_escrow) seals: the domain separator, then the
    /// album's canonical CBOR in its normalized form.
    fn escrow_plaintext(&self) -> Result<Vec<u8>> {
        let mut album = self.clone();
        album.normalize();
        let body =
            cbor::to_canonical_vec(&album).map_err(|e| AlbumStoreError::Cbor(e.to_string()))?;
        let mut plaintext = Vec::with_capacity(ALBUM_KEY_ESCROW_DOMAIN.len() + body.len());
        plaintext.extend_from_slice(ALBUM_KEY_ESCROW_DOMAIN);
        plaintext.extend_from_slice(&body);
        Ok(plaintext)
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
    ///
    /// `authority` is the album's public authority the artifact carried. It is adopted only by an
    /// album that holds none — an unknown album, or one recovered earlier from an artifact that
    /// carried no ledger. A **held authority is never replaced**, for the same downgrade reason.
    pub fn merge_amks(
        &mut self,
        album_id: Uuid,
        name: &str,
        rows: impl IntoIterator<Item = AmkRow>,
        authority: Option<PersistedAuthority>,
    ) -> usize {
        let rows: Vec<AmkRow> = rows.into_iter().collect();
        match self.albums.binary_search_by_key(&album_id, |a| a.album_id) {
            Ok(i) => {
                let existing = &mut self.albums[i];
                if existing.authority.is_none() {
                    existing.authority = authority;
                }
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
                let album = PersistedAlbum::recovered(album_id, name, rows, authority);
                let added = album.amks.len();
                self.albums.insert(i, album);
                added
            }
        }
    }

    /// Unseal a store from the bytes its file holds.
    ///
    /// Refuses bytes the master key did not seal and a store declaring a newer format version.
    /// Reading those bytes from `{root}/.library/albums.cbor` is `capsule-library`'s business.
    #[tracing::instrument(skip_all, fields(sealed_bytes = sealed.len()))]
    pub fn open_sealed(sealed: &[u8], master: &MasterKey) -> Result<Self> {
        let plaintext = master.open(sealed)?;
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
        Ok(store)
    }

    /// Seal the store for persistence: canonical CBOR, albums sorted by id and normalized, at the
    /// current format version, under the master key. Writing the bytes durably is
    /// `capsule-library`'s business.
    #[tracing::instrument(skip_all, fields(albums = self.albums.len()))]
    pub fn seal(&self, master: &MasterKey) -> Result<Vec<u8>> {
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
        tracing::debug!(
            sealed_bytes = sealed.len(),
            epochs = canonical.albums.iter().map(|a| a.amks.len()).sum::<usize>(),
            "album store: sealed"
        );
        Ok(sealed)
    }
}

#[cfg(test)]
mod tests {
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
            None,
        ));
        (store, writable, recovered)
    }

    #[test]
    fn album_store_round_trips_under_master_key() {
        let master = MasterKey::generate();
        let (store, writable, recovered) = fixture();
        let sealed = store.seal(&master).unwrap();

        // The sealed bytes are not the plaintext.
        assert!(
            !sealed.windows(32).any(|w| w == [7u8; 32]),
            "a raw AMK must never appear in the sealed file"
        );

        let back = AlbumStore::open_sealed(&sealed, &master).unwrap();
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
        let master = MasterKey::generate();
        let mut bytes = fixture().0.seal(&master).unwrap();

        let err = AlbumStore::open_sealed(&bytes, &MasterKey::generate()).unwrap_err();
        assert!(
            matches!(err, AlbumStoreError::Crypto(CryptoError::Auth(_))),
            "wrong master key must authenticate-fail, got {err:?}"
        );

        // Tampering with a sealed byte is equally rejected (AES-GCM tag).
        *bytes.last_mut().unwrap() ^= 0x01;
        assert!(AlbumStore::open_sealed(&bytes, &master).is_err());
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
            None,
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

        // And the sealed bytes *are* fresh-nonced: two seals of identical state differ.
        let master = MasterKey::generate();
        let first = ordered.seal(&master).unwrap();
        let second = ordered.seal(&master).unwrap();
        assert_ne!(
            first, second,
            "the store is rewritten on every album mutation, so the nonce MUST be fresh per seal"
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
            None,
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
            store.merge_amks(fresh, "Restored", vec![(fresh, 5, [1; 32])], None),
            1
        );
        let f = store.get(&fresh).unwrap();
        assert!(f.write_tier_key().unwrap().is_none());
        assert_eq!(f.current_epoch, 5);
    }

    /// #468: a backup's public authority is adopted by an album that holds none — and only by
    /// such an album. A held authority is never swapped out, and adopting one grants no signing
    /// capability.
    #[test]
    fn merge_amks_adopts_an_authority_only_where_none_is_held() {
        let (mut store, writable, recovered) = fixture();
        let other_admin = signing_key(5, 6);
        let foreign = |album| {
            PersistedAuthority::Reference(Box::new(
                ReferenceAuthority::new(album, other_admin.verifying_key())
                    .with_epoch(
                        &other_admin,
                        AmkVersion(1),
                        &signing_key(7, 8).verifying_key(),
                        true,
                    )
                    .to_ledger(),
            ))
        };
        let admin_of = |store: &AlbumStore, album: Uuid| {
            let a = store.get(&album).unwrap();
            match a.authority.as_ref().unwrap() {
                PersistedAuthority::Reference(l) => l.admin_pub.clone(),
                PersistedAuthority::OpenMls(_) => panic!("reference fixture"),
            }
        };

        // A held authority is kept.
        store.merge_amks(writable, "Trip", vec![], Some(foreign(writable)));
        assert_eq!(
            admin_of(&store, writable),
            signing_key(1, 2).verifying_key()
        );

        // A recovered album that had none adopts it, and stays read-only.
        store.merge_amks(recovered, "Restored", vec![], Some(foreign(recovered)));
        assert_eq!(admin_of(&store, recovered), other_admin.verifying_key());
        let r = store.get(&recovered).unwrap();
        assert!(r.write_tier_key().unwrap().is_none());
        assert!(r.admin_key().unwrap().is_none());

        // An unknown album lands with it, and it re-verifies on restore.
        let fresh = Uuid::from_u128(0xFF);
        store.merge_amks(
            fresh,
            "Restored",
            vec![(fresh, 1, [1; 32])],
            Some(foreign(fresh)),
        );
        let f = store.get(&fresh).unwrap();
        let authority = f
            .authority
            .as_ref()
            .unwrap()
            .restore(fresh, &f.held_epochs())
            .unwrap();
        assert!(authority.admin_chain_verifies());
        assert!(authority.has_amk(AmkVersion(1)));
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
        let master = MasterKey::generate();
        let album_id = Uuid::from_u128(0xC0FFEE);

        let mut store = AlbumStore::new();
        store.upsert(PersistedAlbum::recovered(
            album_id,
            "MLS album",
            vec![(album_id, 1, [3; 32])],
            Some(PersistedAuthority::OpenMls(ByteBuf::from(vec![1, 2, 3, 4]))),
        ));
        let sealed = store.seal(&master).unwrap();

        // The store still loads — the album is present, only its authority is unusable here.
        let back = AlbumStore::open_sealed(&sealed, &master).unwrap();
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
        let master = MasterKey::generate();
        let mut store = AlbumStore::new();
        store.version = ALBUM_STORE_VERSION + 1;
        // `seal` normalizes the version, so seal the bytes by hand.
        let plaintext = cbor::to_canonical_vec(&store).unwrap();

        assert!(matches!(
            AlbumStore::open_sealed(&master.seal(&plaintext), &master).unwrap_err(),
            AlbumStoreError::UnsupportedVersion(v) if v == ALBUM_STORE_VERSION + 1
        ));
    }

    /// An album's escrow opens under the master key to the whole album: every epoch, both
    /// signing seeds and an authority that re-verifies.
    #[test]
    fn an_album_key_escrow_round_trips_the_whole_album_under_the_master_key() {
        let master = MasterKey::generate();
        let (store, writable, _) = fixture();
        let album = store.get(&writable).unwrap();
        let blob = album.seal_escrow(&master).unwrap();
        assert!(
            !blob.windows(32).any(|w| w == [7u8; 32]),
            "a raw AMK must never appear in an escrow"
        );

        let back = PersistedAlbum::open_escrow(&blob, &master, writable).unwrap();
        assert_eq!(back.amks, album.amks);
        assert_eq!(back.current_epoch, album.current_epoch);
        assert_eq!(back.write_tier_seed64, album.write_tier_seed64);
        assert_eq!(back.admin_seed64, album.admin_seed64);
        let authority = back
            .authority
            .as_ref()
            .unwrap()
            .restore(writable, &back.held_epochs())
            .unwrap();
        assert!(authority.admin_chain_verifies());
        assert!(back.escrow_matches(&blob, &master));
    }

    /// A blob under another master key, another kind of master-sealed blob, and an escrow served
    /// under the wrong album id are each refused with their own error.
    #[test]
    fn an_album_key_escrow_refuses_the_wrong_key_the_wrong_kind_and_the_wrong_album() {
        let master = MasterKey::generate();
        let (store, writable, recovered) = fixture();
        let blob = store.get(&writable).unwrap().seal_escrow(&master).unwrap();

        assert!(matches!(
            PersistedAlbum::open_escrow(&blob, &MasterKey::generate(), writable),
            Err(AlbumStoreError::Crypto(_))
        ));
        let keystore_shaped = master.seal(&cbor::to_canonical_vec(&store).unwrap());
        assert!(matches!(
            PersistedAlbum::open_escrow(&keystore_shaped, &master, writable),
            Err(AlbumStoreError::NotAnAlbumKeyEscrow)
        ));
        assert!(matches!(
            PersistedAlbum::open_escrow(&blob, &master, recovered),
            Err(AlbumStoreError::EscrowAlbumMismatch { expected, found })
                if expected == recovered && found == writable
        ));
    }

    /// An escrow matches the state it was sealed from and nothing else: a new epoch, another
    /// album, or bytes that do not open all read as stale.
    #[test]
    fn an_album_key_escrow_matches_only_the_state_it_was_sealed_from() {
        let master = MasterKey::generate();
        let (mut store, writable, recovered) = fixture();
        let before = store.get(&recovered).unwrap().clone();
        let blob = before.seal_escrow(&master).unwrap();
        assert!(before.escrow_matches(&blob, &master));
        assert!(
            before.escrow_matches(&before.seal_escrow(&master).unwrap(), &master),
            "a second seal of one state differs in bytes and still matches"
        );

        store.merge_amks(recovered, "Restored", [(recovered, 3, [5; 32])], None);
        let after = store.get(&recovered).unwrap();
        assert!(
            !after.escrow_matches(&blob, &master),
            "a new epoch is stale"
        );
        assert!(!store.get(&writable).unwrap().escrow_matches(&blob, &master));
        assert!(!before.escrow_matches(b"not an escrow", &master));
        assert!(!before.escrow_matches(&blob, &MasterKey::generate()));
    }

    /// #837: a read-only album (restored from a backup: no signing seeds, perhaps no authority)
    /// never supersedes an escrow that carries the seeds. On the same epochs the escrow covers
    /// it ([`EscrowCoverage::Behind`]); with an extra local epoch neither may replace the other
    /// ([`EscrowCoverage::Divergent`]).
    #[test]
    fn a_read_only_album_never_supersedes_an_escrow_that_carries_its_seeds() {
        let master = MasterKey::generate();
        let (store, writable, _) = fixture();
        let full = store.get(&writable).unwrap().clone();
        let escrow = full.seal_escrow(&master).unwrap();

        let bare = PersistedAlbum::recovered(writable, "Trip", full.amks.clone(), None);
        let with_authority =
            PersistedAlbum::recovered(writable, "Trip", full.amks.clone(), full.authority.clone());
        let mut no_admin = full.clone();
        no_admin.admin_seed64 = None;
        let mut no_write_tier = full.clone();
        no_write_tier.write_tier_seed64 = None;
        let mut no_authority = full.clone();
        no_authority.authority = None;
        for (label, local) in [
            ("no seeds, no authority", &bare),
            ("no seeds", &with_authority),
            ("no admin seed", &no_admin),
            ("no write-tier seed", &no_write_tier),
            ("no authority", &no_authority),
        ] {
            assert_eq!(
                local.escrow_coverage(&escrow, &master),
                EscrowCoverage::Behind,
                "{label}: same epochs, the escrow holds what this album lacks"
            );
        }

        let mut ahead = bare.clone();
        ahead.amks.push((writable, 2, [6; 32]));
        assert_eq!(
            ahead.escrow_coverage(&escrow, &master),
            EscrowCoverage::Divergent,
            "an extra epoch does not license dropping the escrowed seeds"
        );
    }

    /// #837 controls: replacing an escrow is still allowed whenever this album holds every
    /// capability the escrow holds — a writable album adding an epoch or its seeds, and a
    /// read-only album extending a read-only escrow.
    #[test]
    fn an_album_holding_every_escrowed_capability_still_supersedes_it() {
        let master = MasterKey::generate();
        let (store, writable, recovered) = fixture();
        let full = store.get(&writable).unwrap().clone();

        let bare = PersistedAlbum::recovered(writable, "Trip", full.amks.clone(), None);
        assert_eq!(
            full.escrow_coverage(&bare.seal_escrow(&master).unwrap(), &master),
            EscrowCoverage::Superseded,
            "a writable album adds its seeds and authority to a read-only escrow"
        );

        let mut full_ahead = full.clone();
        full_ahead.amks.push((writable, 2, [6; 32]));
        assert_eq!(
            full_ahead.escrow_coverage(&full.seal_escrow(&master).unwrap(), &master),
            EscrowCoverage::Superseded,
            "a writable album adds an epoch to its own escrow"
        );

        let read_only = store.get(&recovered).unwrap().clone();
        let mut read_only_ahead = read_only.clone();
        read_only_ahead.amks.push((recovered, 3, [5; 32]));
        assert_eq!(
            read_only_ahead.escrow_coverage(&read_only.seal_escrow(&master).unwrap(), &master),
            EscrowCoverage::Superseded,
            "a read-only album adds an epoch to a read-only escrow"
        );
    }
}
