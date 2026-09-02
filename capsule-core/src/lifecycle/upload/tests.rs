//! `S-D18` — the upload-bundle accessor, including its `S-A10` interaction.

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::crypto::primitives::Argon2Params;

/// The fast-Argon2 cost the lifecycle suite uses; the production tier would dominate runtime.
const FAST: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A library with exactly one imported asset. Returns `(library dir, album id, asset id)`.
fn library_with_one_asset(lib: &TempDir, src: &TempDir) -> (Uuid, Uuid) {
    let img = src.path().join("photo.jpg");
    fs::write(
        &img,
        b"\xFF\xD8\xFF upload-bundle fixture bytes, long enough to chunk",
    )
    .unwrap();
    let mut ws = Workspace::create_with_params(lib.path(), b"passphrase", FAST).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    (album, asset)
}

/// The bundle's ciphertext is the one the signed manifest committed to: re-deriving it from the
/// recorded nonce prefix content-addresses back to `ciphertext_hash`, and every envelope field
/// mirrors the head manifest.
#[test]
fn upload_bundle_ciphertext_matches_the_manifest_hash() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (album, asset_id) = library_with_one_asset(&lib, &src);

    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let bundle = ws.upload_bundle(&asset_id).expect("bundle");
    let head = &ws
        .asset(&asset_id)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core;

    assert_eq!(bundle.ciphertext_hash, head.ciphertext_hash);
    assert_eq!(
        crate::crypto::hash::hash_bytes(&bundle.ciphertext),
        head.ciphertext_hash,
        "the bundle's bytes must content-address to the manifest's hash"
    );
    assert_eq!(bundle.asset_id, head.file_id);
    assert_eq!(bundle.album_id, album);
    assert_eq!(bundle.amk_version, head.amk_version.0);
    assert_eq!(bundle.plaintext_size, head.plaintext_size);
    assert_eq!(bundle.chunk_size, head.chunk_size);
    assert_eq!(bundle.crypto_suite_id, head.crypto_suite_id);
    assert_eq!(bundle.protocol_version, head.protocol_version);
    assert_eq!(bundle.key_mode, head.key_mode);
    assert_eq!(bundle.metadata_blob_hash, head.metadata_blob_hash);
    assert_eq!(bundle.created_by_user, head.created_by_user);
    assert_eq!(bundle.created_by_device, head.created_by_device);
    assert_eq!(bundle.client_version, head.client_version);
    assert_eq!(bundle.timestamp, head.timestamp);
    assert_eq!(bundle.action, head.action);
    assert_eq!(bundle.prior_provenance_hash, head.prior_provenance_hash);
    assert_eq!(bundle.retention_until, head.retention_until);
    assert!(
        bundle.ciphertext_size() > bundle.plaintext_size,
        "STREAM adds per-chunk tags"
    );

    // The sealed metadata blob is carried verbatim and content-addresses to what the manifest
    // committed to — it cannot be regenerated (`seal_metadata_blob` draws a fresh nonce).
    let blob_hash = crate::crypto::hash::hash_bytes(&bundle.metadata_blob);
    assert_eq!(Some(blob_hash), bundle.metadata_blob_hash);
}

/// **The `S-A10` interaction.** A bundle built after closing and reopening the library is
/// byte-identical to one built before the close: album keys, authorities, the asset's chain and
/// its exact sealed metadata blob all survive, so a push is re-runnable across processes. Before
/// `S-A10` the reopened workspace held no AMK at all and this could not even be attempted.
#[test]
fn upload_bundle_survives_a_reopen() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_, asset_id) = library_with_one_asset(&lib, &src);

    let before = {
        let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
        ws.upload_bundle(&asset_id).expect("bundle before close")
    };
    let after = {
        let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
        ws.upload_bundle(&asset_id).expect("bundle after reopen")
    };

    assert_eq!(before.ciphertext, after.ciphertext, "ciphertext is stable");
    assert_eq!(before.ciphertext_hash, after.ciphertext_hash);
    assert_eq!(
        before.metadata_blob, after.metadata_blob,
        "the sealed metadata blob must be the exact bytes on disk, not a re-seal"
    );
    assert_eq!(before.metadata_blob_hash, after.metadata_blob_hash);
    assert_eq!(before.album_id, after.album_id);
    assert_eq!(before.amk_version, after.amk_version);
    assert_eq!(before.timestamp, after.timestamp);
    assert_eq!(before.content_type, after.content_type);

    // The durable artifacts `S-A10` promises are actually on disk.
    assert!(lib.path().join(".library").join("albums.cbor").exists());
    let blob = walk_find(lib.path(), &format!("{}.metadata.bin", asset_id.simple()));
    assert!(blob, "the sealed metadata blob is persisted per asset");
}

/// An unknown asset id is a typed `NotFound`, never a panic.
#[test]
fn upload_bundle_rejects_an_unknown_asset() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    library_with_one_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    assert!(matches!(
        ws.upload_bundle(&Uuid::now_v7()),
        Err(LifecycleError::NotFound(_))
    ));
}

/// `export_backup` runs on the same accessor, so the artifact it writes still round-trips —
/// the refactor moved the crypto, it did not change it.
#[test]
fn export_backup_still_round_trips_through_the_accessor() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_, asset_id) = library_with_one_asset(&lib, &src);
    let out = TempDir::new().unwrap();
    let archive = out.path().join("backup.capsule");

    let (exporter_pub, bundle) = {
        let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
        ws.export_backup(&archive, b"backup-pass").unwrap();
        (
            ws.exporter_verifying_key(),
            ws.upload_bundle(&asset_id).unwrap(),
        )
    };
    assert!(archive.exists());

    let peer = TempDir::new().unwrap();
    let mut fresh = Workspace::create_with_params(peer.path(), b"peer-passphrase", FAST).unwrap();
    let added = fresh
        .import_backup(&archive, b"backup-pass", &exporter_pub)
        .unwrap();
    assert_eq!(added, 1, "the exported asset restores into a fresh library");
    let restored = fresh.upload_bundle(&asset_id).expect("restored bundle");
    assert_eq!(
        restored.ciphertext_hash, bundle.ciphertext_hash,
        "the restored asset re-derives the same ciphertext"
    );
}

/// Whether any file named `name` exists under `root`.
fn walk_find(root: &std::path::Path, name: &str) -> bool {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .any(|e| e.file_name().to_string_lossy() == name)
}

// ── Derivative blobs cross the network encrypted (S-B1 / encryption.md) ──────

/// A library with one **decodable** asset large enough to earn a real derivative, so the
/// derivative path is exercised rather than the `original` sentinel.
///
/// A 512x384 PNG, built here rather than committed: the repository carries no binary fixtures.
fn library_with_a_thumbnailed_asset(lib: &TempDir, src: &TempDir) -> (Uuid, Uuid) {
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
    let png = encode_rgb_image_to_vec(
        &frame,
        &ImageMetadata::default(),
        &EncodeOptions::PngZune(ZunePngEncodeConfig {
            common: CommonEncodeOptions {
                metadata: MetadataEmbedOptions::none(),
                bit_depth: BitDepth::Eight,
            },
            ..ZunePngEncodeConfig::default()
        }),
    )
    .expect("the fixture PNG encodes");

    let img = src.path().join("photo.png");
    fs::write(&img, &png).unwrap();
    let mut ws = Workspace::create_with_params(lib.path(), b"passphrase", FAST).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    (album, asset)
}

/// The derivative round trip, end to end: the plaintext the library holds is **not** what the
/// bundle ships, the bundle's bytes content-address to the signed `ciphertext_hash`, and
/// decrypting them with the manifest's recorded `nonce_prefix` yields the plaintext back.
///
/// This is the property the encryption doc states without qualification — "every asset —
/// original bytes, derivative bytes, metadata blob — is encrypted client-side" — and a thumbnail
/// is a recognisable low-resolution copy of a private photo, so shipping one in the clear would
/// hand the server the picture it is not allowed to see.
#[test]
fn derivative_blobs_ship_ciphertext_that_decrypts_to_the_bytes_on_disk() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);

    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert_eq!(bundle.derivatives.len(), 1, "one encodable format today");
    let blob = &bundle.derivatives[0];
    assert_eq!(blob.format, "image/jxl");

    // The plaintext the local gallery paints, straight off disk.
    let asset = ws.asset(&asset_id).expect("the asset is held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    let stem = asset_id.simple().to_string();
    let plaintext = fs::read(dir.join(format!("{stem}.thumbnail.jxl"))).unwrap();
    assert_eq!(
        &plaintext[..2],
        b"\xFF\x0A",
        "the on-disk derivative is a bare JXL codestream"
    );

    assert_ne!(
        blob.bytes, plaintext,
        "what crosses the network is not what sits on disk"
    );
    assert_eq!(
        hash::hash_bytes(&blob.bytes),
        blob.ciphertext_hash,
        "the blob's declared address is its own content address"
    );

    // And that address is the one the *signed* manifest committed to.
    let bundle_path = dir.join(format!("{stem}.derivatives.cbor"));
    let manifests: Vec<DerivativeManifest> =
        cbor::from_slice(&fs::read(&bundle_path).unwrap()).expect("the bundle decodes");
    let core = &manifests[0].core;
    assert_eq!(core.ciphertext_hash, blob.ciphertext_hash);

    // The receiver's half: the recorded prefix selects the key and the nonces.
    let album_keys = ws.album(&asset.album_id).unwrap();
    let file_key = ws.file_key(
        album_keys,
        core.amk_version.unwrap().0,
        &asset_id,
        &core.nonce_prefix,
    );
    let recovered =
        stream::decrypt_asset_vec(&file_key, &core.nonce_prefix, &blob.bytes).expect("it opens");
    assert_eq!(
        recovered, plaintext,
        "and it decrypts to exactly the derivative the client holds"
    );
}

/// A derivative whose on-disk bytes have been altered no longer re-derives to the address its
/// manifest signed, so it is skipped rather than shipped. The bundle still carries the original
/// and its metadata — a stale thumbnail is regenerable, a missing backup is not.
#[test]
fn a_tampered_derivative_is_skipped_rather_than_shipped() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);

    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let asset = ws.asset(&asset_id).expect("the asset is held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    let path = dir.join(format!("{}.thumbnail.jxl", asset_id.simple()));

    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    fs::write(&path, &bytes).unwrap();

    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert!(
        bundle.derivatives.is_empty(),
        "a derivative that does not match its signed manifest is not uploaded"
    );
    assert!(
        !bundle.ciphertext.is_empty(),
        "and the original is still shipped — the backup is what must not be lost"
    );
}

/// The `original` sentinel carries no bytes **by design**, so the bundle simply has no
/// derivative blob for that tier — and the skip is not a warning, because an expected absence
/// logged as a problem is how people learn to ignore warnings.
#[test]
fn the_original_sentinel_contributes_no_blob_and_is_not_an_error() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    // The 8x8 JPEG fixture is far inside the 256 px thumbnail cap, so its tier is the sentinel.
    let (_album, asset_id) = library_with_one_asset(&lib, &src);

    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert!(
        bundle.derivatives.is_empty(),
        "the sentinel references the original blob; there is nothing extra to upload"
    );
}

/// Rewrite an asset's derivative bundle with `manifests`, returning the directory it lives in.
fn rewrite_bundle(lib: &TempDir, ws: &Workspace, asset_id: Uuid, manifests: &[DerivativeManifest]) {
    let asset = ws.asset(&asset_id).expect("the asset is held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{}.derivatives.cbor", asset_id.simple())),
        cbor::to_canonical_vec(&manifests.to_vec()).unwrap(),
    )
    .unwrap();
}

/// Sign a derivative manifest with the given role and wire `format`, over `ciphertext_hash`.
fn signed_derivative(
    asset_id: Uuid,
    role: DerivativeRole,
    format: &str,
    ciphertext_hash: crate::crypto::hash::Hash32,
) -> DerivativeManifest {
    use crate::crypto::keys::{AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::crypto::provenance::manifest::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore};

    let device = HybridSigningKey::from_seed_bytes(&[21; 32], &[22; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[23; 32], &[24; 32]);
    DerivativeCore {
        version: DERIVATIVE_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: Some(PROTOCOL_VERSION.into()),
        amk_version: Some(AmkVersion(1)),
        source_asset_id: asset_id,
        role,
        format: format.into(),
        ciphertext_hash,
        nonce_prefix: [7, 6, 5, 4, 3, 2, 1],
        generated_by_device: Uuid::from_u128(0xD1),
        generated_by_client: "capsule-core/test".into(),
        model_id: None,
        model_version: None,
        generated_at: "2026-09-02T00:00:00Z".into(),
        prior_provenance_hash: None,
    }
    .sign(&device, &write)
    .expect("signing")
}

/// **The closed-format rule, at the boundary that ships bytes.** A still-role manifest naming a
/// format outside the committed set is a structural rejection, so its bytes never reach the
/// network — even though they are sitting on disk and hash correctly.
///
/// The embedding role is deliberately exempt: it writes `embedding/{model_id}` into the same
/// field, a grammar this set does not model, so it must not be caught in the crossfire.
#[test]
fn a_still_role_derivative_outside_the_closed_set_is_skipped() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();

    // The bytes on disk stay exactly as the import wrote them; only the manifest's format moves.
    let asset = ws.asset(&asset_id).expect("held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    let plaintext = fs::read(dir.join(format!("{}.thumbnail.jxl", asset_id.simple()))).unwrap();
    let album_keys = ws.album(&asset.album_id).unwrap();
    let file_key = ws.file_key(album_keys, 1, &asset_id, &[7, 6, 5, 4, 3, 2, 1]);
    let (_, ciphertext) =
        stream::encrypt_asset_vec_with_prefix(&file_key, [7, 6, 5, 4, 3, 2, 1], &plaintext);
    let address = hash::hash_bytes(&ciphertext);

    // A recognised format ships...
    rewrite_bundle(
        &lib,
        &ws,
        asset_id,
        &[signed_derivative(
            asset_id,
            DerivativeRole::Thumbnail,
            "image/jxl",
            address,
        )],
    );
    assert_eq!(
        ws.upload_bundle(&asset_id).unwrap().derivatives.len(),
        1,
        "a format inside the closed set is uploaded"
    );

    // ...and an unrecognised one does not, with everything else held equal.
    rewrite_bundle(
        &lib,
        &ws,
        asset_id,
        &[signed_derivative(
            asset_id,
            DerivativeRole::Thumbnail,
            "image/future-codec",
            address,
        )],
    );
    assert!(
        ws.upload_bundle(&asset_id).unwrap().derivatives.is_empty(),
        "an unrecognised still format is a structural rejection, not a blob"
    );
}

/// A manifest with a **recognised** format and no bytes on disk is a genuine problem and is
/// skipped, which is what keeps the sentinel's quiet skip from being a blanket amnesty for
/// missing files.
#[test]
fn a_non_sentinel_manifest_with_no_bytes_is_still_skipped() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();

    let asset = ws.asset(&asset_id).expect("held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    fs::remove_file(dir.join(format!("{}.thumbnail.jxl", asset_id.simple()))).unwrap();

    assert!(
        ws.upload_bundle(&asset_id).unwrap().derivatives.is_empty(),
        "a thumbnail manifest whose bytes have gone is not shipped"
    );
}

/// The `capsule import` acceptance case, at the bundle boundary: the bytes a push would send for
/// the thumbnail tier are **not** the bytes on disk, and their magic differs — the on-disk file
/// is a bare JXL codestream (`FF 0A`), the wire blob is STREAM ciphertext.
///
/// The magic check is the cheap, legible version of the round trip above: it is what someone
/// eyeballing a packet capture would look for.
#[test]
fn the_pushed_thumbnail_is_not_the_jxl_on_disk() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);

    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let asset = ws.asset(&asset_id).expect("held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    let disk = fs::read(dir.join(format!("{}.thumbnail.jxl", asset_id.simple()))).unwrap();
    assert_eq!(&disk[..2], b"\xFF\x0A", "on disk: a bare JXL codestream");

    let mut bundle = ws.upload_bundle(&asset_id).unwrap();
    let blob = bundle.derivatives.remove(0);
    assert_ne!(
        &blob.bytes[..2],
        b"\xFF\x0A",
        "on the wire: ciphertext, so it does not begin with the JXL magic"
    );
    assert_ne!(blob.bytes, disk);
}
