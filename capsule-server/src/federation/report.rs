//! The signed federated moderation report, and what verifies one (`S-C49`).
//!
//! # Why a report is signed rather than authenticated
//!
//! A peer filing a report holds no capability on this server — it is reporting *this* server's
//! content, not pulling it — so there is no bearer to present and nothing to check a bearer
//! against. What there is, is the peer's operational Ed25519 key, which an operator has pinned
//! ([`PeerStore::pin`](super::PeerStore::pin)). So the report carries its own signature and the
//! route verifies it against that pinned key: intake is unauthenticated in the HTTP sense and
//! attributed in every sense that matters.
//!
//! That is also why intake is not TOFU. Fetching a peer's key at the moment it first files would
//! make the first report from a new server the thing that decides whether to trust that server.
//!
//! # What is signed
//!
//! The canonical CBOR of [`ReportClaim`] — every field of the report except the signature — so
//! the bytes a peer signs are reproducible from the body this server received and nothing about
//! JSON key order or number formatting can change them. Canonical CBOR is the same encoding
//! every other signed document in Capsule uses, and `capsule-core` owns it.
//!
//! Replay is bounded by the `(reporting_server, reported_user)` budget rather than by a nonce:
//! a replayed report is a duplicate row in an operator's queue, not an action, and a nonce table
//! would be a second store for a threat whose worst outcome is a duplicate.

use serde::{Deserialize, Serialize};

/// The report's signed payload: every field except the signature.
///
/// Canonical CBOR sorts a map's keys, so the encoding depends on the field *names* and their
/// values and not on the order they are declared in here — which is what lets a peer implement
/// the format from the design doc rather than from this file. Renaming a field, adding one, or
/// changing one's type is still a breaking change to every peer, and
/// `tests::the_signing_bytes_are_stable` pins the encoding so it fails here rather than in the
/// field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportClaim {
    /// The peer filing the report, as its own `server-info` names it.
    pub reporting_server: String,
    /// The account on the receiving server the report is about.
    pub reported_user: String,
    /// The content address of the asset complained about.
    pub asset_hash: String,
    /// The album it was pulled from.
    pub album_id: String,
    /// A short reason, where the peer gives one.
    pub reason: Option<String>,
    /// When the peer says it was reported, RFC 3339.
    pub reported_at: String,
}

/// Why a report was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    /// The claim could not be encoded, which can only be this server's own fault.
    #[error("the report's signing bytes could not be produced")]
    Unencodable,
    /// The signature does not verify under the peer's pinned key.
    #[error("the report's signature does not verify")]
    NotAuthentic,
}

impl ReportClaim {
    /// The exact bytes a peer signs.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError::Unencodable`] if the claim does not serialize, which nothing a
    /// peer can send causes: every field is a string.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, ReportError> {
        capsule_core::cbor::to_canonical_vec(self).map_err(|error| {
            tracing::error!(%error, "a federated report's claim did not serialize");
            ReportError::Unencodable
        })
    }

    /// Whether `signature` is this claim's, under `key`.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError::NotAuthentic`] when it is not. No part of the signature or the key
    /// is logged: which check failed is the whole of what is safe to say.
    pub fn verify(&self, signature: &[u8], key: &[u8; 32]) -> Result<(), ReportError> {
        let bytes = self.signing_bytes()?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.as_slice())
            .verify(&bytes, signature)
            .map_err(|_| {
                tracing::info!(
                    from = %self.reporting_server,
                    "a federated report's signature did not verify under the pinned key"
                );
                ReportError::NotAuthentic
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim() -> ReportClaim {
        ReportClaim {
            reporting_server: "other.test".to_owned(),
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
        let mutations: [fn(&mut ReportClaim); 7] = [
            |claim| claim.reporting_server = "third.test".to_owned(),
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
            "a666726561736f6e646373616d68616c62756d5f6964782430313866336631652d346237612d376339642d386532662d3161326233633464356536306a61737365745f686173687840616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616b7265706f727465645f617474323032362d30392d30325430303a30303a30305a6d7265706f727465645f75736572782430313933376237632d303030302d373030302d383030302d303030303030303030306230707265706f7274696e675f7365727665726a6f746865722e74657374",
            "if this changed, every peer's signature changed with it"
        );
    }

    fn hex_of(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        bytes.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }
}
