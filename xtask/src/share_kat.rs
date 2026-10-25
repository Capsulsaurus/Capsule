//! Cross-language share-link Known-Answer-Test (KAT) fixture generator.
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
//! The top-level fixture is an **asset-scoped** grant: [`ScopeMaterial::Asset`] carries the
//! per-file key directly, so a single served ciphertext blob round-trips end to end without an
//! album AMK ledger. Its `album` member is an **album-scoped** grant over one `derived` asset and
//! one adopted web-upload drop (`key_mode = wrapped`), so the browser proves it recovers both key
//! modes (#598). Every served ciphertext is produced with the real
//! [`encrypt_asset_vec_with_prefix`] STREAM construction the serve path returns verbatim.
//!
//! Each grant also has a `served` member (#614): the link published through [`publish_share`]
//! and laid out as the server serves it, so the bun test drives the viewer's whole fetch
//! sequence — the metadata record, the sealed blob, the wrapped secret — through the wasm.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::encryption::{
    NONCE_PREFIX_LEN, encrypt_asset_vec_with_prefix, seal_file_key,
};
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::Amk;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::provenance::{KeyMode, WrappedFileKey};
use capsule_core::sharing::{
    LINK_SECRET_LEN, OPAQUE_ID_LEN, ScopeMaterial, SharedAsset, WrappedScope, encapsulate_scope,
    publish_share,
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

/// What the server serves for one published link (#614), in the server's own wire shapes: the
/// `GET /s/{opaque-id}` body, the octets of every `/s/{opaque-id}/blob/{hash}` the link names, and
/// the `/s/{opaque-id}/wrapped-secret` octets when there is a wrapped secret.
///
/// Built by [`publish_share`], the issuer function a client runs before `POST /v1/shares`, so the
/// bun test drives the viewer's fetch sequence against bytes no different from a real link's.
fn served_link(
    wrapped: &WrappedScope,
    fragment: &[u8; LINK_SECRET_LEN],
    opaque_id: &[u8; OPAQUE_ID_LEN],
    expires_at: Option<&str>,
    assets: Vec<(SharedAsset, &[u8])>,
) -> Result<serde_json::Value> {
    let mut blobs = serde_json::Map::new();
    let mut records = Vec::new();
    for (asset, ciphertext) in assets {
        blobs.insert(
            asset.content_hash.to_hex(),
            json!(BASE64.encode(ciphertext)),
        );
        records.push(asset);
    }
    let publication = publish_share(
        wrapped,
        fragment,
        opaque_id,
        expires_at.map(str::to_owned),
        records,
    )
    .context("publish the share metadata record")?;
    let metadata_hash = publication.metadata_hash.to_hex();
    blobs.insert(
        metadata_hash.clone(),
        json!(BASE64.encode(&publication.metadata_blob)),
    );
    Ok(json!({
        "metadata": {
            "metadata_hash": metadata_hash,
            "passphrase_protected": publication.wrapped_secret.is_some(),
        },
        "blobs": blobs,
        "wrappedSecretB64": publication.wrapped_secret.map(|bytes| BASE64.encode(bytes)),
    }))
}

/// A served asset record over `ciphertext`, as the issuer describes it.
fn shared_asset(
    asset_id: Uuid,
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    amk_version: u32,
    plaintext: &[u8],
    ciphertext: &[u8],
    wrapped_file_key: Option<Vec<u8>>,
) -> SharedAsset {
    SharedAsset {
        asset_id,
        content_hash: hash_bytes(ciphertext),
        content_type: "image/png".to_owned(),
        size: plaintext.len() as u64,
        amk_version,
        nonce_prefix,
        key_mode: if wrapped_file_key.is_some() {
            KeyMode::Wrapped
        } else {
            KeyMode::Derived
        },
        wrapped_file_key: wrapped_file_key.map(WrappedFileKey),
        sidecar: None,
    }
}

/// The album-scoped half of the fixture (#598): one grant over an album holding a `derived`
/// asset and an adopted web-upload drop (`key_mode = wrapped`), each served as ciphertext under
/// its own key, plus a wrapped key sealed under a foreign AMK for the fail-closed case.
///
/// The drop's key `K` is one a guest chose, so no AMK derives it: the viewer has to unwrap it
/// from `wrapped_file_key`, which is the path this half proves end to end through the wasm.
fn album_fixture(
    fragment: &[u8; LINK_SECRET_LEN],
    opaque_id: &[u8; OPAQUE_ID_LEN],
    argon: Argon2Params,
    plaintext: &[u8],
) -> Result<serde_json::Value> {
    let album_id = Uuid::from_u128(0x00A1_B0A1_B0A1_B0A1_B0A1_B0A1_B0A1_B0A1);
    let amk_version = 3u32;
    let amk_bytes = [0x3Cu8; 32];
    let amk = Amk::from_bytes(amk_bytes);

    let derived_id = Uuid::from_u128(0x00D3_D3D3_D3D3_D3D3_D3D3_D3D3_D3D3_D3D3);
    let derived_prefix = [0x0Du8; NONCE_PREFIX_LEN];
    let derived_key = amk.derive_file_key(&derived_id, &derived_prefix);
    let (_, derived_ct) = encrypt_asset_vec_with_prefix(&derived_key, derived_prefix, plaintext);

    let wrapped_id = Uuid::from_u128(0x00F7_F7F7_F7F7_F7F7_F7F7_F7F7_F7F7_F7F7);
    let wrapped_prefix = [0x0Fu8; NONCE_PREFIX_LEN];
    let guest_key = [0x6Bu8; 32];
    let wrapped_key = seal_file_key(&amk, &wrapped_id, &guest_key);
    let foreign_wrapped = seal_file_key(&Amk::from_bytes([0x99u8; 32]), &wrapped_id, &guest_key);
    let (_, wrapped_ct) = encrypt_asset_vec_with_prefix(&guest_key, wrapped_prefix, plaintext);

    let material = ScopeMaterial::Album {
        album_id,
        amks: BTreeMap::from([(amk_version, amk_bytes)]),
    };
    let link_only = encapsulate_scope(&material, fragment, opaque_id, None, argon)
        .context("seal album link-only scope")?;
    let served = served_link(
        &link_only,
        fragment,
        opaque_id,
        None,
        vec![
            (
                shared_asset(
                    derived_id,
                    derived_prefix,
                    amk_version,
                    plaintext,
                    &derived_ct,
                    None,
                ),
                &derived_ct,
            ),
            (
                shared_asset(
                    wrapped_id,
                    wrapped_prefix,
                    amk_version,
                    plaintext,
                    &wrapped_ct,
                    Some(wrapped_key.clone()),
                ),
                &wrapped_ct,
            ),
        ],
    )?;

    Ok(json!({
        "served": served,
        "linkOnlyWrappedB64": wrapped_b64(&link_only)?,
        "amkVersion": amk_version,
        "derived": {
            "fileId": derived_id.to_string(),
            "noncePrefixHex": hex::encode(derived_prefix),
            "ciphertextB64": BASE64.encode(&derived_ct),
        },
        "wrapped": {
            "fileId": wrapped_id.to_string(),
            "noncePrefixHex": hex::encode(wrapped_prefix),
            "wrappedFileKeyHex": hex::encode(&wrapped_key),
            "foreignWrappedFileKeyHex": hex::encode(&foreign_wrapped),
            "ciphertextB64": BASE64.encode(&wrapped_ct),
        },
    }))
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
    let album = album_fixture(&fragment, &opaque_id, argon, &plaintext)?;
    let asset = || {
        (
            shared_asset(file_id, nonce_prefix, 0, &plaintext, &ciphertext, None),
            ciphertext.as_slice(),
        )
    };
    let served = json!({
        "linkOnly": served_link(
            &link_only,
            &fragment,
            &opaque_id,
            Some("2030-01-01T00:00:00Z"),
            vec![asset()],
        )?,
        "passphrase": served_link(&passphrase_wrapped, &fragment, &opaque_id, None, vec![asset()])?,
    });

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
        "album": album,
        "served": served,
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
