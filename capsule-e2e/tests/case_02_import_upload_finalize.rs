//! **E2E case 2** — full import + upload + finalize.
//!
//! Local import → the library's upload bundle → the SDK's staged ladder plus the provenance
//! rung → every blob finalized at its content address under the server's blob root, byte for
//! byte → the server's storage-verify answer is durable → the asset is on the feed with its
//! original held and its metadata blob named.
//!
//! The still is the 8×8 fixture, which sits inside the thumbnail tier's cap: the media stack
//! signs the byte-free `original` sentinel for it, so T1 has nothing to upload and the ladder
//! is T0 then T2. A still past the cap gets a real JXL thumbnail, and that upload is refused by
//! the server's closed content-type set, which does not name `image/jxl` — issue #470;
//! `fixtures::large_synthetic_jpeg` is the still that reproduces it.

use capsule_core::crypto::hash::Hash32;
use capsule_core::import::UploadTier;
use capsule_e2e::push::{provenance_bytes, push_asset};
use capsule_e2e::{Device, Server, entry_for};
use capsule_sdk::verify::{AssetQuery, StorageVerifyClient, VerifyTransport};

#[tokio::test]
async fn e2e_case_2_import_upload_finalize_lands_every_blob_at_its_content_address() {
    let server = Server::boot().await;
    let mut device = Device::register(&server, "importer").await;
    let asset = device.import_jpeg("photo.jpg");

    let pushed = push_asset(&device, &server, &asset).await;
    let bundle = &pushed.bundle;
    assert_eq!(bundle.asset_id, asset);
    assert!(
        bundle.derivatives.is_empty(),
        "an 8×8 still gets the byte-free sentinel, not derivative bytes: {:?}",
        bundle
            .derivatives
            .iter()
            .map(|d| &d.format)
            .collect::<Vec<_>>()
    );

    // The ladder ran T0 and T2; the sentinel left T1 nothing to open a session for.
    assert_eq!(
        pushed.report.tier_sequence(),
        vec![UploadTier::Index, UploadTier::Original]
    );
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
    assert_eq!(
        on_disk(&pushed.provenance_hash),
        provenance_bytes(&device, &asset)
    );

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
        entry.manifest_cbor,
        provenance_bytes(&device, &asset),
        "the feed serves the provenance blob's bytes unchanged"
    );
}
