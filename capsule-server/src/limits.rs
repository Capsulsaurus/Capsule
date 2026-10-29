//! The transport limits the server applies before a handler is entered.
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
//! exists are one action, which is the exact failure of the retired Salvo surface —
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
//! [Transfer — Upload: Chunk Rules](../../capsule-docs/src/content/docs/design/transfer/upload.md#chunk-rules-and-strictness)
//! caps at **16 MiB**. That bound is protocol surface and the protocol answers a breach of it
//! itself, with `413 error.upload.chunk_too_large` — a coded, localizable rejection that tells a
//! client its chunking is wrong. Setting this cap *at* 16 MiB would make that rejection
//! unreachable and replace a diagnosis with a bare status.
//!
//! So the cap sits at twice the largest legitimate body: high enough that every refusal a client
//! can act on still comes from the route that understands it, and low enough that no body a
//! handler is ever handed can be used to exhaust memory. The chunk route enforces the
//! protocol bound itself and this stays what it is — the floor below which nothing
//! reaches a handler at all.
//!
//! The backstop is now the cap only where nothing smaller is legitimate: the chunk routes and
//! the two writes whose bodies the protocol leaves unbounded. Every other operation carries
//! [`MAX_BOUNDED_BODY_BYTES`] and the federation surface [`MAX_FEDERATION_BODY_BYTES`] (#576).
//!
//! # What it is not
//!
//! It is not a per-field length check. Whether the `#[schema]` constraints are re-declared once
//! Kynos enforces them, or stay a handler concern, is not
//! decided here: nothing in this module makes the document promise a check the server skips,
//! which is the property that mattered.

use std::time::Duration;

use kynos::middleware::limits::body_size::BodySize;
use kynos::server::protocol::http1::Http1Config;

/// The largest request body any operation will accept, in bytes.
///
/// See the module docs for why it is twice the upload protocol's 16 MiB chunk bound rather than
/// equal to it.
pub const MAX_REQUEST_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// The body-size interceptor the groups whose bodies have no bound below an upload chunk mount.
///
/// Exactly six operations (#576): the four that carry chunk bytes or may — `create_upload`,
/// `append_chunk`, and the guest-drop pair `create_drop` and `append_drop_chunk` — so that
/// `413 error.upload.chunk_too_large` stays the route's to answer, and the two whose largest
/// legitimate body the protocol does not bound at all: `apply_op`, whose `metadata-update`
/// carries the whole encrypted sidecar inline (a blob the upload path otherwise sends as a
/// chunk, and whose size nothing caps), and `issue_share`, whose `serves` enumerates every blob
/// a whole-album link may serve. Every other operation mounts [`bounded_body_size`].
///
/// One function rather than a literal at the mount site so the cap has a name, a place to be
/// documented, and one value the tests assert against.
pub fn body_size() -> BodySize {
    BodySize::new(MAX_REQUEST_BODY_BYTES)
}

/// The cap on every operation whose largest legitimate body is bounded: **1 MiB** (#576).
///
/// The account, enrollment, album, upgrade, roster, verification and link-management writes,
/// and every read and exempt operation that is not a drop chunk. Their largest legitimate
/// bodies, measured from each request type and the bound its own module enforces:
///
/// | Operation | Largest legitimate body | Bound |
/// | --- | --- | --- |
/// | `publish_album_roster` | ≈ 683 KiB: base64 of a 512 KiB roster in a one-field JSON object | `routes::roster::MAX_ROSTER_BYTES` |
/// | `publish_device_directory` | 512 KiB raw CBOR | `directory::MAX_DIRECTORY_BYTES` |
/// | `verify_storage` | ≈ 216 KiB: 100 assets × 32 quoted 64-hex hashes | `verify::MAX_ASSETS_PER_REQUEST`, `MAX_BLOBS_PER_ASSET` |
/// | `relay_enrollment_payload` | 64 KiB payload string plus a direction | `enrollment::MAX_RELAY_BYTES` |
/// | `begin_album_upgrade` | 64 KiB raw CBOR intent | `routes::upgrade::MAX_INTENT_BYTES` |
/// | `store_escrow`, `store_sealed_identity` | 64 KiB raw blob | `escrow::MAX_ESCROW_BYTES` |
/// | everything else | a few KiB at most: credentials, a code, a hybrid key or signature, a manifest envelope | the request type's fixed fields |
///
/// 1 MiB is above the largest of them with room for the JSON a client wraps it in, so each
/// route's own coded refusal of a document one byte past its bound stays reachable — the same
/// reason the backstop sits above the chunk bound — and `tests` below pins that against every
/// bound in the table. It is a thirty-second of the backstop, so an unauthenticated
/// `register` or `login` can no longer be handed 32 MiB to parse before anything looks at it.
pub const MAX_BOUNDED_BODY_BYTES: u64 = 1024 * 1024;

/// The body-size interceptor the bounded groups mount, at [`MAX_BOUNDED_BODY_BYTES`].
pub fn bounded_body_size() -> BodySize {
    BodySize::new(MAX_BOUNDED_BODY_BYTES)
}

/// The cap on a federation write's body: **16 KiB**.
///
/// The transport backstop above is sized for a 16 MiB upload chunk. A federation write is
/// nothing like one — the largest legitimate body on that surface is a signed moderation report,
/// six short strings and a base64 signature, under a kilobyte in practice — and it matters
/// because `POST /v1/federation/reports` is this server's only **unauthenticated** write: under
/// the backstop an anonymous caller could hand it a body two thousand times larger than any real
/// report and have it parsed before anything looks at who is speaking.
///
/// # Why the cap is per group
///
/// Mounting a second [`BodySize`] on the federation group *under* a router-wide one is refused
/// by Kynos at compile time, correctly:
///
/// ```text
/// evaluation panicked: two interceptors covering this route answer with the same status;
/// a consumer could not tell which one replied
/// ```
///
/// Both answer `413`, and an operation cannot declare two producers of one status. So the cap is
/// not mounted on the router at all (#478): every group carries exactly one — the federation
/// group this one, the chunk-carrying and unbounded groups [`body_size`]'s backstop, and every
/// other group [`bounded_body_size`] (#576) — the twelve operations the protocol gate exempts
/// included, in groups that hold nothing but the cap. `413` therefore
/// stays declared on every operation, which `tests/conformance.rs` pins, and no route can be
/// mounted outside a group without losing it, which `tests/limits.rs` pins.
///
/// The field-length caps ([`crate::routes::federation`]) still run first in the handler, and the
/// [`CounterKey::FederatedIntake`](crate::counter::CounterKey::FederatedIntake) budget once the
/// signature verifies; this bounds what happens before either — the bytes parsed.
pub const MAX_FEDERATION_BODY_BYTES: u64 = 16 * 1024;

/// The body-size interceptor the federation group mounts, at [`MAX_FEDERATION_BODY_BYTES`].
pub fn federation_body_size() -> BodySize {
    BodySize::new(MAX_FEDERATION_BODY_BYTES)
}

/// The listener's HTTP/1 settings: the header-read and keep-alive idle bound (#717).
///
/// transfer/upload.md bounds every connection — "header, body-read and idle timeouts close a
/// connection that sends too slowly". This is the header and idle half; the body-read half is
/// the chunk extractor's ([`crate::upload::body`]), because only there can it end before
/// finalization begins.
///
/// # One timeout, two bounds
///
/// hyper's HTTP/1 driver arms its header-read timer each time the connection waits for a
/// request head — on the first request, and again after every kept-alive response. So the same
/// `header_read_timeout` both closes a client that dribbles its head and closes a kept-alive
/// connection that sits idle for that long, and a separate idle setting would have nothing to
/// configure. `tests/limits.rs` pins both behaviours on a socket.
///
/// Keep-alive itself stays on: a client uploading many chunks reuses one connection, and the
/// idle bound is what keeps that from being a way to hold one open.
///
/// # A connection that never sends a byte
///
/// Kynos reads a connection's first bytes to tell HTTP/1 from HTTP/2 before hyper's driver,
/// and so before hyper's header timer, exists. Kynos holds that first head — sniff included —
/// to this same `header_read_timeout`, counted from accept, so a client that connects and stays
/// silent is closed at the same bound (#795; pinned on a socket in `tests/limits.rs`).
pub fn http1(header_read_timeout: Duration) -> Http1Config {
    Http1Config::default()
        .keep_alive(true)
        .header_read_timeout(Some(header_read_timeout))
}

#[cfg(test)]
mod tests;
