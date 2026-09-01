//! The transport limits the server applies before a handler is entered (slice `S-C33`).
//!
//! # Why this module exists
//!
//! Kynos 0.1.0 renders `#[schema(min_length / max_length)]` into the emitted document but does
//! **not** enforce it on the request path — verified while porting the auth surface: a request
//! with an empty password reached the handler. The constraints were removed rather than
//! published as validation the server does not perform, because a generated client trusts the
//! document. What is added back here is the control that genuinely belongs at this layer and is
//! genuinely enforced: a cap on how many bytes a request body may be.
//!
//! # Kynos already has one
//!
//! [`BodySize`] is Kynos's own interceptor, and it is used rather than reimplemented. It is
//! worth saying why it is the right one and not merely the available one: an interceptor
//! declares the responses it can produce as an associated type, so mounting it **contributes
//! `413` to every operation it covers**. Configuring the limit and documenting that the limit
//! exists are one action, which is the exact failure `S-C28` catalogued — thirteen Salvo
//! response variants rendering statuses the published schema never declared — made
//! unrepresentable. A hand-written `tower::Layer` doing the same job would have declared
//! nothing.
//!
//! Its behaviour, from Kynos's own documentation: a request that declares a `Content-Length`
//! larger than the cap is refused **before a byte of the body is read**, and one within the cap
//! streams through untouched. A request declaring no length — a chunked upload — is read frame
//! by frame and abandoned the moment the running count passes the cap.
//!
//! # The number
//!
//! 32 MiB, and it is a **backstop**, not the protocol's own rule.
//!
//! The largest body the wire contract will ever legitimately carry is one upload chunk, which
//! [Upload Protocol — Bounds](../../capsule-docs/src/content/docs/design/import/upload-protocol.md)
//! caps at **16 MiB**. That bound is protocol surface and the protocol answers a breach of it
//! itself, with `413 error.upload.chunk_too_large` — a coded, localizable rejection that tells a
//! client its chunking is wrong. Setting this cap *at* 16 MiB would make that rejection
//! unreachable and replace a diagnosis with a bare status.
//!
//! So the cap sits at twice the largest legitimate body: high enough that every refusal a client
//! can act on still comes from the route that understands it, and low enough that no body a
//! handler is ever handed can be used to exhaust memory. When `S-C1` lands the chunk route, the
//! protocol bound is enforced there and this stays what it is — the floor below which nothing
//! reaches a handler at all.
//!
//! # What it is not
//!
//! It is not a per-field length check. Whether the `#[schema]` constraints are re-declared once
//! Kynos enforces them, or stay a handler concern, is the other half of `S-C33` and is not
//! decided here: nothing in this module makes the document promise a check the server skips,
//! which is the property that mattered.

use kynos::middleware::limits::BodySize;

/// The largest request body any operation will accept, in bytes.
///
/// See the module docs for why it is twice the upload protocol's 16 MiB chunk bound rather than
/// equal to it.
pub const MAX_REQUEST_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// The body-size interceptor the router mounts.
///
/// One function rather than a literal at the mount site so the cap has a name, a place to be
/// documented, and one value the tests assert against.
pub fn body_size() -> BodySize {
    BodySize::new(MAX_REQUEST_BODY_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The upload protocol's own maximum chunk size.
    ///
    /// Restated here only as the subject of the assertion below; the bound itself is owned by
    /// the upload-protocol design doc and enforced by `S-C1`'s route.
    const PROTOCOL_MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

    /// The cap is a backstop, not the protocol's rule.
    ///
    /// If it ever falls to the chunk bound or below, `error.upload.chunk_too_large` stops being
    /// reachable and an over-chunking client gets a bare status instead of a diagnosis.
    #[test]
    fn the_cap_leaves_the_protocol_rejection_reachable() {
        const {
            assert!(
                MAX_REQUEST_BODY_BYTES > PROTOCOL_MAX_CHUNK_BYTES,
                "the transport backstop must sit above the protocol bound the upload route \
                 answers for itself"
            );
        }
        assert_eq!(body_size().limit, MAX_REQUEST_BODY_BYTES);
    }
}
