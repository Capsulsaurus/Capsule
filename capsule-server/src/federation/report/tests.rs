use super::*;

fn claim() -> ReportClaim {
    ReportClaim {
        reporting_server: "other.test".to_owned(),
        recipient_server: "capsule.test".to_owned(),
        nonce: "0192f1a8-7c4e-4b6a-9d3e-5f7a8b9c0d1e".to_owned(),
        reported_user: "01937b7c-0000-7000-8000-0000000000b0".to_owned(),
        asset_hash: "a".repeat(64),
        album_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60".to_owned(),
        reason: Some("csam".to_owned()),
        reported_at: "2026-09-02T00:00:00Z".to_owned(),
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
fn a_report_verifies_under_the_key_that_signed_it_and_no_other() {
    let (pair, public) = keypair();
    let claim = claim();
    let signature = pair.sign(&claim.signing_bytes().expect("it encodes"));
    assert_eq!(claim.verify(signature.as_ref(), &public), Ok(()));

    let (_, other) = keypair();
    assert_eq!(
        claim.verify(signature.as_ref(), &other),
        Err(ReportError::NotAuthentic)
    );
}

#[test]
fn every_field_is_covered_by_the_signature() {
    // The whole point of signing the claim rather than a digest of part of it: a peer
    // cannot have its signature over one report re-used to file a different one.
    let (pair, public) = keypair();
    let original = claim();
    let signature = pair.sign(&original.signing_bytes().expect("it encodes"));

    // Plain function pointers rather than boxed closures: none of them captures, and the
    // list is the point — one entry per field of the claim, so a field added without a
    // mutation here is a field this case silently stops covering.
    let mutations: [fn(&mut ReportClaim); 9] = [
        |claim| claim.reporting_server = "third.test".to_owned(),
        |claim| claim.recipient_server = "third.test".to_owned(),
        |claim| claim.nonce = "0192f1a8-7c4e-4b6a-9d3e-5f7a8b9c0dff".to_owned(),
        |claim| claim.reported_user = "01937b7c-0000-7000-8000-0000000000cc".to_owned(),
        |claim| claim.asset_hash = "b".repeat(64),
        |claim| claim.album_id = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff".to_owned(),
        |claim| claim.reason = Some("spam".to_owned()),
        |claim| claim.reason = None,
        |claim| claim.reported_at = "2026-09-03T00:00:00Z".to_owned(),
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut mutated = original.clone();
        mutate(&mut mutated);
        assert_eq!(
            mutated.verify(signature.as_ref(), &public),
            Err(ReportError::NotAuthentic),
            "mutation {index} was not covered by the signature"
        );
    }
}

#[test]
fn the_signing_bytes_are_stable() {
    // Pinned as a literal: this encoding is the contract every peer signs against, and a
    // field reordering or an encoder change would break every peer at once. It must fail
    // here rather than in the field.
    let bytes = claim().signing_bytes().expect("it encodes");
    assert_eq!(
        hex_of(&bytes),
        "a8656e6f6e6365782430313932663161382d376334652d346236612d396433652d35663761386239633064316566726561736f6e646373616d68616c62756d5f6964782430313866336631652d346237612d376339642d386532662d3161326233633464356536306a61737365745f686173687840616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616b7265706f727465645f617474323032362d30392d30325430303a30303a30305a6d7265706f727465645f75736572782430313933376237632d303030302d373030302d383030302d30303030303030303030623070726563697069656e745f7365727665726c63617073756c652e74657374707265706f7274696e675f7365727665726a6f746865722e74657374",
        "if this changed, every peer's signature changed with it"
    );
}

#[test]
fn freshness_is_symmetric_and_bounded() {
    let now: Timestamp = "2026-09-02T00:00:00Z".parse().expect("an instant");
    assert!(is_fresh(now, now));
    assert!(is_fresh(now - REPORT_FRESHNESS, now), "the edge is inside");
    assert!(
        is_fresh(now + REPORT_FRESHNESS, now),
        "a fast clock is tolerated"
    );
    let past = now - REPORT_FRESHNESS - SignedDuration::from_secs(1);
    let future = now + REPORT_FRESHNESS + SignedDuration::from_secs(1);
    assert!(!is_fresh(past, now), "a captured report goes stale");
    assert!(
        !is_fresh(future, now),
        "a report cannot be pre-signed for later"
    );
    // The nonce must outlive every instant its report could still be fresh at.
    assert!(REPORT_NONCE_WINDOW >= REPORT_FRESHNESS + REPORT_FRESHNESS);
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}
