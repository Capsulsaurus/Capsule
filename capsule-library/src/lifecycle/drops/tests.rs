use jiff::Timestamp;
use tempfile::TempDir;

use super::super::fast_workspace;
use super::*;

/// The full guest-drop path through the `Workspace` — issue an upload link, seal a
/// drop to its Drop Key (the WASM/browser half), stage it, and adopt it into an album.
/// The adopting `create` manifest is `key_mode = wrapped`, self-verifies, and a second
/// member (holding the same AMK) unwraps `wrapped_file_key` and decrypts the unchanged
/// ciphertext. Also covers revocation, the passphrase verifier, and discard.
#[test]
fn upload_link_seal_stage_and_adopt() {
    use capsule_core::crypto::encryption::unseal_file_key;
    use capsule_core::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Guest contributions").unwrap();

    // Provision an upload link with a passphrase abuse gate.
    let caps = LinkCaps {
        max_file_count: Some(10),
        single_use: false,
        ..Default::default()
    };
    let link = UploadLinkIssuer::create_link(&mut ws, caps, Some("open sesame")).unwrap();
    assert_eq!(
        link.drop_pubkey.len(),
        capsule_core::crypto::keys::DEK_PUBLIC_LEN
    );
    assert_eq!(
        ws.upload_link_opaque_id(&link.link_id).unwrap(),
        link.opaque_id
    );
    assert!(ws.upload_link_created_at(&link.link_id).is_some());
    assert!(
        ws.upload_link_passphrase(&link.link_id)
            .unwrap()
            .verify("open sesame")
    );
    assert!(ws.upload_link_is_live(&link.link_id, Timestamp::now()));

    // A guest seals an asset to the link's Drop Key public half (no account, no album key).
    let plaintext = b"\xFF\xD8\xFF a guest-uploaded photo, sealed in the browser";
    let sealed = seal_drop(plaintext, &link.drop_pubkey, "image/jpeg").unwrap();
    let ciphertext = sealed.ciphertext.clone();

    // The server stages it into the owner's inbox; the owner sees it awaiting review.
    let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
    assert_eq!(DropAdopter::list_inbox(&ws).unwrap().len(), 1);

    // The owner adopts it into the album on their trusted device.
    let adopted = DropAdopter::adopt(&mut ws, drop_id, album).unwrap();
    let manifest = &adopted.manifest;
    assert_eq!(manifest.core.key_mode, KeyMode::Wrapped);
    assert!(manifest.core.wrapped_file_key.is_some());
    assert_eq!(manifest.core.album_id, album);
    // The bytes stayed put: the manifest references the drop's ciphertext hash unchanged.
    assert_eq!(manifest.core.ciphertext_hash, hash::hash_bytes(&ciphertext));
    // Adoption emptied the inbox.
    assert!(DropAdopter::list_inbox(&ws).unwrap().is_empty());

    // The drop is now a library asset, registered as an import registers one: its
    // plaintext is readable, it re-verifies from that plaintext through the wrapped key,
    // its chain is the one `create` record handed back, and it has an index row.
    let asset_id = adopted.asset_id();
    assert_eq!(asset_id, manifest.core.file_id);
    assert!(ws.asset_ids().contains(&asset_id));
    assert_eq!(ws.read_plaintext(&asset_id).unwrap(), plaintext);
    assert_eq!(ws.verify(&asset_id).unwrap(), VerifyOutcome::Accept);
    assert_eq!(adopted.record.asset_id, asset_id);
    assert_eq!(adopted.record.manifest, *manifest);
    assert_eq!(adopted.record.prior_provenance_hash, None);
    let state = &ws.assets[&asset_id];
    assert_eq!(state.chain.records(), std::slice::from_ref(&adopted.record));
    assert_eq!(state.metadata_blob, adopted.metadata_blob);
    assert_eq!(
        state.ext, "bin",
        "seal_drop names no file, so the fallback applies"
    );
    assert!(
        ws.db()
            .find_by_uuid(&asset_id.to_string())
            .unwrap()
            .is_some(),
        "the adopted asset has a queryable index row"
    );
    // The handed-back metadata blob is the one the manifest commits to.
    assert_eq!(
        manifest.core.metadata_blob_hash,
        Some(blob_ciphertext_hash(&adopted.metadata_blob))
    );
    // A push bundle builds for it: the regenerated ciphertext is the guest's, byte for byte,
    // and the index tier is the blob and record adoption returned.
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert_eq!(bundle.original.to_vec().unwrap(), ciphertext);
    assert_eq!(bundle.key_mode, KeyMode::Wrapped);
    assert_eq!(bundle.metadata_blob, adopted.metadata_blob);
    assert_eq!(
        bundle.provenance_blob,
        capsule_core::cbor::to_canonical_vec(&adopted.record).unwrap()
    );
    // Adopting the same drop again is NotFound: the inbox row is gone.
    assert!(matches!(
        DropAdopter::adopt(&mut ws, drop_id, album),
        Err(DropError::NotFound)
    ));

    // A second album member unwraps K under the AMK and decrypts the unchanged ciphertext.
    let epoch = manifest.core.amk_version.0;
    let amk = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch]);
    let k = unseal_file_key(
        &amk,
        &manifest.core.file_id,
        &manifest.core.wrapped_file_key.as_ref().unwrap().0,
    )
    .unwrap();
    assert_eq!(
        decrypt_asset_vec(&k, &manifest.core.nonce_prefix, &ciphertext).unwrap(),
        plaintext,
    );

    // Revocation makes the link non-live and refuses further drops.
    UploadLinkIssuer::revoke_link(&mut ws, link.link_id).unwrap();
    assert!(!ws.upload_link_is_live(&link.link_id, Timestamp::now()));
    let sealed2 = seal_drop(b"late drop", &link.drop_pubkey, "image/jpeg").unwrap();
    assert!(matches!(
        ws.receive_drop(link.link_id, sealed2),
        Err(DropError::LinkRefused(_))
    ));
    // Revoking an unknown link is NotFound; re-revocation is idempotent.
    assert!(matches!(
        UploadLinkIssuer::revoke_link(&mut ws, UploadLinkId(Uuid::now_v7())),
        Err(DropError::NotFound)
    ));
    UploadLinkIssuer::revoke_link(&mut ws, link.link_id).unwrap();
}

/// An adopted drop is durable library state, not an in-memory record: it survives a
/// close and reopen with its chain, its sealed metadata blob, and a `verify` that
/// regenerates the guest's ciphertext through the wrapped key — and a later metadata edit
/// chains onto the adopting `create` like any other asset's.
#[test]
fn an_adopted_drop_survives_reopen_and_takes_edits() {
    use capsule_core::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

    let lib = TempDir::new().unwrap();
    let plaintext = b"\xFF\xD8\xFF a guest photo that outlives the session";
    let (asset_id, adopted) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Guests").unwrap();
        let link = UploadLinkIssuer::create_link(&mut ws, LinkCaps::default(), None).unwrap();
        let mut sealed = seal_drop(plaintext, &link.drop_pubkey, "image/jpeg").unwrap();
        sealed.descriptor.suggested_filename = Some("Beach.JPG".to_string());
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
        let adopted = DropAdopter::adopt(&mut ws, drop_id, album).unwrap();
        assert_eq!(ws.assets[&adopted.asset_id()].ext, "jpg");
        (adopted.asset_id(), adopted)
    };

    let mut ws2 = Workspace::open(lib.path(), b"passphrase", super::super::FAST_PARAMS).unwrap();
    let restored = ws2
        .asset(&asset_id)
        .expect("the adopted asset is restored from disk");
    assert_eq!(
        restored.chain.records(),
        std::slice::from_ref(&adopted.record)
    );
    assert_eq!(restored.metadata_blob, adopted.metadata_blob);
    assert_eq!(ws2.read_plaintext(&asset_id).unwrap(), plaintext);
    assert_eq!(ws2.verify(&asset_id).unwrap(), VerifyOutcome::Accept);

    ws2.tag_add(&asset_id, "guest").unwrap();
    let chain = ws2.asset(&asset_id).unwrap().chain.records();
    assert_eq!(chain.len(), 2);
    assert_eq!(
        chain[1].manifest.core.prior_provenance_hash,
        Some(adopted.record.record_hash())
    );
    assert_eq!(ws2.verify(&asset_id).unwrap(), VerifyOutcome::Accept);
}

/// A tampered head on an adopted asset — a `wrapped_file_key` that no longer opens, or a
/// `wrapped` head stripped of its key — is a `verify` reject, not an error: the unverified
/// key fields never short-circuit `verify_asset` (web-upload.md, "Forged
/// `wrapped_file_key`").
#[test]
fn a_tampered_wrapped_file_key_is_a_verify_reject() {
    use capsule_core::crypto::verify_asset::RejectReason;
    use capsule_core::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Guests").unwrap();
    let link = UploadLinkIssuer::create_link(&mut ws, LinkCaps::default(), None).unwrap();
    let sealed = seal_drop(b"a guest photo", &link.drop_pubkey, "image/jpeg").unwrap();
    let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
    let adopted = DropAdopter::adopt(&mut ws, drop_id, album).unwrap();
    let asset_id = adopted.asset_id();
    assert_eq!(ws.verify(&asset_id).unwrap(), VerifyOutcome::Accept);

    let flipped = |m: &mut capsule_core::crypto::provenance::AssetManifest| {
        let key = m.core.wrapped_file_key.as_mut().unwrap();
        let last = key.0.len() - 1;
        key.0[last] ^= 0x01;
    };
    let stripped = |m: &mut capsule_core::crypto::provenance::AssetManifest| {
        m.core.wrapped_file_key = None;
    };
    // A key that does not open leaves nothing to regenerate, so the content check rejects;
    // a `wrapped` head without a key breaks the structural key-mode coupling first.
    type Tamper<'a> = &'a dyn Fn(&mut capsule_core::crypto::provenance::AssetManifest);
    let tampers: [(Tamper<'_>, _); 2] = [
        (&flipped, RejectReason::CiphertextHashMismatch),
        (&stripped, RejectReason::Structural),
    ];
    for (tamper, reason) in tampers {
        let mut record = adopted.record.clone();
        tamper(&mut record.manifest);
        let mut chain = ProvenanceChain::new();
        chain.append(record).unwrap();
        ws.assets.get_mut(&asset_id).unwrap().chain = chain;
        assert_eq!(
            ws.verify(&asset_id).unwrap(),
            VerifyOutcome::TerminalReject(reason)
        );
    }
}

/// The guest-named extension is admitted only as a short ASCII-alphanumeric suffix.
#[test]
fn adopted_extension_admits_only_a_short_alphanumeric_suffix() {
    assert_eq!(adopted_extension(Some("IMG_0001.JPG")), "jpg");
    assert_eq!(adopted_extension(Some("clip.tar.mp4")), "mp4");
    assert_eq!(adopted_extension(None), "bin");
    assert_eq!(adopted_extension(Some("no-extension")), "bin");
    assert_eq!(adopted_extension(Some("trailing.")), "bin");
    assert_eq!(adopted_extension(Some("evil.jpg/../x")), "bin");
    assert_eq!(adopted_extension(Some("a.j-g")), "bin");
    assert_eq!(adopted_extension(Some("a.abcdefghijk")), "bin");
    assert_eq!(adopted_extension(Some("a.ünï")), "bin");
}

/// A drop whose staged bytes decrypt but are not the canonical re-encryption of their
/// plaintext — here, a chunk-multiple plaintext sealed with a trailing empty last chunk,
/// which STREAM decryption accepts — is refused before anything is registered, and the
/// drop stays in the inbox; so is a drop whose descriptor misstates `plaintext_size`.
#[test]
fn a_non_canonical_drop_is_refused_and_stays_in_the_inbox() {
    use aes_gcm::aead::KeyInit;
    use aes_gcm::aead::generic_array::GenericArray;
    use aes_gcm::aead::stream::EncryptorBE32;
    use aes_gcm::{Aes256Gcm, Key};
    use capsule_core::crypto::encryption::PLAINTEXT_CHUNK;
    use capsule_core::crypto::keys::ESEED_LEN;
    use capsule_core::drop::{DropAdopter, UploadLinkIssuer, seal_drop_derand};

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Guests").unwrap();
    let link = UploadLinkIssuer::create_link(&mut ws, LinkCaps::default(), None).unwrap();
    let k = [0x42; 32];
    let nonce_prefix = [0x07; 7];
    let seal = |plaintext: &[u8]| {
        seal_drop_derand(
            plaintext,
            &link.drop_pubkey,
            "image/jpeg",
            &k,
            &nonce_prefix,
            &[0x11; ESEED_LEN],
            &[0x22; 12],
        )
        .unwrap()
    };

    // A one-chunk plaintext, re-sealed as a full chunk followed by an empty last chunk.
    let plaintext = vec![0xAB; PLAINTEXT_CHUNK];
    let mut trailing = seal(&plaintext);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&k));
    let mut enc =
        EncryptorBE32::<Aes256Gcm>::from_aead(cipher, GenericArray::from_slice(&nonce_prefix));
    let mut bytes = enc.encrypt_next(plaintext.as_slice()).unwrap();
    bytes.extend(enc.encrypt_last(&[][..]).unwrap());
    assert_eq!(
        decrypt_asset_vec(&k, &nonce_prefix, &bytes).unwrap(),
        plaintext,
        "STREAM decryption accepts the trailing empty chunk"
    );
    assert_ne!(bytes, trailing.ciphertext);
    trailing.descriptor.ciphertext_hash = hash::hash_bytes(&bytes);
    trailing.ciphertext = bytes;

    // A canonical ciphertext whose descriptor overstates the plaintext size.
    let mut misstated = seal(b"a short guest photo");
    misstated.descriptor.plaintext_size += 1;

    for sealed in [trailing, misstated] {
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
        assert!(matches!(
            DropAdopter::adopt(&mut ws, drop_id, album),
            Err(DropError::Crypto(
                "staged drop is not the canonical encryption of its plaintext"
            ))
        ));
        assert!(
            DropAdopter::list_inbox(&ws)
                .unwrap()
                .iter()
                .any(|p| p.drop_id == drop_id),
            "a refused drop stays adoptable-or-discardable in the inbox"
        );
    }
    assert!(ws.asset_ids().is_empty(), "nothing was registered");
}

/// A rejected drop is discarded — removed from the inbox with no library trace.
#[test]
fn discarded_drop_leaves_no_trace() {
    use capsule_core::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let link = UploadLinkIssuer::create_link(&mut ws, LinkCaps::default(), None).unwrap();
    let sealed = seal_drop(b"unwanted", &link.drop_pubkey, "image/jpeg").unwrap();
    let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();

    assert_eq!(DropAdopter::list_inbox(&ws).unwrap().len(), 1);
    DropAdopter::discard(&mut ws, drop_id).unwrap();
    assert!(DropAdopter::list_inbox(&ws).unwrap().is_empty());
    // Discarding again (or an unknown drop) is NotFound.
    assert!(matches!(
        DropAdopter::discard(&mut ws, drop_id),
        Err(DropError::NotFound)
    ));
}
