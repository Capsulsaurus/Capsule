//! **E2E case 13** — web drop → adopt, the server leg.
//!
//! The provisioning user's library issues an upload link; the link is provisioned on the
//! server; a guest with no account and **no protocol handshake** seals a drop to the link's
//! Drop Key and deposits it through the two exempt guest operations; the owner's inbox shows
//! it; the owner's library decapsulates, rewraps the key under the album AMK and adopts it in
//! place; the server adopts the same manifest and holds the drop's bytes as the asset's
//! durable original. The feed leg waits on a library seam recorded at the end of the test.
//!
//! The browser half of the seal is the cross-language KAT (`capsule-core/tests/drop_adopt_kat.rs`
//! and `capsule-web`'s `drop-seal.test.ts`); here the seal runs natively. Verification on a
//! second device waits on key transfer (cases 6 and 12).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::primitives::CRYPTO_SUITE_ID;
use capsule_core::crypto::provenance::manifest::KeyMode;
use capsule_core::drop::{DropAdopter as _, LinkCaps, UploadLinkIssuer as _, seal_drop};
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::push::wire_envelope;
use capsule_e2e::{Device, PROTOCOL_VERSION, Server, entry_for};
use capsule_sdk::rest;
use capsule_sdk::rest::types::{AdoptRequest, CreateDropRequest, ProvisionLinkRequest};
use capsule_sdk::verify::{AssetQuery, StorageVerifyClient, VerifyTransport};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[tokio::test]
async fn e2e_case_13_a_guest_drop_is_deposited_without_a_handshake_and_adopted_in_place() {
    let server = Server::boot().await;
    let mut owner = Device::register(&server, "owner").await;
    let album = owner.workspace.default_album_id();
    let generated = owner.generated(&server);

    // The owner's library issues the link; the server learns its opaque id and Drop Key.
    let link = owner
        .workspace
        .create_link(LinkCaps::default(), None)
        .expect("the library issues an upload link");
    let opaque_id = hex(&link.opaque_id);
    let provisioned = generated
        .provision_link(
            PROTOCOL_VERSION,
            None,
            &ProvisionLinkRequest {
                opaque_id: opaque_id.clone(),
                drop_pubkey: BASE64.encode(&link.drop_pubkey),
                crypto_suite_id: i64::from(CRYPTO_SUITE_ID),
                expires_at: None,
                max_total_bytes: None,
                max_file_count: None,
                max_file_size: None,
                single_use: Some(false),
                passphrase_verifier: None,
            },
        )
        .await
        .expect("the link provisions")
        .into_inner();
    assert_eq!(provisioned.opaque_id, opaque_id);

    // The guest: a bare generated client — no session, no default headers, no handshake.
    let guest = rest::Client::new(server.base_url()).expect("the API root parses");
    let plaintext = synthetic_jpeg();
    let sealed = seal_drop(&plaintext, &link.drop_pubkey, "image/jpeg").expect("the drop seals");
    let ciphertext_hash = sealed.descriptor.ciphertext_hash.to_hex();
    let created = guest
        .create_drop(
            &opaque_id,
            &CreateDropRequest {
                content_type: "image/jpeg".to_owned(),
                size: sealed.ciphertext.len() as i64,
                ciphertext_hash: ciphertext_hash.clone(),
                kem_ct: BASE64.encode(&sealed.descriptor.kem_ct),
                passphrase_proof: None,
                suggested_filename: Some("drop.jpg".to_owned()),
            },
        )
        .await
        .expect("the exempt guest operation admits a client with no handshake")
        .into_inner();
    guest
        .append_drop_chunk(
            &opaque_id,
            &created.upload_id,
            rest::AppendDropChunkParams {
                x_capsule_offset: Some("0".to_owned()),
                x_capsule_checksum: Some(hash_bytes(&sealed.ciphertext).to_hex()),
            },
            &rest::types::RequestBody4e14fb73::from(sealed.ciphertext.clone()),
        )
        .await
        .expect("the exempt chunk operation admits the bytes");

    // The owner's inbox shows the drop, and the bytes are filed at their content address.
    let inbox = generated
        .list_inbox(PROTOCOL_VERSION, None)
        .await
        .expect("the inbox answers")
        .into_inner();
    assert_eq!(inbox.drops.len(), 1);
    let pending = &inbox.drops[0];
    assert_eq!(pending.opaque_id, opaque_id);
    assert_eq!(pending.ciphertext_hash, ciphertext_hash);
    assert_eq!(pending.size, sealed.ciphertext.len() as i64);
    assert!(!pending.adopting);
    assert_eq!(
        std::fs::read(server.blob_path(&ciphertext_hash)).expect("the drop is filed"),
        sealed.ciphertext
    );

    // The owner's library decapsulates and adopts in place: a wrapped-key create manifest.
    let drop_id = owner
        .workspace
        .receive_drop(link.link_id, sealed.clone())
        .expect("the library receives the drop");
    let manifest = owner
        .workspace
        .adopt(drop_id, album)
        .expect("the library adopts the drop");
    let core = &manifest.core;
    assert_eq!(core.ciphertext_hash.to_hex(), ciphertext_hash);
    assert_eq!(core.key_mode, KeyMode::Wrapped);
    assert!(
        core.wrapped_file_key.is_some(),
        "the guest's key is rewrapped under the AMK"
    );
    let asset = core.file_id;
    assert_eq!(core.created_by_device, owner.workspace.device_id());
    assert_eq!(core.plaintext_size, plaintext.len() as u64);

    // The server adopts the same manifest: the staged bytes become the asset's original.
    let adopted = generated
        .adopt_drop(
            &pending.drop_id,
            PROTOCOL_VERSION,
            None,
            &AdoptRequest {
                album_id: album.to_string(),
                asset_id: asset.to_string(),
                size: sealed.ciphertext.len() as i64,
                hash: ciphertext_hash.clone(),
                content_type: "image/jpeg".to_owned(),
                crypto_suite_id: i64::from(CRYPTO_SUITE_ID),
                protocol_version: core.protocol_version.clone(),
                key_mode: "wrapped".to_owned(),
                manifest_envelope: wire_envelope(core, &ciphertext_hash),
            },
        )
        .await
        .expect("the server adopts the drop")
        .into_inner();
    assert_eq!(adopted.asset_id, asset.to_string());
    assert!(
        generated
            .list_inbox(PROTOCOL_VERSION, None)
            .await
            .expect("the inbox answers")
            .into_inner()
            .drops
            .is_empty(),
        "the adopted drop left the inbox"
    );

    // The server holds the drop's bytes as the asset's original: stored, indexed, retrievable.
    let verify = StorageVerifyClient::new(VerifyTransport::with_session(
        owner.session.clone(),
        server.v1(),
    ));
    let verdicts = verify
        .verify(
            &[AssetQuery {
                asset_id: asset,
                blob_hashes: vec![core.ciphertext_hash],
            }],
            false,
        )
        .await
        .expect("the verify surface answers");
    assert_eq!(verdicts.len(), 1);
    assert!(
        verdicts[0].durable,
        "the adopted original is durable: {:?}",
        verdicts[0]
    );

    // Where the server leg stops: the feed publishes an asset only once it holds the index
    // tier — the sealed metadata blob and the provenance record — and the library's adopt
    // returns the signed manifest without registering the asset or handing back the metadata
    // blob it sealed, so the owner has nothing to publish — issue #469, which the feed leg
    // and the second-device verify wait on.
    let feed = owner.feed(&server).await;
    assert!(
        entry_for(&feed, &asset).is_none(),
        "an adopted asset with no index tier is not yet published"
    );
}
