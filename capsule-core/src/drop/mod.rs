//! Web-upload guest drops — **contract skeleton** (SSoT: [Web Upload]).
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
//! inbox→album promotion) live in the server's drop module.
//!
//! [Web Upload]: https://docs/design/web-upload/
//! [`KeyMode::Wrapped`]: crate::crypto::provenance::KeyMode

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::crypto::encryption::{
    NONCE_PREFIX_LEN, encrypt_asset_vec_with_prefix, open_blob, seal_blob_with_nonce,
};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{
    DEK_CIPHERTEXT_LEN, DekKeypair, ESEED_LEN, encapsulate_to_public_derand,
};
use crate::crypto::primitives::Argon2Params;
use crate::crypto::provenance::{AssetManifest, ProvenanceRecord};
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

/// What adopting a drop produced: the asset it became, and the index tier a push publishes.
///
/// The drop's ciphertext is not here — the server already holds it as the inbox blob the
/// manifest's `ciphertext_hash` names, and adoption promotes it in place. What the server does
/// not hold until the owner uploads it is the **index tier**: the provenance record (the
/// asset's chain head) and the sealed metadata blob. Both are un-regenerable — the blob's nonce
/// is folded into its key — so they are handed back rather than left for a caller to rebuild.
#[derive(Debug, Clone)]
pub struct AdoptedDrop {
    /// The signed adopting `create` manifest (`key_mode = wrapped`). Its `file_id` is the new
    /// asset's id.
    pub manifest: AssetManifest,
    /// The **exact** sealed metadata-blob wire bytes `manifest.core.metadata_blob_hash` commits
    /// to.
    pub metadata_blob: Vec<u8>,
    /// The asset's one provenance record — the `create` that heads its chain.
    pub record: ProvenanceRecord,
    /// Whether the import still pipeline produced thumbnail derivatives, dimensions and an
    /// LQIP for the adopted bytes, and if not, why — the same report an import returns.
    /// Adoption never fails over unreadable pixels, so this is a report, not a rejection.
    ///
    /// Native-only: the pipeline lives in `lifecycle`, which the WASM sealing build omits,
    /// and only a native client adopts.
    #[cfg(feature = "native")]
    pub derivatives: crate::derivative_format::DerivativeStatus,
    /// How many `(tier, format)` pairs the tier table commits to and this build cannot
    /// encode; zero when the still did not decode, because nothing was attempted.
    #[cfg(feature = "native")]
    pub deferred_formats: u32,
}

impl AdoptedDrop {
    /// The adopted asset's id (the manifest's `file_id`).
    #[must_use]
    pub fn asset_id(&self) -> Uuid {
        self.manifest.core.file_id
    }
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
    /// The verified adoption could not be written into the library (its files or its index
    /// row). The drop stays in the inbox, so the adoption can be retried.
    #[error("adopted drop could not be registered: {0}")]
    Register(String),
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
    /// calls this to gate a drop-session; a wrong passphrase is refused.
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

    /// Adopt a drop into `album_id` in place (no byte re-upload). The drop becomes a
    /// library asset — registered with its one-record provenance chain and sealed metadata
    /// blob, exactly as an import registers one, including the import's still pipeline
    /// (header-derived `content_type`, pixel dimensions, LQIP, signed thumbnail derivatives)
    /// — and the returned [`AdoptedDrop`] carries what publishing it needs: the signed
    /// adopting `create` manifest (whose `ciphertext_hash` references the inbox blob), the
    /// sealed metadata blob, and the provenance record.
    fn adopt(&mut self, drop: DropId, album_id: Uuid) -> Result<AdoptedDrop, DropError>;

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
/// this Rust implementation across the language boundary (the cross-language KAT), and so the
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
    let (enc, ciphertext) = encrypt_asset_vec_with_prefix(k, *nonce_prefix, plaintext);

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
mod tests;
