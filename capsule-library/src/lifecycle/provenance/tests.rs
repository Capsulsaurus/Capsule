use std::fs;

use capsule_core::crypto::keys::{Amk, HybridSigningKey};
use tempfile::TempDir;

use super::super::fast_workspace;
use super::*;

/// The `Workspace` populates `metadata_blob_hash` per the sealing order, the sidecar
/// binds to the manifest through the prior head, and a one-byte sidecar mutation quarantines.
#[test]
fn metadata_binding_populated_and_enforced() {
    use capsule_core::crypto::verify_asset::{
        BindingReject, MetadataBinding, verify_metadata_binding,
    };

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF metadata-binding bytes").unwrap();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();

    // The create manifest commits to a metadata blob; its sidecar references no prior head,
    // and that absence equals the manifest's `prior_provenance_hash` (both `None` on create).
    let st = ws.asset(&asset).unwrap();
    let head = &st.chain.records().last().unwrap().manifest;
    let epoch = head.core.amk_version.0;
    assert!(
        head.core.metadata_blob_hash.is_some(),
        "create must bind a metadata blob"
    );
    assert_eq!(st.sidecar.provenance_chain_hash, None);
    assert_eq!(
        head.core.prior_provenance_hash,
        st.sidecar.provenance_chain_hash
    );

    // The stored blob round-trips to the signed sidecar under the asset's blob key,
    // re-derived from the blob's own (folded) nonce.
    let blob_key = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch])
        .derive_blob_key(&asset, &blob_nonce(&st.metadata_blob).unwrap());
    assert_eq!(
        verify_metadata_binding(
            head,
            &st.metadata_blob,
            &blob_key,
            &st.sidecar.to_canonical_vec()
        ),
        MetadataBinding::Bound
    );
    // A one-byte mutation of the local sidecar quarantines (surfaced, never persisted).
    let mut tampered = st.sidecar.to_canonical_vec();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    assert_eq!(
        verify_metadata_binding(head, &st.metadata_blob, &blob_key, &tampered),
        MetadataBinding::Quarantine(BindingReject::SidecarMismatch)
    );

    // A metadata-update re-binds: the sidecar references the PRIOR head (the create record),
    // equal to the update manifest's `prior_provenance_hash`.
    let create_head = ws.asset(&asset).unwrap().chain.records()[0].record_hash();
    ws.tag_add(&asset, "vacation").unwrap();
    let st = ws.asset(&asset).unwrap();
    let update = &st.chain.records().last().unwrap().manifest;
    assert!(update.core.metadata_blob_hash.is_some());
    assert_eq!(st.sidecar.provenance_chain_hash, Some(create_head));
    assert_eq!(
        update.core.prior_provenance_hash,
        st.sidecar.provenance_chain_hash
    );
    // The re-seal drew a fresh nonce folded into a new blob key — re-derive from the
    // updated blob's own nonce (the create-era `blob_key` no longer opens it).
    let update_blob_key = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch])
        .derive_blob_key(&asset, &blob_nonce(&st.metadata_blob).unwrap());
    assert_ne!(
        update_blob_key, blob_key,
        "the metadata-update re-rolled the blob key"
    );
    assert_eq!(
        verify_metadata_binding(
            update,
            &st.metadata_blob,
            &update_blob_key,
            &st.sidecar.to_canonical_vec()
        ),
        MetadataBinding::Bound
    );

    // A delete mints no metadata blob: the head manifest commits to none, and that is
    // structurally valid under the presence-by-action rule.
    ws.soft_delete(&asset, 30).unwrap();
    let st = ws.asset(&asset).unwrap();
    let del = &st.chain.records().last().unwrap().manifest;
    assert!(
        del.core.metadata_blob_hash.is_none(),
        "delete binds no metadata blob"
    );
    assert!(del.structural_ok());
}

/// Re-point `ws` at a different signing device — and optionally a different **account** —
/// publishing a directory that holds it. What a second phone, or a shared album's member,
/// looks like to everything below the signer.
fn become_device(
    ws: &mut Workspace,
    account: Option<(Uuid, HybridSigningKey)>,
    device_id: Uuid,
    dsk: HybridSigningKey,
) {
    use capsule_core::crypto::keys::{DeviceEntry, DirectoryCore};

    let entry = DeviceEntry {
        device_id,
        dsk_public: dsk.verifying_key(),
        dek_public: None,
        // Must precede any manifest it signs; the workspace stamps `now`.
        added_at: "2020-01-01T00:00:00Z".into(),
        revoked_at: None,
        unknown: Default::default(),
    };
    ws.directory = match account {
        // A second device of the *same* account: appended to the account's own directory,
        // which is re-signed by the account IK at a higher version.
        None => {
            let mut core = ws.directory.core.clone();
            core.directory_version += 1;
            core.devices.push(entry);
            core.sign(&ws.account.user_ik)
        }
        // A different account entirely: its own directory, under its own IK.
        Some((user_id, ref ik)) => {
            let directory = DirectoryCore {
                user_id,
                directory_version: 1,
                updated_at: now_rfc3339(),
                devices: vec![entry],
                unknown: Default::default(),
            }
            .sign(ik);
            ws.account.user_id = user_id;
            directory
        }
    };
    ws.account.device.device_id = device_id;
    ws.device_signer = Box::new(dsk);
}

fn imported(lib: &TempDir, src: &TempDir) -> (Workspace, Uuid, Uuid) {
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF continuation-authorship bytes").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    (ws, album, asset)
}

/// **A second device of the same account continues a chain, and the result verifies.**
///
/// The case `sign_lifecycle` used to break outright: it inherited `created_by_user` and
/// `created_by_device` from the chain head while signing with the *current* device, so a
/// delete from device B claimed device A and failed `verify_asset` step 8 — the device
/// signature does not verify under the named entry's key. Ordinary two-device use, no
/// sharing required.
#[test]
fn a_continuation_from_a_second_device_names_it_and_verifies() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, _album, asset) = imported(&lib, &src);

    let creator = ws.account.device.device_id;
    let second = Uuid::from_u128(0xD2);
    become_device(
        &mut ws,
        None,
        second,
        HybridSigningKey::from_seed_bytes(&[9; 32], &[10; 32]),
    );

    ws.soft_delete(&asset, 30).unwrap();

    let st = ws.asset(&asset).unwrap();
    let head = &st.chain.records().last().unwrap().manifest;
    assert_eq!(head.core.action, Action::Delete);
    assert_eq!(
        head.core.created_by_device, second,
        "the continuation names the device that signed it"
    );
    assert_ne!(
        head.core.created_by_device, creator,
        "and not the one that created the asset"
    );
    assert_eq!(
        ws.verify(&asset).unwrap(),
        VerifyOutcome::Accept,
        "which is the only reason it can verify at all"
    );

    // The creator is not lost — it is where the append-only chain keeps it.
    assert_eq!(
        st.chain.records()[0].manifest.core.created_by_device,
        creator
    );
    assert_eq!(st.chain.records()[0].manifest.core.action, Action::Create);
}

/// **A member of a shared album continues the owner's chain under the member's own account**,
/// and it verifies against the *member's* directory.
///
/// The write-tier signature is what carries album authority (step 10) and it is unaffected:
/// the member holds the epoch's write-tier key, which is what membership *is*. Naming the
/// acting member as the record's author therefore does not weaken the owner's album — it is
/// the only way the record can be verified by anyone.
#[test]
fn a_members_continuation_verifies_under_the_members_own_directory() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, _album, asset) = imported(&lib, &src);

    let owner = ws.account.user_id;
    let member = Uuid::from_u128(0xB0B);
    become_device(
        &mut ws,
        Some((
            member,
            HybridSigningKey::from_seed_bytes(&[11; 32], &[12; 32]),
        )),
        Uuid::from_u128(0xD3),
        HybridSigningKey::from_seed_bytes(&[13; 32], &[14; 32]),
    );

    ws.soft_delete(&asset, 30).unwrap();

    let st = ws.asset(&asset).unwrap();
    let head = &st.chain.records().last().unwrap().manifest;
    assert_eq!(
        head.core.created_by_user, member,
        "a member's write is authored by the member"
    );
    assert_ne!(head.core.created_by_user, owner);
    assert_eq!(
        ws.verify(&asset).unwrap(),
        VerifyOutcome::Accept,
        "verified under the member's directory, against the owner's album authority"
    );
    assert_eq!(
        st.chain.records()[0].manifest.core.created_by_user,
        owner,
        "and the album's asset is still the owner's creation"
    );
}

/// **The first metadata-bearing edit keeps the blob the create binds** (issue #1091), byte for
/// byte, before it replaces it. A `delete` / `trash-restore` mints no blob and keeps nothing, and
/// a later edit leaves the kept copy alone: the current blob is no longer the create's.
#[test]
fn the_first_edit_keeps_the_create_blob_and_later_writes_leave_it() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF kept-create-blob bytes").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    let created = ws.asset(&asset).unwrap().metadata_blob.clone();
    let kept = ws.create_metadata_blob_path(&asset).unwrap();

    ws.soft_delete(&asset, 30).unwrap();
    ws.restore(&asset).unwrap();
    assert!(!kept.exists(), "a delete and a restore mint no blob");

    ws.tag_add(&asset, "beach").unwrap();
    assert_eq!(
        fs::read(&kept).unwrap(),
        created,
        "the create's exact bytes"
    );
    let edited = ws.asset(&asset).unwrap().metadata_blob.clone();
    assert_ne!(edited, created);

    ws.set_caption(&asset, "later").unwrap();
    assert_eq!(
        fs::read(&kept).unwrap(),
        created,
        "a later edit keeps nothing new"
    );
    assert_eq!(
        ws.verify(&asset).unwrap(),
        VerifyOutcome::Accept,
        "the kept copy is not taken for the original"
    );
}
