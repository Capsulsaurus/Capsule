//! The opaque, server-MAC'd sync cursor (validation invariant 22).
//!
//! # What it is for
//!
//! [Sync — Cursor Authenticity and
//! Anti-Rewind](../../../capsule-docs/src/content/docs/design/sync.md) makes the
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
//! # Scope (`S-C51`, `S-E5`)
//!
//! A cursor is issued for one of three shapes — the caller's own feed, one album's page read by
//! the caller as its owner or a member, or one album's page pulled by a federated peer — and all
//! three carry the owner's sequence numbers, so a cursor that crossed between them would skip
//! unseen entries exactly as a foreign one would. The shape is therefore MAC input too: the tag
//! is taken over `payload || len(reader) as u32 BE || reader || 0x00`, `… || 0x01 || album` for
//! an album page, or `… || 0x02 || album` for a peer's page, where the reader is the account or
//! the peer server. The reader is length-prefixed because a variable-length field follows it.
//! The version byte was **not** bumped: the wire layout below is unchanged, and a cursor minted
//! before the scope entered the MAC fails as `NotAuthentic` — a one-time full resync, the same
//! event a key rotation is, and indistinguishable from it to a client.
//!
//! # Layout
//!
//! Two layouts, base64url without padding on the wire:
//!
//! - version 1: `1 || position(8, big-endian u64) || hmac_sha256(32)`, for an owner at sync
//!   epoch 0;
//! - version 2: `2 || epoch(8, big-endian u64) || position(8) || hmac_sha256(32)`, for an owner
//!   whose epoch an index rebuild has moved (#697).
//!
//! The codec writes version 1 at epoch 0 and version 2 above it, so every cursor minted before
//! epochs existed is still the cursor this server mints, and a client is never made to resync by
//! a release. A version-1 cursor decodes at epoch 0. Opaque: no client parses this, and the
//! encoding may change behind the version byte without a protocol version bump.
//!
//! # The epoch is in the payload, not only in the MAC
//!
//! The scope is MAC input only, because a cursor from another scope is simply not authentic. A
//! cursor from an earlier epoch *is* authentic — this server minted it for this reader — and the
//! client has to be told something different about it: not "discard and start over because the
//! cursor is wrong" but "your library's numbering was rebuilt; resync from zero and keep what you
//! hold". So the epoch is readable after verification, and the route compares it
//! ([`CursorPosition::epoch`]).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;

use crate::federation::PeerId;
use crate::store::{AlbumId, OwnerId};

/// The layout an owner at epoch 0 is issued. An unknown version is [`CursorError::Malformed`],
/// never a best-effort parse.
const CURSOR_VERSION: u8 = 1;

/// The layout an owner above epoch 0 is issued: the epoch rides in the payload.
const EPOCH_CURSOR_VERSION: u8 = 2;

/// `version(1) || position(8)`.
const PAYLOAD_LEN: usize = 9;

/// `version(1) || epoch(8) || position(8)`.
const EPOCH_PAYLOAD_LEN: usize = 17;

/// HMAC-SHA256 tag length.
const TAG_LEN: usize = 32;

/// The full authenticated version-1 cursor, before base64.
const CURSOR_LEN: usize = PAYLOAD_LEN + TAG_LEN;

/// The full authenticated version-2 cursor, before base64.
const EPOCH_CURSOR_LEN: usize = EPOCH_PAYLOAD_LEN + TAG_LEN;

/// Where an authenticated cursor resumes: the owner's sync epoch it was minted under, and the
/// position in that epoch's numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    /// The owner's sync epoch when the cursor was minted. `0` for a version-1 cursor.
    pub epoch: u64,
    /// The last position the reader was handed; `0` for "nothing yet".
    pub position: u64,
}

/// The server-only MAC key length.
pub const CURSOR_KEY_LEN: usize = 32;

/// A generous guess at the album half of a MAC input: a scope byte and a hyphenated UUID.
const SCOPE_ESTIMATE: usize = 1 + 36;

/// Why a cursor was not accepted.
///
/// Every variant maps to the same client-facing rejection — `error.sync.cursor_invalid` — and
/// the distinction is diagnostic only. Telling a caller *which* way its cursor failed tells a
/// forger which byte to change next.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorError {
    /// Not base64url, not `CURSOR_LEN` bytes, or an unknown version byte.
    #[error("the cursor is malformed")]
    Malformed,
    /// The MAC did not verify: forged, mutated, from another key, or from another owner.
    #[error("the cursor did not authenticate")]
    NotAuthentic,
}

/// What a cursor is issued for: a caller's own feed, one album's page, or one album's page as a
/// federated peer pulls it (`S-C51`, `S-E5`).
///
/// Part of the MAC input, so a cursor minted for one shape cannot be presented on another:
/// positions are the owner's sequence numbers in every shape, and a cursor that crossed between
/// them would skip a reader's unseen entries. A peer is its own shape rather than an account
/// reading an album, because a peer id and an account id are different identifiers that could
/// spell the same string, and one scope byte is what keeps them structurally apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorScope<'a> {
    /// An account's own feed.
    Feed {
        /// The account the cursor was issued to.
        caller: &'a OwnerId,
    },
    /// One album's page, read by an account as its owner or a member.
    Album {
        /// The account the cursor was issued to.
        caller: &'a OwnerId,
        /// The album whose page it resumes.
        album: &'a AlbumId,
    },
    /// One album's page, pulled by a peer server under a capability.
    Peer {
        /// The peer the cursor was issued to.
        peer: &'a PeerId,
        /// The album whose page it resumes.
        album: &'a AlbumId,
    },
}

impl<'a> CursorScope<'a> {
    /// `caller`'s own feed.
    #[must_use]
    pub fn feed(caller: &'a OwnerId) -> Self {
        Self::Feed { caller }
    }

    /// `album`'s page, as read by `caller`.
    #[must_use]
    pub fn album(caller: &'a OwnerId, album: &'a AlbumId) -> Self {
        Self::Album { caller, album }
    }

    /// `album`'s page, as pulled by `peer`.
    #[must_use]
    pub fn peer(peer: &'a PeerId, album: &'a AlbumId) -> Self {
        Self::Peer { peer, album }
    }

    /// The identifier the cursor is bound to, and the scope byte and album that follow it.
    fn parts(&self) -> (&'a [u8], u8, Option<&'a AlbumId>) {
        match *self {
            Self::Feed { caller } => (caller.as_str().as_bytes(), 0, None),
            Self::Album { caller, album } => (caller.as_str().as_bytes(), 1, Some(album)),
            Self::Peer { peer, album } => (peer.as_str().as_bytes(), 2, Some(album)),
        }
    }
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
    /// cursor with it (guides/self-hosting.md). A client whose cursor stops
    /// authenticating re-syncs from the beginning; nothing is lost, but a rotation is a
    /// full-resync event and the operator should know that before performing one.
    pub fn new(key: &[u8; CURSOR_KEY_LEN]) -> Self {
        Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
        }
    }

    /// The bytes the tag is taken over: the payload, the length-prefixed reader, then the
    /// scope byte and the album when there is one.
    ///
    /// The reader is length-prefixed because a second variable-length field follows it:
    /// without the prefix `("ab", album "c")` and `("a", album "bc")` would share a MAC input.
    /// The scope byte keeps a feed cursor, an album cursor and a peer cursor apart even when
    /// the reader's bytes are the same.
    fn signed_bytes(payload: &[u8], scope: &CursorScope<'_>) -> Vec<u8> {
        let (reader, kind, album) = scope.parts();
        let mut bytes = Vec::with_capacity(payload.len() + 4 + reader.len() + SCOPE_ESTIMATE);
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(
            &u32::try_from(reader.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        bytes.extend_from_slice(reader);
        bytes.push(kind);
        if let Some(album) = album {
            bytes.extend_from_slice(album.as_str().as_bytes());
        }
        bytes
    }

    /// Mint the cursor that resumes `scope` after `position`, in the owner's sync `epoch`.
    ///
    /// Version 1 at epoch 0 and version 2 above it; see the module docs.
    pub fn encode(&self, scope: &CursorScope<'_>, epoch: u64, position: u64) -> String {
        let mut payload = Vec::with_capacity(EPOCH_CURSOR_LEN);
        if epoch == 0 {
            payload.push(CURSOR_VERSION);
        } else {
            payload.push(EPOCH_CURSOR_VERSION);
            payload.extend_from_slice(&epoch.to_be_bytes());
        }
        payload.extend_from_slice(&position.to_be_bytes());
        let tag = hmac::sign(&self.key, &Self::signed_bytes(&payload, scope));
        payload.extend_from_slice(tag.as_ref());
        URL_SAFE_NO_PAD.encode(&payload)
    }

    /// Recover the epoch and position a cursor names, for `scope`.
    ///
    /// An **absent or empty** cursor is the first-sync sentinel and decodes to `None`: it
    /// carries no epoch, so it is valid in every one, and resumes after position `0` — which is
    /// why sequence numbers start at 1: "I have seen nothing" and "resume after 0" are the same
    /// request, so a client needs no special first-call shape.
    ///
    /// # Errors
    ///
    /// [`CursorError`] when the cursor is not well-formed or does not authenticate for `scope`.
    pub fn decode(
        &self,
        scope: &CursorScope<'_>,
        cursor: Option<&str>,
    ) -> Result<Option<CursorPosition>, CursorError> {
        let Some(cursor) = cursor.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let raw = URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| CursorError::Malformed)?;
        let payload_len = match (raw.first(), raw.len()) {
            (Some(&CURSOR_VERSION), CURSOR_LEN) => PAYLOAD_LEN,
            (Some(&EPOCH_CURSOR_VERSION), EPOCH_CURSOR_LEN) => EPOCH_PAYLOAD_LEN,
            _ => return Err(CursorError::Malformed),
        };
        let (payload, tag) = raw.split_at(payload_len);
        // Verified *before* anything is read, so a tampered epoch or position is never briefly
        // a value this function has computed with.
        hmac::verify(&self.key, &Self::signed_bytes(payload, scope), tag)
            .map_err(|_| CursorError::NotAuthentic)?;
        let word = |at: usize| -> Result<u64, CursorError> {
            payload
                .get(at..at + 8)
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                .map(u64::from_be_bytes)
                .ok_or(CursorError::Malformed)
        };
        Ok(Some(if payload_len == PAYLOAD_LEN {
            CursorPosition {
                epoch: 0,
                position: word(1)?,
            }
        } else {
            CursorPosition {
                epoch: word(1)?,
                position: word(9)?,
            }
        }))
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
            let cursor = codec.encode(&CursorScope::feed(&owner), 0, position);
            assert_eq!(
                codec.decode(&CursorScope::feed(&owner), Some(&cursor)),
                Ok(Some(CursorPosition { epoch: 0, position }))
            );
        }
    }

    #[test]
    fn no_cursor_is_the_first_sync_sentinel() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        assert_eq!(codec.decode(&CursorScope::feed(&owner), None), Ok(None));
        assert_eq!(codec.decode(&CursorScope::feed(&owner), Some("")), Ok(None));
    }

    #[test]
    fn another_owners_cursor_does_not_authenticate() {
        let codec = codec(1);
        let mine = OwnerId::new("owner-1");
        let theirs = OwnerId::new("owner-2");
        let cursor = codec.encode(&CursorScope::feed(&theirs), 0, 500);
        assert_eq!(
            codec.decode(&CursorScope::feed(&mine), Some(&cursor)),
            Err(CursorError::NotAuthentic),
            "a cursor lifted from another library authenticated, and per-owner sequence \
             numbers make that a way to skip your own unseen entries"
        );
    }

    #[test]
    fn another_servers_cursor_does_not_authenticate() {
        let owner = OwnerId::new("owner-1");
        let cursor = codec(2).encode(&CursorScope::feed(&owner), 0, 7);
        assert_eq!(
            codec(1).decode(&CursorScope::feed(&owner), Some(&cursor)),
            Err(CursorError::NotAuthentic)
        );
    }

    #[test]
    fn every_mutation_of_a_cursor_is_refused() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        let cursor = codec.encode(&CursorScope::feed(&owner), 0, 9);
        let raw = URL_SAFE_NO_PAD
            .decode(&cursor)
            .expect("the codec emits base64url");

        for byte in 0..raw.len() {
            let mut tampered = raw.clone();
            tampered[byte] ^= 0x01;
            let encoded = URL_SAFE_NO_PAD.encode(&tampered);
            assert!(
                codec
                    .decode(&CursorScope::feed(&owner), Some(&encoded))
                    .is_err(),
                "flipping a bit of byte {byte} produced a cursor the server accepted"
            );
        }
    }

    #[test]
    fn an_album_cursor_and_a_feed_cursor_do_not_cross() {
        // Both carry the owner's sequence numbers, so a cursor that crossed between the two
        // shapes would skip a member's unseen entries. The scope is in the MAC input.
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        let album = AlbumId::new("album-1");
        let other = AlbumId::new("album-2");
        let on_album = codec.encode(&CursorScope::album(&owner, &album), 0, 5);
        assert_eq!(
            codec.decode(&CursorScope::album(&owner, &album), Some(&on_album)),
            Ok(Some(CursorPosition {
                epoch: 0,
                position: 5
            }))
        );
        assert_eq!(
            codec.decode(&CursorScope::feed(&owner), Some(&on_album)),
            Err(CursorError::NotAuthentic)
        );
        assert_eq!(
            codec.decode(&CursorScope::album(&owner, &other), Some(&on_album)),
            Err(CursorError::NotAuthentic)
        );
        let on_feed = codec.encode(&CursorScope::feed(&owner), 0, 5);
        assert_eq!(
            codec.decode(&CursorScope::album(&owner, &album), Some(&on_feed)),
            Err(CursorError::NotAuthentic)
        );
        // And the framing keeps `(caller, album)` pairs apart however the bytes split.
        let ab = codec.encode(
            &CursorScope::album(&OwnerId::new("ab"), &AlbumId::new("c")),
            0,
            1,
        );
        assert_eq!(
            codec.decode(
                &CursorScope::album(&OwnerId::new("a"), &AlbumId::new("bc")),
                Some(&ab)
            ),
            Err(CursorError::NotAuthentic)
        );
    }

    #[test]
    fn the_account_shapes_mac_input_is_the_one_issued_cursors_were_minted_under() {
        // Pinned as literals minted before the scope became an enum: the version byte was not
        // bumped, so every account cursor a client holds must still decode. A re-ordering of
        // the scope bytes would fail here rather than as a silent fleet-wide resync.
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        let album = AlbumId::new("album-1");
        assert_eq!(
            codec.encode(&CursorScope::feed(&owner), 0, 42),
            "AQAAAAAAAAAq0bh6pMbAFpAT3Awj06WzfE_5-4xMYAo6dRjCneIlXvw"
        );
        assert_eq!(
            codec.encode(&CursorScope::album(&owner, &album), 0, 42),
            "AQAAAAAAAAAqHUJ3cwdgPp9A4G9QbArkodTHyoCrzQ1H8tItJ1yio2M"
        );
    }

    #[test]
    fn a_peers_cursor_does_not_cross_into_an_accounts_even_under_the_same_bytes() {
        // A peer id and an account id could spell the same string; the scope byte is what keeps
        // the two cursors apart, and one peer's cursor is not another peer's.
        let codec = codec(1);
        let album = AlbumId::new("album-1");
        let peer = PeerId::new("same-bytes");
        let account = OwnerId::new("same-bytes");
        let on_peer = codec.encode(&CursorScope::peer(&peer, &album), 0, 5);
        assert_eq!(
            codec.decode(&CursorScope::peer(&peer, &album), Some(&on_peer)),
            Ok(Some(CursorPosition {
                epoch: 0,
                position: 5
            }))
        );
        assert_eq!(
            codec.decode(&CursorScope::album(&account, &album), Some(&on_peer)),
            Err(CursorError::NotAuthentic)
        );
        assert_eq!(
            codec.decode(&CursorScope::feed(&account), Some(&on_peer)),
            Err(CursorError::NotAuthentic)
        );
        assert_eq!(
            codec.decode(
                &CursorScope::peer(&PeerId::new("other.peer"), &album),
                Some(&on_peer)
            ),
            Err(CursorError::NotAuthentic)
        );
        let on_album = codec.encode(&CursorScope::album(&account, &album), 0, 5);
        assert_eq!(
            codec.decode(&CursorScope::peer(&peer, &album), Some(&on_album)),
            Err(CursorError::NotAuthentic)
        );
    }

    #[test]
    fn a_cursor_of_the_wrong_shape_is_malformed_not_unauthentic() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        assert_eq!(
            codec.decode(&CursorScope::feed(&owner), Some("not base64!!")),
            Err(CursorError::Malformed)
        );
        assert_eq!(
            codec.decode(
                &CursorScope::feed(&owner),
                Some(&URL_SAFE_NO_PAD.encode([0_u8; 8]))
            ),
            Err(CursorError::Malformed)
        );

        // An unknown version is malformed rather than a best-effort parse: the layout behind
        // the byte is allowed to change completely.
        let mut future = vec![EPOCH_CURSOR_VERSION + 1];
        future.extend_from_slice(&[0_u8; CURSOR_LEN - 1]);
        assert_eq!(
            codec.decode(
                &CursorScope::feed(&owner),
                Some(&URL_SAFE_NO_PAD.encode(&future))
            ),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn the_codec_never_prints_its_key() {
        let rendered = format!("{:?}", codec(0xAB));
        assert_eq!(rendered, "CursorCodec(<key redacted>)");
    }

    #[test]
    fn a_cursor_carries_the_epoch_it_was_minted_in() {
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        for (epoch, position) in [
            (0_u64, 0_u64),
            (0, 9),
            (1, 0),
            (3, 42),
            (u64::MAX, u64::MAX),
        ] {
            let cursor = codec.encode(&CursorScope::feed(&owner), epoch, position);
            assert_eq!(
                codec.decode(&CursorScope::feed(&owner), Some(&cursor)),
                Ok(Some(CursorPosition { epoch, position }))
            );
        }
    }

    #[test]
    fn epoch_zero_is_still_the_version_one_layout() {
        // Every cursor a client held before epochs existed is the cursor this server still
        // mints at epoch 0, so no release makes a client resync.
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        let raw = URL_SAFE_NO_PAD
            .decode(codec.encode(&CursorScope::feed(&owner), 0, 7))
            .expect("the codec emits base64url");
        assert_eq!((raw[0], raw.len()), (CURSOR_VERSION, CURSOR_LEN));
        let raw = URL_SAFE_NO_PAD
            .decode(codec.encode(&CursorScope::feed(&owner), 2, 7))
            .expect("the codec emits base64url");
        assert_eq!(
            (raw[0], raw.len()),
            (EPOCH_CURSOR_VERSION, EPOCH_CURSOR_LEN)
        );
    }

    #[test]
    fn every_mutation_of_an_epoch_cursor_is_refused() {
        // The epoch is under the MAC: a client cannot carry an old cursor into a new epoch by
        // editing it.
        let codec = codec(1);
        let owner = OwnerId::new("owner-1");
        let raw = URL_SAFE_NO_PAD
            .decode(codec.encode(&CursorScope::feed(&owner), 4, 9))
            .expect("the codec emits base64url");
        for byte in 0..raw.len() {
            let mut tampered = raw.clone();
            tampered[byte] ^= 0x01;
            assert!(
                codec
                    .decode(
                        &CursorScope::feed(&owner),
                        Some(&URL_SAFE_NO_PAD.encode(&tampered))
                    )
                    .is_err(),
                "flipping a bit of byte {byte} produced a cursor the server accepted"
            );
        }
    }
}
