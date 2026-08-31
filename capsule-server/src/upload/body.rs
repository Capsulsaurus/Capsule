//! [`ChunkBody`] — the opaque ciphertext a `PATCH` carries.
//!
//! The mechanism, and the reason a raw-bytes body needs one at all, is [`crate::body`]. This
//! module is one `impl` and a name: the chunk body is `application/octet-stream`, and its two
//! rejections carry `error.upload.*` codes because that is the surface a client is on when it
//! gets one.

use capsule_i18n::error_codes;
use kynos::extract::media::OctetStream;

use crate::body::{CodedMedia, OpaqueBody};

/// The opaque ciphertext bytes of one chunk.
///
/// The payload is literally ciphertext; anything but `application/octet-stream` is a client
/// that has misunderstood what it is sending.
pub type ChunkBody = OpaqueBody<OctetStream>;

impl CodedMedia for OctetStream {
    const UNSUPPORTED_MEDIA_TYPE: &'static str = error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE;
    const UNREADABLE: &'static str = error_codes::UPLOAD_MALFORMED_REQUEST;
}
