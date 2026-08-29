//! [`ChunkBody`] — the opaque bytes a `PATCH` carries, with a rejection this crate owns.
//!
//! # Why not `Binary<OctetStream>` directly
//!
//! Kynos's [`Binary`] is exactly the right extractor: it enforces the media type before the
//! handler runs and describes the body as raw bytes rather than as a JSON schema. Its
//! *rejection* is the problem. Kynos's [`BodyRejection`](kynos::error::rejection::BodyRejection)
//! is shared by every body extractor, so it
//! declares `400`, `415` **and `422`** — and a binary body cannot produce a `422`, because
//! there is no schema for bytes to violate. Two consequences, both bad:
//!
//! - the emitted document promises a response the server cannot send, which is the `S-C28`
//!   defect this rebuild exists to make unrepresentable — and `assert_declared_responses_covered`
//!   fails on it, correctly;
//! - the `415` it does send is a bare RFC 9457 problem with **no `error.*` code** (`S-C36`), so
//!   a client cannot localize the one rejection the strictness table names by code:
//!   `error.upload.unsupported_media_type`.
//!
//! # What this is
//!
//! A newtype that delegates the *work* to [`Binary`] — the media-type enforcement and the
//! description are Kynos's, unchanged — and replaces only the rejection with one this crate
//! owns. The result declares the two statuses a raw-bytes body can actually produce, and both
//! carry their catalog code.
//!
//! It holds the [`Binary`] rather than its `Bytes` so that no `bytes` dependency appears in
//! this crate's manifest and, more to the point, so that a 16 MiB chunk is not copied on its
//! way to the append.

use capsule_i18n::error_codes;
use kynos::extract::FromRequest;
use kynos::extract::body::binary::Binary;
use kynos::extract::describe::Describe;
use kynos::extract::media::OctetStream;
use kynos::http::Request;
use kynos::prelude::*;
use kynos::router::operation::OperationCx;

/// The opaque ciphertext bytes of one chunk.
#[derive(Debug)]
pub struct ChunkBody(Binary<OctetStream>);

impl ChunkBody {
    /// The bytes, borrowed.
    pub fn bytes(&self) -> &[u8] {
        &self.0.bytes
    }
}

/// Why the chunk's body could not be taken.
///
/// Two variants, because a raw-bytes body has two ways to fail and no third.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ChunkBodyRejection {
    /// The `Content-Type` was absent or was not `application/octet-stream`.
    ///
    /// The payload is literally opaque ciphertext; anything else is a client that has
    /// misunderstood what it is sending.
    #[error("a chunk body must be application/octet-stream")]
    #[problem(status = 415, title = "Unsupported media type")]
    UnsupportedMediaType {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The body could not be read to its end.
    #[error("the chunk body could not be read")]
    #[problem(status = 400, title = "Malformed request")]
    Unreadable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl<C: Sync> FromRequest<C> for ChunkBody {
    type Rejection = ChunkBodyRejection;

    async fn from_request(request: Request, context: &C) -> Result<Self, Self::Rejection> {
        Binary::<OctetStream>::from_request(request, context)
            .await
            .map(Self)
            .map_err(|rejection| {
                if rejection.status() == kynos::http::StatusCode::UNSUPPORTED_MEDIA_TYPE {
                    ChunkBodyRejection::UnsupportedMediaType {
                        code: error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE,
                    }
                } else {
                    // A transport failure part-way through the body. `Binary` cannot produce a
                    // schema failure, so this is the only other shape.
                    tracing::info!(%rejection, "a chunk body did not arrive whole");
                    ChunkBodyRejection::Unreadable {
                        code: error_codes::UPLOAD_MALFORMED_REQUEST,
                    }
                }
            })
    }
}

/// The description is Kynos's own: the body is raw bytes of one media type, and saying so is
/// [`Binary`]'s job, not this newtype's.
impl Describe for ChunkBody {
    fn describe(operation: &mut OperationCx<'_>) {
        Binary::<OctetStream>::describe(operation);
    }
}
