//! The `.well-known/capsule/*` registry — the attestation-key record (slice `S-C15`).
//!
//! # Public, and that is the point
//!
//! This is the only operation on the server with no `Auth`. A client pins these keys on first
//! contact and verifies receipts against them for the life of the assets they cover; a peer
//! checking a proof of loss has no account here at all. Requiring a credential to fetch the key
//! that checks the server's own liability would let the server decline to be checked.
//!
//! Nothing here is user-scoped. The registry's rule is explicit — *never a user list* — and this
//! record is server-scoped by construction: it carries an origin and public keys.
//!
//! # The history is append-only, and why that is the whole record
//!
//! A receipt names the key that signed it (`server_key_id`), so a key retired years ago must
//! still resolve or every receipt under it becomes unverifiable at once — which from outside is
//! indistinguishable from the server having forged them. Publishing only the *active* key would
//! make rotation a silent repudiation of everything signed before it.
//!
//! [`crate::attestation`] owns the history and derives the active key's entry from the signer,
//! so a server cannot publish a set that omits the key it is currently signing with.
//!
//! # The other registry records
//!
//! `server-info`, `revoked-jti` and `deprecation` are `S-C18`'s; `moved/{user}` is post-v1 with
//! Account Portability and is deliberately not served — it is the one record that would name a
//! user.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::attestation::AttestationContext;

/// The discovery surface: what a client or peer can learn without an account.
#[derive(Tag)]
#[tag(
    name = "well-known",
    description = "Public, server-scoped discovery records. Never a user list."
)]
pub struct WellKnownTag;

/// One published attestation key.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PublishedKeyResponse {
    /// The fingerprint a receipt's `server_key_id` selects on, lowercase hex.
    pub key_id: String,
    /// The hybrid public key, base64 (Ed25519 ‖ ML-DSA-65).
    pub public: String,
    /// The signature algorithm this key is used with.
    pub algorithm: String,
    /// When it began signing, RFC 3339.
    pub active_from: String,
    /// When it stopped, or absent while it is the active key.
    pub active_to: Option<String>,
}

/// The `.well-known/capsule/attestation-keys` record.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AttestationKeysResponse {
    /// This server's canonical origin — the other half of the binding that refuses a
    /// cross-server replay.
    pub server_id: String,
    /// Every key this server has signed with, oldest first, the active one last.
    pub keys: Vec<PublishedKeyResponse>,
}

/// The algorithm identifier the hybrid attestation signature is published under.
const ALGORITHM: &str = "hybrid-ed25519-mldsa65";

/// Serve this server's storage-attestation keys and their append-only history.
///
/// Cacheable and unauthenticated. It changes only when a key rotates, and a client that pinned
/// a stale copy still resolves every receipt signed before it fetched — which is the property
/// the append-only ordering buys.
#[kynos::get(
    "/.well-known/capsule/attestation-keys",
    operation_id = "attestation_keys",
    tag = WellKnownTag
)]
pub async fn attestation_keys(
    Inject(attestation): Inject<AttestationContext>,
) -> Json<AttestationKeysResponse> {
    Json(AttestationKeysResponse {
        server_id: attestation.signer().server_id().to_owned(),
        keys: attestation
            .history()
            .iter()
            .map(|key| PublishedKeyResponse {
                key_id: key.key_id.to_hex(),
                public: BASE64.encode(key.public.to_bytes()),
                algorithm: ALGORITHM.to_owned(),
                active_from: key.active_from.to_string(),
                active_to: key.active_to.map(|at| at.to_string()),
            })
            .collect(),
    })
}
