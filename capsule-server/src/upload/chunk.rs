//! The chunk rules — the part of the upload contract a deployment may not move.
//!
//! [Upload Protocol — Chunk Rules and
//! Strictness](../../../capsule-docs/src/content/docs/design/import/upload-protocol.md) fixes
//! these for a protocol version: the 4 KiB alignment, the 16 MiB ceiling, the required
//! checksum's spelling, and what an offset means. They are constants and pure predicates here
//! rather than fields on [`super::UploadPolicy`] precisely because moving one is a protocol
//! break, not a configuration change — the date in `X-Capsule-Protocol` is what a client
//! checks them against.
//!
//! Everything in this module is total, pure and unit-tested, so the route reads as a sequence
//! of named decisions rather than a sequence of arithmetic.

/// The alignment every chunk but the last must satisfy, in bytes.
///
/// 4 KiB keeps server-side writes page-aligned and doubles as a tripwire: a client whose
/// offset arithmetic has drifted mis-aligns long before it corrupts anything.
pub const CHUNK_ALIGNMENT: u64 = 4096;

/// The largest chunk the protocol accepts, in bytes.
///
/// Protocol surface. The transport backstop in [`crate::limits`] deliberately sits *above*
/// this, so a breach is answered by this module's coded rejection rather than by a bare
/// framework `413`.
pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// The media type a chunk body must declare.
///
/// The payload is opaque ciphertext, so anything else is a client that has misunderstood what
/// it is sending. TUS v2's `application/partial-upload` is deliberately not accepted: we
/// implement v1 semantics with our own headers, and advertising v2's media type would claim a
/// compatibility we do not have.
pub const CHUNK_MEDIA_TYPE: &str = "application/octet-stream";

/// Where a `PATCH`'s offset sits relative to the session's acknowledged region.
///
/// The three cases are decided by arithmetic alone; distinguishing a *replay* from a
/// *conflict* inside [`Self::Behind`] needs the recorded chunk, which is a store read the
/// route performs — so that distinction is deliberately not made here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetPosition {
    /// Exactly at the acknowledged head: the only offset that may be written.
    AtHead,
    /// At or below an already-acknowledged byte — a replay, a conflict, or a stale offset.
    Behind,
    /// Past the acknowledged head: the client skipped ahead and left a gap.
    Ahead,
}

/// Where `offset` sits for a session that has acknowledged `received_bytes`.
pub fn classify_offset(offset: u64, received_bytes: u64) -> OffsetPosition {
    match offset.cmp(&received_bytes) {
        std::cmp::Ordering::Equal => OffsetPosition::AtHead,
        std::cmp::Ordering::Less => OffsetPosition::Behind,
        std::cmp::Ordering::Greater => OffsetPosition::Ahead,
    }
}

/// The `X-Capsule-Offset` header's value, or `None` if it is not one.
///
/// Strict on purpose: only ASCII digits, no sign, no whitespace, no `0x`. A header a client
/// spelled loosely is a client whose offset arithmetic is not to be trusted with the next
/// gigabyte.
pub fn parse_offset(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// The `X-Capsule-Checksum` header's value, or `None` if it is not a bare lowercase-hex
/// SHA-256.
///
/// The header is **required**, because the `(upload_id, offset, chunk_hash)` idempotency
/// tuple is undefined without it: a re-send after a lost acknowledgement could not be told
/// from a client retrying with different bytes.
pub fn parse_checksum(raw: &str) -> Option<&str> {
    is_sha256_hex(raw).then_some(raw)
}

/// True if `text` is 64 characters of lowercase hex — the spelling every digest crosses the
/// wire in.
pub fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase() && byte <= b'f')
}

/// Whether a chunk of `len` bytes written at `offset` satisfies the alignment rule for a blob
/// of `total_size` bytes.
///
/// Every chunk except the final one must be a multiple of [`CHUNK_ALIGNMENT`]; the final one
/// is exempt because a blob's total size is arbitrary. "Final" is decided by the declared
/// total, not by the client saying so.
pub fn alignment_ok(offset: u64, len: u64, total_size: u64) -> bool {
    let is_final = offset.saturating_add(len) == total_size;
    is_final || len.is_multiple_of(CHUNK_ALIGNMENT)
}

/// Whether a chunk is within the protocol's per-chunk ceiling.
pub fn within_chunk_ceiling(len: u64) -> bool {
    len <= MAX_CHUNK_BYTES
}

/// Whether `content_type` names the opaque-bytes media type, parameters ignored.
pub fn is_chunk_media_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case(CHUNK_MEDIA_TYPE))
}

/// The starting chunk size to suggest for a blob of `total_size` bytes.
///
/// A *starting point only*, and explicitly not protocol surface: `capsule-sdk` owns
/// adaptation and may move away from it immediately. The tiers are the Salvo deployment's, so
/// a client's first chunk after the port is the size it was before.
pub fn suggested_chunk_size(total_size: u64) -> u64 {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    match total_size {
        size if size < 10 * MIB => 256 * KIB,
        size if size < 100 * MIB => MIB,
        _ => 4 * MIB,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offset_is_at_the_head_behind_or_ahead() {
        assert_eq!(classify_offset(0, 0), OffsetPosition::AtHead);
        assert_eq!(classify_offset(4096, 4096), OffsetPosition::AtHead);
        assert_eq!(classify_offset(0, 4096), OffsetPosition::Behind);
        assert_eq!(classify_offset(8192, 4096), OffsetPosition::Ahead);
    }

    #[test]
    fn an_offset_header_is_read_strictly_or_not_at_all() {
        assert_eq!(parse_offset("0"), Some(0));
        assert_eq!(parse_offset("4096"), Some(4096));
        for bad in [
            "", " 4096", "4096 ", "+4096", "-1", "0x10", "4_096", "4.0", "nine",
        ] {
            assert_eq!(parse_offset(bad), None, "{bad:?} is not an offset");
        }
    }

    #[test]
    fn a_checksum_header_is_a_bare_lowercase_sha256() {
        let good = "a".repeat(64);
        assert_eq!(parse_checksum(&good), Some(good.as_str()));
        assert_eq!(
            parse_checksum(&"0123456789abcdef".repeat(4)),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );

        for bad in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
            format!("sha256:{}", "a".repeat(64)),
        ] {
            assert_eq!(parse_checksum(&bad), None, "{bad:?} is not a checksum");
        }
    }

    #[test]
    fn every_chunk_but_the_last_is_aligned() {
        // A 10 000-byte blob sent as 4096 + 4096 + 1808.
        assert!(alignment_ok(0, 4096, 10_000));
        assert!(alignment_ok(4096, 4096, 10_000));
        assert!(
            alignment_ok(8192, 1808, 10_000),
            "the final chunk is exempt"
        );

        // The same blob sent as 4000 + … is a client whose arithmetic has drifted.
        assert!(!alignment_ok(0, 4000, 10_000));

        // A blob smaller than the alignment is one final chunk.
        assert!(alignment_ok(0, 10, 10));
    }

    #[test]
    fn the_chunk_ceiling_is_inclusive() {
        assert!(within_chunk_ceiling(MAX_CHUNK_BYTES));
        assert!(!within_chunk_ceiling(MAX_CHUNK_BYTES + 1));
    }

    #[test]
    fn the_chunk_media_type_ignores_parameters_and_case() {
        assert!(is_chunk_media_type("application/octet-stream"));
        assert!(is_chunk_media_type(
            "application/octet-stream; charset=binary"
        ));
        assert!(is_chunk_media_type("Application/Octet-Stream"));
        assert!(!is_chunk_media_type("application/json"));
        assert!(
            !is_chunk_media_type("application/partial-upload"),
            "TUS v2's media type would claim a compatibility we do not have"
        );
    }

    #[test]
    fn the_suggested_size_follows_the_deployment_tiers() {
        assert_eq!(suggested_chunk_size(1), 256 * 1024);
        assert_eq!(suggested_chunk_size(10 * 1024 * 1024 - 1), 256 * 1024);
        assert_eq!(suggested_chunk_size(10 * 1024 * 1024), 1024 * 1024);
        assert_eq!(suggested_chunk_size(100 * 1024 * 1024), 4 * 1024 * 1024);

        // Whatever the tier, the suggestion is itself a legal non-final chunk — a suggestion
        // the protocol would reject is worse than no suggestion.
        for total in [1, 10 * 1024 * 1024, 100 * 1024 * 1024, u64::MAX] {
            let suggested = suggested_chunk_size(total);
            assert!(suggested.is_multiple_of(CHUNK_ALIGNMENT));
            assert!(within_chunk_ceiling(suggested));
        }
    }
}
