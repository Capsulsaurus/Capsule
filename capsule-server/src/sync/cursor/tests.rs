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
