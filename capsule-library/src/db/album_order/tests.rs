//! Test contract for the album order.
//!
//! - `an_album_ranks_by_its_latest_capture` — latest first, whatever the creation order.
//! - `an_empty_album_ranks_by_its_creation_time` — the UUIDv7 instant stands in.
//! - `an_undated_empty_album_ranks_last` — a derived id with no visible asset.
//! - `ties_break_by_ascending_album_id`
//! - `creation_time_is_read_only_from_a_v7_id`

use std::collections::HashMap;

use uuid::{NoContext, Timestamp, Uuid};

use super::*;

fn v7_at(secs: u64) -> String {
    Uuid::new_v7(Timestamp::from_unix(NoContext, secs, 0)).to_string()
}

/// A v8 (custom) UUID, the shape of the derived default-album id.
fn v8(bytes: [u8; 16]) -> String {
    uuid::Builder::from_custom_bytes(bytes)
        .into_uuid()
        .to_string()
}

fn recency(entries: &[(&str, i64)]) -> AlbumRecency {
    AlbumRecency::from_latest_captures(
        entries
            .iter()
            .map(|(id, at)| ((*id).to_string(), *at))
            .collect::<HashMap<_, _>>(),
    )
}

fn ordered(recency: &AlbumRecency, mut ids: Vec<String>) -> Vec<String> {
    recency.sort(&mut ids, Clone::clone);
    ids
}

#[test]
fn an_album_ranks_by_its_latest_capture() {
    // Created newest-first, but the oldest album holds the most recent asset.
    let old = v7_at(1_000);
    let new = v7_at(2_000);
    let r = recency(&[(&old, 5_000), (&new, 3_000)]);
    assert_eq!(
        ordered(&r, vec![new.clone(), old.clone()]),
        vec![old.clone(), new.clone()]
    );
    assert_eq!(r.rank(&old), Some(5_000));
}

#[test]
fn an_empty_album_ranks_by_its_creation_time() {
    let filled = v7_at(1_000);
    let empty_recent = v7_at(4_000);
    let empty_old = v7_at(500);
    let r = recency(&[(&filled, 3_000)]);
    assert_eq!(
        ordered(
            &r,
            vec![empty_old.clone(), filled.clone(), empty_recent.clone()]
        ),
        vec![empty_recent.clone(), filled, empty_old]
    );
    assert_eq!(r.latest_capture(&empty_recent), None);
    assert_eq!(r.rank(&empty_recent), Some(4_000));
}

#[test]
fn an_undated_empty_album_ranks_last() {
    // The default album's id is a derived v8 UUID: it carries no creation time.
    let derived = v8([0x5a; 16]);
    let dated = v7_at(1);
    let r = recency(&[]);
    assert_eq!(
        ordered(&r, vec![derived.clone(), dated.clone()]),
        vec![dated, derived.clone()]
    );
    // Holding a visible asset gives it the asset's rank like any other album.
    let r = recency(&[(&derived, 10)]);
    assert_eq!(r.rank(&derived), Some(10));
}

#[test]
fn ties_break_by_ascending_album_id() {
    let a = "00000000-0000-4000-8000-000000000001".to_string();
    let b = "00000000-0000-4000-8000-000000000002".to_string();
    let r = recency(&[(&a, 7), (&b, 7)]);
    assert_eq!(
        ordered(&r, vec![b.clone(), a.clone()]),
        vec![a.clone(), b.clone()]
    );
    // Two undated albums are ordered by id as well.
    assert_eq!(
        ordered(&recency(&[]), vec![b.clone(), a.clone()]),
        vec![a, b]
    );
}

#[test]
fn creation_time_is_read_only_from_a_v7_id() {
    assert_eq!(
        album_creation_secs(&v7_at(1_700_000_000)),
        Some(1_700_000_000)
    );
    assert_eq!(album_creation_secs(&Uuid::new_v4().to_string()), None);
    assert_eq!(album_creation_secs(&v8([7; 16])), None);
    assert_eq!(album_creation_secs("album-1"), None);
}
