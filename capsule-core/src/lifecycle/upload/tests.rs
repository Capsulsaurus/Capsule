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
        crate::crypto::hash::hash_bytes(&bundle.original.to_vec().unwrap()),
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

/// An original is never refused for its type (issue #695): one the protocol's closed
/// `content_type` enum names declares itself, any other declares the opaque type, and the
/// sidecar keeps the true one either way.
#[test]
fn an_original_outside_the_closed_enum_declares_the_opaque_type() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = Workspace::create_with_params(lib.path(), b"passphrase", FAST).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    for (name, sidecar_type, wire_type) in [
        ("clip.mp4", "video/mp4", "video/mp4"),
        ("clip.mkv", "video/x-matroska", OPAQUE_CONTENT_TYPE),
        ("clip.avi", "video/x-msvideo", OPAQUE_CONTENT_TYPE),
        ("scan.pdf", OPAQUE_CONTENT_TYPE, OPAQUE_CONTENT_TYPE),
    ] {
        let path = src.path().join(name);
        fs::write(&path, format!("{name} bytes")).unwrap();
        let asset_id = ws.import_asset(album, &path).unwrap();
        assert_eq!(
            ws.asset(&asset_id).unwrap().sidecar.content_type,
            sidecar_type
        );
        let bundle = ws.upload_bundle(&asset_id).expect("bundle");
        assert_eq!(bundle.content_type, wire_type, "for {name}");
    }
}

#[test]
fn wire_content_type_passes_the_closed_enum_and_masks_the_rest() {
    for kind in WIRE_ORIGINAL_CONTENT_TYPES {
        assert_eq!(wire_content_type(kind), *kind);
    }
    for kind in [
        "image/x-sony-arw",
        "video/x-matroska",
        "application/pdf",
        "",
    ] {
        assert_eq!(wire_content_type(kind), OPAQUE_CONTENT_TYPE);
    }
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

    assert_eq!(
        before.original.to_vec().unwrap(),
        after.original.to_vec().unwrap(),
        "ciphertext is stable"
    );
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
        ws.export_backup_with_params(&archive, b"backup-pass", FAST)
            .unwrap();
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

/// How many derivative blobs the 512x384 fixture of [`library_with_a_thumbnailed_asset`] yields
/// in this build: every generated tier times every encodable delivery format. One (the
/// thumbnail JXL) without `media-linked-codecs`; four (thumbnail and preview, JXL and AVIF) with
/// it. The thumbnail JXL is always first.
fn blobs_per_thumbnailed_asset() -> usize {
    crate::media::DerivativeTier::GENERATED.len()
        * crate::derivative_format::DerivativeFormat::STILL_DELIVERY_ORDER
            .iter()
            .filter(|f| f.is_encodable())
            .count()
}

/// Whether the bundle still ships the thumbnail tier's JXL.
fn ships_the_thumbnail_jxl(bundle: &UploadBundle) -> bool {
    bundle
        .derivatives
        .iter()
        .any(|d| d.role == DerivativeRole::Thumbnail && d.format == "image/jxl")
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
    assert_eq!(
        bundle.derivatives.len(),
        blobs_per_thumbnailed_asset(),
        "every generated (tier, format) pair is shipped"
    );
    let blob = &bundle.derivatives[0];
    assert_eq!(blob.format, "image/jxl");
    assert_eq!(blob.role, DerivativeRole::Thumbnail);

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
        crate::crypto::encryption::decrypt_asset_vec(&file_key, &core.nonce_prefix, &blob.bytes)
            .expect("it opens");
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
        !ships_the_thumbnail_jxl(&bundle),
        "a derivative that does not match its signed manifest is not uploaded"
    );
    assert_eq!(
        bundle.derivatives.len(),
        blobs_per_thumbnailed_asset() - 1,
        "and only that one is dropped: the untouched derivatives still ship"
    );
    assert!(
        bundle.original.size() > 0,
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
    signed_derivative_at_epoch(asset_id, role, format, ciphertext_hash, 1)
}

/// As [`signed_derivative`], with an explicit `amk_version` — so a manifest can name an epoch
/// the album does not hold.
fn signed_derivative_at_epoch(
    asset_id: Uuid,
    role: DerivativeRole,
    format: &str,
    ciphertext_hash: crate::crypto::hash::Hash32,
    epoch: u32,
) -> DerivativeManifest {
    use crate::crypto::keys::{AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::crypto::provenance::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore};

    let device = HybridSigningKey::from_seed_bytes(&[21; 32], &[22; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[23; 32], &[24; 32]);
    DerivativeCore {
        version: DERIVATIVE_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: Some(PROTOCOL_VERSION.into()),
        amk_version: Some(AmkVersion(epoch)),
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
    let (_, ciphertext) = crate::crypto::encryption::encrypt_asset_vec_with_prefix(
        &file_key,
        [7, 6, 5, 4, 3, 2, 1],
        &plaintext,
    );
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

    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert!(
        !ships_the_thumbnail_jxl(&bundle),
        "a thumbnail manifest whose bytes have gone is not shipped"
    );
    assert_eq!(bundle.derivatives.len(), blobs_per_thumbnailed_asset() - 1);
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

/// **The `F1` contract at the import boundary.** A derivative that cannot be produced must never
/// cost the original: the asset still lands signed, encrypted and `verify_asset`-accepting, and
/// the run reports `DecodeFailed` rather than failing.
///
/// Exercised through a *decodable* still whose derivative directory is then made unwritable, so
/// the failure happens after a successful decode — the exact shape that used to propagate as
/// `LifecycleError::Io` and lose the import.
#[test]
fn a_derivative_that_cannot_be_persisted_never_costs_the_original() {
    use crate::crypto::verify_asset::VerifyOutcome;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);

    // The asset committed, derivatives or not.
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    assert_eq!(ws.verify(&asset_id).unwrap(), VerifyOutcome::Accept);

    // And the original's own bytes are on disk and re-derive to their signed address, which is
    // the property that makes this a backup rather than a thumbnail service.
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert!(bundle.original.size() > 0);
    assert_eq!(
        hash::hash_bytes(&bundle.original.to_vec().unwrap()),
        bundle.ciphertext_hash
    );
}

/// A persisted derivative survives a `Workspace` reopen and still reaches `UploadBundle`: the
/// bundle is rebuilt from the library directory alone, so the manifest, the on-disk plaintext
/// and the re-derived ciphertext all have to agree across processes.
#[test]
fn a_derivative_survives_a_reopen_and_still_reaches_the_bundle() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);

    // A second `Workspace::open` — the S-A10 shape: nothing shared but the directory.
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert_eq!(
        bundle.derivatives.len(),
        blobs_per_thumbnailed_asset(),
        "the derivatives survive a reopen"
    );
    let blob = &bundle.derivatives[0];
    assert_eq!(
        hash::hash_bytes(&blob.bytes),
        blob.ciphertext_hash,
        "and its ciphertext still content-addresses to the signed manifest"
    );
}

/// Two formats persisted for one role are told apart by **(role, format)**, not by whichever
/// filename sorts first.
///
/// `#437` lands AVIF beside JXL, at which point a role-prefix match would read one file and
/// content-address it against the other manifest — skipping both. Asserted before that lands,
/// because the failure mode is a silent skip rather than an error.
#[test]
fn two_formats_for_one_role_are_addressed_by_format_not_by_filename_order() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();

    let asset = ws.asset(&asset_id).expect("held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    let stem = asset_id.simple().to_string();
    let jxl = fs::read(dir.join(format!("{stem}.thumbnail.jxl"))).unwrap();

    // A decoy AVIF for the same role, sorting *before* `.jxl`, with different bytes.
    let avif = b"not a real avif, and deliberately different".to_vec();
    fs::write(dir.join(format!("{stem}.thumbnail.avif")), &avif).unwrap();

    // Both manifests, each addressing its own ciphertext.
    let album_keys = ws.album(&asset.album_id).unwrap();
    let address = |bytes: &[u8]| {
        let key = ws.file_key(album_keys, 1, &asset_id, &[7, 6, 5, 4, 3, 2, 1]);
        let (_, ct) = crate::crypto::encryption::encrypt_asset_vec_with_prefix(
            &key,
            [7, 6, 5, 4, 3, 2, 1],
            bytes,
        );
        hash::hash_bytes(&ct)
    };
    rewrite_bundle(
        &lib,
        &ws,
        asset_id,
        &[
            signed_derivative(
                asset_id,
                DerivativeRole::Thumbnail,
                "image/jxl",
                address(&jxl),
            ),
            signed_derivative(
                asset_id,
                DerivativeRole::Thumbnail,
                "image/avif",
                address(&avif),
            ),
        ],
    );

    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert_eq!(
        bundle.derivatives.len(),
        2,
        "both formats of the role are shipped; neither is mistaken for the other"
    );
    let formats: Vec<&str> = bundle
        .derivatives
        .iter()
        .map(|d| d.format.as_str())
        .collect();
    assert_eq!(formats, vec!["image/jxl", "image/avif"]);
}

/// **A manifest naming an epoch the album does not hold is skipped, not a panic.**
///
/// `file_key` reaches `album.amks[&epoch]`, which panics on a missing key — and the epoch comes
/// off an unverified `.cbor` on disk, inside a function whose whole contract is that it never
/// fails the bundle. This is reachable without any tampering at all: an album recovered from a
/// backup holds only the epochs it escrowed.
#[test]
fn a_derivative_naming_an_unheld_epoch_is_skipped_rather_than_panicking() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();

    rewrite_bundle(
        &lib,
        &ws,
        asset_id,
        &[signed_derivative_at_epoch(
            asset_id,
            DerivativeRole::Thumbnail,
            "image/jxl",
            hash::hash_bytes(b"whatever"),
            9_999,
        )],
    );

    // The assertion is as much that this returns at all as that it returns nothing.
    let bundle = ws
        .upload_bundle(&asset_id)
        .expect("an unheld epoch must not fail the bundle");
    assert!(
        bundle.derivatives.is_empty(),
        "a derivative whose key epoch is absent is skipped"
    );
    assert!(
        bundle.original.size() > 0,
        "and the original is still shipped"
    );
}

/// An embedding-role manifest is **out of this reader's scope**, and says so quietly.
///
/// Its `embedding/{model_id}` grammar is not in the still format set and `crate::ml` produces no
/// derivative manifest at all, so the reader neither ships it nor complains about it — the
/// previous behaviour logged "no bytes on disk" for a manifest it had already decided not to
/// handle, which is a misleading warning for an artefact with no writer.
#[test]
fn an_embedding_role_manifest_is_out_of_scope_and_skipped() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();

    rewrite_bundle(
        &lib,
        &ws,
        asset_id,
        &[signed_derivative(
            asset_id,
            DerivativeRole::Embedding,
            "embedding/mobileclip-b",
            hash::hash_bytes(b"an embedding"),
        )],
    );

    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert!(
        bundle.derivatives.is_empty(),
        "embeddings are not this reader's business until something produces them"
    );
    assert_eq!(
        verify_still_format(&signed_derivative(
            asset_id,
            DerivativeRole::Embedding,
            "embedding/mobileclip-b",
            hash::hash_bytes(b"an embedding"),
        )),
        Ok(None),
        "and the closed-set check reports them as out of scope rather than rejecting them"
    );
}

// ── Derivative manifests travel with the bundle (#556) ───────────────────────

/// Decode a bundle's `derivative_manifest` blobs back into the signed manifests they carry.
fn shipped_manifests(bundle: &UploadBundle) -> Vec<DerivativeManifest> {
    bundle
        .derivative_manifests
        .iter()
        .map(|bytes| cbor::from_slice(bytes).expect("a shipped manifest decodes"))
        .collect()
}

/// Every shipped derivative travels with the signed manifest that opens it: one manifest per
/// blob, each committing to exactly that blob's address, carrying the nonce prefix and epoch a
/// receiver derives the file key from — and encoded canonically, so the bytes are a pure
/// function of the signed manifest the library recorded.
#[test]
fn every_shipped_derivative_carries_the_signed_manifest_that_opens_it() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let bundle = ws.upload_bundle(&asset_id).unwrap();

    let manifests = shipped_manifests(&bundle);
    assert_eq!(
        manifests.len(),
        bundle.derivatives.len(),
        "one manifest per shipped derivative"
    );
    for (blob, manifest) in bundle.derivatives.iter().zip(&manifests) {
        assert_eq!(manifest.core.ciphertext_hash, blob.ciphertext_hash);
        assert_eq!(manifest.core.role, blob.role);
        assert_eq!(manifest.core.format, blob.format);
        assert_eq!(manifest.core.source_asset_id, asset_id);
    }

    // The receiver's half, from the shipped bytes alone: the manifest's prefix and epoch open
    // the blob it names.
    let asset = ws.asset(&asset_id).expect("held");
    let album_keys = ws.album(&asset.album_id).unwrap();
    let core = &manifests[0].core;
    let file_key = ws.file_key(
        album_keys,
        core.amk_version.unwrap().0,
        &asset_id,
        &core.nonce_prefix,
    );
    crate::crypto::encryption::decrypt_asset_vec(
        &file_key,
        &core.nonce_prefix,
        &bundle.derivatives[0].bytes,
    )
    .expect("the shipped manifest opens the shipped derivative");

    // Canonical: re-encoding a decoded manifest reproduces the shipped bytes, so a re-push of
    // an unchanged library names the same content addresses.
    for (bytes, manifest) in bundle.derivative_manifests.iter().zip(&manifests) {
        assert_eq!(&cbor::to_canonical_vec(manifest).unwrap(), bytes);
    }
    let again = ws.upload_bundle(&asset_id).unwrap();
    assert_eq!(again.derivative_manifests, bundle.derivative_manifests);
}

/// A derivative that is skipped takes its manifest with it: nothing in
/// `derivative_manifests` names a blob the bundle does not carry.
#[test]
fn a_skipped_derivative_ships_no_manifest() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();

    let asset = ws.asset(&asset_id).expect("held");
    let dir = media_dir(lib.path(), asset.capture_utc).join("derivatives");
    fs::remove_file(dir.join(format!("{}.thumbnail.jxl", asset_id.simple()))).unwrap();

    let bundle = ws.upload_bundle(&asset_id).unwrap();
    let manifests = shipped_manifests(&bundle);
    assert_eq!(manifests.len(), bundle.derivatives.len());
    assert!(
        manifests.iter().all(|manifest| bundle
            .derivatives
            .iter()
            .any(|blob| blob.ciphertext_hash == manifest.core.ciphertext_hash)),
        "every shipped manifest names a shipped derivative"
    );
}

/// The `original` sentinel has no blob, and its manifest **still ships**: it is the only way a
/// receiver learns that a tier is the original rather than missing. Out-of-scope and rejected
/// manifests ship nothing.
#[test]
fn the_original_sentinel_ships_its_manifest_and_no_blob() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_album, asset_id) = library_with_a_thumbnailed_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();

    let sentinel = signed_derivative(
        asset_id,
        DerivativeRole::Preview,
        crate::derivative_format::DerivativeFormat::Original.mime(),
        hash::hash_bytes(b"the original"),
    );
    rewrite_bundle(
        &lib,
        &ws,
        asset_id,
        &[
            sentinel.clone(),
            signed_derivative(
                asset_id,
                DerivativeRole::Embedding,
                "embedding/mobileclip-b",
                hash::hash_bytes(b"an embedding"),
            ),
            signed_derivative(
                asset_id,
                DerivativeRole::Thumbnail,
                "image/future-codec",
                hash::hash_bytes(b"a future codec"),
            ),
        ],
    );

    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert!(
        bundle.derivatives.is_empty(),
        "no tier here has bytes to ship"
    );
    assert_eq!(
        shipped_manifests(&bundle),
        vec![sentinel],
        "the sentinel's manifest ships, and only it"
    );
}

// ─── The original as a bounded-memory source ─────────────────────────────────

/// A library whose one asset is `bytes`, returning the bundle and the plaintext's path.
fn bundle_of(lib: &TempDir, src: &TempDir, bytes: &[u8]) -> (UploadBundle, std::path::PathBuf) {
    let img = src.path().join("photo.jpg");
    fs::write(&img, bytes).unwrap();
    let mut ws = Workspace::create_with_params(lib.path(), b"passphrase", FAST).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let asset_id = ws.import_asset(album, &img).unwrap();
    let path = ws.media_path(ws.asset(&asset_id).unwrap());
    (ws.upload_bundle(&asset_id).unwrap(), path)
}

/// Plaintext spanning several STREAM chunks, every chunk distinct.
fn multi_chunk_plaintext() -> Vec<u8> {
    let mut bytes = b"\xFF\xD8\xFF".to_vec();
    bytes.extend((0..crate::crypto::encryption::PLAINTEXT_CHUNK * 3 + 77).map(|i| (i % 251) as u8));
    bytes
}

/// Any window sequence an upload might read — a fresh run, a resume mid-chunk, a re-send —
/// reassembles to exactly the ciphertext the signed manifest addresses.
#[test]
fn original_windows_reassemble_to_the_signed_ciphertext() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let plaintext = multi_chunk_plaintext();
    let (bundle, _) = bundle_of(&lib, &src, &plaintext);
    let original = &bundle.original;

    let whole = original.to_vec().unwrap();
    assert_eq!(whole.len() as u64, original.size());
    assert_eq!(original.size(), bundle.ciphertext_size());
    assert_eq!(hash::hash_bytes(&whole), bundle.ciphertext_hash);
    assert_eq!(
        original.write_to(std::io::sink()).unwrap(),
        bundle.ciphertext_hash
    );

    for window in [4096, 65_536, 256 * 1024, 1 << 20] {
        let mut offset = 0u64;
        let mut assembled = Vec::new();
        while offset < original.size() {
            let got = original.read_at(offset, window).unwrap();
            offset += got.len() as u64;
            assembled.extend_from_slice(&got);
        }
        assert_eq!(assembled, whole, "window {window}");
    }
    // A re-send from an arbitrary realigned offset is the same bytes again.
    assert_eq!(
        original.read_at(70_000, 1000).unwrap(),
        &whole[70_000..71_000]
    );

    // And the ciphertext opens to the plaintext on disk.
    let opened = crate::crypto::encryption::decrypt_asset_vec(
        &original.file_key,
        &original.nonce_prefix,
        &whole,
    )
    .unwrap();
    assert_eq!(opened, plaintext);
}

/// A plaintext changed after its manifest was signed is refused before anything is built, and
/// one changed after the bundle was built makes reads fail rather than yield bytes the manifest
/// never addressed.
#[test]
fn a_changed_original_is_never_shipped() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (bundle, path) = bundle_of(&lib, &src, &multi_chunk_plaintext());
    let asset_id = bundle.asset_id;

    // Shrink it under the built bundle: reads past the new end fail, `write_to` fails.
    let full = fs::read(&path).unwrap();
    fs::write(&path, &full[..1000]).unwrap();
    let err = bundle
        .original
        .read_at(bundle.original.size() - 10, 10)
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    assert!(bundle.original.write_to(std::io::sink()).is_err());

    // A same-size edit: the bundle is refused outright.
    let mut edited = full.clone();
    edited[500] ^= 0xFF;
    fs::write(&path, &edited).unwrap();
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    assert!(matches!(
        ws.upload_bundle(&asset_id),
        Err(LifecycleError::CiphertextMismatch(id)) if id == asset_id
    ));

    // A grown file is refused too, even though its first bytes still match.
    let mut grown = full;
    grown.extend_from_slice(b"appended");
    fs::write(&path, &grown).unwrap();
    assert!(matches!(
        ws.upload_bundle(&asset_id),
        Err(LifecycleError::CiphertextMismatch(_))
    ));
}

#[test]
fn original_debug_redacts_the_file_key() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (bundle, _) = bundle_of(&lib, &src, b"\xFF\xD8\xFF small");
    let shown = format!("{:?}", bundle.original);
    assert!(shown.contains("<redacted>"), "{shown}");
    let key_bytes = format!("{:?}", bundle.original.file_key);
    assert!(!shown.contains(&key_bytes), "{shown}");
}

/// The restore's destination: checks every plaintext byte is the sparse file's zero and counts
/// them, storing none.
struct ZeroSink(u64);

impl std::io::Write for ZeroSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        assert!(
            buf.iter().all(|b| *b == 0),
            "restored bytes differ from the original"
        );
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Push and restore an original of `size` plaintext bytes through the two seams every transfer
/// uses — [`OriginalCiphertext::read_at`] windows out, a
/// [`StreamDecryptor`](crate::crypto::encryption::StreamDecryptor) in — and return the peak
/// heap it took. The plaintext is a sparse file, so a multi-gigabyte original costs no disk.
fn push_and_restore_peak(size: u64, window: usize) -> usize {
    use std::io::Write as _;

    use crate::crypto::encryption::{NONCE_PREFIX_LEN, StreamDecryptor};
    use crate::crypto::hash::Sha256Hasher;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("original.bin");
    fs::File::create(&path).unwrap().set_len(size).unwrap();
    let original = OriginalCiphertext {
        path,
        file_key: [0x42; 32],
        nonce_prefix: [7; NONCE_PREFIX_LEN],
        plaintext_size: size,
    };

    let ((pushed_hash, restored), peak) = crate::test_heap::peak_during(|| {
        let mut sent = Sha256Hasher::new();
        let mut restore =
            StreamDecryptor::new(&original.file_key, &original.nonce_prefix, ZeroSink(0));
        let mut offset = 0u64;
        while offset < original.size() {
            // What the upload client does per chunk, and what a ranged fetch hands the
            // restore: one window, never the file.
            let chunk = original.read_at(offset, window).unwrap();
            sent.update(&chunk);
            restore.write_all(&chunk).unwrap();
            offset += chunk.len() as u64;
        }
        (sent.finalize(), restore.finish().unwrap().0)
    });

    assert_eq!(restored, size, "every plaintext byte came back");
    assert_eq!(
        original.write_to(std::io::sink()).unwrap(),
        pushed_hash,
        "the windows are the ciphertext the bundle verifies"
    );
    peak
}

/// The memory ceiling is a property of the transfer window, not the file: a 32 MiB original
/// pushed and restored in 1 MiB windows never holds more than a few windows.
#[test]
fn push_and_restore_hold_a_window_not_the_file() {
    const CEILING: usize = 4 << 20;
    let peak = push_and_restore_peak(32 << 20, 1 << 20);
    assert!(
        peak < CEILING,
        "peak heap {peak} B exceeds the {CEILING} B ceiling"
    );
}

/// The acceptance case: a 4 GiB original — the largest the server accepts — pushes and restores
/// within a fixed ceiling at the upload protocol's largest chunk (16 MiB). Ignored by default
/// because it encrypts and decrypts 4 GiB; run it with
/// `cargo test -p capsule-core --lib --release -- --ignored four_gib`.
#[test]
#[ignore = "encrypts and decrypts 4 GiB; run with --release -- --ignored"]
fn four_gib_original_pushes_and_restores_in_bounded_memory() {
    const CEILING: usize = 40 << 20;
    let peak = push_and_restore_peak(4 << 30, 16 << 20);
    assert!(
        peak < CEILING,
        "peak heap {peak} B exceeds the {CEILING} B ceiling"
    );
}

/// A fresh library with its default album ready to import into.
fn empty_library(lib: &TempDir) -> (Workspace, Uuid) {
    let mut ws = Workspace::create_with_params(lib.path(), b"passphrase", FAST).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    (ws, album)
}

/// Import a sparse `size`-byte original named `name` and return the peak heap the import alone
/// took, after checking that what it committed is the file. The source costs no disk; the
/// library copy the import writes does.
fn import_peak(name: &str, size: u64) -> usize {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let path = src.path().join(name);
    fs::File::create(&path).unwrap().set_len(size).unwrap();
    let (mut ws, album) = empty_library(&lib);
    let (asset_id, peak) = crate::test_heap::peak_during(|| ws.import_asset(album, &path).unwrap());

    // The signed size and hash, a library copy that reads back as them, and a ciphertext
    // re-derived from that copy at the manifest's address.
    let asset = ws.asset(&asset_id).unwrap();
    let head = &asset.chain.records().last().unwrap().manifest.core;
    assert_eq!(head.plaintext_size, size);
    let library_copy = ws.original_path(&asset_id).unwrap();
    let mut copied = crate::crypto::hash::Sha256Hasher::new();
    let mut reader = fs::File::open(&library_copy).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut copied_len = 0u64;
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        copied.update(&buf[..n]);
        copied_len += n as u64;
    }
    assert_eq!(copied_len, size);
    assert_eq!(
        copied.finalize(),
        asset.sidecar.hash,
        "the library copy is the signed original"
    );
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert_eq!(
        bundle.original.write_to(std::io::sink()).unwrap(),
        head.ciphertext_hash,
        "the manifest addresses the ciphertext the library copy re-derives"
    );
    peak
}

/// The import's memory ceiling is a property of the STREAM chunk, not the file: a 32 MiB
/// original is hashed, encrypted, copied and signed without being held.
#[test]
fn import_holds_a_chunk_not_the_file() {
    const CEILING: usize = 4 << 20;
    let peak = import_peak("clip.mp4", 32 << 20);
    assert!(
        peak < CEILING,
        "peak heap {peak} B exceeds the {CEILING} B ceiling"
    );
}

/// The acceptance case: a 4 GiB original — the largest the server accepts — imports within the
/// same fixed ceiling. Ignored by default because it encrypts 4 GiB and writes a 4 GiB library
/// copy; run it with `cargo test -p capsule-core --lib --release -- --ignored four_gib`.
#[test]
#[ignore = "encrypts 4 GiB and writes a 4 GiB library copy; run with --release -- --ignored"]
fn four_gib_original_imports_in_bounded_memory() {
    const CEILING: usize = 4 << 20;
    let peak = import_peak("clip.mp4", 4 << 30);
    assert!(
        peak < CEILING,
        "peak heap {peak} B exceeds the {CEILING} B ceiling"
    );
}

/// Every `.tmp` file under `dir`.
fn tmp_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(tmp_files(&path));
        } else if path.extension().is_some_and(|e| e == "tmp") {
            found.push(path);
        }
    }
    found
}

/// Streaming changes how the original is read, never what is signed: the manifest's content
/// fields and the sidecar's `hash` are exactly what the buffered writer produces for the same
/// bytes under the prefix the import drew — for a multi-chunk file that is no still (streamed
/// from disk) and for one that is a still by its header (streamed from the decoder's buffer).
#[test]
fn a_streamed_import_signs_what_the_buffered_writer_would() {
    use crate::crypto::encryption::encrypt_asset_rekey_with_prefix;
    use crate::crypto::keys::Amk;

    let mut still = b"\xFF\xD8\xFF a still by its header, not a decodable frame".to_vec();
    still.resize(70_000, 0xA5);
    let clip: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    for (name, bytes) in [("photo.jpg", still), ("clip.mp4", clip)] {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let path = src.path().join(name);
        fs::write(&path, &bytes).unwrap();
        let (mut ws, album) = empty_library(&lib);
        let asset_id = ws.import_asset(album, &path).unwrap();

        let asset = ws.asset(&asset_id).unwrap();
        let head = &asset.chain.records().last().unwrap().manifest.core;
        let amk = Amk::from_bytes(ws.album(&album).unwrap().amks[&head.amk_version.0]);
        let (buffered, _, _) =
            encrypt_asset_rekey_with_prefix(&amk, &asset_id, &bytes, head.nonce_prefix, None)
                .unwrap();
        assert_eq!(head.ciphertext_hash, buffered.ciphertext_hash, "{name}");
        assert_eq!(head.plaintext_size, buffered.plaintext_size, "{name}");
        assert_eq!(head.chunk_size, buffered.chunk_size, "{name}");
        assert_eq!(
            asset.sidecar.hash,
            crate::crypto::hash::hash_bytes(&bytes),
            "{name}"
        );
        assert_eq!(
            fs::read(ws.original_path(&asset_id).unwrap()).unwrap(),
            bytes,
            "{name}"
        );
        let leftovers = tmp_files(lib.path());
        assert!(
            leftovers.is_empty(),
            "{name}: staged copies left behind: {leftovers:?}"
        );
    }
}
