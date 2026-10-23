//! The signed federated moderation report, and what verifies one.
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
        verify_signed_report(&bytes, signature, key).inspect_err(|_| {
            tracing::info!(
                from = %self.reporting_server,
                "a federated report's signature did not verify under the pinned key"
            );
        })
    }
}

/// Whether `signature` covers `signed` under `key`.
///
/// The re-verification an operator does against a filed report, months after intake: it takes
/// the two byte strings the row carries and the peer's pinned key, and nothing that was derived
/// or normalized. Deliberately *not* a method on [`ReportClaim`] — reconstructing a claim from a
/// stored row is exactly the mistake this exists to make unnecessary.
///
/// # Errors
///
/// Returns [`ReportError::NotAuthentic`] when it does not.
pub fn verify_signed_report(
    signed: &[u8],
    signature: &[u8],
    key: &[u8; 32],
) -> Result<(), ReportError> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.as_slice())
        .verify(signed, signature)
        .map_err(|_| ReportError::NotAuthentic)
}

#[cfg(test)]
mod tests;
