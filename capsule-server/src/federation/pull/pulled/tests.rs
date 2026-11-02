use super::*;
use crate::federation::PeerId;
use crate::store::AlbumId;

fn job() -> JobKey {
    JobKey::new(PeerId::new("home.test"), AlbumId::new("album"))
}

fn hash(byte: u8) -> Hash32 {
    Hash32([byte; 32])
}

fn page(high_water: u64) -> ValidatedPage {
    ValidatedPage {
        entries: Vec::new(),
        metadata_blobs: BTreeMap::from([(hash(9), b"meta".to_vec())]),
        state: PulledAlbum {
            cursor: Some(format!("c{high_water}")),
            high_water: Some(high_water),
            ..PulledAlbum::default()
        },
    }
}

#[tokio::test]
async fn an_album_never_pulled_reads_as_the_start() {
    let store = InMemoryPulledAlbums::new();
    assert_eq!(
        store.read(&job()).await.expect("read"),
        PulledAlbum::default()
    );
}

#[tokio::test]
async fn a_commit_lands_only_from_the_position_it_was_validated_at() {
    let store = InMemoryPulledAlbums::new();
    assert!(store.commit(&job(), None, page(3)).await.expect("commit"));
    assert_eq!(store.read(&job()).await.expect("read").high_water, Some(3));
    assert_eq!(
        store.metadata_blob(&job(), &hash(9)),
        Some(b"meta".to_vec())
    );

    // Validated from the start, but the album has moved to 3 since.
    assert!(!store.commit(&job(), None, page(5)).await.expect("commit"));
    assert_eq!(store.read(&job()).await.expect("read").high_water, Some(3));

    assert!(
        store
            .commit(&job(), Some(3), page(5))
            .await
            .expect("commit")
    );
    assert_eq!(store.read(&job()).await.expect("read").high_water, Some(5));
}

#[tokio::test]
async fn a_restart_forgets_the_position_and_keeps_the_heads() {
    let store = InMemoryPulledAlbums::new();
    let mut first = page(4);
    first.state.assets.insert(
        "asset".into(),
        AssetHead {
            head: hash(1),
            superseded: Vec::new(),
            deleted: false,
        },
    );
    first.state.amk_version = Some(2);
    assert!(store.commit(&job(), None, first).await.expect("commit"));

    store.restart(&job()).await.expect("restart");
    let state = store.read(&job()).await.expect("read");
    assert_eq!(state.cursor, None);
    assert_eq!(state.high_water, None);
    assert_eq!(state.amk_version, Some(2));
    assert_eq!(state.assets["asset"].head, hash(1));
}

#[test]
fn advancing_a_head_remembers_what_it_superseded() {
    let start = AssetHead {
        head: hash(1),
        superseded: Vec::new(),
        deleted: false,
    };
    let same = start.advanced(hash(1), true);
    assert_eq!(same.head, hash(1));
    assert!(
        same.superseded.is_empty(),
        "the same head supersedes nothing"
    );
    assert!(same.deleted);

    let next = start.advanced(hash(2), false);
    assert_eq!(next.head, hash(2));
    assert_eq!(next.superseded, vec![hash(1)]);
}

#[test]
fn the_superseded_heads_are_bounded() {
    let mut head = AssetHead {
        head: Hash32([0; 32]),
        superseded: Vec::new(),
        deleted: false,
    };
    for step in 1..=200_u16 {
        let mut bytes = [0_u8; 32];
        bytes[..2].copy_from_slice(&step.to_be_bytes());
        head = head.advanced(Hash32(bytes), false);
    }
    assert_eq!(head.superseded.len(), MAX_SUPERSEDED_HEADS);
    let mut newest = [0_u8; 32];
    newest[..2].copy_from_slice(&199_u16.to_be_bytes());
    assert_eq!(head.superseded.last(), Some(&Hash32(newest)));
}
