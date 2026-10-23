use super::*;

#[test]
fn the_listener_carries_the_configured_header_bound_and_keeps_connections_alive() {
    let config = http1(Duration::from_secs(7));
    assert_eq!(config.header_read_timeout, Some(Duration::from_secs(7)));
    assert!(config.keep_alive);
}

/// The upload protocol's own maximum chunk size.
///
/// Restated here only as the subject of the assertion below; the bound itself is owned by
/// the transfer upload design doc and enforced by the chunk route.
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

/// The federation cap is tighter than the backstop, and is the one the federation group gets.
///
/// A federation cap at or above the backstop would be a second number doing nothing.
#[test]
fn the_federation_cap_is_tighter_than_the_backstop() {
    const {
        assert!(
            MAX_FEDERATION_BODY_BYTES < MAX_REQUEST_BODY_BYTES,
            "the federation group's cap exists to be tighter than the transport backstop"
        );
    }
    assert_eq!(federation_body_size().limit, MAX_FEDERATION_BODY_BYTES);
}

/// The bounded cap sits strictly between the federation cap and the backstop.
#[test]
fn the_bounded_cap_is_between_the_federation_cap_and_the_backstop() {
    const {
        assert!(
            MAX_FEDERATION_BODY_BYTES < MAX_BOUNDED_BODY_BYTES,
            "the federation group's cap is the tightest one"
        );
        assert!(
            MAX_BOUNDED_BODY_BYTES < MAX_REQUEST_BODY_BYTES,
            "the bounded cap exists to be tighter than the transport backstop"
        );
    }
    assert_eq!(bounded_body_size().limit, MAX_BOUNDED_BODY_BYTES);
}

/// Every document bound a bounded route enforces itself stays reachable under the cap.
///
/// A document one byte past its route's own bound must reach that route, so the client gets
/// the route's coded refusal rather than a bare transport `413`. The roster rides as base64
/// inside JSON, so its body is the encoded length plus the wrapper; the others are bounded
/// on the bytes or the string the body carries, wrapped in at most a small JSON object.
#[test]
fn every_route_bound_under_the_bounded_cap_stays_reachable() {
    /// Room for the JSON object and field names a bounded document is wrapped in.
    const WRAPPER: u64 = 1024;
    let base64 = |decoded: usize| (decoded as u64).div_ceil(3) * 4;
    let bounds = [
        (
            "roster",
            base64(crate::routes::roster::MAX_ROSTER_BYTES + 1),
        ),
        (
            "directory",
            crate::directory::MAX_DIRECTORY_BYTES as u64 + 1,
        ),
        ("escrow", crate::escrow::MAX_ESCROW_BYTES as u64 + 1),
        (
            "upgrade intent",
            crate::routes::upgrade::MAX_INTENT_BYTES as u64 + 1,
        ),
        (
            "relay payload",
            crate::enrollment::MAX_RELAY_BYTES as u64 + 1,
        ),
        (
            "storage verification",
            (crate::verify::MAX_ASSETS_PER_REQUEST
                * (64 + crate::verify::MAX_BLOBS_PER_ASSET * (64 + 3))) as u64,
        ),
    ];
    for (name, body) in bounds {
        assert!(
            body + WRAPPER < MAX_BOUNDED_BODY_BYTES,
            "a {name} body past its own bound ({body} bytes) must reach its route"
        );
    }
}
