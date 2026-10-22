use super::*;
use crate::metadata::crdt::AddId;

const ALBUM: Uuid = Uuid::from_u128(0xA1B);
const DEV_A: Uuid = Uuid::from_u128(0xA);
const DEV_B: Uuid = Uuid::from_u128(0xB);

fn ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}

fn named(name: &str, ts: &str, by: Uuid) -> AlbumDocV1 {
    let mut doc = AlbumDocV1::new(ALBUM);
    doc.name.set(name.to_string(), ts, by);
    doc
}

#[test]
fn schema_is_field_zero_canonically_first() {
    let bytes = named("Trip", "2026-05-31T10:00:00Z", DEV_A).to_canonical_vec();
    assert!(
        (0xa0..=0xbb).contains(&bytes[0]),
        "expected a CBOR map head"
    );
    assert_eq!(bytes[1], 0x00, "first map key must be integer 0");
    assert_eq!(bytes[2], 0x01, "schema value 1");
}

#[test]
fn sign_verify_round_trip_through_canonical_cbor() {
    let mut doc = named("Trip", "2026-05-31T10:00:00Z", DEV_A);
    doc.labels.add(
        "family".into(),
        AddId {
            device: DEV_A,
            counter: 0,
        },
    );
    doc.sign(&ik());
    let back =
        AlbumDocV1::from_canonical_slice(&doc.to_canonical_vec(), ALBUM_DOC_SCHEMA_V1).unwrap();
    assert_eq!(back, doc);
    assert!(back.verify(&ik().verifying_key()));
    assert_eq!(back.current_name(), Some("Trip"));
}

#[test]
fn unknown_keys_survive_under_the_signature() {
    let mut doc = named("Trip", "2026-05-31T10:00:00Z", DEV_A);
    doc.unknown
        .insert("cover".into(), Value::Text("future".into()));
    doc.sign(&ik());
    let back =
        AlbumDocV1::from_canonical_slice(&doc.to_canonical_vec(), ALBUM_DOC_SCHEMA_V1).unwrap();
    assert_eq!(
        back.unknown.get("cover"),
        Some(&Value::Text("future".into()))
    );
    assert!(back.verify(&ik().verifying_key()));
    let mut stripped = back;
    stripped.unknown.clear();
    assert!(!stripped.verify(&ik().verifying_key()));
}

#[test]
fn refuses_a_schema_newer_than_known() {
    let mut doc = AlbumDocV1::new(ALBUM);
    doc.album_doc_schema = 2;
    let bytes = doc.to_canonical_vec();
    assert!(matches!(
        AlbumDocV1::from_canonical_slice(&bytes, ALBUM_DOC_SCHEMA_V1),
        Err(DocError::SchemaTooNew {
            found: 2,
            max_known: 1
        })
    ));
}

#[test]
fn concurrent_renames_converge_on_the_later_write() {
    let a = named("Trip", "2026-05-31T10:00:00Z", DEV_A);
    let b = named("Lisbon 2026", "2026-05-31T11:00:00Z", DEV_B);
    let mut ab = a.clone();
    ab.merge(&b).unwrap();
    let mut ba = b.clone();
    ba.merge(&a).unwrap();
    assert_eq!(ab.current_name(), Some("Lisbon 2026"));
    assert_eq!(ab.current_name(), ba.current_name());
}

#[test]
fn merge_drops_the_signature_only_when_something_changed() {
    let mut a = named("Trip", "2026-05-31T10:00:00Z", DEV_A);
    a.sign(&ik());
    let same = a.clone();
    a.merge(&same).unwrap();
    assert!(
        a.verify(&ik().verifying_key()),
        "a no-op merge keeps the signature"
    );

    let b = named("Lisbon", "2026-05-31T11:00:00Z", DEV_B);
    a.merge(&b).unwrap();
    assert!(a.signature.is_none(), "a changing merge must be re-signed");
}

#[test]
fn merge_refuses_another_album() {
    let mut a = AlbumDocV1::new(ALBUM);
    let other = AlbumDocV1::new(Uuid::from_u128(0xFFFF));
    assert!(matches!(a.merge(&other), Err(DocError::IdMismatch { .. })));
}

#[test]
fn seals_under_the_amk_and_opens_only_for_its_album() {
    let amk = Amk::from_bytes([7; 32]);
    let mut doc = named("Trip", "2026-05-31T10:00:00Z", DEV_A);
    doc.sign(&ik());
    let wire = doc.seal(&amk).unwrap();
    // The plaintext name never appears in the sealed bytes.
    assert!(!wire.windows(4).any(|w| w == b"Trip"));
    let opened = AlbumDocV1::open_sealed(&wire, &amk, &ALBUM, ALBUM_DOC_SCHEMA_V1).unwrap();
    assert_eq!(opened, doc);
    // The wrong AMK, or the wrong album id (a different blob key), does not open it.
    assert!(
        AlbumDocV1::open_sealed(
            &wire,
            &Amk::from_bytes([8; 32]),
            &ALBUM,
            ALBUM_DOC_SCHEMA_V1
        )
        .is_err()
    );
    assert!(
        AlbumDocV1::open_sealed(&wire, &amk, &Uuid::from_u128(1), ALBUM_DOC_SCHEMA_V1).is_err()
    );
}
