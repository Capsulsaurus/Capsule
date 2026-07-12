//! The live [`AlbumAuthority`] backed by a real OpenMLS group (RFC 9420), pinned to the
//! X-Wing PQ ciphersuite `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`) via the
//! formally-verified libcrux provider. This is the design-target authority the offline
//! [`ReferenceAuthority`](super::ReferenceAuthority) stands in for — it drops in behind the
//! same `&dyn AlbumAuthority` seam without touching [`verify_asset`](crate::crypto::verify_asset).
//!
//! **Scope (slice S-X1): the backend/authority layer only.** This lands single-member (self)
//! group creation and the epoch-ledger semantics `verify_asset` consumes:
//! - the monotonic **epoch ceiling**, advanced by a self-update commit,
//! - the per-epoch **write-tier public key** (the album's [hybrid write authority]),
//! - **AMK content-key export** from the MLS epoch secrets (the RFC 9420 exporter), and
//! - **`has_amk`** modelling the pending-vs-terminal window (an epoch attested by the chain
//!   whose in-band AMK broadcast has not yet been delivered locally).
//!
//! Membership ceremonies (`Add`/`Remove`), `Welcome`, and history delivery are **slice S-X2**
//! and are deliberately *not* implemented here — see the [S-X2 seams](#s-x2-seams) section.
//!
//! [hybrid write authority]: https://docs/design/cryptography/keys/#album-master-keys-amks

use std::collections::{BTreeMap, BTreeSet};

use openmls::prelude::{
    BasicCredential, Ciphersuite, CredentialWithKey, LeafNodeParameters, MlsGroup,
};
use openmls_basic_credential::SignatureKeyPair;
use openmls_libcrux_crypto::Provider as LibcruxProvider;
use openmls_traits::OpenMlsProvider;
use thiserror::Error;
use uuid::Uuid;

use super::AlbumAuthority;
use crate::crypto::keys::{AmkVersion, HybridSigningKey, HybridVerifyingKey};

/// The MLS ciphersuite Capsule pins every album group to: X-Wing (ML-KEM-768 + X25519) KEM,
/// ChaCha20-Poly1305 AEAD, SHA-256, Ed25519 signatures — codepoint `0x004D`. The suite ships
/// today in OpenMLS via its libcrux provider (SSoT: [Cryptography — MLS](https://docs/design/cryptography/mls/)).
pub(crate) const PINNED_CIPHERSUITE: Ciphersuite =
    Ciphersuite::MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519;

/// The wire codepoint of [`PINNED_CIPHERSUITE`]. Asserted in tests so an upstream re-pin can
/// never silently change the suite Capsule negotiates.
pub const PINNED_CIPHERSUITE_ID: u16 = 0x004D;

/// The RFC 9420 exporter label the AMK content key is derived under. A fixed, suite-scoped
/// domain-separation string; the album id is the exporter *context*, so two albums at the same
/// epoch never export the same AMK.
const AMK_EXPORT_LABEL: &str = "capsule amk v1";

/// The AMK content-key length (32 bytes), matching [`Amk`](crate::crypto::keys::Amk).
const AMK_LEN: usize = 32;

/// Errors from constructing or advancing an [`OpenMlsAuthority`]. OpenMLS/libcrux error
/// details are captured as English strings (server-internal diagnostics, not user-facing).
#[derive(Debug, Error)]
pub enum OpenMlsAuthorityError {
    /// The libcrux crypto provider failed to initialise.
    #[error("libcrux provider init failed: {0}")]
    Provider(String),
    /// The MLS leaf signature keypair could not be created or stored.
    #[error("mls signature keypair: {0}")]
    Signature(String),
    /// The MLS group could not be created at the pinned ciphersuite.
    #[error("mls group create failed: {0}")]
    GroupCreate(String),
    /// A self-update commit (epoch advance) failed to be produced or merged.
    #[error("mls self-update commit failed: {0}")]
    SelfUpdate(String),
    /// The RFC 9420 exporter failed to derive the AMK for the current epoch.
    #[error("mls exporter failed: {0}")]
    Export(String),
    /// The exporter returned an AMK of the wrong length (never expected under a fixed suite).
    #[error("exported AMK wrong length: expected {AMK_LEN}, got {0}")]
    AmkLength(usize),
}

type Result<T> = std::result::Result<T, OpenMlsAuthorityError>;

/// Per-epoch write authority: the hybrid Ed25519 + ML-DSA-65 write-tier signing key minted for
/// an epoch, and the AMK content key exported from that epoch's MLS secrets. The private write
/// key is retained locally so this member (the single writer, in S-X1) can sign asset manifests;
/// only its public half is returned through the [`AlbumAuthority`] seam.
struct EpochState {
    write_tier: HybridSigningKey,
    amk: [u8; AMK_LEN],
}

/// An [`AlbumAuthority`] backed by a live OpenMLS group pinned to [`PINNED_CIPHERSUITE`].
///
/// One instance owns one album's group. In S-X1 the group has a single (self) member; the
/// epoch ledger is produced by self-update commits and every `verify_asset` answer is read from
/// real MLS group state, never a server assertion.
pub struct OpenMlsAuthority {
    album_id: Uuid,
    /// The libcrux-backed crypto/rand/storage provider. Owned alongside the group so the
    /// exporter and self-update commits always run against this album's own state.
    provider: LibcruxProvider,
    /// The live MLS group. Its epoch (`u64`) maps to `amk_version = epoch + 1`.
    group: MlsGroup,
    /// The leaf signature keypair that authors this member's MLS commits.
    signer: SignatureKeyPair,
    /// Per-epoch write authority + AMK, keyed by `amk_version`.
    epochs: BTreeMap<u32, EpochState>,
    /// The epochs whose AMK content key is considered locally delivered. `has_amk` reads this,
    /// modelling the pending window: the chain can attest an epoch (bumping the ceiling) before
    /// its in-band AMK broadcast (slice S-X2) arrives.
    amk_held: BTreeSet<u32>,
    /// The monotonic epoch ceiling (`= max attested amk_version`).
    ceiling: u32,
}

impl OpenMlsAuthority {
    /// Create a fresh single-member (self) album group at the pinned X-Wing ciphersuite and
    /// attest its first epoch (`amk_version = 1`, MLS epoch 0). Mints the epoch-1 write-tier key
    /// and exports the epoch-1 AMK from the group secrets; the AMK is marked locally held (this
    /// member authored the epoch, so it holds the key).
    ///
    /// This is the S-X1 entry point. Adding *other* members (and thus delivering the AMK over a
    /// `Welcome`) is slice S-X2.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, ciphersuite = PINNED_CIPHERSUITE_ID))]
    pub fn create_self_group(album_id: Uuid) -> Result<Self> {
        let provider = LibcruxProvider::new()
            .map_err(|e| OpenMlsAuthorityError::Provider(format!("{e:?}")))?;

        let signer = SignatureKeyPair::new(PINNED_CIPHERSUITE.signature_algorithm())
            .map_err(|e| OpenMlsAuthorityError::Signature(format!("{e:?}")))?;
        signer
            .store(provider.storage())
            .map_err(|e| OpenMlsAuthorityError::Signature(format!("store: {e:?}")))?;

        // The MLS credential identity is the album id — a stable, non-secret label. Device- and
        // user-identity binding (the hybrid LeafNode attestation from the keys doc) rides the
        // identity layer and the S-X2 membership ceremonies, not this self-group bootstrap.
        let credential = BasicCredential::new(album_id.as_bytes().to_vec());
        let credential_with_key = CredentialWithKey {
            credential: credential.into(),
            signature_key: signer.to_public_vec().into(),
        };

        let group = MlsGroup::builder()
            .ciphersuite(PINNED_CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .build(&provider, &signer, credential_with_key)
            .map_err(|e| OpenMlsAuthorityError::GroupCreate(format!("{e:?}")))?;

        let mut authority = Self {
            album_id,
            provider,
            group,
            signer,
            epochs: BTreeMap::new(),
            amk_held: BTreeSet::new(),
            ceiling: 0,
        };
        // MLS epoch 0 → amk_version 1; the creator holds its own AMK.
        authority.ingest_current_epoch(true)?;
        tracing::info!(album_id = %album_id, epoch = authority.ceiling, "OpenMLS self-group created");
        Ok(authority)
    }

    /// Advance to a fresh epoch via a self-update commit: the RFC 9420 ratchet moves forward
    /// (new epoch secrets ⇒ a new exported AMK), the ceiling bumps by one, and a fresh write-tier
    /// key is minted — the design's "AMK bump + write-tier rotation are one commit" atomicity.
    ///
    /// `amk_present` records whether this epoch's AMK is considered locally delivered. In the
    /// single-member self case it is `true` (the committer holds its own key); the `false` path
    /// exists so S-X2 can model an epoch whose ceiling is attested by a received commit while its
    /// in-band AMK broadcast is still in flight (the *pending*, not forged, window). Returns the
    /// new `amk_version`.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, amk_present))]
    pub fn advance_epoch(&mut self, amk_present: bool) -> Result<AmkVersion> {
        // A self-update produces a pending commit; merging it moves the group to the next epoch.
        self.group
            .self_update(&self.provider, &self.signer, LeafNodeParameters::default())
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("{e:?}")))?;
        self.group
            .merge_pending_commit(&self.provider)
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("merge: {e:?}")))?;
        let version = self.ingest_current_epoch(amk_present)?;
        tracing::info!(album_id = %self.album_id, epoch = version.0, "OpenMLS epoch advanced");
        Ok(version)
    }

    /// Record the group's *current* epoch as the next `amk_version`: mint its write-tier key,
    /// export its AMK from the live MLS secrets, and advance the ceiling. Enforces the
    /// `amk_version = mls_epoch + 1` invariant.
    fn ingest_current_epoch(&mut self, amk_present: bool) -> Result<AmkVersion> {
        let version = self.ceiling + 1;
        debug_assert_eq!(
            u64::from(version),
            self.group.epoch().as_u64() + 1,
            "amk_version must track the MLS epoch (epoch 0 = amk_version 1)"
        );
        let write_tier = HybridSigningKey::generate();
        let amk = self.export_current_amk()?;
        self.epochs.insert(version, EpochState { write_tier, amk });
        if amk_present {
            self.amk_held.insert(version);
        }
        self.ceiling = version;
        Ok(AmkVersion(version))
    }

    /// Export the AMK content key for the group's **current** epoch from the RFC 9420 exporter.
    /// Deterministic for a fixed epoch (same secret ⇒ same bytes); a self-update commit changes
    /// the exporter secret and therefore the AMK. The album id is the exporter context so two
    /// albums never collide at the same epoch.
    pub fn export_current_amk(&self) -> Result<[u8; AMK_LEN]> {
        let bytes = self
            .group
            .export_secret(
                self.provider.crypto(),
                AMK_EXPORT_LABEL,
                self.album_id.as_bytes(),
                AMK_LEN,
            )
            .map_err(|e| OpenMlsAuthorityError::Export(format!("{e:?}")))?;
        bytes
            .try_into()
            .map_err(|v: Vec<u8>| OpenMlsAuthorityError::AmkLength(v.len()))
    }

    /// The cached AMK content key for `epoch`, or `None` if the chain attests no such epoch.
    /// This is the key `verify_asset`'s consumers decrypt with once `has_amk` is true.
    pub fn amk(&self, epoch: AmkVersion) -> Option<[u8; AMK_LEN]> {
        self.epochs.get(&epoch.0).map(|e| e.amk)
    }

    /// Mark an epoch's AMK content key as now locally delivered — the in-band
    /// `AlbumKeyDistribution` (S-X2) arrived — flipping a *pending* asset to verifiable. Mirrors
    /// [`ReferenceAuthority::mark_amk_present`](super::ReferenceAuthority::mark_amk_present).
    pub fn mark_amk_present(&mut self, epoch: AmkVersion) {
        if self.epochs.contains_key(&epoch.0) {
            self.amk_held.insert(epoch.0);
        }
    }

    /// The write-tier **signing** key for `epoch` — the private half this member signs asset
    /// manifests with (it is both admin and writer in the single-member S-X1 case). Distribution
    /// of this key to *other* writers over MLS is slice S-X2; here it is the seam the lifecycle
    /// (and tests) use to author a manifest `verify_asset` will accept.
    pub fn write_tier_signing_key(&self, epoch: AmkVersion) -> Option<&HybridSigningKey> {
        self.epochs.get(&epoch.0).map(|e| &e.write_tier)
    }

    /// The current (ceiling) epoch's write-tier signing key — the one new writes are signed under.
    pub fn current_write_tier(&self) -> &HybridSigningKey {
        &self
            .epochs
            .get(&self.ceiling)
            .expect("ceiling epoch always has state")
            .write_tier
    }

    /// The live MLS group epoch (`u64`). `amk_version` is always this plus one.
    pub fn mls_epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    /// The ciphersuite the group is actually running — asserted `== PINNED_CIPHERSUITE` in tests.
    pub fn ciphersuite(&self) -> Ciphersuite {
        self.group.ciphersuite()
    }

    // ── S-X2 seams (membership ceremonies) ──────────────────────────────────
    //
    // Deliberately unimplemented in S-X1; named here so the follow-up lands against known hooks:
    //
    // - `add_member(&mut self, key_package) -> (Commit, Welcome)` — MLS `Add` + `Commit`; the
    //   `Welcome` carries `AMK_v_current` (and, per `history_policy`, the prior AMKs) as an
    //   extension. Consumes `self.epochs[*].amk` for the history blob.
    // - `remove_member(&mut self, leaf) -> Commit` — MLS `Remove` + `Commit`, then
    //   `advance_epoch(true)` to re-key so the removed member cannot read future epochs.
    // - `process_welcome(welcome) -> Self` — the joiner side: adopt the group, learn the epoch
    //   ceiling from the commit chain, and populate `amk_held` from the delivered AMK range.
    // - `deliver_amk(&mut self, amk_version, bytes)` — apply an in-band `AlbumKeyDistribution`;
    //   the received-commit path that flips `amk_held` for an epoch attested but not yet keyed.
    //
    // SSoT for all four: [Cryptography — MLS](https://docs/design/cryptography/mls/).
}

impl AlbumAuthority for OpenMlsAuthority {
    fn album_id(&self) -> Uuid {
        self.album_id
    }

    fn epoch_ceiling(&self) -> AmkVersion {
        AmkVersion(self.ceiling)
    }

    fn write_tier_pubkey(&self, epoch: AmkVersion) -> Option<HybridVerifyingKey> {
        self.epochs
            .get(&epoch.0)
            .map(|e| e.write_tier.verifying_key())
    }

    fn has_amk(&self, epoch: AmkVersion) -> bool {
        self.amk_held.contains(&epoch.0)
    }

    fn admin_chain_verifies(&self) -> bool {
        // The live group *is* the admin-signed commit chain: OpenMLS validated every commit as it
        // was processed, so a group we hold is trusted iff it is the pinned suite, still active
        // (not evicted), and has attested at least the genesis epoch with a consistent ceiling.
        self.group.ciphersuite() == PINNED_CIPHERSUITE
            && self.group.is_active()
            && self.ceiling >= 1
            && self.ceiling == self.epochs.keys().copied().max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::authority::ReferenceAuthority;
    use crate::crypto::hash;
    use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
    use crate::crypto::keys::{DeviceDirectory, HybridSigningKey};
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::crypto::provenance::action::Action;
    use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
    use crate::crypto::verify_asset::{VerifyOutcome, verify_asset};

    const USER: u128 = 0x05E2;
    const DEVICE: u128 = 0xD1;
    const CIPHERTEXT: &[u8] = b"the asset ciphertext bytes";

    /// A device directory + device key so a manifest signed by this device verifies.
    fn directory_for(user: Uuid, device_id: Uuid, device: &HybridSigningKey) -> DeviceDirectory {
        let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
        DirectoryCore {
            user_id: user,
            directory_version: 1,
            updated_at: "2026-05-30T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id,
                dsk_public: device.verifying_key(),
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
            }],
        }
        .sign(&ik)
    }

    #[test]
    fn ciphersuite_is_pinned_xwing_0x004d() {
        let auth = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
        assert_eq!(auth.ciphersuite(), PINNED_CIPHERSUITE);
        assert_eq!(u16::from(auth.ciphersuite()), PINNED_CIPHERSUITE_ID);
        assert_eq!(PINNED_CIPHERSUITE_ID, 0x004D);
    }

    #[test]
    fn genesis_epoch_and_ceiling() {
        let auth = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
        assert!(auth.admin_chain_verifies());
        assert_eq!(auth.epoch_ceiling(), AmkVersion(1));
        assert_eq!(auth.mls_epoch(), 0);
        assert!(auth.write_tier_pubkey(AmkVersion(1)).is_some());
        assert!(auth.write_tier_pubkey(AmkVersion(2)).is_none());
        assert!(auth.has_amk(AmkVersion(1)));
        assert!(!auth.has_amk(AmkVersion(2)));
    }

    #[test]
    fn amk_export_is_deterministic_and_advances_with_epoch() {
        let mut auth = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
        // Exporting the current epoch's AMK twice yields identical bytes.
        let a1 = auth.export_current_amk().unwrap();
        let a2 = auth.export_current_amk().unwrap();
        assert_eq!(a1, a2, "AMK export must be deterministic within an epoch");
        assert_eq!(auth.amk(AmkVersion(1)), Some(a1));

        // Advancing the epoch (self-update commit) changes the AMK and bumps the ceiling.
        let v2 = auth.advance_epoch(true).unwrap();
        assert_eq!(v2, AmkVersion(2));
        assert_eq!(auth.epoch_ceiling(), AmkVersion(2));
        assert_eq!(auth.mls_epoch(), 1);
        let b = auth.export_current_amk().unwrap();
        assert_ne!(a1, b, "advancing the epoch must change the AMK");
        assert_eq!(auth.amk(AmkVersion(2)), Some(b));
        // The prior epoch's AMK stays available (assets under epoch 1 remain decryptable).
        assert_eq!(auth.amk(AmkVersion(1)), Some(a1));
    }

    #[test]
    fn different_albums_export_different_amks() {
        let a = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xA1)).unwrap();
        let b = OpenMlsAuthority::create_self_group(Uuid::from_u128(0xB2)).unwrap();
        // Same genesis epoch, different album context ⇒ different AMK.
        assert_ne!(a.amk(AmkVersion(1)), b.amk(AmkVersion(1)));
    }

    #[test]
    fn verify_asset_accepts_manifest_under_live_mls_authority() {
        let album = Uuid::from_u128(0xA1);
        let auth = OpenMlsAuthority::create_self_group(album).unwrap();
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let directory = directory_for(Uuid::from_u128(USER), Uuid::from_u128(DEVICE), &device);

        let write_tier = auth.write_tier_signing_key(AmkVersion(1)).unwrap();
        let manifest = create_manifest_core(album, AmkVersion(1))
            .sign(&device, write_tier)
            .unwrap();

        // A live OpenMLS authority drops in behind the same seam ReferenceAuthority uses.
        assert_eq!(
            verify_asset(&manifest, CIPHERTEXT, &directory, &auth, None),
            VerifyOutcome::Accept
        );
    }

    #[test]
    fn wrong_epoch_terminal_rejects_like_reference_authority() {
        let album = Uuid::from_u128(0xA1);
        let auth = OpenMlsAuthority::create_self_group(album).unwrap();
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let directory = directory_for(Uuid::from_u128(USER), Uuid::from_u128(DEVICE), &device);

        // A manifest claiming epoch 2 while the chain only attests epoch 1 → terminal WrongEpoch,
        // the same answer the offline ReferenceAuthority gives for the same over-ceiling claim.
        let write_tier = auth.write_tier_signing_key(AmkVersion(1)).unwrap();
        let manifest = create_manifest_core(album, AmkVersion(2))
            .sign(&device, write_tier)
            .unwrap();
        assert!(matches!(
            verify_asset(&manifest, CIPHERTEXT, &directory, &auth, None),
            VerifyOutcome::TerminalReject(_)
        ));
    }

    #[test]
    fn pending_when_amk_not_yet_delivered_then_accept_on_delivery() {
        let album = Uuid::from_u128(0xA1);
        let mut auth = OpenMlsAuthority::create_self_group(album).unwrap();
        // Advance the ceiling but withhold the AMK — the pending, not-forged window (S-X2 delivery
        // in flight). Mirrors ReferenceAuthority attesting an epoch with `amk_present = false`.
        let v2 = auth.advance_epoch(false).unwrap();
        assert_eq!(v2, AmkVersion(2));
        assert!(!auth.has_amk(AmkVersion(2)));

        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let directory = directory_for(Uuid::from_u128(USER), Uuid::from_u128(DEVICE), &device);
        let write_tier = auth.write_tier_signing_key(AmkVersion(2)).unwrap();
        let manifest = create_manifest_core(album, AmkVersion(2))
            .sign(&device, write_tier)
            .unwrap();
        // Epoch is within the attested ceiling but its AMK is not local yet → Pending.
        assert!(matches!(
            verify_asset(&manifest, CIPHERTEXT, &directory, &auth, None),
            VerifyOutcome::Pending(_)
        ));
        // Deliver the AMK; the same manifest now accepts.
        auth.mark_amk_present(AmkVersion(2));
        assert!(auth.has_amk(AmkVersion(2)));
        assert_eq!(
            verify_asset(&manifest, CIPHERTEXT, &directory, &auth, None),
            VerifyOutcome::Accept
        );
    }

    #[test]
    fn parity_with_reference_authority_over_equivalent_history() {
        // For an equivalent single-member album history, both authorities answer the trait
        // consistently: same ceiling progression, same lookup-beyond-ceiling = None, same
        // has_amk semantics, both admin chains verify.
        let album = Uuid::from_u128(0xA1);
        let mut mls = OpenMlsAuthority::create_self_group(album).unwrap();

        let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
        let w1 = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let w2 = HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]);
        let mut reference = ReferenceAuthority::new(album, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &w1.verifying_key(),
            true,
        );

        // Genesis parity.
        assert_eq!(mls.epoch_ceiling(), reference.epoch_ceiling());
        assert_eq!(mls.album_id(), reference.album_id());
        assert!(mls.admin_chain_verifies() && reference.admin_chain_verifies());
        assert!(mls.has_amk(AmkVersion(1)) && reference.has_amk(AmkVersion(1)));
        assert!(mls.write_tier_pubkey(AmkVersion(2)).is_none());
        assert!(reference.write_tier_pubkey(AmkVersion(2)).is_none());

        // Advance both by one epoch and re-check parity.
        mls.advance_epoch(true).unwrap();
        reference.attest_epoch(&admin, AmkVersion(2), &w2.verifying_key(), true);
        assert_eq!(mls.epoch_ceiling(), AmkVersion(2));
        assert_eq!(mls.epoch_ceiling(), reference.epoch_ceiling());
        assert!(mls.has_amk(AmkVersion(2)) && reference.has_amk(AmkVersion(2)));
        assert!(mls.write_tier_pubkey(AmkVersion(3)).is_none());
        assert!(reference.write_tier_pubkey(AmkVersion(3)).is_none());
    }

    /// A valid `create` manifest core for `album`/`epoch`, signed by the caller.
    fn create_manifest_core(album: Uuid, epoch: AmkVersion) -> ManifestCore {
        ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: Uuid::from_u128(0xF11E),
            album_id: album,
            amk_version: epoch,
            ciphertext_hash: hash::hash_bytes(CIPHERTEXT),
            plaintext_size: 12,
            chunk_size: 65_520,
            nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
            key_mode: KeyMode::Derived,
            wrapped_file_key: None,
            metadata_blob_hash: Some(hash::Hash32([0x4D; 32])),
            created_by_user: Uuid::from_u128(USER),
            created_by_device: Uuid::from_u128(DEVICE),
            client_version: "capsule-cli/0.1.0".into(),
            timestamp: "2026-05-31T12:00:00Z".into(),
            action: Action::Create,
            prior_provenance_hash: None,
            retention_until: None,
        }
    }
}
