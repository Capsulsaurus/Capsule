use super::*;
use crate::metadata::crdt::AddId;

const STACK: Uuid = Uuid::from_u128(0x51);
const ALBUM: Uuid = Uuid::from_u128(0xA1B);
const DEV_A: Uuid = Uuid::from_u128(0xA);
const DEV_B: Uuid = Uuid::from_u128(0xB);

fn ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}

fn add(device: Uuid, counter: u64) -> AddId {
    AddId { device, counter }
}

#[test]
fn sign_verify_round_trip_through_canonical_cbor() {
    let mut doc = StackDocV1::new(STACK, ALBUM);
    doc.labels.add("keeper".into(), add(DEV_A, 0));
    doc.sign(&ik());
    let bytes = doc.to_canonical_vec();
    assert_eq!(bytes[1], 0x00, "schema is field 0");
    let back = StackDocV1::from_canonical_slice(&bytes, STACK_DOC_SCHEMA_V1).unwrap();
    assert_eq!(back, doc);
    assert!(back.verify(&ik().verifying_key()));
    // Tampering after signing breaks verification.
    let mut tampered = back;
    tampered.labels.add("injected".into(), add(DEV_B, 9));
    assert!(!tampered.verify(&ik().verifying_key()));
}

#[test]
fn unknown_keys_survive_under_the_signature() {
    let mut doc = StackDocV1::new(STACK, ALBUM);
    doc.unknown.insert("future".into(), Value::Bool(true));
    doc.sign(&ik());
    let back =
        StackDocV1::from_canonical_slice(&doc.to_canonical_vec(), STACK_DOC_SCHEMA_V1).unwrap();
    assert_eq!(back.unknown.get("future"), Some(&Value::Bool(true)));
    assert!(back.verify(&ik().verifying_key()));
}

#[test]
fn refuses_a_schema_newer_than_known() {
    let mut doc = StackDocV1::new(STACK, ALBUM);
    doc.stack_doc_schema = 9;
    assert!(matches!(
        StackDocV1::from_canonical_slice(&doc.to_canonical_vec(), STACK_DOC_SCHEMA_V1),
        Err(DocError::SchemaTooNew { found: 9, .. })
    ));
}

#[test]
fn concurrent_add_and_remove_converge() {
    let mut base = StackDocV1::new(STACK, ALBUM);
    base.labels.add("best".into(), add(DEV_A, 0));

    let mut a = base.clone();
    a.labels.remove(add(DEV_A, 0)).unwrap();
    let mut b = base;
    b.labels.add("hdr".into(), add(DEV_B, 0));

    let mut ab = a.clone();
    ab.merge(&b).unwrap();
    let mut ba = b;
    ba.merge(&a).unwrap();
    assert_eq!(ab.labels, ba.labels);
    assert_eq!(
        ab.labels.value().into_iter().collect::<Vec<_>>(),
        vec!["hdr".to_string()]
    );
}

#[test]
fn merge_refuses_another_stack_or_album() {
    let mut doc = StackDocV1::new(STACK, ALBUM);
    assert!(
        doc.merge(&StackDocV1::new(Uuid::from_u128(1), ALBUM))
            .is_err()
    );
    assert!(
        doc.merge(&StackDocV1::new(STACK, Uuid::from_u128(2)))
            .is_err()
    );
}

#[test]
fn seals_under_the_amk_and_opens_only_for_its_stack() {
    let amk = Amk::from_bytes([3; 32]);
    let mut doc = StackDocV1::new(STACK, ALBUM);
    doc.labels.add("burst".into(), add(DEV_A, 0));
    doc.sign(&ik());
    let wire = doc.seal(&amk).unwrap();
    let opened = StackDocV1::open_sealed(&wire, &amk, &STACK, STACK_DOC_SCHEMA_V1).unwrap();
    assert_eq!(opened, doc);
    assert!(StackDocV1::open_sealed(&wire, &amk, &ALBUM, STACK_DOC_SCHEMA_V1).is_err());
}
