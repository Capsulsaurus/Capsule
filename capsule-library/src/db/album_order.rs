//! The one album order every native app and the CLI list container albums in (SSoT:
//! design/organization § Album Order and Recency).
//!
//! **Most recent asset first.** An album ranks by the latest *effective* capture time among its
//! assets that are neither hidden nor trashed: the index's `COALESCE(capture_utc,
//! capture_timestamp)`, which the write path and a rebuild both project from the sidecar's
//! effective capture (the user's correction over the as-imported value). An album with no such
//! asset ranks by its creation time, read from its UUIDv7 id so it is the same on every device
//! and across an index rebuild. An album whose id carries no creation time (the derived default
//! album) and holds no visible asset ranks after every dated album. Ties are broken by ascending
//! `album_id`.
//!
//! Recency is derived, never stored: [`DatabaseDriver::album_recency`](super::DatabaseDriver::album_recency)
//! reads it from the index on demand, so imports, moves, deletes, restores and capture-date
//! edits change it with nothing to keep in step.

use std::cmp::Reverse;
use std::collections::HashMap;

use uuid::Uuid;

/// The latest effective capture (Unix seconds) of each album's visible, untrashed assets, keyed
/// by the album id as the index stores it. An album absent from the map has no such asset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumRecency {
    latest_capture: HashMap<String, i64>,
}

impl AlbumRecency {
    /// Recency over `latest_capture` (album id → latest effective capture, Unix seconds).
    pub fn from_latest_captures(latest_capture: HashMap<String, i64>) -> Self {
        Self { latest_capture }
    }

    /// The latest effective capture among `album_id`'s visible, untrashed assets, if it has any.
    pub fn latest_capture(&self, album_id: &str) -> Option<i64> {
        self.latest_capture.get(album_id).copied()
    }

    /// The instant `album_id` ranks by: its latest capture, else its creation time, else `None`
    /// (ranked after every album that has one).
    pub fn rank(&self, album_id: &str) -> Option<i64> {
        self.latest_capture(album_id)
            .or_else(|| album_creation_secs(album_id))
    }

    /// Sort `items` into the album order: by [`rank`](Self::rank), latest first and undated
    /// last, then by ascending `album_id`.
    pub fn sort<T>(&self, items: &mut [T], album_id: impl Fn(&T) -> String) {
        items.sort_by_cached_key(|item| {
            let id = album_id(item);
            (Reverse(self.rank(&id)), id)
        });
    }
}

/// An album's creation time (Unix seconds), read from its UUIDv7 id; `None` for an id that
/// carries none (a derived or non-v7 id, or one that is not a UUID).
pub fn album_creation_secs(album_id: &str) -> Option<i64> {
    let id = Uuid::parse_str(album_id).ok()?;
    if id.get_version_num() != 7 {
        return None;
    }
    let (secs, _nanos) = id.get_timestamp()?.to_unix();
    i64::try_from(secs).ok()
}

#[cfg(test)]
mod tests;
