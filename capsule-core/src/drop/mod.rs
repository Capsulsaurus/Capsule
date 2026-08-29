//! Web-upload guest drops — **contract skeleton** (slice `S-A6` in the repo-root
//! `SLICES.md`; SSoT: [Web Upload]).
//!
//! A guest with an upload link seals each asset under a fresh random key `K`,
//! encapsulates `K` to the link's Drop Key, and uploads the sealed bytes to the
//! provisioning user's staging inbox. Nothing becomes a library asset until one of that
//! user's trusted clients **adopts** the drop — decapsulating `K`, rewrapping it under the
//! album AMK (`asset-keywrap/v1`, [`KeyMode::Wrapped`]), and signing an ordinary `create`
//! manifest. The guest is never a signer; drops never flow through `verify_asset`.
//!
//! This module owns the client-side halves: link issuance, drop sealing (compiled to WASM
//! for `capsule-web`), and adoption. The server halves (drop store, inbox, atomic
//! inbox→album promotion) live in `capsule-api-media::drops`.
//!
//! [Web Upload]: https://docs/design/web-upload/
//! [`KeyMode::Wrapped`]: crate::crypto::provenance::KeyMode

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::crypto::encryption::stream::NONCE_PREFIX_LEN;
use crate::crypto::encryption::{open_blob, seal_blob_with_nonce, stream};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{
    DEK_CIPHERTEXT_LEN, DekKeypair, ESEED_LEN, encapsulate_to_public_derand,
};
use crate::crypto::primitives::Argon2Params;
use crate::crypto::provenance::AssetManifest;
use crate::crypto::{pwkdf, rng};

/// Length of an upload-link opaque URL-path id: a **full 128 bits** of CSPRNG entropy —
/// never a structured/UUIDv7 id (identical rule to a [share link], whose embedded v7
/// timestamp would cut real entropy to ~62 bits). SSoT: [Web Upload] Security Contract.
///
/// [share link]: https://docs/design/share-links/#security-contract
/// [Web Upload]: https://docs/design/web-upload/
pub const OPAQUE_ID_LEN: usize = 16;

/// Draw a fresh opaque upload-link URL-path id: a full 128 bits of CSPRNG entropy, **not** a
/// structured or sequential identifier. Mirrors the share-link opaque-id discipline exactly
/// (SSoT: [Web Upload] Security Contract — the URL is `.../u/{opaque-id}#{drop_pubkey}`).
///
/// [Web Upload]: https://docs/design/web-upload/
pub fn generate_opaque_id() -> [u8; OPAQUE_ID_LEN] {
    rng::random_array::<OPAQUE_ID_LEN>()
}

/// A provisioned upload link: the server-held record plus the fragment-delivered public
/// half. The `opaque_id` follows the share-link rule (random ≥128-bit, never structured);
/// `drop_pubkey` travels only in the URL fragment and never reaches the server.
#[derive(Debug, Clone)]
pub struct UploadLink {
    /// Revocation handle (internal owner-held; never in the URL, so a creation-time-leaking
    /// UUIDv7 is fine here — unlike [`opaque_id`](UploadLink::opaque_id), which is
    /// URL-exposed and must be non-structured).
    pub link_id: UploadLinkId,
    /// The link's random 128-bit opaque id (the URL path component).
    pub opaque_id: [u8; 16],
    /// The Drop Key public half (KEM encapsulation key; URL fragment only).
    pub drop_pubkey: Vec<u8>,
    /// The caps this link was provisioned with.
    pub caps: LinkCaps,
}

/// Identifies a provisioned upload link for revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UploadLinkId(pub Uuid);

/// Identifies a pending drop in the provisioning user's inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DropId(pub Uuid);

/// Per-link caps, enforced server-side at the no-key layer on every drop-session
/// creation ([Web Upload — Security Contract](https://docs/design/web-upload/)).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCaps {
    /// RFC 3339 expiry; `None` = no expiry (revocation still applies).
    pub expires_at: Option<String>,
    /// Cumulative byte cap across all drops on this link.
    pub max_total_bytes: Option<u64>,
    /// Maximum number of files this link may deposit.
    pub max_file_count: Option<u32>,
    /// Maximum single-file size.
    pub max_file_size: Option<u64>,
    /// Whether the link dies after its first successful drop.
    pub single_use: bool,
}

/// The unsigned descriptor a guest uploads beside the sealed ciphertext. Deliberately
/// **not** an `AssetManifest`: no signatures, no `album_id`, no provenance link. Its
/// integrity is established only when a trusted client decapsulates `K` and the STREAM
/// tags verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropDescriptor {
    /// Closed enum for the link's pinned `protocol_version` (same set as a manifest's).
    pub content_type: String,
    /// Total plaintext byte length.
    pub plaintext_size: u64,
    /// The STREAM plaintext chunk size (owned by Encryption).
    pub chunk_size: u32,
    /// The STREAM nonce prefix used for this seal.
    pub nonce_prefix: [u8; 7],
    /// Content-address digest of the STREAM ciphertext.
    pub ciphertext_hash: Hash32,
    /// `K` encapsulated to the link's Drop Key; length fixed by `crypto_suite_id`.
    #[serde(with = "serde_bytes")]
    pub kem_ct: Vec<u8>,
    /// Guest-supplied, unverified; advisory only.
    pub suggested_filename: Option<String>,
}

/// A sealed drop ready for upload: the descriptor plus the STREAM ciphertext.
#[derive(Debug, Clone)]
pub struct SealedDrop {
    /// The unsigned descriptor.
    pub descriptor: DropDescriptor,
    /// The STREAM ciphertext bytes.
    pub ciphertext: Vec<u8>,
}

/// A drop awaiting review in the provisioning user's inbox.
#[derive(Debug, Clone)]
pub struct PendingDrop {
    /// The inbox row id.
    pub drop_id: DropId,
    /// The guest's descriptor.
    pub descriptor: DropDescriptor,
    /// The link it arrived through.
    pub via_link: UploadLinkId,
    /// Server-attested arrival time (RFC 3339, `received_at`).
    pub received_at: String,
}

/// Failure surfaced by the drop lifecycle.
#[derive(Debug, Error)]
pub enum DropError {
    /// The link is expired, revoked, or over a cap.
    #[error("upload link refused: {0}")]
    LinkRefused(&'static str),
    /// The KEM decapsulation or STREAM verification failed.
    #[error("drop crypto failure: {0}")]
    Crypto(&'static str),
    /// The drop was not found in the caller's inbox.
    #[error("pending drop not found")]
    NotFound,
}

/// An Argon2id **abuse-gate** verifier for a passphrase-protected upload link. Unlike a
/// [share-link passphrase] (which wraps a *read* secret the client unwraps locally), this
/// gates a **write**, so the server must verify possession: the link record stores the
/// `salt` + Argon2id-derived `verifier`, and the guest proves possession at drop-session
/// creation. The passphrase itself is never transmitted or stored, and this adds **no**
/// confidentiality — the guest already encrypts every asset (SSoT: [Web Upload] Security
/// Contract — Optional passphrase).
///
/// [share-link passphrase]: https://docs/design/share-links/#security-contract
/// [Web Upload]: https://docs/design/web-upload/
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassphraseVerifier {
    /// Argon2id memory cost (KiB) used to derive the verifier.
    pub mem_kib: u32,
    /// Argon2id iteration cost `t`.
    pub t_cost: u32,
    /// Argon2id parallelism `p`.
    pub p_cost: u32,
    /// 16-byte CSPRNG salt.
    pub salt: [u8; 16],
    /// The Argon2id output the guest's proof must equal.
    pub verifier: [u8; 32],
}

impl PassphraseVerifier {
    /// Derive a fresh verifier for `passphrase` under `params` (a fresh random salt each
    /// time). The KDF cost rate-limits guessing on top of the serve path's per-IP/per-link
    /// limiters.
    pub fn derive(passphrase: &str, params: Argon2Params) -> Result<Self, DropError> {
        let salt = rng::random_array::<16>();
        let verifier = pwkdf::derive_wrap_key(passphrase.as_bytes(), &salt, params)
            .map_err(|_| DropError::Crypto("passphrase verifier derivation failed"))?;
        Ok(Self {
            mem_kib: params.mem_kib,
            t_cost: params.t_cost,
            p_cost: params.p_cost,
            salt,
            verifier,
        })
    }

    /// Constant-time check that `passphrase` reproduces the stored verifier. The serve path
    /// (S-C5) calls this to gate a drop-session; a wrong passphrase is refused.
    pub fn verify(&self, passphrase: &str) -> bool {
        let params = Argon2Params {
            mem_kib: self.mem_kib,
            t_cost: self.t_cost,
            p_cost: self.p_cost,
        };
        match pwkdf::derive_wrap_key(passphrase.as_bytes(), &self.salt, params) {
            Ok(proof) => {
                proof
                    .iter()
                    .zip(self.verifier.iter())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
            }
            Err(_) => false,
        }
    }
}

/// Issues and revokes upload links on a trusted (native) client — the seam
/// `lifecycle::Workspace` will implement. Provisioning mints the Drop Key, wraps its
/// private half under the master key + OGK escrow, and registers the link record.
pub trait UploadLinkIssuer {
    /// Provision an upload link with `caps`; `passphrase` adds the server-verified
    /// Argon2id abuse gate (never transmitted — the record stores a verifier).
    fn create_link(
        &mut self,
        caps: LinkCaps,
        passphrase: Option<&str>,
    ) -> Result<UploadLink, DropError>;

    /// Revoke a link; the serve path refuses it within its fail-closed cache window.
    fn revoke_link(&mut self, link: UploadLinkId) -> Result<(), DropError>;
}

/// Reviews and adopts pending drops on a trusted (native) client — decapsulate `K`,
/// rewrap under the destination album's AMK, author the sidecar, sign the `create`
/// manifest with `key_mode = wrapped`, and submit the atomic inbox→album promotion.
pub trait DropAdopter {
    /// The provisioning user's pending drops.
    fn list_inbox(&self) -> Result<Vec<PendingDrop>, DropError>;

    /// Adopt a drop into `album_id` in place (no byte re-upload). Returns the signed
    /// adopting `create` manifest whose `ciphertext_hash` references the inbox blob.
    fn adopt(&mut self, drop: DropId, album_id: Uuid) -> Result<AssetManifest, DropError>;

    /// Discard a pending drop; its bytes are GC'd and the quota freed.
    fn discard(&mut self, drop: DropId) -> Result<(), DropError>;
}

/// Seal `plaintext` for a guest drop: draw a fresh random `K`, STREAM-encrypt under it,
/// and encapsulate `K` to `drop_pubkey` (the link's KEM public half, from the URL
/// fragment). Runs in the browser (WASM) and on native clients alike — it is the **only**
/// path compiled to `wasm32-unknown-unknown`.
///
/// The `kem_ct` is a KEM-DEM: the X-Wing ciphertext (encapsulating a fresh shared secret
/// `ss` to the Drop Key) followed by `seal_blob(ss, K)`. `K` was chosen by the guest and is
/// **carried** wrapped — it is never derived from an AMK — so a weak or reused `K`
/// compromises at most its own drop (SSoT: [Web Upload] Failure Modes). The output length
/// is fixed by the suite: [`DEK_CIPHERTEXT_LEN`] + the sealed-blob overhead.
///
/// [Web Upload]: https://docs/design/web-upload/
pub fn seal_drop(
    plaintext: &[u8],
    drop_pubkey: &[u8],
    content_type: &str,
) -> Result<SealedDrop, DropError> {
    // The production path draws every value the seal randomizes — the asset key `K`, the STREAM
    // nonce prefix, the KEM encapsulation seed, and the key-wrap nonce — from the OS CSPRNG, then
    // delegates to the deterministic core so the two share one implementation.
    let k = rng::random_array::<32>();
    let nonce_prefix = rng::random_array::<NONCE_PREFIX_LEN>();
    let eseed = rng::random_array::<ESEED_LEN>();
    let blob_nonce = rng::random_array::<12>();
    seal_drop_derand(
        plaintext,
        drop_pubkey,
        content_type,
        &k,
        &nonce_prefix,
        &eseed,
        &blob_nonce,
    )
}

/// The deterministic core of [`seal_drop`]: every randomized value is supplied explicitly.
///
/// **Exposed for known-answer testing only** — the public path is [`seal_drop`], which draws all
/// four values from the CSPRNG. Feeding fixed `k`, `nonce_prefix`, `eseed`, and `blob_nonce`
/// makes the sealed bytes reproducible so the browser (WASM) seal can be proven byte-identical to
/// this Rust implementation across the language boundary (the S-D3 cross-language KAT), and so the
/// fixture that a Rust adopter consumes is a pure function of its inputs. **Never** reuse a
/// `(k, nonce_prefix)` pair for two distinct plaintexts (STREAM nonce reuse) outside a KAT.
///
/// - `k` — the 32-byte asset key.
/// - `nonce_prefix` — the 7-byte STREAM nonce prefix.
/// - `eseed` — the 64-byte X-Wing encapsulation seed (`m ‖ x25519-ephemeral`).
/// - `blob_nonce` — the 12-byte AEAD nonce wrapping `K` under the KEM shared secret.
pub fn seal_drop_derand(
    plaintext: &[u8],
    drop_pubkey: &[u8],
    content_type: &str,
    k: &[u8; 32],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
    eseed: &[u8; ESEED_LEN],
    blob_nonce: &[u8; 12],
) -> Result<SealedDrop, DropError> {
    // 1. STREAM-encrypt the asset under K with the given nonce prefix, hashing the ciphertext.
    let (enc, ciphertext) = stream::encrypt_asset_vec_with_prefix(k, *nonce_prefix, plaintext);

    // 2. Encapsulate K to the Drop Key public half (KEM-DEM): X-Wing ciphertext ‖ seal(ss, K).
    let (kem_ct, ss) = encapsulate_to_public_derand(drop_pubkey, eseed)
        .map_err(|_| DropError::Crypto("drop key encapsulation failed"))?;
    let wrapped_k = seal_blob_with_nonce(&ss, *blob_nonce, k);
    let mut kem_ct_dem = Vec::with_capacity(kem_ct.len() + wrapped_k.len());
    kem_ct_dem.extend_from_slice(&kem_ct);
    kem_ct_dem.extend_from_slice(&wrapped_k);

    // 3. Emit the unsigned descriptor beside the ciphertext.
    let descriptor = DropDescriptor {
        content_type: content_type.to_owned(),
        plaintext_size: enc.plaintext_size,
        chunk_size: enc.chunk_size,
        nonce_prefix: enc.nonce_prefix,
        ciphertext_hash: enc.ciphertext_hash,
        kem_ct: kem_ct_dem,
        suggested_filename: None,
    };
    Ok(SealedDrop {
        descriptor,
        ciphertext,
    })
}

/// Recover the guest-chosen asset key `K` from a drop's `kem_ct` using the Drop Key's
/// private half — the first step of [adoption]. Splits the X-Wing ciphertext from the
/// AEAD-wrapped key, decapsulates the shared secret, and opens the wrap. A truncated
/// `kem_ct`, a foreign/tampered ciphertext, or a wrong Drop Key all fail closed
/// ([`DropError::Crypto`]).
///
/// [adoption]: https://docs/design/web-upload/#4-review-and-adopt-in-place-native-client-provisioning-user
pub fn open_drop_key(drop_key: &DekKeypair, kem_ct: &[u8]) -> Result<[u8; 32], DropError> {
    if kem_ct.len() <= DEK_CIPHERTEXT_LEN {
        return Err(DropError::Crypto("kem_ct too short"));
    }
    let (ct, wrapped_k) = kem_ct.split_at(DEK_CIPHERTEXT_LEN);
    let ss = drop_key
        .decapsulate(ct)
        .map_err(|_| DropError::Crypto("drop key decapsulation failed"))?;
    let k =
        open_blob(&ss, wrapped_k).map_err(|_| DropError::Crypto("wrapped drop key open failed"))?;
    k.as_slice()
        .try_into()
        .map_err(|_| DropError::Crypto("recovered key wrong length"))
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::cbor;
    use crate::crypto::authority::ReferenceAuthority;
    use crate::crypto::encryption::keywrap::{seal_file_key, unseal_file_key};
    use crate::crypto::encryption::stream::decrypt_asset_vec;
    use crate::crypto::hash;
    use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
    use crate::crypto::keys::{Amk, AmkVersion, DekKeypair, HybridSigningKey};
    use crate::crypto::primitives::{Argon2Params, CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::crypto::provenance::action::Action;
    use crate::crypto::provenance::manifest::{
        ASSET_MANIFEST_VERSION, KeyMode, ManifestCore, WrappedFileKey,
    };
    use crate::crypto::verify_asset::{VerifyOutcome, verify_asset};

    /// Expected KEM-DEM `kem_ct` length under suite `V1`: the X-Wing ciphertext
    /// (`DEK_CIPHERTEXT_LEN`) followed by `seal_blob(ss, K)` = suite(2) ‖ nonce(12) ‖ K(32)
    /// ‖ tag(16) = 62 bytes.
    const EXPECTED_KEM_CT_LEN: usize = DEK_CIPHERTEXT_LEN + 2 + 12 + 32 + 16;

    /// Doc "Drop seal round-trip (unit)": seal a plaintext under a random `K` to a Drop Key
    /// public half; decapsulate with the private half; STREAM-decrypt; assert byte-equality;
    /// assert `kem_ct` length matches the suite; assert the descriptor round-trips through
    /// canonical CBOR.
    #[test]
    fn drop_seal_round_trip() {
        let drop_key = DekKeypair::generate();
        let plaintext = b"a guest's photo bytes, sealed under a fresh random K in the browser";

        let sealed = seal_drop(plaintext, &drop_key.public_bytes(), "image/jpeg").unwrap();

        // The encapsulated-key length is fixed by the suite (server-observable, deterministic).
        assert_eq!(sealed.descriptor.kem_ct.len(), EXPECTED_KEM_CT_LEN);
        assert_eq!(sealed.descriptor.plaintext_size, plaintext.len() as u64);
        // The descriptor commits to the exact ciphertext bytes.
        assert_eq!(
            sealed.descriptor.ciphertext_hash,
            hash::hash_bytes(&sealed.ciphertext)
        );

        // The Drop Key holder recovers K, and STREAM-decrypt returns the exact plaintext.
        let k = open_drop_key(&drop_key, &sealed.descriptor.kem_ct).unwrap();
        let back =
            decrypt_asset_vec(&k, &sealed.descriptor.nonce_prefix, &sealed.ciphertext).unwrap();
        assert_eq!(back, plaintext);

        // The descriptor round-trips byte-identically through canonical CBOR.
        let bytes = cbor::to_canonical_vec(&sealed.descriptor).unwrap();
        let decoded: DropDescriptor = cbor::from_slice(&bytes).unwrap();
        assert_eq!(decoded, sealed.descriptor);

        // A different Drop Key cannot recover K (server-blindness / contribute-only).
        let attacker = DekKeypair::generate();
        let wrong = open_drop_key(&attacker, &sealed.descriptor.kem_ct);
        // Decapsulation yields a different shared secret → the AEAD wrap open fails closed.
        assert!(wrong.is_err() || wrong.unwrap() != k);
    }

    /// A minimal, fully-valid signing setup so the adoption test can build a manifest
    /// `verify_asset` accepts — the same construction the provenance suite's `Fixture` uses.
    struct AdopterFixture {
        device: HybridSigningKey,
        write: HybridSigningKey,
        directory: crate::crypto::keys::DeviceDirectory,
        authority: ReferenceAuthority,
        user_id: Uuid,
        device_id: Uuid,
        album_id: Uuid,
    }

    impl AdopterFixture {
        fn new() -> Self {
            let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
            let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
            let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
            let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
            let user_id = Uuid::from_u128(0x05E2);
            let device_id = Uuid::from_u128(0xD1);
            let album_id = Uuid::from_u128(0xA1);

            let directory = DirectoryCore {
                user_id,
                directory_version: 1,
                updated_at: "2026-05-30T00:00:00Z".into(),
                devices: vec![DeviceEntry {
                    device_id,
                    dsk_public: device.verifying_key(),
                    dek_public: None,
                    added_at: "2026-05-30T00:00:00Z".into(),
                    revoked_at: None,
                }],
            }
            .sign(&ik);

            let authority = ReferenceAuthority::new(album_id, admin.verifying_key()).with_epoch(
                &admin,
                AmkVersion(1),
                &write.verifying_key(),
                true,
            );

            Self {
                device,
                write,
                directory,
                authority,
                user_id,
                device_id,
                album_id,
            }
        }
    }

    /// Doc "Adoption rewrap accepts (unit)": decapsulate a drop, rewrap `K` under a test AMK
    /// (`asset-keywrap/v1`), build the `create` manifest with `key_mode = wrapped`; assert
    /// `verify_asset` accepts and a second member can unwrap `wrapped_file_key` and
    /// STREAM-decrypt the unchanged ciphertext.
    #[test]
    fn adoption_rewrap_verifies_and_decrypts() {
        let f = AdopterFixture::new();
        let amk = Amk::from_bytes([0x5A; 32]);
        let file_id = Uuid::from_u128(0xF11E);

        // A guest seals a drop to the link's Drop Key (the browser/WASM half).
        let drop_key = DekKeypair::generate();
        let plaintext = b"adopted web-upload drop bytes, chosen by an external party";
        let sealed = seal_drop(plaintext, &drop_key.public_bytes(), "image/jpeg").unwrap();

        // The adopter decapsulates K and rewraps it under the destination album's AMK.
        let k = open_drop_key(&drop_key, &sealed.descriptor.kem_ct).unwrap();
        let wrapped_file_key = seal_file_key(&amk, &file_id, &k);

        // Build + sign the adopting `create` manifest — key_mode = wrapped, ciphertext_hash
        // and nonce_prefix carried from the drop; the adopter is the cryptographic author.
        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id,
            album_id: f.album_id,
            amk_version: AmkVersion(1),
            ciphertext_hash: sealed.descriptor.ciphertext_hash,
            plaintext_size: sealed.descriptor.plaintext_size,
            chunk_size: sealed.descriptor.chunk_size,
            nonce_prefix: sealed.descriptor.nonce_prefix,
            key_mode: KeyMode::Wrapped,
            wrapped_file_key: Some(WrappedFileKey(wrapped_file_key)),
            metadata_blob_hash: Some(hash::hash_bytes(b"the adopter's freshly authored sidecar")),
            created_by_user: f.user_id,
            created_by_device: f.device_id,
            client_version: "capsule-core/0.1.0".into(),
            timestamp: "2026-05-31T12:00:00Z".into(),
            action: Action::Create,
            prior_provenance_hash: None,
            retention_until: None,
        };
        let manifest = core.sign(&f.device, &f.write).unwrap();

        // verify_asset accepts the adopting manifest against the unchanged drop ciphertext.
        assert_eq!(
            verify_asset(
                &manifest,
                &sealed.ciphertext,
                &f.directory,
                &f.authority,
                None
            ),
            VerifyOutcome::Accept,
        );

        // A second album member (holding the same AMK) unwraps K and decrypts the bytes.
        let recovered = unseal_file_key(
            &amk,
            &file_id,
            &manifest.core.wrapped_file_key.as_ref().unwrap().0,
        )
        .unwrap();
        assert_eq!(recovered, k);
        assert_eq!(
            decrypt_asset_vec(
                &recovered,
                &sealed.descriptor.nonce_prefix,
                &sealed.ciphertext
            )
            .unwrap(),
            plaintext,
        );
    }

    /// `seal_drop_derand` is a pure function of its inputs (byte-reproducible), and its output is
    /// adoptable exactly like a CSPRNG-sealed drop — the property the S-D3 cross-language KAT and
    /// its Rust adopter rely on (the browser reproduces these same bytes; the Rust side adopts
    /// them). Also asserts the derandomized seal agrees with the CSPRNG path's *shape*.
    #[test]
    fn seal_drop_derand_is_reproducible_and_adoptable() {
        let drop_key = DekKeypair::from_seed(&[0x5D; 32]);
        let pk = drop_key.public_bytes();
        let plaintext = b"a deterministic guest drop, sealed under fixed key material";
        let k = [0x11; 32];
        let nonce_prefix = [0x22; NONCE_PREFIX_LEN];
        let eseed = [0x33; ESEED_LEN];
        let blob_nonce = [0x44; 12];

        let a = seal_drop_derand(
            plaintext,
            &pk,
            "image/png",
            &k,
            &nonce_prefix,
            &eseed,
            &blob_nonce,
        )
        .unwrap();
        let b = seal_drop_derand(
            plaintext,
            &pk,
            "image/png",
            &k,
            &nonce_prefix,
            &eseed,
            &blob_nonce,
        )
        .unwrap();

        // Byte-for-byte reproducible: identical descriptor and ciphertext.
        assert_eq!(a.descriptor, b.descriptor);
        assert_eq!(a.ciphertext, b.ciphertext);
        assert_eq!(a.descriptor.nonce_prefix, nonce_prefix);
        assert_eq!(a.descriptor.kem_ct.len(), EXPECTED_KEM_CT_LEN);

        // The Drop Key holder recovers K and STREAM-decrypt returns the exact plaintext — the
        // deterministic seal is adoptable just like the CSPRNG path.
        let recovered = open_drop_key(&drop_key, &a.descriptor.kem_ct).unwrap();
        assert_eq!(recovered, k);
        let back =
            decrypt_asset_vec(&recovered, &a.descriptor.nonce_prefix, &a.ciphertext).unwrap();
        assert_eq!(back, plaintext);
    }

    /// Doc "Opaque-id entropy (unit)": generated upload-link ids are ≥128-bit CSPRNG values
    /// and non-sequential — never UUIDv7 or otherwise structured (identical to the
    /// share-link check).
    #[test]
    fn opaque_id_entropy() {
        assert_eq!(OPAQUE_ID_LEN, 16, "opaque id must be a full 128 bits");
        assert_eq!(generate_opaque_id().len(), 16);

        let ids: Vec<[u8; OPAQUE_ID_LEN]> = (0..256).map(|_| generate_opaque_id()).collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "two CSPRNG opaque ids must never collide");
            }
        }
        // No UUIDv7 version/variant structure, and not time-ordered like a v7 timestamp.
        assert!(
            ids.iter().any(|id| (id[6] >> 4) != 0x7),
            "opaque ids must not all carry the UUIDv7 version nibble"
        );
        assert!(
            ids.iter().any(|id| (id[8] & 0xc0) != 0x80),
            "opaque ids must not all carry the UUID variant bits"
        );
        let high48 = |id: &[u8; OPAQUE_ID_LEN]| {
            id[..6]
                .iter()
                .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
        };
        assert!(
            ids.windows(2).any(|w| high48(&w[0]) >= high48(&w[1])),
            "opaque ids must not be time-ordered/sequential like a UUIDv7"
        );
    }

    /// Doc "No library injection (unit)" — the client-side half: a `DropDescriptor` carries
    /// **no** authorization or album-binding fields (no signature, no `album_id`, no
    /// `amk_version`, no provenance link), so it cannot be presented on an album-write path.
    /// Its CBOR is disjoint from an `AssetManifest`'s.
    #[test]
    fn drop_descriptor_carries_no_authorization_fields() {
        let drop_key = DekKeypair::generate();
        let sealed = seal_drop(b"bytes", &drop_key.public_bytes(), "image/jpeg").unwrap();
        let bytes = cbor::to_canonical_vec(&sealed.descriptor).unwrap();

        // The descriptor never decodes as an AssetManifest: it lacks device_sig/write_sig,
        // album_id, amk_version, and every provenance field a signed write must carry.
        assert!(
            cbor::from_slice::<AssetManifest>(&bytes).is_err(),
            "a DropDescriptor must not be interpretable as a signed AssetManifest"
        );
    }

    /// The passphrase abuse-gate verifier round-trips: the correct passphrase verifies, a
    /// wrong one does not, and the passphrase never appears in the stored verifier bytes.
    #[test]
    fn passphrase_verifier_gates_without_leaking() {
        const PW: &str = "spend my quota if you can";
        let fast = Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        };
        let v = PassphraseVerifier::derive(PW, fast).unwrap();
        assert!(v.verify(PW));
        assert!(!v.verify("wrong"));

        // The passphrase is nowhere in the stored salt or verifier.
        let pw = PW.as_bytes();
        let contains = |h: &[u8]| h.windows(pw.len()).any(|w| w == pw);
        assert!(!contains(&v.salt));
        assert!(!contains(&v.verifier));
    }
}
