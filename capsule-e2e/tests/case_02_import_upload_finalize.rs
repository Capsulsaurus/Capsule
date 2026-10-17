//! **E2E case 2** — full import + upload + finalize.
//!
//! Local import → the library's upload bundle → the SDK's staged ladder (the index tier's
//! provenance and metadata blobs, then the JXL thumbnail, then the original) → every blob
//! finalized at its content address under the server's blob root, byte for byte → the server's
//! storage-verify answer is durable → the asset is on the feed with its original held, its
//! derivative referenced and its metadata blob named.
//!
//! The still is 512×512 — past the thumbnail tier's 256-pixel cap — so the media stack decodes
//! it and encodes a real thumbnail, and T1 is a real upload rather than the byte-free sentinel
//! an 8×8 still gets.

use capsule_core::crypto::hash::Hash32;
use capsule_core::import::UploadTier;
use capsule_e2e::fixtures::large_synthetic_jpeg;
use capsule_e2e::push::push_asset;
use capsule_e2e::{Device, Server, entry_for};
use capsule_sdk::verify::{AssetQuery, StorageVerifyClient, VerifyTransport};

#[tokio::test]
async fn e2e_case_2_import_upload_finalize_lands_every_blob_at_its_content_address() {
    let server = Server::boot().await;
    let mut device = Device::register(&server, "importer").await;
    let asset = device.import_file("photo.jpg", &large_synthetic_jpeg());

    let pushed = push_asset(&device, &server, &asset).await;
    let bundle = &pushed.bundle;
    assert_eq!(bundle.asset_id, asset);
    assert!(
        !bundle.derivatives.is_empty(),
        "a still past the thumbnail cap yields derivative bytes"
    );
    assert!(
        bundle.derivatives.iter().all(|d| d.format == "image/jxl"),
        "this build encodes thumbnails as JXL: {:?}",
        bundle
            .derivatives
            .iter()
            .map(|d| &d.format)
            .collect::<Vec<_>>()
    );

    // The ladder ran every tier: two T0 rungs (provenance, then metadata), one T1 per
    // derivative, then T2.
    let mut expected = vec![UploadTier::Index, UploadTier::Index];
    expected.extend(bundle.derivatives.iter().map(|_| UploadTier::Preview));
    expected.push(UploadTier::Original);
    assert_eq!(pushed.report.tier_sequence(), expected);
    assert_eq!(pushed.report.deferred, 0);

    // Every blob is on disk at its content address under the blob root, byte for byte.
    let on_disk = |hex: &str| std::fs::read(server.blob_path(hex)).expect("the blob is filed");
    assert_eq!(on_disk(&bundle.ciphertext_hash.to_hex()), bundle.ciphertext);
    let metadata_hash = bundle
        .metadata_blob_hash
        .expect("a create binds a metadata blob")
        .to_hex();
    assert_eq!(on_disk(&metadata_hash), bundle.metadata_blob);
    for derivative in &bundle.derivatives {
        assert_eq!(
            on_disk(&derivative.ciphertext_hash.to_hex()),
            derivative.bytes
        );
    }
    assert_eq!(on_disk(&pushed.provenance_hash), bundle.provenance_blob);

    // The server's own custody answer for the whole set is durable.
    let mut hashes = vec![
        bundle.ciphertext_hash,
        Hash32::from_hex(&metadata_hash).expect("a digest"),
        Hash32::from_hex(&pushed.provenance_hash).expect("a digest"),
    ];
    hashes.extend(bundle.derivatives.iter().map(|d| d.ciphertext_hash));
    let verify = StorageVerifyClient::new(VerifyTransport::with_session(
        device.session.clone(),
        server.v1(),
    ));
    let verdicts = verify
        .verify(
            &[AssetQuery {
                asset_id: asset,
                blob_hashes: hashes.clone(),
            }],
            false,
        )
        .await
        .expect("the verify surface answers");
    assert_eq!(verdicts.len(), 1);
    let verdict = &verdicts[0];
    assert_eq!(verdict.asset_id, asset);
    assert!(
        verdict.durable,
        "every named blob is stored and indexed: {verdict:?}"
    );
    assert_eq!(verdict.blobs.len(), hashes.len());

    // The feed publishes the asset: original held, derivative referenced, metadata named.
    let feed = device.feed(&server).await;
    let entry = entry_for(&feed, &asset).expect("the asset is on the feed");
    assert!(entry.original_held, "the original finalized");
    assert_eq!(
        entry
            .blobs
            .original
            .as_ref()
            .map(|b| b.ciphertext_hash.as_str()),
        Some(bundle.ciphertext_hash.to_hex().as_str())
    );
    let derivative_hashes: Vec<&str> = entry
        .blobs
        .derivatives
        .iter()
        .filter(|b| b.role == "derivative")
        .map(|b| b.ciphertext_hash.as_str())
        .collect();
    for derivative in &bundle.derivatives {
        assert!(
            derivative_hashes.contains(&derivative.ciphertext_hash.to_hex().as_str()),
            "the {} derivative rides the feed: {derivative_hashes:?}",
            derivative.format
        );
    }
    assert_eq!(
        String::from_utf8(entry.metadata_blob.clone()).expect("a hex content address"),
        metadata_hash,
        "the feed names the metadata blob by its content address"
    );
    assert_eq!(
        entry.manifest_cbor, bundle.provenance_blob,
        "the feed serves the provenance blob's bytes unchanged"
    );
}
