use super::*;
use crate::crypto::keys::{DeviceEntry, DirectoryCore};

const OWNER: Uuid = Uuid::from_u128(0xA11CE);
const DEVICE: Uuid = Uuid::from_u128(0xD1);
const ALBUM: Uuid = Uuid::from_u128(0xA1B);
const BOB: Uuid = Uuid::from_u128(0xB0B);
const CAROL: Uuid = Uuid::from_u128(0xCA501);

fn ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}

fn dsk() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
}

fn other_dsk() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32])
}

fn directory_for(user_id: Uuid, revoked: bool) -> DeviceDirectory {
    DirectoryCore {
        user_id,
        directory_version: 1,
        updated_at: "2026-09-01T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: DEVICE,
            dsk_public: dsk().verifying_key(),
            dek_public: None,
            added_at: "2026-09-01T00:00:00Z".into(),
            revoked_at: revoked.then(|| "2026-09-02T00:00:00Z".to_owned()),
            unknown: Default::default(),
        }],
        unknown: Default::default(),
    }
    .sign(&ik())
}

fn roster() -> AlbumRoster {
    AlbumRoster {
        album_id: ALBUM,
        roster_version: 1,
        amk_epoch: AmkVersion(1),
        attested_by_user: OWNER,
        attested_by_device: DEVICE,
        attested_at: "2026-09-02T00:00:00Z".into(),
        members: vec![
            RosterMember {
                user_id: BOB,
                role: MemberRole::Writer,
            },
            RosterMember {
                user_id: CAROL,
                role: MemberRole::Reader,
            },
        ],
    }
}

#[test]
fn a_roster_signed_by_a_live_owner_device_verifies() {
    let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
    assert_eq!(signed.verify(&directory_for(OWNER, false)), Ok(()));
}

#[test]
fn the_signed_roster_round_trips_through_canonical_cbor() {
    // The server stores the document verbatim and the SDK ships it base64-encoded, so the
    // bytes must decode back to a value that still verifies.
    let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
    let bytes = crate::cbor::to_canonical_vec(&signed).expect("encodes");
    let decoded: SignedAlbumRoster = crate::cbor::from_slice(&bytes).expect("decodes");
    assert_eq!(decoded, signed);
    assert_eq!(decoded.verify(&directory_for(OWNER, false)), Ok(()));
    // And the signing bytes are stable: the same roster encodes to the same bytes.
    assert_eq!(
        roster().signing_bytes().expect("encodes"),
        decoded.roster.signing_bytes().expect("encodes")
    );
}

#[test]
fn a_directory_of_another_account_is_refused() {
    let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
    assert_eq!(
        signed.verify(&directory_for(BOB, false)),
        Err(MembershipError::Attester(
            "the roster's attested_by_user is not this directory's account"
        ))
    );
}

#[test]
fn a_device_the_directory_does_not_hold_is_refused() {
    let mut unknown = roster();
    unknown.attested_by_device = Uuid::from_u128(0xD2);
    let signed = SignedAlbumRoster::sign(unknown, &dsk()).expect("signs");
    assert_eq!(
        signed.verify(&directory_for(OWNER, false)),
        Err(MembershipError::Attester(
            "the attesting device is not in the directory"
        ))
    );
}

#[test]
fn a_revoked_device_may_not_attest_a_roster() {
    // Stricter than the upgrade intent: the entry is retained so old manifests verify, not
    // so the device can keep issuing new documents.
    let signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
    assert_eq!(
        signed.verify(&directory_for(OWNER, true)),
        Err(MembershipError::Attester("the attesting device is revoked"))
    );
}

#[test]
fn a_tampered_member_role_does_not_verify() {
    let mut signed = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
    signed.roster.members[1].role = MemberRole::Writer;
    assert_eq!(
        signed.verify(&directory_for(OWNER, false)),
        Err(MembershipError::Attester(
            "the attester's DSK signature does not verify"
        ))
    );
}

#[test]
fn a_tampered_version_or_epoch_does_not_verify() {
    let refused = Err(MembershipError::Attester(
        "the attester's DSK signature does not verify",
    ));
    let mut bumped = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
    bumped.roster.roster_version = 2;
    assert_eq!(bumped.verify(&directory_for(OWNER, false)), refused);

    let mut rolled = SignedAlbumRoster::sign(roster(), &dsk()).expect("signs");
    rolled.roster.amk_epoch = AmkVersion(2);
    assert_eq!(rolled.verify(&directory_for(OWNER, false)), refused);
}

#[test]
fn a_signature_by_the_wrong_key_does_not_verify() {
    // The right device id, the wrong DSK: what a member forging the owner's attestation
    // looks like.
    let signed = SignedAlbumRoster::sign(roster(), &other_dsk()).expect("signs");
    assert_eq!(
        signed.verify(&directory_for(OWNER, false)),
        Err(MembershipError::Attester(
            "the attester's DSK signature does not verify"
        ))
    );
}

#[test]
fn roles_encode_as_their_snake_case_tokens() {
    let bytes = crate::cbor::to_canonical_vec(&MemberRole::Reader).expect("encodes");
    let value: ciborium::Value = ciborium::from_reader(bytes.as_slice()).expect("decodes");
    assert_eq!(value, ciborium::Value::Text("reader".into()));
}
