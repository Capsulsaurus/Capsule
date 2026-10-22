//! [`ChunkBody`] — the opaque ciphertext a `PATCH` carries, read within a deadline.
//!
//! The raw-bytes mechanism, and the reason a raw-bytes body needs one at all, is
//! [`crate::body`]: the chunk body is `application/octet-stream`, and its media-type and
//! transport rejections carry `error.upload.*` codes because that is the surface a client is on
//! when it gets one.
//!
//! # The read deadline, and why it sits here (#717)
//!
//! transfer/upload.md bounds every connection: "header, body-read and idle timeouts close a
//! connection that sends too slowly". The header and idle halves are the listener's
//! ([`crate::limits::http1`]). The body-read half is this extractor's, because of what a chunk
//! handler does *after* the body has arrived.
//!
//! The chunk that completes a session runs finalization in the same handler: a claim, a full
//! re-hash of the staged file, a commit and a ledger append. Kynos's own `Timeout` interceptor
//! bounds the whole handler, so mounting it on the chunk routes would also bound finalization,
//! and dropping that future part-way leaves a session claimed with nothing committed. The
//! deadline therefore has to end before finalization can begin — and the one place that is true
//! by construction is the extractor, which reads the whole body before the handler's first line
//! runs. Nothing here can reach a store, so nothing dropped here can leave one half-written.
//!
//! The deadline counts from the moment the body read starts, not from the request's arrival:
//! the head is the listener's to bound, and an authentication lookup that ran first is not time
//! the client spent sending.
//!
//! Both chunk routes take this type — the album path and the guest drop — so both are bounded
//! and both declare the `408`.

use std::future::Future;
use std::time::Duration;

use capsule_i18n::error_codes;
use kynos::di::Provides;
use kynos::extract::FromRequest;
use kynos::extract::describe::Describe;
use kynos::extract::media::OctetStream;
use kynos::http::Request;
use kynos::prelude::*;
use kynos::router::operation::OperationCx;

use super::UploadContext;
use crate::body::{CodedMedia, OpaqueBody, OpaqueBodyRejection};

impl CodedMedia for OctetStream {
    const UNSUPPORTED_MEDIA_TYPE: &'static str = error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE;
    const UNREADABLE: &'static str = error_codes::UPLOAD_MALFORMED_REQUEST;
}

/// The opaque ciphertext bytes of one chunk.
///
/// The payload is literally ciphertext; anything but `application/octet-stream` is a client
/// that has misunderstood what it is sending.
#[derive(Debug)]
pub struct ChunkBody(OpaqueBody<OctetStream>);

impl ChunkBody {
    /// The bytes, borrowed.
    pub fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
}

/// Why a chunk body could not be taken.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ChunkBodyRejection {
    /// The `Content-Type` was absent or was not `application/octet-stream`.
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

    /// The body did not finish arriving before the chunk read deadline.
    #[error("the chunk body did not arrive before its deadline")]
    #[problem(status = 408, title = "Request timeout")]
    TimedOut {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl From<OpaqueBodyRejection> for ChunkBodyRejection {
    fn from(rejection: OpaqueBodyRejection) -> Self {
        match rejection {
            OpaqueBodyRejection::UnsupportedMediaType { code } => {
                Self::UnsupportedMediaType { code }
            }
            OpaqueBodyRejection::Unreadable { code } => Self::Unreadable { code },
        }
    }
}

/// The read did not finish before its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlinePassed;

/// Drive `read` to completion, or give up once `timeout` has passed.
///
/// A zero `timeout` is a deadline that passed before the read began, and refuses without
/// polling `read` at all. Configuration refuses zero, so no deployment reaches that arm; it is
/// what lets one in-process server — whose request bodies are always whole — produce the
/// refusal it documents (see [`super::policy::ChunkReadDeadline`]).
///
/// On refusal `read` is dropped, and with it the request body: hyper stops reading the
/// connection and closes it once the response is written, which is the "close a connection
/// that sends too slowly" half of the contract.
pub async fn read_within<F: Future>(
    timeout: Duration,
    read: F,
) -> Result<F::Output, DeadlinePassed> {
    if timeout.is_zero() {
        return Err(DeadlinePassed);
    }
    tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| DeadlinePassed)
}

impl<C> FromRequest<C> for ChunkBody
where
    C: Provides<UploadContext> + Sync,
{
    type Rejection = ChunkBodyRejection;

    async fn from_request(request: Request, context: &C) -> Result<Self, Self::Rejection> {
        let timeout = context.provide().policy().chunk_read_timeout();
        let path = request.uri().path().to_owned();
        let started = std::time::Instant::now();
        let Ok(read) = read_within(
            timeout,
            OpaqueBody::<OctetStream>::from_request(request, context),
        )
        .await
        else {
            tracing::info!(
                %path,
                timeout_ms = timeout.as_millis(),
                code = error_codes::UPLOAD_CHUNK_TIMEOUT,
                "a chunk was refused: its body did not arrive before the read deadline"
            );
            return Err(ChunkBodyRejection::TimedOut {
                code: error_codes::UPLOAD_CHUNK_TIMEOUT,
            });
        };
        let body = read?;
        tracing::trace!(
            %path,
            bytes = body.bytes().len(),
            elapsed_ms = started.elapsed().as_millis(),
            "a chunk body arrived"
        );
        Ok(Self(body))
    }
}

/// The description is Kynos's own: the body is raw bytes of one media type.
impl Describe for ChunkBody {
    fn describe(operation: &mut OperationCx<'_>) {
        OpaqueBody::<OctetStream>::describe(operation);
    }
}

#[cfg(test)]
mod tests;
