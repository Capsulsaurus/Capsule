//! The opaque, server-MAC'd sync cursor (validation invariant 22).
//!
//! # What it is for
//!
//! [Download & Sync — Discovering What
//! Changed](../../../capsule-docs/src/content/docs/design/import/download-sync.md) makes the
//! cursor opaque to clients and authenticated by the server on every call, "so a client cannot
//! forge or mutate a cursor **and a cursor lifted from another context is rejected at the
//! boundary**". The MAC is the *authenticity* layer only; the anti-rewind layer is the client's
//! own per-album high-water mark, which is the half a malicious server cannot defeat.
//!
//! # The owner is in the MAC, not in the cursor
//!
//! The retired implementation MAC'd `version || feed_seq` and nothing else, so the second half
//! of that sentence was never true: a validly-MAC'd cursor from one account authenticated
//! perfectly for another. Under a single global `feed_seq` that was close to harmless — a
//! foreign cursor was just some position in a shared ordering.
//!
//! It is **not** harmless here. `S-C37` mints sequence numbers **per owner**, so position 500
//! means a different point in every library, and an unbound cursor would let a client skip
//! past its own unseen entries by presenting someone else's. Binding is therefore a
//! correctness requirement of the new sequence design and not merely compliance with the
//! sentence.
//!
//! The owner is bound by being **MAC input**, never a cursor field: the server already knows
//! who is asking, from the credential. That keeps the cursor 41 bytes, keeps an account
//! identifier out of a token clients hand around, and still makes a foreign cursor fail
//! verification rather than decode into a position.
//!
//! # Layout
//!
//! `version(1) || position(8, big-endian u64) || hmac_sha256(32)`, base64url without padding on
//! the wire. Opaque: no client parses this, and the encoding may change behind the version byte
//! without a protocol version bump.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;

use crate::store::OwnerId;

/// Cursor wire-format version. Bumped only on an incompatible layout change; an unknown
/// version is [`CursorError::Malformed`], never a best-effort parse.
const CURSOR_VERSION: u8 = 1;

/// `version(1) || position(8)`.
const PAYLOAD_LEN: usize = 9;

/// HMAC-SHA256 tag length.
const TAG_LEN: usize = 32;

/// The full authenticated cursor, before base64.
const CURSOR_LEN: usize = PAYLOAD_LEN + TAG_LEN;

/// The server-only MAC key length.
pub const CURSOR_KEY_LEN: usize = 32;

/// Why a cursor was not accepted.
///
/// Every variant maps to the same client-facing rejection — `error.sync.cursor_invalid` — and
/// the distinction is diagnostic only. Telling a caller *which* way its cursor failed tells a
/// forger which byte to change next.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorError {
    /// Not base64url, not [`CURSOR_LEN`] bytes, or an unknown version byte.
    #[error("the cursor is malformed")]
    Malformed,
    /// The MAC did not verify: forged, mutated, from another key, or from another owner.
    #[error("the cursor did not authenticate")]
    NotAuthentic,
}

/// Mints and verifies opaque sync cursors under a server-only key.
///
/// `Debug` is hand-written: the derive would print the key material behind [`hmac::Key`].
pub struct CursorCodec {
    key: hmac::Key,
}

impl std::fmt::Debug for CursorCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CursorCodec(<key redacted>)")
    }
}

impl CursorCodec {
    /// Build a codec over the server-only MAC key.
    ///
    /// The key is `SYNC_CURSOR_MAC_KEY`, or HKDF-derived from `JWT_ED25519_DER` when that is
    /// unset — which is why rotating the signing key silently rotates this one, and every live
    /// cursor with it (design/guides/self-hosting.md). A client whose cursor stops
    /// authenticating re-syncs from the beginning; nothing is lost, but a rotation is a
    /// full-resync event and the operator should know that before performing one.
    pub fn new(key: &[u8; CURSOR_KEY_LEN]) -> Self {
        Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
        }
    }

    /// The bytes the tag is taken over: the payload, then the owner it is issued to.
    ///
    /// A length prefix would matter if a second variable-length field were ever appended; there
    /// is not one, and the fixed-width payload comes *first*, so no two `(position, owner)`
    /// pairs share a MAC input.
    fn signed_bytes(payload: &[u8], owner: &OwnerId) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(payload.len() + owner.as_str().len());
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(owner.as_str().as_bytes());
        bytes
    }

    /// Mint the cursor that resumes `owner`'s feed after `position`.
    pub fn encode(&self, owner: &OwnerId, position: u64) -> String {
        let mut payload = Vec::with_capacity(CURSOR_LEN);
        payload.push(CURSOR_VERSION);
        payload.extend_from_slice(&position.to_be_bytes());
        let tag = hmac::sign(&self.key, &Self::signed_bytes(&payload, owner));
        payload.extend_from_slice(tag.as_ref());
        URL_SAFE_NO_PAD.encode(&payload)
    }

    /// Recover the position a cursor names, for `owner`.
    ///
    /// An **absent or empty** cursor is the first-sync sentinel and decodes to `0`, which is
    /// why sequence numbers start at 1: "I have seen nothing" and "resume after 0" are the same
    /// request, so a client needs no special first-call shape.
    ///
    /// # Errors
    ///
    /// [`CursorError`] when the cursor is not well-formed or does not authenticate for `owner`.
    pub fn decode(&self, owner: &OwnerId, cursor: Option<&str>) -> Result<u64, CursorError> {
        let Some(cursor) = cursor.filter(|value| !value.is_empty()) else {
            return Ok(0);
        };
        let raw = URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| CursorError::Malformed)?;
        if raw.len() != CURSOR_LEN {
            return Err(CursorError::Malformed);
        }
        let (payload, tag) = raw.split_at(PAYLOAD_LEN);
        if payload[0] != CURSOR_VERSION {
            return Err(CursorError::Malformed);
        }
        // Verified *before* the position is read, so a tampered position is never briefly a
        // value this function has computed with.
        hmac::verify(&self.key, &Self::signed_bytes(payload, owner), tag)
            .map_err(|_| CursorError::NotAuthentic)?;
        let position = u64::from_be_bytes(
            payload[1..PAYLOAD_LEN]
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        );
        Ok(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec(seed: u8) -> CursorCodec {
        CursorCodec::new(&[seed; CURSOR_KEY_LEN])
    }

    #[test]
    fn a_cursor_round_trips_its_position() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        for position in [0_u64, 1, 42, u64::MAX] {
            let cursor = codec.encode(&owner, position);
            assert_eq!(codec.decode(&owner, Some(&cursor)), Ok(position));
        }
    }

    #[test]
    fn no_cursor_is_the_first_sync_sentinel() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        assert_eq!(codec.decode(&owner, None), Ok(0));
        assert_eq!(codec.decode(&owner, Some("")), Ok(0));
    }

    #[test]
    fn another_owners_cursor_does_not_authenticate() {
        let codec = codec(1);
        let mine = OwnerId::new("owner-1");
        let theirs = OwnerId::new("owner-2");
        let cursor = codec.encode(&theirs, 500);
        assert_eq!(
            codec.decode(&mine, Some(&cursor)),
            Err(CursorError::NotAuthentic),
            "a cursor lifted from another library authenticated, and per-owner sequence \
             numbers make that a way to skip your own unseen entries"
        );
    }

    #[test]
    fn another_servers_cursor_does_not_authenticate() {
        let owner = OwnerId::new("owner-1");
        let cursor = codec(2).encode(&owner, 7);
        assert_eq!(
            codec(1).decode(&owner, Some(&cursor)),
            Err(CursorError::NotAuthentic)
        );
    }

    #[test]
    fn every_mutation_of_a_cursor_is_refused() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        let cursor = codec.encode(&owner, 9);
        let raw = URL_SAFE_NO_PAD
            .decode(&cursor)
            .expect("the codec emits base64url");

        for byte in 0..raw.len() {
            let mut tampered = raw.clone();
            tampered[byte] ^= 0x01;
            let encoded = URL_SAFE_NO_PAD.encode(&tampered);
            assert!(
                codec.decode(&owner, Some(&encoded)).is_err(),
                "flipping a bit of byte {byte} produced a cursor the server accepted"
            );
        }
    }

    #[test]
    fn a_cursor_of_the_wrong_shape_is_malformed_not_unauthentic() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        assert_eq!(
            codec.decode(&owner, Some("not base64!!")),
            Err(CursorError::Malformed)
        );
        assert_eq!(
            codec.decode(&owner, Some(&URL_SAFE_NO_PAD.encode([0_u8; 8]))),
            Err(CursorError::Malformed)
        );

        // An unknown version is malformed rather than a best-effort parse: the layout behind
        // the byte is allowed to change completely.
        let mut future = vec![CURSOR_VERSION + 1];
        future.extend_from_slice(&[0_u8; CURSOR_LEN - 1]);
        assert_eq!(
            codec.decode(&owner, Some(&URL_SAFE_NO_PAD.encode(&future))),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn the_codec_never_prints_its_key() {
        let rendered = format!("{:?}", codec(0xAB));
        assert_eq!(rendered, "CursorCodec(<key redacted>)");
    }
}
