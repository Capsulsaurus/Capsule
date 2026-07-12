//! Cross-language share-link Known-Answer-Test (KAT) fixture generator (slice `S-E1`).
//!
//! Emits a **deterministic** JSON fixture that the browser share-open path
//! (`capsule-web`'s `share-open` bun test, loading the `capsule-wasm` module) reopens, so the
//! Rust issuer crypto (`capsule_core::sharing`) and the TypeScript/wasm recipient crypto are
//! proven byte-identical: byte-exact plaintext recovery, wrong-passphrase refusal, wrong-fragment
//! refusal, and tampered-ciphertext refusal.
//!
//! Everything is drawn from **fixed** bytes (never the CSPRNG) so the fixture is reproducible and
//! the bun test is a true known-answer test — re-running the generator yields identical output, so
//! `build-wasm`/`test-web` are deterministic and offline.
//!
//! The fixture is an **asset-scoped** grant: [`ScopeMaterial::Asset`] carries the per-file key
//! directly, so a single served ciphertext blob round-trips end to end without an album AMK ledger
//! (the album path shares the same `open_scope` + `file_key_for` code, unit-tested in
//! `capsule-core`). The served ciphertext is produced with the real
//! [`encrypt_asset_vec_with_prefix`] STREAM construction the serve path returns verbatim.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::encryption::stream::{NONCE_PREFIX_LEN, encrypt_asset_vec_with_prefix};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::sharing::{
    LINK_SECRET_LEN, OPAQUE_ID_LEN, ScopeMaterial, WrappedScope, encapsulate_scope,
};
use eyre::{Context, Result};
use serde_json::json;
use uuid::Uuid;

/// The passphrase the passphrase-wrapped variant is sealed under.
const PASSPHRASE: &str = "correct horse battery staple";

/// Base64 canonical CBOR of a `WrappedScope` — the exact bytes the serve path returns from
/// `/s/{opaque-id}/wrapped-secret`, opaque to the server.
fn wrapped_b64(scope: &WrappedScope) -> Result<String> {
    Ok(BASE64.encode(
        capsule_core::cbor::to_canonical_vec(scope).context("serialize WrappedScope to CBOR")?,
    ))
}

/// Generate the KAT fixture and write it to `out` (relative to the repo root).
pub(crate) fn run(root: &Path, out_rel: &str) -> Result<()> {
    // Fixed, non-CSPRNG inputs → a reproducible known-answer fixture.
    let fragment = [0xA1u8; LINK_SECRET_LEN];
    let wrong_fragment = [0xB2u8; LINK_SECRET_LEN];
    let opaque_id = [0x5Eu8; OPAQUE_ID_LEN];
    let file_id = Uuid::from_u128(0x00E1_1111_2222_3333_4444_5555_6666_7777);
    let file_key = [0x42u8; 32];
    let nonce_prefix = [0x07u8; NONCE_PREFIX_LEN];

    // Small (fast) Argon2id params keep the fixture generation and the bun unwrap quick; the cost
    // is self-describing in the wrapped material, so the recipient uses these same params.
    let argon = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };

    // An asset-scoped grant: the per-file key travels in the scope material directly.
    let material = ScopeMaterial::Asset { file_id, file_key };
    let link_only = encapsulate_scope(&material, &fragment, &opaque_id, None, argon)
        .context("seal link-only scope")?;
    let passphrase_wrapped =
        encapsulate_scope(&material, &fragment, &opaque_id, Some(PASSPHRASE), argon)
            .context("seal passphrase-wrapped scope")?;

    // The served ciphertext blob: a real STREAM encryption under the file key (spanning enough
    // bytes to exercise a partial chunk), byte-for-byte what `/s/{opaque-id}/blob/{hash}` returns.
    // The plaintext leads with the 8-byte PNG signature so the browser end-to-end render — decrypt
    // → `Blob([bytes], { type: contentType })` — is a genuine `image/png` payload, then a
    // deterministic filler that exercises multiple STREAM chunks.
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut plaintext: Vec<u8> = PNG_SIGNATURE.to_vec();
    plaintext.extend((0..5000u32).map(|i| (i % 251) as u8));
    let (_, ciphertext) = encrypt_asset_vec_with_prefix(&file_key, nonce_prefix, &plaintext);

    let fixture = json!({
        "_generated_by": "cargo xtask share-kat — do not edit by hand",
        "opaqueIdHex": hex::encode(opaque_id),
        "fragmentHex": hex::encode(fragment),
        "wrongFragmentHex": hex::encode(wrong_fragment),
        "fileId": file_id.to_string(),
        "amkVersion": 0,
        "noncePrefixHex": hex::encode(nonce_prefix),
        // The plaintext content type the viewer wraps the decrypted bytes in (a served, key-free
        // fact); PNG magic bytes lead the plaintext so the end-to-end render is a real image.
        "contentType": "image/png",
        "passphrase": PASSPHRASE,
        "wrongPassphrase": "not the passphrase",
        "linkOnlyWrappedB64": wrapped_b64(&link_only)?,
        "passphraseWrappedB64": wrapped_b64(&passphrase_wrapped)?,
        "plaintextB64": BASE64.encode(&plaintext),
        "ciphertextB64": BASE64.encode(&ciphertext),
    });

    let path = root.join(out_rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create fixture dir {}", parent.display()))?;
    }
    let mut body = serde_json::to_string_pretty(&fixture).context("serialize fixture JSON")?;
    body.push('\n');
    std::fs::write(&path, body).with_context(|| format!("write fixture {}", path.display()))?;
    eprintln!("wrote share-link KAT fixture to {}", path.display());
    Ok(())
}
