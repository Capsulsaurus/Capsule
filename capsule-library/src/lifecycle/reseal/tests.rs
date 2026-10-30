//! Issue #822 — re-sealing a never-published create at the album's current epoch.

use std::collections::HashSet;
use std::fs;

use capsule_core::cbor;
use capsule_core::crypto::encryption::{decrypt_asset_vec, open_blob};
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::HybridSigningKey;
use capsule_core::crypto::provenance::DerivativeManifest;
use tempfile::TempDir;

use super::*;
use crate::library::{CustodyReceipt, CustodyReceiptCore, append_receipt};
use crate::lifecycle::{FAST_PARAMS, fast_workspace};

/// A workspace with one plain (non-decodable) asset imported at the album's first epoch.
fn imported(lib: &TempDir, src: &TempDir) -> (Workspace, Uuid, Uuid) {
    let img = src.path().join("photo.jpg");
    fs::write(
        &img,
        b"\xFF\xD8\xFF reseal fixture bytes, long enough to be an original",
    )
    .unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    (ws, album, asset)
}

/// A 512x384 PNG — large enough to earn a real thumbnail, built here because the repository
/// carries no binary fixtures.
fn png() -> Vec<u8> {
    use rawshift_image::core::metadata::ImageMetadata;
    use rawshift_image::core::{BitDepth, MetadataEmbedOptions};
    use rawshift_image::formats::encode_rgb_image_to_vec;
    use rawshift_image::formats::export::{
        CommonEncodeOptions, EncodeOptions, ZunePngEncodeConfig,
    };

    let (w, h) = (512u32, 384u32);
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push(((x * 255 / w) as u16) * 257);
            data.push(((y * 255 / h) as u16) * 257);
            data.push((((x + y) * 255 / (w + h)) as u16) * 257);
        }
    }
    let frame = rawshift_image::core::image::RgbImage::with_color_space(
        w,
        h,
        data,
        rawshift_image::core::ColorSpace::Srgb,
    );
    encode_rgb_image_to_vec(
        &frame,
        &ImageMetadata::default(),
        &EncodeOptions::PngZune(ZunePngEncodeConfig {
            common: CommonEncodeOptions {
                metadata: MetadataEmbedOptions::none(),
                bit_depth: BitDepth::Eight,
            },
        }),
    )
    .expect("the fixture PNG encodes")
}

fn head_core(ws: &Workspace, asset: &Uuid) -> ManifestCore {
    ws.asset(asset)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core
        .clone()
}

/// The asset's persisted derivative manifests, in the order the bundle holds them.
fn derivative_manifests(ws: &Workspace, asset: &Uuid) -> Vec<DerivativeManifest> {
    let state = ws.asset(asset).unwrap();
    let path = media_dir(ws.root(), state.capture_utc)
        .join("derivatives")
        .join(format!("{}.derivatives.cbor", asset.simple()));
    fs::read(&path).map_or_else(|_| Vec::new(), |b| cbor::from_slice(&b).unwrap())
}

fn refusal(error: LifecycleError) -> ResealRefusal {
    match error {
        LifecycleError::ResealRefused { reason, .. } => reason,
        other => panic!("expected a reseal refusal, got {other:?}"),
    }
}

/// **The re-seal.** A create sealed at epoch 1 and re-sealed after a rotation is a lone create at
/// the new epoch that verifies, whose original, metadata blob and nonces are all new, and whose
/// sidecar — and therefore plaintext — is unchanged. It survives a reopen.
#[test]
fn a_resealed_create_verifies_at_the_new_epoch_with_fresh_nonces() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album, asset) = imported(&lib, &src);
    let before = head_core(&ws, &asset);
    let old_bundle = ws.upload_bundle(&asset).unwrap();
    let old_sidecar = ws.asset(&asset).unwrap().sidecar.to_canonical_vec();
    let plaintext = ws.read_plaintext(&asset).unwrap();

    let epoch = ws.rotate_epoch(album).unwrap();
    assert!(epoch > before.amk_version.0);
    let outcome = ws.reseal_create(&asset).expect("the re-seal succeeds");
    assert_eq!(
        outcome,
        ResealOutcome {
            asset_id: asset,
            album_id: album,
            from_epoch: before.amk_version.0,
            to_epoch: epoch,
            derivatives: 0,
        }
    );

    let after = head_core(&ws, &asset);
    assert_eq!(ws.asset(&asset).unwrap().chain.records().len(), 1);
    assert_eq!(after.action, Action::Create);
    assert_eq!(after.prior_provenance_hash, None);
    assert_eq!(after.file_id, asset, "the asset keeps its id");
    assert_eq!(after.amk_version.0, epoch);
    assert_ne!(
        after.nonce_prefix, before.nonce_prefix,
        "the original's prefix is not reused"
    );
    assert_ne!(after.ciphertext_hash, before.ciphertext_hash);
    assert_eq!(after.plaintext_size, before.plaintext_size);
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);

    // The bundle carries only the new epoch's bytes, and they open to the same plaintext.
    let bundle = ws.upload_bundle(&asset).unwrap();
    assert_eq!(bundle.amk_version, epoch);
    assert_ne!(bundle.metadata_blob, old_bundle.metadata_blob);
    assert_ne!(
        blob_nonce(&bundle.metadata_blob),
        blob_nonce(&old_bundle.metadata_blob),
        "the metadata blob's nonce is not reused"
    );
    assert_ne!(bundle.provenance_blob, old_bundle.provenance_blob);
    let amk = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch]);
    let file_key = amk.derive_file_key(&asset, &after.nonce_prefix);
    assert_eq!(
        decrypt_asset_vec(
            &file_key,
            &after.nonce_prefix,
            &bundle.original.to_vec().unwrap()
        )
        .unwrap(),
        plaintext
    );
    let blob_key = amk.derive_blob_key(&asset, &blob_nonce(&bundle.metadata_blob).unwrap());
    assert_eq!(
        open_blob(&blob_key, &bundle.metadata_blob).unwrap(),
        old_sidecar,
        "the same signed sidecar, sealed again"
    );

    // Durable: a reopened library holds the re-sealed create, not the refused one.
    drop(ws);
    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(head_core(&reopened, &asset), after);
    assert_eq!(reopened.verify(&asset).unwrap(), VerifyOutcome::Accept);
    assert_eq!(
        reopened.upload_bundle(&asset).unwrap().metadata_blob,
        bundle.metadata_blob
    );
}

/// **The derivatives move with it.** Every regenerated derivative manifest names the new
/// epoch, starts a fresh chain, references the new original, and draws a prefix the asset never
/// spent — and the bundle ships them and nothing sealed under the old epoch.
#[test]
fn derivatives_are_regenerated_at_the_new_epoch_without_reusing_a_prefix() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.png");
    fs::write(&img, png()).unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();

    let before = head_core(&ws, &asset);
    let old_manifests = derivative_manifests(&ws, &asset);
    assert!(!old_manifests.is_empty(), "the fixture earns derivatives");
    let mut spent: HashSet<_> = old_manifests.iter().map(|m| m.core.nonce_prefix).collect();
    spent.insert(before.nonce_prefix);
    let old_blobs: HashSet<Hash32> = ws
        .upload_bundle(&asset)
        .unwrap()
        .derivatives
        .iter()
        .map(|d| d.ciphertext_hash)
        .collect();

    let epoch = ws.rotate_epoch(album).unwrap();
    let outcome = ws.reseal_create(&asset).unwrap();
    assert_eq!(outcome.derivatives, old_manifests.len());

    let after = head_core(&ws, &asset);
    let new_manifests = derivative_manifests(&ws, &asset);
    assert_eq!(new_manifests.len(), old_manifests.len());
    // A fresh chain links only within the new generation, never to an old-epoch manifest.
    let new_links: HashSet<Hash32> = new_manifests
        .iter()
        .map(|m| hash_bytes(&cbor::to_canonical_vec(m).unwrap()))
        .collect();
    for manifest in &new_manifests {
        let core = &manifest.core;
        assert_eq!(core.amk_version.map(|v| v.0), Some(epoch));
        assert!(
            !spent.contains(&core.nonce_prefix),
            "a regenerated derivative reuses a spent prefix"
        );
        assert!(
            core.prior_provenance_hash
                .is_none_or(|prior| new_links.contains(&prior)),
            "a re-sealed derivative links to an old-epoch manifest"
        );
        if core.format == "original" {
            assert_eq!(core.ciphertext_hash, after.ciphertext_hash);
            assert_eq!(core.nonce_prefix, after.nonce_prefix);
        }
    }

    let bundle = ws.upload_bundle(&asset).unwrap();
    assert!(!bundle.derivatives.is_empty());
    for derivative in &bundle.derivatives {
        assert_eq!(derivative.amk_version, Some(epoch));
        assert!(
            !old_blobs.contains(&derivative.ciphertext_hash),
            "an old-epoch derivative ciphertext is still reachable"
        );
        assert_eq!(hash_bytes(&derivative.bytes), derivative.ciphertext_hash);
    }
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

/// **No newer epoch, no re-seal.** The device that never rotated — or never received the key
/// another device rotated to — is refused, and nothing on disk changes.
#[test]
fn without_a_newer_epoch_the_reseal_is_refused_and_writes_nothing() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, _album, asset) = imported(&lib, &src);
    let epoch = head_core(&ws, &asset).amk_version.0;
    let chain_before = ws.upload_bundle(&asset).unwrap().provenance_blob;

    let error = ws.reseal_create(&asset).unwrap_err();
    assert_eq!(
        refusal(error),
        ResealRefusal::NoNewerEpoch {
            sealed: epoch,
            current: epoch,
        }
    );
    assert_eq!(
        ws.upload_bundle(&asset).unwrap().provenance_blob,
        chain_before
    );
}

/// **An edited asset is never re-sealed.** A second record means the genesis is history some
/// later record links to; replacing it would orphan that record.
#[test]
fn an_asset_with_more_than_its_create_is_refused() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album, asset) = imported(&lib, &src);
    ws.tag_add(&asset, "beach").unwrap();
    ws.rotate_epoch(album).unwrap();
    let chain_before = ws.asset(&asset).unwrap().chain.records().to_vec();

    let error = ws.reseal_create(&asset).unwrap_err();
    assert_eq!(refusal(error), ResealRefusal::NotALoneCreate);
    assert_eq!(ws.asset(&asset).unwrap().chain.records(), chain_before);
}

/// **A published asset is never re-sealed.** A custody receipt in the library is proof the
/// server holds the genesis, and a published genesis is append-only history.
#[test]
fn an_asset_with_a_custody_receipt_is_refused() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album, asset) = imported(&lib, &src);
    let bundle = ws.upload_bundle(&asset).unwrap();
    let key = HybridSigningKey::from_seed64(&[0x42; 64]);
    let core = CustodyReceiptCore {
        version: "custody-receipt/v1".into(),
        crypto_suite_id: bundle.crypto_suite_id,
        protocol_version: bundle.protocol_version.clone(),
        server_id: "capsule.example".into(),
        server_key_id: Hash32::from_bytes([0x11; 32]),
        receipt_seq: 1,
        prior_receipt_hash: None,
        upload_id: "sess-1".into(),
        asset_id: asset.to_string(),
        blob_role: "provenance".into(),
        ciphertext_hash: hash_bytes(&bundle.provenance_blob),
        size: bundle.provenance_blob.len() as u64,
        envelope_hash: Some(hash_bytes(&bundle.provenance_blob)),
        uploaded_by_user: bundle.created_by_user.to_string(),
        uploaded_by_device: None,
        received_at: "2026-01-01T00:00:00Z".into(),
        unknown: Default::default(),
    };
    let server_sig = key.sign(&core.signing_bytes());
    let capture_utc = ws.asset(&asset).map(|a| a.capture_utc);
    append_receipt(ws.root(), capture_utc, &CustodyReceipt { core, server_sig }).unwrap();
    ws.rotate_epoch(album).unwrap();

    let error = ws.reseal_create(&asset).unwrap_err();
    assert_eq!(refusal(error), ResealRefusal::Published);
    assert_eq!(
        ws.upload_bundle(&asset).unwrap().provenance_blob,
        bundle.provenance_blob
    );
}

/// **The plaintext is checked against the signed sidecar.** An original changed on disk since
/// import is refused rather than sealed under a manifest that would misdescribe it.
#[test]
fn a_changed_original_is_refused() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album, asset) = imported(&lib, &src);
    let path = ws.original_path(&asset).unwrap();
    fs::write(
        &path,
        b"\xFF\xD8\xFF different bytes entirely, same asset path",
    )
    .unwrap();
    ws.rotate_epoch(album).unwrap();

    assert!(matches!(
        ws.reseal_create(&asset),
        Err(LifecycleError::CiphertextMismatch(id)) if id == asset
    ));
}

/// **The draw never returns a spent prefix**, and a draw that only ever returns spent ones is a
/// typed failure rather than a reuse.
#[test]
fn the_prefix_draw_skips_spent_prefixes_and_gives_up_on_a_broken_source() {
    let spent: HashSet<[u8; NONCE_PREFIX_LEN]> = [[1; NONCE_PREFIX_LEN]].into_iter().collect();
    let mut draws = [[1; NONCE_PREFIX_LEN], [2; NONCE_PREFIX_LEN]].into_iter();
    assert_eq!(
        draw_unspent_prefix(&spent, || draws.next().unwrap()).unwrap(),
        [2; NONCE_PREFIX_LEN]
    );
    assert!(matches!(
        draw_unspent_prefix(&spent, || [1; NONCE_PREFIX_LEN]),
        Err(LifecycleError::Crypto(CryptoError::NonceReuse))
    ));
}
