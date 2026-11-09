//! [`ChunkBody`] — the opaque ciphertext a `PATCH` carries, read under a stall bound.
//!
//! The raw-bytes mechanism, and the reason a raw-bytes body needs one at all, is
//! [`crate::body`]: the chunk body is `application/octet-stream`, and its media-type and
//! transport rejections carry `error.upload.*` codes because that is the surface a client is on
//! when it gets one.
//!
//! # The read pace, and why it sits here (#717, #1041)
//!
//! transfer/upload.md bounds every connection: "header and idle timeouts close a connection
//! that sends too slowly", and a chunk body is cut when it **stalls** — no bytes for the stall
//! bound — or falls below a minimum sustained rate. It is never cut for its total duration, so a
//! link that sustains the minimum rate finishes any chunk. The header and idle halves are the
//! listener's ([`crate::limits::http1`]). The body-read half is this extractor's, because of
//! what a chunk handler does *after* the body has arrived.
//!
//! The chunk that completes a session starts finalization from the same handler. Kynos's own
//! `Timeout` interceptor bounds the whole handler, so mounting it on the chunk routes would also
//! bound that, and it is a total-duration bound besides. The cut therefore has to end before the
//! handler can begin — and the one place that is true by construction is the extractor, which
//! reads the whole body before the handler's first line runs. Nothing here can reach a store, so
//! nothing dropped here can leave one half-written.
//!
//! The pace counts from the moment the body read starts, not from the request's arrival: the
//! head is the listener's to bound, and an authentication lookup that ran first is not time the
//! client spent sending. [`PaceMeter`] is the decision, a pure function of the bytes seen and
//! when; [`read_paced`] is the read loop that consults it.
//!
//! Both chunk routes take this type — the album path and the guest drop — so both are bounded
//! and both declare the `408`.
//!
//! # A body with no declared length
//!
//! The extractor only bounds a body that reaches it unread. One that declares a
//! `Content-Length` does: Kynos's body-size interceptor ([`crate::limits::body_size`]) checks
//! the declared length and passes the body through untouched. One that declares **none** — a
//! chunked HTTP/1 upload, or an HTTP/2 stream without the header — does not: that interceptor
//! reads it whole, frame by frame against its byte cap and with no clock, before any extractor
//! runs, so a stalled length-less chunk would never be cut at all.
//!
//! [`LengthlessChunkDeadline`] closes that path. Mounted **outside** the body-size cap on the two
//! chunk groups only, it reads a length-less body itself, under the same pace and up to just
//! past the same cap, and hands the bytes on. When the body is cut it hands on an empty body and
//! a [`ChunkReadExpired`] marker instead, and this extractor answers the marker with the same
//! `408`. One status with one producer, so the interceptor declares nothing of its own; and
//! still nothing past the body read is bounded, because the interceptor stops timing the moment
//! it hands on.

use std::convert::Infallible;

use bytes::{Bytes, BytesMut};
use capsule_i18n::error_codes;
use http_body_util::BodyExt as _;
use kynos::di::Provides;
use kynos::extract::FromRequest;
use kynos::extract::describe::Describe;
use kynos::http::Request;
use kynos::http::body::Body;
use kynos::http::media::OctetStream;
use kynos::middleware::{Continued, Interceptor, Next};
use kynos::prelude::*;
use kynos::router::operation::OperationCx;
use tokio::time::Instant;

use super::UploadContext;
use super::policy::ChunkPace;
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

    /// The body stalled, or arrived below the minimum sustained rate, and was cut.
    #[error("the chunk body stopped arriving")]
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

/// Why a chunk read was cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// No bytes arrived for the stall bound.
    Stalled,
    /// Bytes kept arriving, but below the minimum sustained rate.
    BelowMinimumRate,
}

impl Cut {
    /// The name the refusal is logged under.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stalled => "stalled",
            Self::BelowMinimumRate => "below_minimum_rate",
        }
    }
}

/// The cut decision for one body read: a pure function of the [`ChunkPace`], when the read
/// started, and when and how many bytes arrived since.
///
/// The read is cut at [`deadline`](Self::deadline), the earlier of two instants: one stall
/// bound after the last byte arrived, and the instant the bytes received fall behind the
/// minimum rate owed after the grace (see [`ChunkPace`]). [`verdict`](Self::verdict) is
/// `now >= deadline` named, so the read loop that sleeps until the deadline and the verdict it
/// then asks for can never disagree.
///
/// Nothing in it reads a clock: every instant is handed in, so the decision is tested over
/// hours of simulated trickle without waiting for any of it.
#[derive(Debug, Clone, Copy)]
pub struct PaceMeter {
    pace: ChunkPace,
    started: Instant,
    last_progress: Instant,
    received: u64,
}

impl PaceMeter {
    /// A meter for a read that starts at `now`.
    pub fn start(pace: ChunkPace, now: Instant) -> Self {
        Self {
            pace,
            started: now,
            last_progress: now,
            received: 0,
        }
    }

    /// Record `bytes` arriving at `now`. Zero bytes are not progress.
    pub fn on_bytes(&mut self, bytes: u64, now: Instant) {
        if bytes == 0 {
            return;
        }
        self.received = self.received.saturating_add(bytes);
        self.last_progress = now;
    }

    /// The bytes received so far.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// The instant one stall bound after the last byte.
    fn stall_at(&self) -> Instant {
        self.last_progress + self.pace.stall
    }

    /// The first instant the bytes received fall behind the rate owed, or `None` with no floor.
    ///
    /// The rate is owed from one stall bound into the read: at `t` past that grace,
    /// `min × t` bytes are owed, so `received` bytes stay ahead until `t` exceeds
    /// `received / min`. In whole milliseconds, the first `t` with `min × t > 1000 × received`
    /// is `⌊1000 × received / min⌋ + 1`.
    fn behind_at(&self) -> Option<Instant> {
        let rate = u128::from(self.pace.min_bytes_per_second);
        if rate == 0 {
            return None;
        }
        let ahead_ms = u128::from(self.received) * 1000 / rate + 1;
        let ahead = std::time::Duration::from_millis(u64::try_from(ahead_ms).unwrap_or(u64::MAX));
        self.started
            .checked_add(self.pace.stall)
            .and_then(|grace| grace.checked_add(ahead))
    }

    /// When the read is cut unless more bytes arrive first.
    pub fn deadline(&self) -> Instant {
        let stall_at = self.stall_at();
        self.behind_at()
            .map_or(stall_at, |behind| behind.min(stall_at))
    }

    /// Whether the read is cut at `now`, and why.
    ///
    /// # Errors
    ///
    /// The [`Cut`] once `now` has reached [`deadline`](Self::deadline): [`Cut::Stalled`] when the
    /// stall bound passed, otherwise [`Cut::BelowMinimumRate`].
    pub fn verdict(&self, now: Instant) -> Result<(), Cut> {
        if now >= self.stall_at() {
            return Err(Cut::Stalled);
        }
        if self.behind_at().is_some_and(|behind| now >= behind) {
            return Err(Cut::BelowMinimumRate);
        }
        Ok(())
    }
}

/// How a paced read of a body ended.
#[derive(Debug, PartialEq, Eq)]
pub enum PacedRead {
    /// The body ended, or crossed the byte limit, before it was cut: what arrived.
    Whole(Bytes),
    /// The body was cut, holding this many bytes, which are dropped.
    Cut {
        /// Why.
        cut: Cut,
        /// How many bytes had arrived.
        received: u64,
    },
    /// The transport failed part-way: what arrived before it did.
    Failed(Bytes),
}

/// The transport under a body failed part-way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportFailed;

/// Where a paced read takes its bytes from: a request [`Body`], or a test's scripted source.
///
/// A seam because Kynos keeps its body opaque — nothing outside it can build one whose frames
/// arrive over time — and the cut is only provable against bytes that do.
pub trait DataSource {
    /// The next data bytes, skipping any frame that carries none: `None` once the body ended.
    ///
    /// # Errors
    ///
    /// [`TransportFailed`] when the transport failed part-way.
    fn next_data(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Result<Bytes, TransportFailed>>> + Send;
}

impl DataSource for Body {
    async fn next_data(&mut self) -> Option<Result<Bytes, TransportFailed>> {
        loop {
            match self.frame().await? {
                Err(_) => return Some(Err(TransportFailed)),
                Ok(frame) => {
                    if let Ok(data) = frame.into_data() {
                        return Some(Ok(data));
                    }
                }
            }
        }
    }
}

/// Read `body` until it ends, fails, holds more than `limit` bytes, or is cut under `pace`.
///
/// Stopping just past `limit` keeps the buffer bounded and still leaves the body-size cap
/// inside something to refuse with its own `413`. A zero stall bound cuts before `body` is
/// polled at all: configuration refuses zero, so no deployment reaches that arm; it is what lets
/// one in-process server — whose request bodies are always whole — produce the refusal it
/// documents (see [`super::policy::ChunkReadPace`]).
///
/// On a cut `body` is dropped: hyper stops reading the connection and closes it once the
/// response is written, which is the "close a connection that sends too slowly" half of the
/// contract.
pub async fn read_paced<B: DataSource>(mut body: B, pace: ChunkPace, limit: u64) -> PacedRead {
    let mut meter = PaceMeter::start(pace, Instant::now());
    let mut collected = BytesMut::new();
    loop {
        if let Err(cut) = meter.verdict(Instant::now()) {
            return PacedRead::Cut {
                cut,
                received: meter.received(),
            };
        }
        // Waking at the deadline is not itself the cut: the verdict at the top of the loop is,
        // and it agrees with the deadline by construction.
        let Ok(next) = tokio::time::timeout_at(meter.deadline(), body.next_data()).await else {
            continue;
        };
        let data = match next {
            None => return PacedRead::Whole(collected.freeze()),
            Some(Err(TransportFailed)) => return PacedRead::Failed(collected.freeze()),
            Some(Ok(data)) => data,
        };
        meter.on_bytes(
            u64::try_from(data.len()).unwrap_or(u64::MAX),
            Instant::now(),
        );
        collected.extend_from_slice(&data);
        if meter.received() > limit {
            return PacedRead::Whole(collected.freeze());
        }
    }
}

impl<C> FromRequest<C> for ChunkBody
where
    C: Provides<UploadContext> + Sync,
{
    type Rejection = ChunkBodyRejection;

    async fn from_request(request: Request, context: &C) -> Result<Self, Self::Rejection> {
        let pace = context.provide().policy().chunk_pace();
        let path = request.uri().path().to_owned();
        let started = Instant::now();
        // A length-less body the interceptor that read it already cut.
        let expired = request.extensions().get::<ChunkReadExpired>().copied();
        let (parts, body) = request.into_parts();
        let read = match expired {
            Some(ChunkReadExpired { cut, received }) => PacedRead::Cut { cut, received },
            None => read_paced(body, pace, crate::limits::MAX_REQUEST_BODY_BYTES).await,
        };
        let bytes = match read {
            PacedRead::Whole(bytes) => bytes,
            PacedRead::Cut { cut, received } => {
                tracing::info!(
                    %path,
                    cut = cut.as_str(),
                    received,
                    elapsed_ms = started.elapsed().as_millis(),
                    stall_ms = pace.stall.as_millis(),
                    min_bytes_per_second = pace.min_bytes_per_second,
                    code = error_codes::UPLOAD_CHUNK_TIMEOUT,
                    "a chunk was refused: its body stopped arriving"
                );
                return Err(ChunkBodyRejection::TimedOut {
                    code: error_codes::UPLOAD_CHUNK_TIMEOUT,
                });
            }
            PacedRead::Failed(arrived) => {
                tracing::info!(
                    %path,
                    received = arrived.len(),
                    code = error_codes::UPLOAD_MALFORMED_REQUEST,
                    "a chunk body did not arrive whole: the transport failed part-way"
                );
                return Err(ChunkBodyRejection::Unreadable {
                    code: error_codes::UPLOAD_MALFORMED_REQUEST,
                });
            }
        };
        // The bytes are whole; the media type and the description stay `OpaqueBody`'s.
        let request = Request::from_parts(parts, Body::from_bytes(bytes));
        let body = OpaqueBody::<OctetStream>::from_request(request, context).await?;
        tracing::trace!(
            %path,
            bytes = body.bytes().len(),
            elapsed_ms = started.elapsed().as_millis(),
            "a chunk body arrived"
        );
        Ok(Self(body))
    }
}

/// The marker [`LengthlessChunkDeadline`] leaves on a request whose length-less body it cut.
/// [`ChunkBody`] answers it with the `408`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkReadExpired {
    /// Why the body was cut.
    pub cut: Cut,
    /// How many bytes had arrived.
    pub received: u64,
}

/// Reads a chunk body that declares no `Content-Length` under the chunk read pace, before the
/// body-size cap would read it with none. See the module docs.
///
/// Mounted first — outermost — on the groups holding `append_chunk` and `append_drop_chunk`, and
/// on no other: a length-less JSON body elsewhere is not a chunk and is not this pace's.
#[derive(Debug, Clone, Copy, Default)]
pub struct LengthlessChunkDeadline;

impl LengthlessChunkDeadline {
    /// The interceptor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl<C> Interceptor<C> for LengthlessChunkDeadline
where
    C: Provides<UploadContext> + Sync + 'static,
{
    type Reads = ();
    type Adds = ();
    type Short = Infallible;

    async fn intercept(
        &self,
        request: Request,
        reads: (),
        context: &C,
        next: Next<'_, C>,
    ) -> Result<Continued<()>, Infallible> {
        let () = reads;
        if request
            .headers()
            .contains_key(kynos::http::header::CONTENT_LENGTH)
        {
            // Passed through unread by the cap, and timed by the extractor.
            return Ok(next.run(request).await);
        }

        let upload: UploadContext = context.provide();
        let pace = upload.policy().chunk_pace();
        let (mut parts, body) = request.into_parts();
        let body = match read_paced(body, pace, crate::limits::MAX_REQUEST_BODY_BYTES).await {
            // A read that failed hands on what arrived before it, as the cap's own reader does:
            // the extractor beneath sees a short body and says so.
            PacedRead::Whole(bytes) | PacedRead::Failed(bytes) => {
                tracing::trace!(
                    path = %parts.uri.path(),
                    bytes = bytes.len(),
                    "a length-less chunk body arrived"
                );
                Body::from_bytes(bytes)
            }
            PacedRead::Cut { cut, received } => {
                // The extractor logs the refusal and answers it; the unread body is dropped
                // here, so hyper stops reading the connection and closes it after the response.
                parts.extensions.insert(ChunkReadExpired { cut, received });
                Body::empty()
            }
        };
        Ok(next.run(Request::from_parts(parts, body)).await)
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
