//! The puller's half of design/federation.md's Request Signatures: every capability this server
//! presents to a peer is signed with this server's operational key.
//!
//! The claim is [`RequestClaim`], the **same** type the verifying half in
//! [`scheme`](crate::federation::scheme) reads, so the bytes this server signs and the bytes it
//! verifies cannot drift apart: the vector `scheme`'s tests pin covers both directions. Every
//! request is signed afresh — a new nonce, the current second — because the issuer accepts each
//! signature once.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use jiff::Timestamp;

use crate::federation::scheme::{RequestClaim, RequestSignatureError};

/// What signs this server's capability presentations: its operational Ed25519 key, the one each
/// album's home server pinned for it.
pub trait RequestSigner: fmt::Debug + Send + Sync {
    /// The Ed25519 signature over `message`.
    fn sign(&self, message: &[u8]) -> [u8; 64];
}

/// [`RequestSigner`] over the operator's PKCS#8 Ed25519 key: the same bytes
/// [`CapabilityCodec::from_pkcs8`](crate::federation::CapabilityCodec::from_pkcs8) and the
/// session signer take, so the key a peer pinned from this server's `server-info` is the key
/// these signatures verify under.
pub struct Ed25519RequestSigner {
    pair: ring::signature::Ed25519KeyPair,
}

impl Ed25519RequestSigner {
    /// A signer over `pkcs8_der`.
    ///
    /// `from_pkcs8_maybe_unchecked` for the reason the session signer uses it: a v1 PKCS#8 key,
    /// which is what `openssl genpkey` writes, carries no public half to check.
    ///
    /// # Errors
    ///
    /// [`InvalidRequestKey`] when the bytes are not an Ed25519 private key.
    pub fn from_pkcs8(pkcs8_der: &[u8]) -> Result<Self, InvalidRequestKey> {
        ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8_der)
            .map(|pair| Self { pair })
            .map_err(|_| InvalidRequestKey)
    }

    /// The raw public key a peer verifies these signatures under.
    pub fn public_key(&self) -> [u8; 32] {
        use ring::signature::KeyPair as _;
        let mut key = [0_u8; 32];
        key.copy_from_slice(self.pair.public_key().as_ref());
        key
    }
}

impl fmt::Debug for Ed25519RequestSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ed25519RequestSigner(<redacted>)")
    }
}

impl RequestSigner for Ed25519RequestSigner {
    fn sign(&self, message: &[u8]) -> [u8; 64] {
        let mut signature = [0_u8; 64];
        signature.copy_from_slice(self.pair.sign(message).as_ref());
        signature
    }
}

/// The key handed to [`Ed25519RequestSigner::from_pkcs8`] is not an Ed25519 private key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the request-signing key is not a PKCS#8 Ed25519 private key")]
pub struct InvalidRequestKey;

/// The `X-Capsule-Request-Signature` value for presenting `token` on `method path`, signed at
/// `signed_at` with `nonce`.
///
/// `path` is the operation's own path (`/v1/sync`, `/v1/blob/{hash}` with the hash filled in),
/// never the base URL's prefix and never the query.
///
/// # Errors
///
/// [`RequestSignatureError::Unencodable`], which no string or integer argument can cause.
pub fn signature_header(
    signer: &dyn RequestSigner,
    method: &str,
    path: &str,
    token: &str,
    signed_at: Timestamp,
    nonce: &str,
) -> Result<String, RequestSignatureError> {
    let seconds = signed_at.as_second();
    let bytes = RequestClaim::new(method, path, token, seconds, nonce).signing_bytes()?;
    let signature = signer.sign(&bytes);
    Ok(format!(
        "t={seconds}, n={nonce}, s={}",
        BASE64URL.encode(signature)
    ))
}

/// A nonce no request of this process repeats: a UUIDv7's sixteen bytes, base64url — the
/// shortest the issuer accepts. Its instant is already in `t`, so it leaks nothing new.
pub fn fresh_nonce() -> String {
    BASE64URL.encode(uuid::Uuid::now_v7().as_bytes())
}

#[cfg(test)]
mod tests;
