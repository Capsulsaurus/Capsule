//! [`OpaqueBody`] — a raw-bytes request body whose rejections carry their catalog code.
//!
//! # Why not `Binary<M>` directly
//!
//! Kynos's [`Binary`] is exactly the right extractor: it enforces the media type before the
//! handler runs and describes the body as raw bytes rather than as a JSON schema. Its
//! *rejection* is the problem. [`BodyRejection`](kynos::error::rejection::BodyRejection) is
//! shared by every body extractor, so it declares `400`, `415` **and `422`** — and a raw-bytes
//! body cannot produce a `422`, because there is no schema for bytes to violate. Two
//! consequences, both bad:
//!
//! - the emitted document promises a response the server cannot send, which is the `S-C28`
//!   defect this rebuild exists to make unrepresentable — and `assert_declared_responses_covered`
//!   fails on it, correctly;
//! - the `415` it does send is a bare RFC 9457 problem with **no `error.*` code** (`S-C36`), so
//!   a client cannot localize a rejection the i18n contract says is localized by code.
//!
//! # What this is
//!
//! A newtype that delegates the *work* to [`Binary`] — the media-type enforcement and the
//! description are Kynos's, unchanged — and replaces only the rejection with one this crate
//! owns. The result declares the two statuses a raw-bytes body can actually produce, and both
//! carry their catalog code.
//!
//! # Where the codes come from
//!
//! From the media-type marker, through [`CodedMedia`]. The first version of this lived in
//! `upload::body` as a bespoke newtype for the chunk body; the device directory needed the same
//! thing with `error.directory.*` codes instead of `error.upload.*`, and two copies differing
//! only in two string constants is the point at which the constants belong to the type that
//! varies. A surface adding a third raw-bytes body writes an `impl CodedMedia` and nothing
//! else.
//!
//! It holds the [`Binary`] rather than its `Bytes` so that a 16 MiB chunk is not copied on its
//! way to the append.

use kynos::extract::FromRequest;
use kynos::extract::body::binary::Binary;
use kynos::extract::describe::Describe;
use kynos::extract::media::MediaType;
use kynos::http::Request;
use kynos::prelude::*;
use kynos::router::operation::OperationCx;

/// A media type that also names the catalog codes a body of it rejects with.
///
/// Two codes rather than one shared pair, because "you sent the wrong sort of thing" means
/// something different on each surface and the client switches on the code.
pub trait CodedMedia: MediaType {
    /// The code for a body whose `Content-Type` was absent or wrong.
    const UNSUPPORTED_MEDIA_TYPE: &'static str;
    /// The code for a body that did not arrive whole.
    const UNREADABLE: &'static str;
}

/// A raw-bytes body of media type `M`.
#[derive(Debug)]
pub struct OpaqueBody<M: CodedMedia>(Binary<M>);

impl<M: CodedMedia> OpaqueBody<M> {
    /// The bytes, borrowed.
    pub fn bytes(&self) -> &[u8] {
        &self.0.bytes
    }

    /// The bytes, owned.
    pub fn into_vec(self) -> Vec<u8> {
        self.0.into_inner().to_vec()
    }
}

/// Why a raw-bytes body could not be taken.
///
/// Two variants, because such a body has two ways to fail and no third.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum OpaqueBodyRejection {
    /// The `Content-Type` was absent or was not the one this operation takes.
    #[error("the request body is not the media type this operation accepts")]
    #[problem(status = 415, title = "Unsupported media type")]
    UnsupportedMediaType {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The body could not be read to its end.
    #[error("the request body could not be read")]
    #[problem(status = 400, title = "Malformed request")]
    Unreadable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl<C: Sync, M: CodedMedia + Send> FromRequest<C> for OpaqueBody<M> {
    type Rejection = OpaqueBodyRejection;

    async fn from_request(request: Request, context: &C) -> Result<Self, Self::Rejection> {
        Binary::<M>::from_request(request, context)
            .await
            .map(Self)
            .map_err(|rejection| {
                if rejection.status() == kynos::http::StatusCode::UNSUPPORTED_MEDIA_TYPE {
                    OpaqueBodyRejection::UnsupportedMediaType {
                        code: M::UNSUPPORTED_MEDIA_TYPE,
                    }
                } else {
                    // A transport failure part-way through the body. `Binary` cannot produce a
                    // schema failure, so this is the only other shape.
                    tracing::info!(%rejection, "a raw-bytes body did not arrive whole");
                    OpaqueBodyRejection::Unreadable {
                        code: M::UNREADABLE,
                    }
                }
            })
    }
}

/// The description is Kynos's own: the body is raw bytes of one media type, and saying so is
/// [`Binary`]'s job, not this newtype's.
impl<M: CodedMedia> Describe for OpaqueBody<M> {
    fn describe(operation: &mut OperationCx<'_>) {
        Binary::<M>::describe(operation);
    }
}
