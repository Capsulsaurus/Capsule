//! Browser (wasm-bindgen) surface for the Capsule guest web client — the **share-link
//! client-side open path** (SSoT: [Share Links]).
//!
//! A guest opening `https://server.tld/s/{opaque-id}#{secret}` never sends the fragment
//! `#{secret}` to the server (browsers never transmit the fragment) and, for a
//! passphrase-protected link, never sends the passphrase either. Everything that turns the
//! fetched **opaque** wrapped material into usable plaintext therefore has to run *in the
//! browser*. This crate is that surface, and nothing more:
//!
//! 1. [`share_is_passphrase_protected`] — peek at the fetched `WrappedScope` so the viewer knows
//!    whether to prompt for a passphrase **before** attempting to open (the server cannot be
//!    asked — it holds only opaque bytes).
//! 2. [`open_share`] — Argon2id-unwrap (iff passphrase-protected, client-side) then
//!    link-secret-decapsulate the scope material with the URL fragment secret. A wrong passphrase
//!    or a wrong/absent fragment fails here, client-side (SSoT: [Share Links] — Passphrase unwrap
//!    is client-side; scenario #42).
//! 3. [`ShareScope::decrypt_blob`] — derive a covered asset's per-file key from the opened scope
//!    and STREAM-decrypt a served ciphertext blob to plaintext, authenticated (a tampered blob is
//!    rejected by the AEAD tag).
//!
//! The viewer starts from [`open_share_metadata_wasm`] (`openShareMetadata`), which opens the
//! sealed record at the link's `metadata_hash`: the covered assets' key-free parameters and, for
//! a link with no passphrase, the scope [`open_share`] takes.
//!
//! The crate also carries the **inbound** guest surface (SSoT: [Web Upload]) — the
//! browser half of a *guest drop*. A guest opening `https://server.tld/u/{opaque-id}#{drop_pubkey}`
//! seals each selected asset entirely client-side and can never read anything back:
//!
//! 4. [`seal_drop_wasm`] (`sealDrop`) — draw a fresh `K`, STREAM-encrypt the asset under it, and
//!    KEM-encapsulate `K` to the link's Drop Key (parsed from the URL fragment, never sent to the
//!    server). Returns a [`WasmSealedDrop`] the uploader turns into a drop session + chunks.
//! 5. [`drop_passphrase_proof`] (`dropPassphraseProof`) — derive the Argon2id **proof** a
//!    passphrase-gated link requires, from the passphrase + the salt/params carried in the
//!    fragment. Byte-identical to the server's stored verifier, so the guest proves possession
//!    without transmitting the passphrase (SSoT: [Web Upload] — Optional passphrase abuse gate).
//!
//! The crate additionally carries the **LQIP decode** entry point, which
//! is the one surface here that is not about crypto:
//!
//! 6. [`decode_lqip`] (`decodeLqip`) — render a sidecar `lqip` record to packed RGBA the viewer
//!    can hand straight to `CanvasRenderingContext2D.putImageData`. Infallible: a placeholder is
//!    cosmetic, so a malformed record paints a fallback fill rather than failing a gallery. The placeholder lives inside
//!    the *encrypted* metadata blob, so the browser only ever holds it after opening a share
//!    link — which is why this belongs in the same crate as the open path rather than beside a
//!    server route. It is the same [`capsule_core::lqip`] implementation the import pipeline
//!    encodes with and the native apps decode with, so a photo's placeholder does not depend on
//!    which client is painting it.
//!
//! The drop surface is deliberately **contribute-only**: there is no open/decapsulate/decrypt
//! entry point for drops (only the provisioning user's *native* client, holding the Drop Key
//! private half, can adopt). Keep new browser entry points here behind the same thin-glue
//! discipline — the crypto itself always lives in `capsule-core`, so the two platforms stay
//! byte-identical and the cross-language KATs (`capsule-web`'s `share-open` / `drop-seal` bun
//! tests, fed by `cargo xtask share-kat` / `drop-kat`) can prove it.
//!
//! [Web Upload]: https://docs/design/web-upload/
//!
//! Errors cross the boundary as a **stable machine code string** (`Error.message`), never a
//! localized sentence — the web viewer maps the code to an i18n catalog key. The codes are
//! defined in this crate's private `err` module.
//!
//! [Share Links]: https://docs/design/share-links/

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::encryption::{NONCE_PREFIX_LEN, WRAPPED_FILE_KEY_LEN, decrypt_asset_vec};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::provenance::{KeyMode, WrappedFileKey};
use capsule_core::crypto::pwkdf;
use capsule_core::drop::{SealedDrop, seal_drop, seal_drop_derand};
use capsule_core::lqip::{RgbaImage, render as lqip_render};
use capsule_core::sharing::{
    LINK_SECRET_LEN, OPAQUE_ID_LEN, ScopeMaterial, ShareMetadata, SharedAsset, SharingError,
    WrappedScope, open_scope, open_share_metadata,
};
use uuid::Uuid;
use wasm_bindgen::prelude::*;

/// Stable machine error codes returned across the JS boundary (as `Error.message`). The web
/// viewer maps each to an i18n catalog key; they are **not** user-facing prose themselves.
mod err {
    /// The `WrappedScope`/opaque-id/fragment/ciphertext could not be decoded (bad hex/base64/CBOR
    /// or a wrong-length field) — a structurally malformed request.
    pub(crate) const MALFORMED: &str = "malformed";
    /// The link is passphrase-protected but no passphrase was supplied.
    pub(crate) const PASSPHRASE_REQUIRED: &str = "passphrase_required";
    /// The supplied passphrase was wrong, or the wrapped/fragment material does not open the scope
    /// (wrong fragment secret / opaque id) — indistinguishable by design.
    pub(crate) const WRONG_SECRET: &str = "wrong_secret";
    /// The opened scope does not cover the requested asset/epoch.
    pub(crate) const SCOPE_UNAVAILABLE: &str = "scope_unavailable";
    /// The served ciphertext failed authentication (tamper, truncation, wrong key/prefix).
    pub(crate) const TAMPERED: &str = "tampered";
    /// Sealing a guest drop failed — the Drop Key (URL fragment) is malformed, so encapsulation
    /// could not run. Distinct from [`MALFORMED`] only in provenance; the viewer maps both to the
    /// same "this link is broken" surface.
    pub(crate) const SEAL_FAILED: &str = "seal_failed";
}

/// Map a [`SharingError`] to its stable boundary code.
fn sharing_code(e: &SharingError) -> &'static str {
    match e {
        SharingError::PassphraseRequired => err::PASSPHRASE_REQUIRED,
        SharingError::WrongPassphrase => err::WRONG_SECRET,
        SharingError::ScopeUnavailable => err::SCOPE_UNAVAILABLE,
        SharingError::NotFound | SharingError::Crypto(_) | SharingError::Malformed(_) => {
            err::MALFORMED
        }
    }
}

/// Map a per-asset key-recovery failure to its boundary code. The length and hex of a wrapped
/// key are checked before recovery runs, so a [`SharingError::Crypto`] here is a wrapped key
/// that did not open: an authentication failure, reported as `tampered` like the ciphertext's.
fn recovery_code(e: &SharingError) -> &'static str {
    match e {
        SharingError::Crypto(_) => err::TAMPERED,
        other => sharing_code(other),
    }
}

/// A wrong fragment secret and a wrong passphrase both surface as [`SharingError::Crypto`] /
/// [`SharingError::WrongPassphrase`]; collapse the "cannot open" family to one indistinguishable
/// `wrong_secret` code so the viewer cannot become an oracle for *which* half was wrong.
fn open_code(e: &SharingError) -> &'static str {
    match e {
        SharingError::PassphraseRequired => err::PASSPHRASE_REQUIRED,
        SharingError::WrongPassphrase | SharingError::Crypto(_) => err::WRONG_SECRET,
        SharingError::ScopeUnavailable | SharingError::NotFound | SharingError::Malformed(_) => {
            err::MALFORMED
        }
    }
}

/// Decode base64 (canonical CBOR) into a [`WrappedScope`] — the material fetched from
/// `/s/{opaque-id}/wrapped-secret` (or embedded in the metadata response).
fn decode_wrapped(wrapped_scope_b64: &str) -> Result<WrappedScope, JsError> {
    let cbor = BASE64
        .decode(wrapped_scope_b64.trim())
        .map_err(|_| JsError::new(err::MALFORMED))?;
    capsule_core::cbor::from_slice(&cbor).map_err(|_| JsError::new(err::MALFORMED))
}

/// Decode a lowercase-hex field into a fixed-length array, erroring [`err::MALFORMED`] on a bad
/// digit or wrong length.
fn hex_array<const N: usize>(hex_str: &str) -> Result<[u8; N], JsError> {
    let bytes = hex::decode(hex_str.trim()).map_err(|_| JsError::new(err::MALFORMED))?;
    bytes.try_into().map_err(|_| JsError::new(err::MALFORMED))
}

/// Whether the fetched wrapped material is protected by an Argon2id passphrase layer.
///
/// The viewer calls this on the material from `/s/{opaque-id}/wrapped-secret` (or the
/// `passphrase_protected` flag on the metadata response) to decide whether to prompt for a
/// passphrase before calling [`open_share`]. The server holds only the opaque bytes and so cannot
/// answer this — it is a pure client-side inspection.
#[wasm_bindgen(js_name = shareIsPassphraseProtected)]
pub fn share_is_passphrase_protected(wrapped_scope_b64: &str) -> Result<bool, JsError> {
    Ok(decode_wrapped(wrapped_scope_b64)?.is_passphrase_protected())
}

/// The opened scope of a share link — the decryption material recovered **client-side** from the
/// URL fragment secret (and passphrase, if any). Hold it to [`decrypt_blob`](ShareScope::decrypt_blob)
/// the covered ciphertext blobs the serve path returns.
#[wasm_bindgen]
pub struct ShareScope {
    material: ScopeMaterial,
}

/// Open a share link's encapsulated scope material entirely in the browser.
///
/// - `wrapped_scope_b64` — the opaque `WrappedScope` (base64 canonical CBOR) fetched from
///   `/s/{opaque-id}/wrapped-secret`.
/// - `opaque_id_hex` — the `{opaque-id}` URL path component (lowercase hex; the HKDF salt).
/// - `fragment_hex` — the `#{secret}` URL fragment (lowercase hex; never sent to the server).
/// - `passphrase` — supplied **only** when the link is passphrase-protected; unwrapped here via
///   Argon2id and never transmitted.
///
/// Throws (as `Error.message`): `passphrase_required`, `wrong_secret` (wrong passphrase *or*
/// wrong fragment/opaque — indistinguishable), or `malformed`.
#[wasm_bindgen(js_name = openShare)]
pub fn open_share(
    wrapped_scope_b64: &str,
    opaque_id_hex: &str,
    fragment_hex: &str,
    passphrase: Option<String>,
) -> Result<ShareScope, JsError> {
    let wrapped = decode_wrapped(wrapped_scope_b64)?;
    let opaque_id: [u8; OPAQUE_ID_LEN] = hex_array(opaque_id_hex)?;
    let fragment: [u8; LINK_SECRET_LEN] = hex_array(fragment_hex)?;
    let material = open_scope(&wrapped, &opaque_id, &fragment, passphrase.as_deref())
        .map_err(|e| JsError::new(open_code(&e)))?;
    Ok(ShareScope { material })
}

#[wasm_bindgen]
impl ShareScope {
    /// `"asset"` or `"album"` — what the opened scope grants, for the viewer's read-only header.
    #[wasm_bindgen(js_name = scopeKind)]
    #[must_use]
    pub fn scope_kind(&self) -> String {
        match self.material {
            ScopeMaterial::Asset { .. } => "asset".to_string(),
            ScopeMaterial::Album { .. } => "album".to_string(),
        }
    }

    /// Derive the covered asset's per-file key and STREAM-decrypt `ciphertext` to plaintext.
    ///
    /// - `file_id` — the asset/file UUID (the served `asset_id`).
    /// - `amk_version` / `nonce_prefix_hex` — the asset's crypto-manifest parameters. For an
    ///   asset-scoped grant the file key is carried directly and both are unused; for an
    ///   album-scoped grant the key is derived from the epoch AMK with the nonce prefix folded in.
    /// - `ciphertext` — the octets from `/s/{opaque-id}/blob/{hash}`.
    /// - `wrapped_file_key_hex` — lowercase hex of the asset's AMK-sealed `wrapped_file_key`,
    ///   passed **only** when its `key_mode` is `wrapped` (an adopted web-upload drop, whose key
    ///   a guest chose and no AMK derives). Omitted (or `null`) means `key_mode = derived`. A
    ///   trailing optional argument, so an existing three-plus-ciphertext call is unchanged.
    ///
    /// Throws: `scope_unavailable` (the scope does not cover this asset/epoch), `tampered` (the
    /// AEAD tag failed — tamper/truncation/wrong key — or the wrapped key did not open), or
    /// `malformed` (bad `file_id`/prefix/wrapped-key hex or length).
    #[wasm_bindgen(js_name = decryptBlob)]
    pub fn decrypt_blob(
        &self,
        file_id: &str,
        amk_version: u32,
        nonce_prefix_hex: &str,
        ciphertext: &[u8],
        wrapped_file_key_hex: Option<String>,
    ) -> Result<Vec<u8>, JsError> {
        let file_id = Uuid::parse_str(file_id.trim()).map_err(|_| JsError::new(err::MALFORMED))?;
        let nonce_prefix: [u8; NONCE_PREFIX_LEN] = hex_array(nonce_prefix_hex)?;
        let wrapped = wrapped_file_key_hex
            .as_deref()
            .map(hex_array::<WRAPPED_FILE_KEY_LEN>)
            .transpose()?
            .map(|bytes| WrappedFileKey(bytes.to_vec()));
        let key_mode = if wrapped.is_some() {
            KeyMode::Wrapped
        } else {
            KeyMode::Derived
        };
        let file_key = self
            .material
            .file_key_for_key_mode(
                &file_id,
                amk_version,
                &nonce_prefix,
                key_mode,
                wrapped.as_ref(),
            )
            .map_err(|e| JsError::new(recovery_code(&e)))?;
        decrypt_asset_vec(&file_key, &nonce_prefix, ciphertext)
            .map_err(|_| JsError::new(err::TAMPERED))
    }
}

// ─────────────────────────── The share metadata record (#614) ───────────────────────────────

/// The opened share metadata record — what the viewer starts from, recovered client-side from the
/// blob at the link's `metadata_hash` with the URL's opaque id and fragment.
#[wasm_bindgen]
pub struct ShareMetadataView {
    record: ShareMetadata,
}

/// One covered asset of an opened [`ShareMetadataView`], in the shapes
/// [`ShareScope::decrypt_blob`] and `/s/{opaque-id}/blob/{hash}` take.
#[wasm_bindgen]
pub struct SharedAssetView {
    asset: SharedAsset,
}

/// Open the sealed share metadata record entirely in the browser.
///
/// - `blob` — the octets of `/s/{opaque-id}/blob/{metadata_hash}`.
/// - `opaque_id_hex` / `fragment_hex` — the URL's path id and `#{secret}`, as for [`open_share`].
///
/// Throws (as `Error.message`): `wrong_secret` (the blob does not open under this URL — a wrong
/// fragment, a wrong opaque id, or a tampered blob, indistinguishable) or `malformed` (bad hex,
/// or a record this reader does not understand).
#[wasm_bindgen(js_name = openShareMetadata)]
pub fn open_share_metadata_wasm(
    blob: &[u8],
    opaque_id_hex: &str,
    fragment_hex: &str,
) -> Result<ShareMetadataView, JsError> {
    let opaque_id: [u8; OPAQUE_ID_LEN] = hex_array(opaque_id_hex)?;
    let fragment: [u8; LINK_SECRET_LEN] = hex_array(fragment_hex)?;
    let record = open_share_metadata(blob, &opaque_id, &fragment)
        .map_err(|e| JsError::new(open_code(&e)))?;
    Ok(ShareMetadataView { record })
}

#[wasm_bindgen]
impl ShareMetadataView {
    /// The issuer's RFC 3339 expiry, for display; `undefined` when the link does not expire.
    #[wasm_bindgen(js_name = expiresAt)]
    #[must_use]
    pub fn expires_at(&self) -> Option<String> {
        self.record.expires_at.clone()
    }

    /// The link-only scope, base64 canonical CBOR — exactly what [`open_share`] takes. Present iff
    /// the link has no passphrase; a passphrase link's scope is fetched from
    /// `/s/{opaque-id}/wrapped-secret` instead.
    #[wasm_bindgen(js_name = linkScopeB64)]
    pub fn link_scope_b64(&self) -> Result<Option<String>, JsError> {
        self.record
            .link_scope
            .as_ref()
            .map(|scope| {
                capsule_core::cbor::to_canonical_vec(scope)
                    .map(|cbor| BASE64.encode(cbor))
                    .map_err(|_| JsError::new(err::MALFORMED))
            })
            .transpose()
    }

    /// Every covered asset, in the issuer's order.
    #[must_use]
    pub fn assets(&self) -> Vec<SharedAssetView> {
        self.record
            .assets
            .iter()
            .cloned()
            .map(|asset| SharedAssetView { asset })
            .collect()
    }
}

#[wasm_bindgen]
impl SharedAssetView {
    /// The asset's file UUID — `decryptBlob`'s `file_id`.
    #[wasm_bindgen(getter, js_name = assetId)]
    #[must_use]
    pub fn asset_id(&self) -> String {
        self.asset.asset_id.to_string()
    }

    /// The ciphertext's content address, lowercase hex — the `{hash}` to fetch.
    #[wasm_bindgen(getter, js_name = contentHash)]
    #[must_use]
    pub fn content_hash(&self) -> String {
        self.asset.content_hash.to_hex()
    }

    /// The plaintext media type.
    #[wasm_bindgen(getter, js_name = contentType)]
    #[must_use]
    pub fn content_type(&self) -> String {
        self.asset.content_type.clone()
    }

    /// The plaintext byte length, as a JS number.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn size(&self) -> f64 {
        self.asset.size as f64
    }

    /// The AMK epoch — `decryptBlob`'s `amk_version`.
    #[wasm_bindgen(getter, js_name = amkVersion)]
    #[must_use]
    pub fn amk_version(&self) -> u32 {
        self.asset.amk_version
    }

    /// The STREAM nonce prefix, lowercase hex — `decryptBlob`'s `nonce_prefix_hex`.
    #[wasm_bindgen(getter, js_name = noncePrefixHex)]
    #[must_use]
    pub fn nonce_prefix_hex(&self) -> String {
        hex::encode(self.asset.nonce_prefix)
    }

    /// `"derived"` or `"wrapped"`.
    #[wasm_bindgen(getter, js_name = keyMode)]
    #[must_use]
    pub fn key_mode(&self) -> String {
        match self.asset.key_mode {
            KeyMode::Derived => "derived".to_string(),
            KeyMode::Wrapped => "wrapped".to_string(),
        }
    }

    /// The AMK-sealed file key, lowercase hex; present iff the key mode is `wrapped`.
    #[wasm_bindgen(getter, js_name = wrappedFileKeyHex)]
    #[must_use]
    pub fn wrapped_file_key_hex(&self) -> Option<String> {
        self.asset
            .wrapped_file_key
            .as_ref()
            .map(|key| hex::encode(&key.0))
    }
}

// ─────────────────────────── Guest drop sealing ────────────────────────────────

/// A sealed guest drop, ready for the uploader to open a drop session and stream its chunks.
///
/// Holds the STREAM ciphertext plus the unsigned descriptor's fields, projected across the JS
/// boundary in exactly the wire shapes the drop endpoints expect (hex nonce prefix / ciphertext
/// hash, base64 `kem_ct`) so the TypeScript uploader builds the request body without re-encoding
/// crypto. Contribute-only: it exposes no way to recover `K` or decrypt — only a native adopter,
/// holding the Drop Key private half, can do that.
#[wasm_bindgen]
pub struct WasmSealedDrop {
    sealed: SealedDrop,
}

#[wasm_bindgen]
impl WasmSealedDrop {
    /// The declared content type (the closed-enum value passed into [`seal_drop_wasm`]).
    #[wasm_bindgen(js_name = contentType)]
    #[must_use]
    pub fn content_type(&self) -> String {
        self.sealed.descriptor.content_type.clone()
    }

    /// Total plaintext byte length (`plaintext_size`), as a JS number — the drop descriptor wire
    /// field. A guest asset never approaches the 2^53 exact-integer ceiling.
    #[wasm_bindgen(js_name = plaintextSize)]
    #[must_use]
    pub fn plaintext_size(&self) -> f64 {
        self.sealed.descriptor.plaintext_size as f64
    }

    /// The STREAM plaintext chunk size.
    #[wasm_bindgen(js_name = chunkSize)]
    #[must_use]
    pub fn chunk_size(&self) -> u32 {
        self.sealed.descriptor.chunk_size
    }

    /// The 7-byte STREAM nonce prefix, lowercase hex (14 chars) — the descriptor wire field.
    #[wasm_bindgen(js_name = noncePrefixHex)]
    #[must_use]
    pub fn nonce_prefix_hex(&self) -> String {
        hex::encode(self.sealed.descriptor.nonce_prefix)
    }

    /// The ciphertext content-address digest, lowercase hex (64 chars) — the descriptor wire
    /// field **and** the per-chunk / finalization checksum the uploader sends.
    #[wasm_bindgen(js_name = ciphertextHashHex)]
    #[must_use]
    pub fn ciphertext_hash_hex(&self) -> String {
        self.sealed.descriptor.ciphertext_hash.to_hex()
    }

    /// `K` encapsulated to the Drop Key (KEM-DEM), base64 — the descriptor wire field. Opaque to
    /// the server; only the Drop Key private half opens it.
    #[wasm_bindgen(js_name = kemCtB64)]
    #[must_use]
    pub fn kem_ct_b64(&self) -> String {
        BASE64.encode(&self.sealed.descriptor.kem_ct)
    }

    /// Total ciphertext byte length — the drop session's declared `size` and the cumulative
    /// upload target, as a JS number.
    #[wasm_bindgen(js_name = ciphertextLen)]
    #[must_use]
    pub fn ciphertext_len(&self) -> f64 {
        self.sealed.ciphertext.len() as f64
    }

    /// The STREAM ciphertext octets the uploader streams to the drop endpoint (a copy).
    #[wasm_bindgen(js_name = ciphertext)]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.sealed.ciphertext.clone()
    }
}

/// Seal one selected asset for a guest drop, entirely in the browser.
///
/// - `plaintext` — the asset bytes read from the file picker.
/// - `drop_pubkey` — the link's Drop Key public half, decoded from the URL `#{drop_pubkey}`
///   fragment (which never reaches the server).
/// - `content_type` — the asset's media type (validated server-side against the link's closed
///   enum; here it is carried verbatim into the descriptor).
///
/// Draws a fresh `K`, STREAM-encrypts under it, and KEM-encapsulates `K` to the Drop Key. Throws
/// `seal_failed` (as `Error.message`) if the Drop Key is malformed.
#[wasm_bindgen(js_name = sealDrop)]
pub fn seal_drop_wasm(
    plaintext: &[u8],
    drop_pubkey: &[u8],
    content_type: &str,
) -> Result<WasmSealedDrop, JsError> {
    let sealed = seal_drop(plaintext, drop_pubkey, content_type)
        .map_err(|_| JsError::new(err::SEAL_FAILED))?;
    Ok(WasmSealedDrop { sealed })
}

/// Derandomized [`seal_drop_wasm`] — **exposed for the cross-language known-answer test only**.
///
/// Every value the production seal draws from the CSPRNG is supplied explicitly (`k`,
/// `nonce_prefix`, `eseed`, `blob_nonce`, all lowercase hex) so the browser seal is byte-for-byte
/// reproducible and can be proven identical to `capsule_core::drop::seal_drop_derand` — the Rust
/// adopter then consumes those exact bytes (the cross-language KAT). Web app code uses
/// [`seal_drop_wasm`], never this. Throws `malformed` on a bad-length hex field, `seal_failed` on a malformed Drop Key.
#[wasm_bindgen(js_name = sealDropDerand)]
pub fn seal_drop_derand_wasm(
    plaintext: &[u8],
    drop_pubkey: &[u8],
    content_type: &str,
    k_hex: &str,
    nonce_prefix_hex: &str,
    eseed_hex: &str,
    blob_nonce_hex: &str,
) -> Result<WasmSealedDrop, JsError> {
    let k: [u8; 32] = hex_array(k_hex)?;
    let nonce_prefix: [u8; NONCE_PREFIX_LEN] = hex_array(nonce_prefix_hex)?;
    let eseed: [u8; 64] = hex_array(eseed_hex)?;
    let blob_nonce: [u8; 12] = hex_array(blob_nonce_hex)?;
    let sealed = seal_drop_derand(
        plaintext,
        drop_pubkey,
        content_type,
        &k,
        &nonce_prefix,
        &eseed,
        &blob_nonce,
    )
    .map_err(|_| JsError::new(err::SEAL_FAILED))?;
    Ok(WasmSealedDrop { sealed })
}

/// Derive the Argon2id **proof** a passphrase-gated upload link requires at drop-session creation.
///
/// The link's salt + Argon2id parameters travel in the URL fragment beside the Drop Key; the
/// passphrase is entered by the guest and never leaves the browser. The returned lowercase-hex
/// proof equals the server's stored verifier byte-for-byte (both are
/// `Argon2id(passphrase, salt, params)`), so submitting it proves possession without transmitting
/// the passphrase (SSoT: [Web Upload] — the optional-passphrase abuse gate). Throws `malformed`
/// on a bad salt hex, `seal_failed` if the KDF parameters are invalid.
///
/// [Web Upload]: https://docs/design/web-upload/
#[wasm_bindgen(js_name = dropPassphraseProof)]
pub fn drop_passphrase_proof(
    passphrase: &str,
    salt_hex: &str,
    mem_kib: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<String, JsError> {
    let salt = hex::decode(salt_hex.trim()).map_err(|_| JsError::new(err::MALFORMED))?;
    let params = Argon2Params {
        mem_kib,
        t_cost,
        p_cost,
    };
    let proof = pwkdf::derive_wrap_key(passphrase.as_bytes(), &salt, params)
        .map_err(|_| JsError::new(err::SEAL_FAILED))?;
    Ok(hex::encode(proof))
}

// ── Tests ──────────────────────────────────────────────────────────────────────
//
// Host-runnable only, and deliberately confined to the paths that return `Ok`. Every
// `Err` arm here builds a `JsError`, which goes through wasm-bindgen's
// `__wbindgen_error_new` extern; off `wasm32` that symbol is a panicking placeholder, so a
// test that provoked an error would abort rather than assert. The error *codes* are still
// covered, because `sharing_code`/`open_code` return `&'static str` and never touch
// `JsValue`. The wasm-side behaviour of the `#[wasm_bindgen]` entry points is covered by
// `capsule-web`'s bun KATs.

// ─────────────────────────── LQIP placeholder decode ──────────────────────────

/// A decoded LQIP placeholder: packed RGBA8, ready for `putImageData`.
///
/// A struct rather than a bare `Vec<u8>` because the caller cannot know the dimensions in
/// advance: `decode_capped` returns the largest frame that fits *inside* the requested box while
/// preserving the source aspect ratio, so the answer is `(width, height, rgba)` or nothing.
#[wasm_bindgen]
pub struct WasmLqipImage {
    image: RgbaImage,
}

#[wasm_bindgen]
impl WasmLqipImage {
    /// Frame width in pixels — at most the `maxWidth` that was requested.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn width(&self) -> u32 {
        self.image.width
    }

    /// Frame height in pixels — at most the `maxHeight` that was requested.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn height(&self) -> u32 {
        self.image.height
    }

    /// Packed RGBA8 samples, `width * height * 4` bytes — the exact layout
    /// `new ImageData(rgba, width, height)` takes.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn rgba(&self) -> Vec<u8> {
        self.image.rgba.clone()
    }
}

/// Render a sidecar `lqip` record to a paintable placeholder, band-limited to the box being
/// painted.
///
/// - `format_version` — the record's `lqip.format_version`.
/// - `chromahash` — the record's `lqip.chromahash` payload (32 bytes at the committed tier).
/// - `dominant_color` — the record's `lqip.dominant_color`, exactly 3 bytes (opaque RGB).
/// - `max_width` / `max_height` — the box the caller is about to paint. Decoding to the box
///   rather than to a fixed size is the point of `decode_capped`: a grid cell never scales down
///   a larger decode.
///
/// **Infallible.** An unrecognised `format_version`, a payload the parser rejects, *and* a
/// `dominant_color` that is not three bytes all yield a solid fallback fill rather than a throw.
///
/// A placeholder is cosmetic: it never damages a library and its absence never loses data, so
/// failing a viewer over one trades a blurry square for a broken gallery. The earlier version
/// threw on a malformed `dominant_color` while the native FFI painted black for the same input —
/// one record, two behaviours, decided by which client opened it. That is exactly the
/// client-dependent divergence `capsule-core::lqip` exists to prevent, so both surfaces now
/// paint the same fallback fill (black, the conventional empty cell) and say so.
#[wasm_bindgen(js_name = decodeLqip)]
pub fn decode_lqip(
    format_version: u16,
    chromahash: &[u8],
    dominant_color: &[u8],
    max_width: u32,
    max_height: u32,
) -> WasmLqipImage {
    WasmLqipImage {
        image: render_lqip_record(
            format_version,
            chromahash,
            dominant_color,
            max_width,
            max_height,
        ),
    }
}

/// The colour a malformed record paints when it carries no usable `dominant_color`.
///
/// Black, the conventional empty-cell fill, and identical to what `capsule-core-ffi`'s
/// `render_lqip` paints for the same input — the two surfaces are asserted to agree.
const FALLBACK_FILL: [u8; 3] = [0, 0, 0];

/// [`decode_lqip`] without the JS boundary — the whole of its logic, so the host unit tests can
/// exercise it.
///
/// The split is not ceremony: `JsError` cannot be *constructed* off-wasm (its host shim aborts),
/// so a test reaching an error arm through the exported function would abort the test binary
/// rather than fail an assertion. It survives the move to an infallible signature because the
/// `#[wasm_bindgen]` wrapper is still unusable from a host test.
fn render_lqip_record(
    format_version: u16,
    chromahash: &[u8],
    dominant_color: &[u8],
    max_width: u32,
    max_height: u32,
) -> RgbaImage {
    let fill: [u8; 3] = dominant_color.try_into().unwrap_or(FALLBACK_FILL);
    lqip_render(format_version, chromahash, fill, max_width, max_height)
}

#[cfg(test)]
mod tests;
