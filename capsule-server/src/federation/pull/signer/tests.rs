use super::*;
use crate::federation::scheme::RequestSignature;

/// The vector `scheme`'s tests and `capsule-sdk` pin: what this server signs is what a peer
/// verifying the design doc's format reads.
const TOKEN: &str = "eyJhbGciOiJFZERTQSJ9.e30.c2lnbmF0dXJl";
const SIGNED_AT: i64 = 1_767_225_600;
const NONCE: &str = "AZQ3hW1ae6qB2vX9k0cR0w";
const HEADER: &str = concat!(
    "t=1767225600, n=AZQ3hW1ae6qB2vX9k0cR0w, ",
    "s=fIbwDSvO5ktPJD2F-C8c9F2Kx38ZS5s0NrfCejPWJfZuprrjELdhdPxP3dBtgR9X8NqSUIN1Ww0qRK2dyfTgDg",
);

/// The pinned vector's key, from its fixed seed.
#[derive(Debug)]
struct Seeded(ring::signature::Ed25519KeyPair);

impl RequestSigner for Seeded {
    fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.0.sign(message).as_ref().try_into().expect("64 bytes")
    }
}

fn seeded() -> Seeded {
    Seeded(ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).expect("a seed"))
}

fn generated() -> Ed25519RequestSigner {
    let random = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&random).expect("a key");
    Ed25519RequestSigner::from_pkcs8(pkcs8.as_ref()).expect("it reads")
}

#[test]
fn the_header_matches_the_pinned_vector() {
    let at = Timestamp::from_second(SIGNED_AT).expect("an instant");
    let header =
        signature_header(&seeded(), "get", "/v1/blob/aaaa", TOKEN, at, NONCE).expect("signed");
    assert_eq!(header, HEADER);
}

#[test]
fn what_this_server_signs_its_own_verifier_admits() {
    let signer = generated();
    let at = Timestamp::from_second(SIGNED_AT).expect("an instant");
    let nonce = fresh_nonce();
    let header = signature_header(&signer, "GET", "/v1/sync", TOKEN, at, &nonce).expect("signed");

    let parsed = RequestSignature::parse(&header).expect("the verifier parses it");
    assert_eq!(parsed.signed_at, SIGNED_AT);
    assert_eq!(parsed.nonce, nonce);
    let claim = RequestClaim::new("GET", "/v1/sync", TOKEN, SIGNED_AT, &nonce);
    parsed
        .verify(&claim, &signer.public_key())
        .expect("it verifies under the signer's public key");

    let other_path = RequestClaim::new("GET", "/v1/blob/aaaa", TOKEN, SIGNED_AT, &nonce);
    assert!(
        parsed.verify(&other_path, &signer.public_key()).is_err(),
        "a signature binds its path"
    );
}

#[test]
fn every_nonce_is_fresh_and_admissible() {
    let first = fresh_nonce();
    let second = fresh_nonce();
    assert_ne!(first, second);
    assert_eq!(
        first.len(),
        22,
        "sixteen bytes, the shortest the issuer accepts"
    );
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    );
}

#[test]
fn a_key_that_is_not_ed25519_is_refused() {
    assert_eq!(
        Ed25519RequestSigner::from_pkcs8(b"not a key").err(),
        Some(InvalidRequestKey)
    );
}

#[test]
fn the_signer_never_prints_its_key() {
    assert_eq!(
        format!("{:?}", generated()),
        "Ed25519RequestSigner(<redacted>)"
    );
}
