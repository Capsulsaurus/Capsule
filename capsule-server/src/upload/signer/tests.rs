//! The signer check, over real signatures: the signing device's own manifest, a delegate's
//! upload of it, and every way the admission must refuse.

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{
    AmkVersion, DeviceDirectory, DeviceEntry, DirectoryCore, HybridSigningKey,
};
use capsule_core::crypto::primitives::CRYPTO_SUITE_ID;
use capsule_core::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, ProvenanceRecord,
};

use super::*;

const USER: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e62";
const OTHER_USER: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e99";
const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60";
const ASSET: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61";
const ORIGIN: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f";
const DELEGATE: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70";
const WRITTEN_AT: &str = "2026-05-31T00:00:00Z";
const PROTOCOL: &str = "2026-05-31";

fn id(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("the literal is a uuid")
}

fn key(seed: u8) -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[seed; 32], &[seed; 32])
}

fn origin_key() -> HybridSigningKey {
    key(0x0A)
}

fn delegate_key() -> HybridSigningKey {
    key(0x0B)
}

fn entry(device: &str, signer: &HybridSigningKey, revoked: bool) -> DeviceEntry {
    DeviceEntry {
        device_id: id(device),
        dsk_public: signer.verifying_key(),
        dek_public: None,
        added_at: "2026-05-01T00:00:00Z".to_owned(),
        revoked_at: revoked.then(|| "2026-05-20T00:00:00Z".to_owned()),
        unknown: Default::default(),
    }
}

/// `user`'s directory listing the origin (revoked or not) and the delegate.
fn directory_of(user: &str, origin_revoked: bool) -> DeviceDirectory {
    DirectoryCore {
        user_id: id(user),
        directory_version: 3,
        updated_at: "2026-05-20T00:00:00Z".to_owned(),
        devices: vec![
            entry(ORIGIN, &origin_key(), origin_revoked),
            entry(DELEGATE, &delegate_key(), false),
        ],
        unknown: Default::default(),
    }
    .sign(&key(0x1D))
}

fn directory() -> DeviceDirectory {
    directory_of(USER, false)
}

fn core() -> ManifestCore {
    ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL.to_owned(),
        file_id: id(ASSET),
        album_id: id(ALBUM),
        amk_version: AmkVersion(1),
        ciphertext_hash: Hash32([0x11; 32]),
        plaintext_size: 10,
        chunk_size: 65_536,
        nonce_prefix: [0u8; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(Hash32([0x22; 32])),
        created_by_user: id(USER),
        created_by_device: id(ORIGIN),
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: WRITTEN_AT.to_owned(),
        action: Action::Create,
        prior_provenance_hash: None,
        upgraded_from: None,
        unknown: Default::default(),
        retention_until: None,
    }
}

/// The provenance blob a device produces for `core`, signed with `signer`.
fn blob(core: ManifestCore, signer: &HybridSigningKey) -> Vec<u8> {
    let manifest = core.sign(signer, signer).expect("a software key signs");
    let record = ProvenanceRecord::new(manifest.core.file_id, manifest, None);
    capsule_core::cbor::to_canonical_vec(&record).expect("a record encodes")
}

/// The envelope a client declares for the provenance member of `core`'s bundle.
fn envelope_for(core: &ManifestCore) -> ManifestEnvelope {
    ManifestEnvelope {
        crypto_suite_id: core.crypto_suite_id,
        protocol_version: core.protocol_version.clone(),
        album_id: Some(core.album_id.to_string()),
        file_id: core.file_id.to_string(),
        amk_version: core.amk_version.0,
        // The provenance session's own address: never compared with the manifest's.
        ciphertext_hash: "33".repeat(32),
        plaintext_size: core.plaintext_size,
        chunk_size: core.chunk_size,
        key_mode: "derived".to_owned(),
        metadata_blob_hash: core.metadata_blob_hash.map(|hash| hash.to_hex()),
        original_blob_hash: None,
        derivative_role: None,
        derivative_format: None,
        created_by_user: core.created_by_user.to_string(),
        created_by_device: core.created_by_device.to_string(),
        client_version: core.client_version.clone(),
        timestamp: core.timestamp.clone(),
        action: "create".to_owned(),
        prior_provenance_hash: None,
        retention_until: None,
    }
}

fn check(bytes: &[u8], envelope: &ManifestEnvelope, uploader: &str) -> Result<(), SignerReject> {
    check_signed_manifest(bytes, envelope, &UserId::new(uploader), Some(&directory())).map(|_| ())
}

#[test]
fn the_signing_devices_own_manifest_is_admitted() {
    let core = core();
    assert_eq!(
        check(
            &blob(core.clone(), &origin_key()),
            &envelope_for(&core),
            USER
        ),
        Ok(())
    );
}

#[test]
fn a_manifest_one_device_signed_is_admitted_whoever_of_the_account_uploads_it() {
    // The check reads nothing about the uploading device: a delegate's upload of the origin's
    // sealed bundle is the same bytes under the same envelope, and is admitted on the origin's
    // signature.
    let core = core();
    let bytes = blob(core.clone(), &origin_key());
    let admitted = check_signed_manifest(
        &bytes,
        &envelope_for(&core),
        &UserId::new(USER),
        Some(&directory()),
    )
    .expect("the origin's signature verifies");
    assert_eq!(admitted.core.created_by_device, id(ORIGIN));
}

#[test]
fn a_delegate_that_re_signs_as_the_origin_is_refused() {
    // The delegate holds its own key, not the origin's: a manifest naming the origin but signed
    // by the delegate does not verify under the origin's published key.
    let core = core();
    assert_eq!(
        check(
            &blob(core.clone(), &delegate_key()),
            &envelope_for(&core),
            USER
        ),
        Err(SignerReject::BadSignature)
    );
}

#[test]
fn a_tampered_manifest_is_refused() {
    let core = core();
    let original = blob(core.clone(), &origin_key());
    let signed = ProvenanceRecord::decode(&original).expect("the record decodes");
    let mut bytes = original.clone();
    // Flip bit 0 at positions spread across the whole record (every one would cost a hybrid
    // verify each, thousands of them). Wherever the change lands in the signed core or the
    // device signature, the manifest is refused. The one part the server does not judge is
    // `write_sig`: it verifies under the album's write-tier key, which only a member holds, so
    // it is every receiving client's `verify_asset` check, not this one.
    let envelope = envelope_for(&core);
    let mut refused_on_signature = false;
    let step = bytes.len() / 150 + 1;
    for at in (0..bytes.len()).step_by(step).chain(0..64) {
        bytes[at] ^= 0x01;
        match check(&bytes, &envelope, USER) {
            Ok(()) => {
                let admitted =
                    ProvenanceRecord::decode(&bytes).expect("an admitted record decodes");
                assert_eq!(admitted.manifest.core, signed.manifest.core, "byte {at}");
                assert_eq!(
                    admitted.manifest.device_sig, signed.manifest.device_sig,
                    "byte {at}"
                );
                assert_ne!(admitted.manifest.write_sig, signed.manifest.write_sig);
            }
            Err(SignerReject::BadSignature) => refused_on_signature = true,
            Err(_) => {}
        }
        bytes[at] ^= 0x01;
    }
    assert!(
        refused_on_signature,
        "some alteration decodes and is refused on its signature"
    );
}

#[test]
fn a_re_encoding_that_decodes_to_the_same_manifest_is_refused() {
    // CBOR spells `null` two ways; `undefined` decodes to the same `None`. The values (and so
    // the signature) are unchanged, but the bytes are not the ones the signer produced, and the
    // chain head is the hash of the bytes: the next record the signer chains on would not
    // match what the server holds.
    let core = core();
    let bytes = blob(core.clone(), &origin_key());
    let null = bytes
        .iter()
        .position(|byte| *byte == 0xF6)
        .expect("the record spells some field null");
    let mut respelled = bytes.clone();
    respelled[null] = 0xF7;
    assert_eq!(
        check(&respelled, &envelope_for(&core), USER),
        Err(SignerReject::NotCanonical)
    );
}

#[test]
fn a_device_of_another_account_is_refused() {
    // Another account's device signs a manifest naming its own account; the uploader is ours.
    let mut core = core();
    core.created_by_user = id(OTHER_USER);
    let bytes = blob(core.clone(), &origin_key());
    let envelope = envelope_for(&core);
    assert_eq!(
        check(&bytes, &envelope, USER),
        Err(SignerReject::EnvelopeDisagrees("created_by_user"))
    );
    // And under that account's own directory, uploaded by us, it is still not ours to admit.
    assert_eq!(
        check_signed_manifest(
            &bytes,
            &envelope,
            &UserId::new(USER),
            Some(&directory_of(OTHER_USER, false)),
        )
        .map(|_| ()),
        Err(SignerReject::EnvelopeDisagrees("created_by_user"))
    );
}

#[test]
fn a_device_the_uploaders_directory_does_not_list_is_refused() {
    let mut core = core();
    core.created_by_device = id("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e71");
    assert_eq!(
        check(
            &blob(core.clone(), &origin_key()),
            &envelope_for(&core),
            USER
        ),
        Err(SignerReject::BadSignature)
    );
}

#[test]
fn a_revoked_device_is_refused_and_the_wire_answer_is_device_not_authorized() {
    let core = core();
    let refused = check_signed_manifest(
        &blob(core.clone(), &origin_key()),
        &envelope_for(&core),
        &UserId::new(USER),
        Some(&directory_of(USER, true)),
    )
    .map(|_| ());
    assert_eq!(refused, Err(SignerReject::Revoked));
    assert_eq!(
        SignerReject::Revoked.gate(),
        GateReject::DeviceNotAuthorized
    );
}

#[test]
fn no_published_directory_is_a_refusal() {
    let core = core();
    assert_eq!(
        check_signed_manifest(
            &blob(core.clone(), &origin_key()),
            &envelope_for(&core),
            &UserId::new(USER),
            None,
        )
        .map(|_| ()),
        Err(SignerReject::NoDirectory)
    );
}

#[test]
fn bytes_that_are_not_a_manifest_are_refused() {
    let core = core();
    assert!(matches!(
        check(b"not a manifest", &envelope_for(&core), USER),
        Err(SignerReject::NotAManifest(_))
    ));
    assert_eq!(
        SignerReject::NotAManifest(ManifestFault::Undecodable).gate(),
        GateReject::SignatureUnverified
    );
}

#[test]
fn an_envelope_describing_another_manifest_is_refused_by_the_field_it_contradicts() {
    let core = core();
    let bytes = blob(core.clone(), &origin_key());
    type Mutation = fn(&mut ManifestEnvelope);
    let cases: [(&str, Mutation); 8] = [
        ("file_id", |e| e.file_id = DELEGATE.to_owned()),
        ("album_id", |e| e.album_id = None),
        ("created_by_device", |e| {
            e.created_by_device = DELEGATE.to_owned();
        }),
        ("action", |e| e.action = "replace".to_owned()),
        ("amk_version", |e| e.amk_version = 2),
        ("timestamp", |e| {
            e.timestamp = "2026-05-31T00:00:01Z".to_owned();
        }),
        ("metadata_blob_hash", |e| e.metadata_blob_hash = None),
        ("original_blob_hash", |e| {
            e.original_blob_hash = Some("44".repeat(32));
        }),
    ];
    for (field, mutate) in cases {
        let mut envelope = envelope_for(&core);
        mutate(&mut envelope);
        assert_eq!(
            check(&bytes, &envelope, USER),
            Err(SignerReject::EnvelopeDisagrees(field)),
            "{field}"
        );
    }
}

#[test]
fn an_envelope_naming_the_manifests_own_original_agrees() {
    let core = core();
    let mut envelope = envelope_for(&core);
    envelope.original_blob_hash = Some(core.ciphertext_hash.to_hex());
    assert_eq!(check(&blob(core, &origin_key()), &envelope, USER), Ok(()));
}

#[test]
fn each_reject_has_a_stable_name() {
    let names = [
        SignerReject::NotAManifest(ManifestFault::Undecodable).as_str(),
        SignerReject::NotCanonical.as_str(),
        SignerReject::EnvelopeDisagrees("file_id").as_str(),
        SignerReject::NoDirectory.as_str(),
        SignerReject::Revoked.as_str(),
        SignerReject::BadSignature.as_str(),
    ];
    let mut unique = names.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len());
}
