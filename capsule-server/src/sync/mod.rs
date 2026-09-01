//! The sync feed: what changed in a library, and the cursor that resumes it (slice `S-C2`).
//!
//! # The transport changed; the contract did not
//!
//! The retired feed was a `capsule.sync.v1` gRPC service (plus gRPC-web) served from the server
//! root. This is REST, per design/api-surfaces.md — but the cursor, the `sync_seq` monotonicity
//! and the `original_held` completeness fact are the same contract, and the clients that check
//! them are unchanged.
//!
//! # It reads, and that is all
//!
//! The feed owns no state. Positions come from [`crate::index::AssetIndex`], manifest bytes
//! come from [`crate::blob::BlobStore`], and the cursor is a pure function of a position and a
//! key. That is what makes the whole surface testable without a container, and it is also what
//! keeps `S-C30` enforceable: the module that serves a manifest has no way to *make* one.
//!
//! # Where the manifest comes from
//!
//! `S-C30` settled that the signed manifest rides as a `provenance` blob, so an entry's
//! manifest is the blob's bytes read back and emitted verbatim. There is no re-serialization
//! step to get wrong — the retired server had one, `prepare_feed_input`, and it produced bytes
//! carrying neither `device_sig` nor `write_sig`, so a receiving client could not run
//! `verify_asset` on a feed entry at all.
//!
//! **What this port does not do, and it is `S-C30`'s open question, not an oversight:** the
//! server never parses the provenance blob. It gates publication on the blob being *present*,
//! not on its agreeing with the envelope projection it validated at upload. A device authorized
//! to write can therefore upload a well-formed envelope beside manifest bytes that say
//! something else, and the server will serve both. The client catches it — `verify_asset` runs
//! over the bytes as received and the signatures are over the manifest, not the projection — so
//! the failure is *detected* rather than *prevented*, which is the key-free server's normal
//! position. Closing it means teaching the server to parse signed CBOR and to reach the device
//! directory's public keys, and that is a slice of its own.

pub mod cursor;

use std::sync::Arc;

pub use self::cursor::{CURSOR_KEY_LEN, CursorCodec, CursorError};
use crate::blob::BlobStore;
use crate::index::AssetIndex;

/// How many entries a page carries when the client does not say.
///
/// 100: a page is bounded by its manifests, and a signed manifest is a few hundred bytes, so a
/// default page is tens of kilobytes — the "discovering a thousand new assets costs a few
/// hundred kilobytes" figure the design doc quotes, at ten round trips.
pub const DEFAULT_PAGE_SIZE: usize = 100;

/// The largest page a client may ask for.
///
/// A ceiling rather than a suggestion: the feed reads one blob per entry, so an unbounded page
/// size is an unbounded amount of server work bought with one request.
pub const MAX_PAGE_SIZE: usize = 500;

/// The largest provenance blob the feed will inline.
///
/// A signed manifest is small; anything approaching this is a bug or an abuse, and the upload
/// ceiling should have refused it long before. The cap exists so that "something got past the
/// ceiling" degrades into one loud log line and an entry without a manifest, rather than the
/// feed reading an arbitrary blob into memory once per entry per client.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// Everything the feed reads from, as one injectable value.
#[derive(Debug, Clone)]
pub struct SyncContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    cursors: Arc<CursorCodec>,
}

impl SyncContext {
    /// Assembles the feed from its collaborators.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        cursors: Arc<CursorCodec>,
    ) -> Self {
        Self {
            index,
            blobs,
            cursors,
        }
    }

    /// The asset index (`S-C37`) — the only source of positions and entries.
    pub fn index(&self) -> &dyn AssetIndex {
        self.index.as_ref()
    }

    /// The blob store (`S-C35`) — where a manifest's bytes are read from, verbatim.
    pub fn blobs(&self) -> &dyn BlobStore {
        self.blobs.as_ref()
    }

    /// The cursor codec.
    pub fn cursors(&self) -> &CursorCodec {
        &self.cursors
    }
}

/// Clamp a requested page size into the range this server serves.
///
/// A request for zero or for more than [`MAX_PAGE_SIZE`] is **clamped, not refused**: a page
/// size is a hint about batching, and failing a sync because a client asked for one entry too
/// many would turn a performance preference into an outage. The client learns the real size by
/// counting what it got, which it has to be able to do anyway.
pub fn clamp_page_size(requested: Option<usize>) -> usize {
    match requested {
        None => DEFAULT_PAGE_SIZE,
        Some(0) => 1,
        Some(size) => size.min(MAX_PAGE_SIZE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_size_is_clamped_rather_than_refused() {
        assert_eq!(clamp_page_size(None), DEFAULT_PAGE_SIZE);
        assert_eq!(clamp_page_size(Some(0)), 1);
        assert_eq!(clamp_page_size(Some(7)), 7);
        assert_eq!(clamp_page_size(Some(MAX_PAGE_SIZE)), MAX_PAGE_SIZE);
        assert_eq!(clamp_page_size(Some(usize::MAX)), MAX_PAGE_SIZE);
    }
}
