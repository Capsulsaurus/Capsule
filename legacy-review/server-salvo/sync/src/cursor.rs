//! The opaque, server-MAC'd sync cursor (threat-model invariant 22; SSoT:
//! [Download & Sync — Discovering What Changed](https://docs/design/import/download-sync/#discovering-what-changed)).
//!
//! The cursor is opaque to clients and authenticated by the server on every `Sync` call: it
//! carries a feed position under an HMAC-SHA256 tag keyed by a server-only key, so a forged
//! or mutated cursor — or one lifted from another context — is rejected at the boundary. The
//! MAC is the *authenticity* layer only; the independent per-album `sync_seq` monotonicity
//! check (client-side) is the anti-rewind layer.
//!
//! Wire layout (opaque): `version(1) || feed_seq(8, big-endian i64) || hmac_sha256(32)`.

use ring::hmac;
use thiserror::Error;

/// Cursor wire-format version (bumped only on an incompatible layout change).
const CURSOR_VERSION: u8 = 1;
/// `version(1) || feed_seq(8)`.
const PAYLOAD_LEN: usize = 9;
/// HMAC-SHA256 tag length.
const TAG_LEN: usize = 32;
/// Full authenticated cursor length.
const CURSOR_LEN: usize = PAYLOAD_LEN + TAG_LEN;

/// A cursor that failed to authenticate. Both variants map to the same client-facing
/// rejection (`error.sync.cursor_invalid`) — the distinction is diagnostic only.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CursorError {
    /// Wrong length or an unknown version byte.
    #[error("malformed cursor")]
    Malformed,
    /// The MAC did not verify (forged, mutated, or from another key/context).
    #[error("cursor MAC verification failed")]
    BadMac,
}

/// Encodes and verifies opaque sync cursors under a server-only HMAC key.
#[derive(Clone)]
pub struct CursorCodec {
    key: hmac::Key,
}

impl CursorCodec {
    /// Build a codec from the server-only 32-byte MAC key.
    #[must_use]
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
        }
    }

    /// Encode a feed position into an opaque, authenticated cursor.
    #[must_use]
    pub fn encode(&self, feed_seq: i64) -> Vec<u8> {
        let mut payload = Vec::with_capacity(CURSOR_LEN);
        payload.push(CURSOR_VERSION);
        payload.extend_from_slice(&feed_seq.to_be_bytes());
        let tag = hmac::sign(&self.key, &payload);
        payload.extend_from_slice(tag.as_ref());
        payload
    }

    /// Verify a cursor and recover its feed position. An empty cursor is the first-sync
    /// sentinel and decodes to `0` (before any entry). Any tamper is rejected.
    pub fn decode(&self, cursor: &[u8]) -> Result<i64, CursorError> {
        if cursor.is_empty() {
            return Ok(0);
        }
        if cursor.len() != CURSOR_LEN {
            return Err(CursorError::Malformed);
        }
        let (payload, tag) = cursor.split_at(PAYLOAD_LEN);
        if payload[0] != CURSOR_VERSION {
            return Err(CursorError::Malformed);
        }
        hmac::verify(&self.key, payload, tag).map_err(|_| CursorError::BadMac)?;
        let seq = i64::from_be_bytes(
            payload[1..PAYLOAD_LEN]
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        );
        if seq < 0 {
            return Err(CursorError::Malformed);
        }
        Ok(seq)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn codec() -> CursorCodec {
        CursorCodec::new(&[7u8; 32])
    }

    #[test]
    fn round_trips_a_position() {
        let c = codec();
        for seq in [0i64, 1, 42, i64::MAX] {
            let cursor = c.encode(seq);
            assert_eq!(cursor.len(), CURSOR_LEN);
            assert_eq!(c.decode(&cursor).unwrap(), seq);
        }
    }

    #[test]
    fn empty_cursor_is_the_start() {
        assert_eq!(codec().decode(&[]).unwrap(), 0);
    }

    #[test]
    fn a_flipped_bit_is_rejected() {
        let c = codec();
        let mut cursor = c.encode(99);
        // Flip a bit in the MAC region.
        let last = cursor.len() - 1;
        cursor[last] ^= 0x01;
        assert_eq!(c.decode(&cursor), Err(CursorError::BadMac));
    }

    #[test]
    fn a_mutated_position_is_rejected() {
        let c = codec();
        let mut cursor = c.encode(5);
        // Bump the feed_seq bytes without re-MACing.
        cursor[PAYLOAD_LEN - 1] ^= 0xFF;
        assert_eq!(c.decode(&cursor), Err(CursorError::BadMac));
    }

    #[test]
    fn a_foreign_key_is_rejected() {
        let cursor = codec().encode(10);
        let other = CursorCodec::new(&[9u8; 32]);
        assert_eq!(other.decode(&cursor), Err(CursorError::BadMac));
    }

    #[test]
    fn a_truncated_cursor_is_rejected() {
        let c = codec();
        let cursor = c.encode(1);
        assert_eq!(
            c.decode(&cursor[..CURSOR_LEN - 1]),
            Err(CursorError::Malformed)
        );
    }
}
