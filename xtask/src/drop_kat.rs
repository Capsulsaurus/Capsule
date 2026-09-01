//! Cross-language **guest-drop** Known-Answer-Test (KAT) fixture generator (slice `S-D3`).
//!
//! The reverse of the share-link KAT: here the **browser seals** and **Rust adopts**. This emits a
//! deterministic JSON fixture the `capsule-web` `drop-seal` bun test loads to prove the browser
//! (WASM) seal is byte-identical to `capsule_core::drop::seal_drop_derand`:
//!
//! - `sealDropDerand(plaintext, dropPubkey, contentType, k, noncePrefix, eseed, blobNonce)` in the
//!   browser must reproduce the exact `descriptor` + `ciphertext` bytes computed here in Rust, and
//! - `dropPassphraseProof(passphrase, salt, params)` must reproduce the Argon2id verifier the
//!   server stores — the abuse-gate proof wire shape.
//!
//! The Rust **adoption** half — decapsulate the very bytes the browser reproduces, rewrap under an
//! album AMK, sign a `create` manifest, and assert `verify_asset` accepts — lives in
//! `capsule-core/tests/drop_adopt_kat.rs`, keyed on the **same fixed inputs** as this generator so
//! the drop the browser reproduces is exactly the drop Rust adopts (E2E case 13's browser half at
//! the level the repo runs locally; a live-browser run is still owed).
//!
//! Everything is drawn from **fixed** bytes (never the CSPRNG), so the fixture is reproducible and
//! the bun test is a true known-answer test — `build-wasm` / `drop-kat` stay deterministic and
//! offline. The values here are the single source of truth; the Rust adoption test mirrors them.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::keys::DekKeypair;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::pwkdf;
use capsule_core::drop::seal_drop_derand;
use eyre::{Context, Result};
use serde_json::json;

/// The passphrase the abuse-gate proof vector is derived from.
const PASSPHRASE: &str = "let me contribute to your album";
/// The content type the fixed drop is sealed under (in the drop protocol's closed enum).
const CONTENT_TYPE: &str = "image/jpeg";

/// Generate the guest-drop KAT fixture and write it to `out` (relative to the repo root). The
/// fixed inputs below are mirrored by `capsule-core/tests/drop_adopt_kat.rs`.
pub(crate) fn run(root: &Path, out_rel: &str) -> Result<()> {
    // Fixed, non-CSPRNG seal inputs → a reproducible known-answer fixture.
    let drop_seed = [0x5Du8; 32];
    let k = [0x11u8; 32];
    let nonce_prefix = [0x22u8; 7];
    let eseed = [0x33u8; 64];
    let blob_nonce = [0x44u8; 12];

    // The Drop Key: its public half is what the browser seals to (from the URL fragment); its
    // private half (reconstructed from the seed) is what the Rust adopter decapsulates with.
    let drop_key = DekKeypair::from_seed(&drop_seed);
    let drop_pubkey = drop_key.public_bytes();

    // A plaintext large enough to span multiple STREAM chunks — the same shape a real guest photo
    // takes on the wire.
    let plaintext: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();

    // The expected sealed drop, computed by the Rust core the browser must match byte-for-byte.
    let sealed = seal_drop_derand(
        &plaintext,
        &drop_pubkey,
        CONTENT_TYPE,
        &k,
        &nonce_prefix,
        &eseed,
        &blob_nonce,
    )
    .context("seal the deterministic guest drop")?;
    let d = &sealed.descriptor;

    // The passphrase abuse-gate proof vector: fast Argon2id params (self-describing in the link
    // fragment), a fixed salt, and the derived proof the browser must reproduce.
    let salt = [0x9Au8; 16];
    let argon = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    let expected_proof = pwkdf::derive_wrap_key(PASSPHRASE.as_bytes(), &salt, argon)
        .context("derive the passphrase abuse-gate proof")?;

    let fixture = json!({
        "_generated_by": "cargo xtask drop-kat — do not edit by hand",
        // ── Fixed seal inputs the browser feeds sealDropDerand ──
        "dropSeedHex": hex::encode(drop_seed),
        "dropPubkeyB64": BASE64.encode(&drop_pubkey),
        "contentType": CONTENT_TYPE,
        "plaintextB64": BASE64.encode(&plaintext),
        "kHex": hex::encode(k),
        "noncePrefixHex": hex::encode(nonce_prefix),
        "eseedHex": hex::encode(eseed),
        "blobNonceHex": hex::encode(blob_nonce),
        // ── Expected sealed output (the browser must reproduce these byte-for-byte) ──
        "descriptor": {
            "contentType": d.content_type,
            "plaintextSize": d.plaintext_size,
            "chunkSize": d.chunk_size,
            "noncePrefixHex": hex::encode(d.nonce_prefix),
            "ciphertextHashHex": d.ciphertext_hash.to_hex(),
            "kemCtB64": BASE64.encode(&d.kem_ct),
        },
        "ciphertextB64": BASE64.encode(&sealed.ciphertext),
        // ── Passphrase abuse-gate proof vector ──
        "passphrase": PASSPHRASE,
        "wrongPassphrase": "not the passphrase",
        "passphraseSaltHex": hex::encode(salt),
        "passphraseMemKib": argon.mem_kib,
        "passphraseTCost": argon.t_cost,
        "passphrasePCost": argon.p_cost,
        "expectedProofHex": hex::encode(expected_proof),
    });

    let path = root.join(out_rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create fixture dir {}", parent.display()))?;
    }
    let mut body = serde_json::to_string_pretty(&fixture).context("serialize fixture JSON")?;
    body.push('\n');
    std::fs::write(&path, body).with_context(|| format!("write fixture {}", path.display()))?;
    eprintln!("wrote guest-drop KAT fixture to {}", path.display());
    Ok(())
}
