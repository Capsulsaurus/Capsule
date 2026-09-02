//! The protocol header names, spelled once.
//!
//! These are contract, not framework: the same names have to come out of the replacement
//! server byte-for-byte, and every client already switches on them. They lived as repeated
//! string literals across the handlers, where a typo in one arm was invisible.
//!
//! Only the **response** side is declared here — the side the response taxonomies write.
//! Request-side parsing keeps its own spellings until it moves in the same direction.

/// Authoritative byte offset the server holds for an upload session.
pub const OFFSET: &str = "X-Capsule-Offset";

/// Declared total size of an upload, when the server knows it.
pub const CONTENT_LENGTH: &str = "X-Capsule-Content-Length";

/// Lifecycle state of an upload session, as a header token.
pub const UPLOAD_STATUS: &str = "X-Capsule-Upload-Status";

/// Chunk size the server would like the client to use next.
pub const SUGGESTED_CHUNK_SIZE: &str = "X-Capsule-Suggested-Chunk-Size";

/// Lowest upload-protocol version this server accepts.
pub const PROTOCOL_MIN: &str = "X-Capsule-Protocol-Min";

/// Highest upload-protocol version this server accepts.
pub const PROTOCOL_MAX: &str = "X-Capsule-Protocol-Max";

#[cfg(test)]
mod tests {
    use super::*;

    /// Header names are case-insensitive on the wire but not in a diff: pin the canonical
    /// spelling so a rename has to be deliberate.
    #[test]
    fn protocol_headers_keep_their_canonical_spelling() {
        for name in [
            OFFSET,
            CONTENT_LENGTH,
            UPLOAD_STATUS,
            SUGGESTED_CHUNK_SIZE,
            PROTOCOL_MIN,
            PROTOCOL_MAX,
        ] {
            assert!(
                name.starts_with("X-Capsule-"),
                "{name} is not in the X-Capsule- namespace"
            );
        }
        assert_eq!(OFFSET, "X-Capsule-Offset");
        assert_eq!(SUGGESTED_CHUNK_SIZE, "X-Capsule-Suggested-Chunk-Size");
    }
}
