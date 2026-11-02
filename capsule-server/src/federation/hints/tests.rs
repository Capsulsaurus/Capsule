use super::*;
use crate::federation::PeerId;
use crate::store::AlbumId;

fn claim() -> HintClaim {
    HintClaim {
        home_server: "other.test".to_owned(),
        recipient_server: "capsule.test".to_owned(),
        nonce: "0192f1a8-7c4e-4b6a-9d3e-5f7a8b9c0d1e".to_owned(),
        album_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60".to_owned(),
        sent_at: "2026-09-02T00:00:00Z".to_owned(),
    }
}

/// A key pair, and the raw thirty-two public bytes an operator pins.
fn keypair() -> (ring::signature::Ed25519KeyPair, [u8; 32]) {
    use ring::signature::KeyPair as _;
    let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("a key generates");
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(der.as_ref()).expect("it parses");
    let public: [u8; 32] = pair.public_key().as_ref().try_into().expect("32 bytes");
    (pair, public)
}

#[test]
fn a_hint_verifies_under_the_key_that_signed_it_and_no_other() {
    let (pair, public) = keypair();
    let claim = claim();
    let signature = pair.sign(&claim.signing_bytes().expect("it encodes"));
    assert_eq!(claim.verify(signature.as_ref(), &public), Ok(()));

    let (_, other) = keypair();
    assert_eq!(
        claim.verify(signature.as_ref(), &other),
        Err(HintError::NotAuthentic)
    );
    assert_eq!(
        claim.verify(b"not a signature", &public),
        Err(HintError::NotAuthentic)
    );
}

#[test]
fn every_field_is_covered_by_the_signature() {
    let (pair, public) = keypair();
    let original = claim();
    let signature = pair.sign(&original.signing_bytes().expect("it encodes"));

    // One entry per field of the claim, so a field added without a mutation here is a field this
    // case silently stops covering.
    let mutations: [fn(&mut HintClaim); 5] = [
        |claim| claim.home_server = "third.test".to_owned(),
        |claim| claim.recipient_server = "third.test".to_owned(),
        |claim| claim.nonce = "0192f1a8-7c4e-4b6a-9d3e-5f7a8b9c0dff".to_owned(),
        |claim| claim.album_id = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff".to_owned(),
        |claim| claim.sent_at = "2026-09-03T00:00:00Z".to_owned(),
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut mutated = original.clone();
        mutate(&mut mutated);
        assert_eq!(
            mutated.verify(signature.as_ref(), &public),
            Err(HintError::NotAuthentic),
            "mutation {index} was not covered by the signature"
        );
    }
}

#[test]
fn a_report_signature_never_verifies_as_a_hint() {
    // The context is what separates them: a peer's key signs both, and a signature over one
    // must not be presentable as the other even where the field values line up.
    let (pair, public) = keypair();
    let hint = claim();
    let report = crate::federation::ReportClaim {
        reporting_server: hint.home_server.clone(),
        recipient_server: hint.recipient_server.clone(),
        nonce: hint.nonce.clone(),
        reported_user: String::new(),
        asset_hash: String::new(),
        album_id: hint.album_id.clone(),
        reason: None,
        reported_at: hint.sent_at.clone(),
    };
    let signature = pair.sign(&report.signing_bytes().expect("it encodes"));
    assert_eq!(
        hint.verify(signature.as_ref(), &public),
        Err(HintError::NotAuthentic)
    );
}

#[test]
fn the_signing_bytes_are_stable() {
    // Pinned as a literal: this encoding is the contract every peer signs against, and it must
    // fail here rather than in the field.
    let bytes = claim().signing_bytes().expect("it encodes");
    assert_eq!(
        hex_of(&bytes),
        "a6656e6f6e6365782430313932663161382d376334652d346236612d396433652d35663761386239633064316567636f6e74657874781a63617073756c652d66656465726174696f6e2d68696e742d76316773656e745f617474323032362d30392d30325430303a30303a30305a68616c62756d5f6964782430313866336631652d346237612d376339642d386532662d3161326233633464356536306b686f6d655f7365727665726a6f746865722e7465737470726563697069656e745f7365727665726c63617073756c652e74657374",
        "if this changed, every peer's signature changed with it"
    );
}

#[test]
fn freshness_is_symmetric_and_bounded() {
    let now: Timestamp = "2026-09-02T00:00:00Z".parse().expect("an instant");
    assert!(is_fresh(now, now));
    assert!(is_fresh(now - HINT_FRESHNESS, now), "the edge is inside");
    assert!(
        is_fresh(now + HINT_FRESHNESS, now),
        "a fast clock is tolerated"
    );
    let one = SignedDuration::from_secs(1);
    assert!(
        !is_fresh(now - HINT_FRESHNESS - one, now),
        "a captured hint goes stale"
    );
    assert!(
        !is_fresh(now + HINT_FRESHNESS + one, now),
        "a hint cannot be pre-signed"
    );
    // The nonce must outlive every instant its hint could still be fresh at.
    assert!(HINT_NONCE_WINDOW >= HINT_FRESHNESS + HINT_FRESHNESS);
}

#[tokio::test]
async fn the_recording_double_records_each_job_and_answers_what_it_was_told() {
    let hints = RecordingHints::new();
    let job = JobKey::new(PeerId::new("other.test"), AlbumId::new("album"));
    assert_eq!(
        HintReceiver::hint(&hints, &job).await,
        HintOutcome::Unchanged
    );
    hints.answer(HintOutcome::Ignored);
    assert_eq!(HintReceiver::hint(&hints, &job).await, HintOutcome::Ignored);
    assert_eq!(hints.received(), vec![job.clone(), job]);
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}
