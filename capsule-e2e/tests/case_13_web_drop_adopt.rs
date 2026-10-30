//! **E2E case 13** — web drop → adopt, the server leg.
//!
//! The provisioning user's library issues an upload link; the link is provisioned on the
//! server; a guest with no account and **no protocol handshake** seals a drop to the link's
//! Drop Key and deposits it through the two exempt guest operations; the owner's inbox shows
//! it; the owner's library decapsulates, rewraps the key under the album AMK and adopts it in
//! place, registering it as a library asset; the server adopts the same manifest and holds the
//! drop's bytes as the asset's durable original. The library then holds everything a push
//! publishes — the bundle regenerates the guest's ciphertext and carries the metadata blob and
//! provenance record adoption returned. The feed leg pushes that index tier and is ignored on
//! #467, recorded above it.
//!
//! The browser half of the seal is the cross-language KAT (`capsule-core/tests/drop_adopt_kat.rs`
//! and `capsule-web`'s `drop-seal.test.ts`); here the seal runs natively. Verification on a
//! second device waits on key transfer (cases 6 and 12).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::primitives::CRYPTO_SUITE_ID;
use capsule_core::crypto::provenance::KeyMode;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::drop::{
    AdoptedDrop, DropAdopter as _, LinkCaps, SealedDrop, UploadLinkIssuer as _, seal_drop,
};
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::push::{push_asset, wire_envelope};
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

/// What the server leg left behind: the owner, what the library's adoption returned, and the
/// guest's sealed drop and plaintext to compare against.
struct AdoptedInPlace {
    owner: Device,
    adopted: AdoptedDrop,
    sealed: SealedDrop,
    plaintext: Vec<u8>,
}

/// The server leg: issue and provision a link, deposit a guest drop with no handshake, adopt
/// it in the library and on the server, and confirm the server holds the drop's bytes as the
/// asset's durable original.
async fn adopt_in_place(server: &Server) -> AdoptedInPlace {
    let mut owner = Device::register(server, "owner").await;
    let album = owner.workspace.default_album_id();
    let generated = owner.generated(server);

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

    // The owner's inbox shows the drop, and the bytes are filed at their content address. The
    // completing chunk is answered as soon as the deposit starts, never once it ends, so the
    // inbox is read until the detached deposit has filed it.
    let inbox = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let inbox = generated
                .list_inbox(PROTOCOL_VERSION, None)
                .await
                .expect("the inbox answers")
                .into_inner();
            if !inbox.drops.is_empty() {
                return inbox;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the detached deposit files the drop");
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
    let adopted = owner
        .workspace
        .adopt(drop_id, album)
        .expect("the library adopts the drop");
    let core = &adopted.manifest.core;
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
    let promoted = generated
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
    assert_eq!(promoted.asset_id, asset.to_string());
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

    AdoptedInPlace {
        owner,
        adopted,
        sealed,
        plaintext,
    }
}

#[tokio::test]
async fn e2e_case_13_a_guest_drop_is_deposited_without_a_handshake_and_adopted_in_place() {
    let server = Server::boot().await;
    let AdoptedInPlace {
        owner,
        adopted,
        sealed,
        plaintext,
    } = adopt_in_place(&server).await;
    let asset = adopted.asset_id();

    // The adopted drop is a library asset: its plaintext is held, it re-verifies through the
    // wrapped key, and its chain is the one `create` record adoption handed back.
    assert_eq!(
        owner
            .workspace
            .read_plaintext(&asset)
            .expect("the adopted asset is in the library"),
        plaintext
    );
    assert_eq!(
        owner.workspace.verify(&asset).expect("the asset verifies"),
        VerifyOutcome::Accept
    );
    assert_eq!(
        owner
            .workspace
            .asset(&asset)
            .expect("registered")
            .chain
            .records(),
        std::slice::from_ref(&adopted.record)
    );

    // And the library can publish it: the push bundle regenerates the guest's ciphertext byte
    // for byte, and its index tier is exactly the metadata blob and provenance record the
    // adoption returned — what the feed waits for (`upload/visibility.rs`).
    let bundle = owner
        .workspace
        .upload_bundle(&asset)
        .expect("the library builds an upload bundle for the adopted asset");
    assert_eq!(bundle.original.to_vec().unwrap(), sealed.ciphertext);
    assert_eq!(bundle.key_mode, KeyMode::Wrapped);
    assert_eq!(bundle.metadata_blob, adopted.metadata_blob);
    assert_eq!(
        bundle.provenance_blob,
        capsule_core::cbor::to_canonical_vec(&adopted.record).expect("the record encodes")
    );
}

// Blocked by #467, like every other case that pushes through the SDK ladder: the adopting
// manifest's `created_by_user` is the library's locally minted account id, and #405 refuses an
// upload whose `created_by_user` is not the authenticated caller. The adoption itself is
// accepted (the adopt route checks the device, not the account), so the server leg above runs;
// publishing the index tier is the upload that cannot. Un-ignore when #467 lands its
// constructor. The second-device verify additionally waits on key transfer (cases 6 and 12).
#[tokio::test]
#[ignore = "#467: a Workspace has no seam to adopt the server's account id, so the \
           real SDK push path cannot satisfy #405's created_by_user == caller rule"]
async fn e2e_case_13_an_adopted_drop_is_published_once_its_index_tier_is_pushed() {
    let server = Server::boot().await;
    let AdoptedInPlace { owner, adopted, .. } = adopt_in_place(&server).await;
    let asset = adopted.asset_id();

    let pushed = push_asset(&owner, &server, &asset).await;
    assert_eq!(pushed.bundle.metadata_blob, adopted.metadata_blob);

    let feed = owner.feed(&server).await;
    let entry = entry_for(&feed, &asset).expect("the adopted asset is on the feed");
    assert!(entry.original_held, "the adopted original is held");
    assert_eq!(
        entry.metadata_blob,
        adopted
            .manifest
            .core
            .metadata_blob_hash
            .expect("the adopting create binds a metadata blob")
            .to_hex()
            .into_bytes()
    );
}
