//! Browser (wasm-bindgen) surface for the Capsule guest web client — the **share-link
//! client-side open path** (slice `S-E1` in the repo-root `SLICES.md`; SSoT: [Share Links]).
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
//! The surface is deliberately **open-only**: the drop-sealing browser surface is slice S-D3's,
//! and a follow-up slice extends this crate with it. Keep new browser entry points here behind
//! the same thin-glue discipline — the crypto itself always lives in `capsule-core`, so the two
//! platforms stay byte-identical and the cross-language KAT (`capsule-web`'s `share-open` bun
//! test, fed by `cargo xtask share-kat`) can prove it.
//!
//! Errors cross the boundary as a **stable machine code string** (`Error.message`), never a
//! localized sentence — the web viewer maps the code to an i18n catalog key. Codes: see
//! [`err`].
//!
//! [Share Links]: https://docs/design/share-links/

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::encryption::stream::{NONCE_PREFIX_LEN, decrypt_asset_vec};
use capsule_core::sharing::{
    LINK_SECRET_LEN, OPAQUE_ID_LEN, ScopeMaterial, SharingError, WrappedScope, open_scope,
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
}

/// Map a [`SharingError`] to its stable boundary code.
fn sharing_code(e: &SharingError) -> &'static str {
    match e {
        SharingError::PassphraseRequired => err::PASSPHRASE_REQUIRED,
        SharingError::WrongPassphrase => err::WRONG_SECRET,
        SharingError::ScopeUnavailable => err::SCOPE_UNAVAILABLE,
        SharingError::NotFound | SharingError::Crypto(_) => err::MALFORMED,
    }
}

/// A wrong fragment secret and a wrong passphrase both surface as [`SharingError::Crypto`] /
/// [`SharingError::WrongPassphrase`]; collapse the "cannot open" family to one indistinguishable
/// `wrong_secret` code so the viewer cannot become an oracle for *which* half was wrong.
fn open_code(e: &SharingError) -> &'static str {
    match e {
        SharingError::PassphraseRequired => err::PASSPHRASE_REQUIRED,
        SharingError::WrongPassphrase | SharingError::Crypto(_) => err::WRONG_SECRET,
        SharingError::ScopeUnavailable | SharingError::NotFound => err::MALFORMED,
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
    ///
    /// Throws: `scope_unavailable` (the scope does not cover this asset/epoch), `tampered` (the
    /// AEAD tag failed — tamper/truncation/wrong key), or `malformed` (bad `file_id`/prefix).
    #[wasm_bindgen(js_name = decryptBlob)]
    pub fn decrypt_blob(
        &self,
        file_id: &str,
        amk_version: u32,
        nonce_prefix_hex: &str,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let file_id = Uuid::parse_str(file_id.trim()).map_err(|_| JsError::new(err::MALFORMED))?;
        let nonce_prefix: [u8; NONCE_PREFIX_LEN] = hex_array(nonce_prefix_hex)?;
        let file_key = self
            .material
            .file_key_for(&file_id, amk_version, &nonce_prefix)
            .map_err(|e| JsError::new(sharing_code(&e)))?;
        decrypt_asset_vec(&file_key, &nonce_prefix, ciphertext)
            .map_err(|_| JsError::new(err::TAMPERED))
    }
}
