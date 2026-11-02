use super::*;

fn hash(byte: u8) -> Hash32 {
    Hash32([byte; 32])
}

/// A head at `head` that has superseded nothing yet.
fn at(head: Hash32, deleted: bool) -> AssetHead {
    AssetHead {
        head,
        superseded: Vec::new(),
        deleted,
        amk_version: 1,
        metadata_blob: Some(hash(9)),
    }
}

#[tokio::test]
async fn the_in_memory_store_conforms() {
    conformance::run_all(&InMemoryPulledAlbums::new()).await;
}

#[test]
fn advancing_a_head_remembers_what_it_superseded() {
    let start = at(hash(1), false);
    let same = start.advanced(at(hash(1), true));
    assert_eq!(same.head, hash(1));
    assert!(
        same.superseded.is_empty(),
        "the same head supersedes nothing"
    );
    assert!(same.deleted);

    let next = start.advanced(AssetHead {
        amk_version: 2,
        metadata_blob: Some(hash(8)),
        ..at(hash(2), false)
    });
    assert_eq!(next.head, hash(2));
    assert_eq!(next.superseded, vec![hash(1)]);
    assert_eq!(next.amk_version, 2, "the epoch is the new record's");
    assert_eq!(next.metadata_blob, Some(hash(8)));

    // History is the held head's, whatever the successor was built with.
    let after = next.advanced(AssetHead {
        superseded: vec![hash(7)],
        ..at(hash(3), false)
    });
    assert_eq!(after.superseded, vec![hash(1), hash(2)]);
}

#[test]
fn the_superseded_heads_are_bounded() {
    let mut head = at(Hash32([0; 32]), false);
    for step in 1..=200_u16 {
        let mut bytes = [0_u8; 32];
        bytes[..2].copy_from_slice(&step.to_be_bytes());
        head = head.advanced(at(Hash32(bytes), false));
    }
    assert_eq!(head.superseded.len(), MAX_SUPERSEDED_HEADS);
    let mut newest = [0_u8; 32];
    newest[..2].copy_from_slice(&199_u16.to_be_bytes());
    assert_eq!(head.superseded.last(), Some(&Hash32(newest)));
}
