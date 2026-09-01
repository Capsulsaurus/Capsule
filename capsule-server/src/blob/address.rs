//! The content address, and the on-disk layout it dictates (slice `S-C33`).
//!
//! # One type owns the shard
//!
//! `blobs/{hash[0:2]}/{hash[2:4]}/{hash}.bin` is the settled layout
//! (design/filesystem/server.md, "Blob Store Layout"), and the shard segments are slices of the
//! address's *own* hex. That only works while the address really is 64 lowercase-hex characters,
//! so the shard is derived from exactly one place — [`ContentAddress`] — and that type has one
//! constructor, [`ContentAddress::parse`], which is the invariant's only gate. There is no
//! `From<String>`, no `new`, and no public field: a `&str` cannot reach a path.
//!
//! # If the digest ever changes
//!
//! The layout is valid for a 64-character lowercase-hex address and for nothing else. A digest
//! change is therefore a **data move behind `.server/version`**, not an edit here, and the
//! project must be told so loudly rather than discovering it as corrupted paths. Three things
//! make that so:
//!
//! - the const assertions below fail to compile if [`CONTENT_ADDRESS_LEN`] stops being SHA-256's
//!   64, or if the shard prefix stops fitting inside the address;
//! - [`ContentAddress::parse`] rejects every other shape, so a longer digest cannot be stored at
//!   all — it can never be silently filed under a truncated shard;
//! - [`digest_change_is_a_layout_change`] states the coupling as a test, so a change made anyway
//!   fails with the reason rather than with a mystery.

use std::fmt;
use std::path::{Path, PathBuf};

/// A SHA-256 ciphertext content address is exactly 64 lowercase-hex characters.
///
/// The same constant, and the same shape check, the shipped `capsule-api::service::blob_store`
/// enforces — carried over deliberately so the two stores address blobs identically while both
/// exist.
pub const CONTENT_ADDRESS_LEN: usize = 64;

/// How many hex characters each shard directory's name is.
pub const SHARD_SEGMENT_LEN: usize = 2;

/// How many shard directories a blob sits under.
pub const SHARD_DEPTH: usize = 2;

// The shard is a prefix *of the address*, so it must fit inside one and must leave the address
// itself distinguishable. A digest change that violates either is a compile error here.
const _: () = assert!(
    SHARD_DEPTH * SHARD_SEGMENT_LEN < CONTENT_ADDRESS_LEN,
    "the shard prefix must fit inside the content address with room to spare"
);
const _: () = assert!(
    CONTENT_ADDRESS_LEN == 64,
    "the layout is SHA-256 hex; a digest change is a `.server/version` data move, not an edit"
);

/// The finalized store, sharded two levels deep on the address's own hex prefix.
pub const BLOBS_DIR: &str = "blobs";

/// Live uploads: one append-only `{upload_id}.bin` per session. Flat — see the module docs on
/// [`super`].
pub const INCOMING_DIR: &str = "incoming";

/// Bytes preserved for forensic inspection rather than dropped. Flat.
pub const QUARANTINE_DIR: &str = "quarantine";

/// The suffix every stored blob's file name carries.
pub const BLOB_SUFFIX: &str = ".bin";

/// The suffix a quarantined blob's sibling rejection record carries.
pub const QUARANTINE_REASON_SUFFIX: &str = ".reason.json";

/// A string that is not a content address.
///
/// A parse failure, not a store failure: a malformed address can address no committed blob, so a
/// caller treats this as "unknown content address" rather than as a server error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MalformedAddress {
    /// The wrong number of characters.
    #[error("a content address is {CONTENT_ADDRESS_LEN} characters, not {actual}")]
    Length {
        /// How many characters were offered.
        actual: usize,
    },

    /// A character that is not lowercase hex.
    ///
    /// Case is part of the invariant, not a normalization the store performs: an uppercase
    /// address would name a *different* path on a case-sensitive filesystem and the same one on a
    /// case-insensitive filesystem, which is precisely how one blob becomes two.
    #[error("a content address is lowercase hex; byte {position} is not")]
    NotLowercaseHex {
        /// Which byte offended.
        position: usize,
    },
}

/// A validated ciphertext content address.
///
/// Ordered by its own hex, which is also the order [`super::BlobStore::enumerate`] walks the
/// shard tree in — the shard being an address prefix is exactly what makes a lexicographic
/// directory walk and a flat walk the same sequence.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentAddress(String);

impl ContentAddress {
    /// Validates `hash` as a lowercase-hex ciphertext content address.
    ///
    /// # Errors
    ///
    /// [`MalformedAddress`] when `hash` is not exactly [`CONTENT_ADDRESS_LEN`] lowercase-hex
    /// characters.
    pub fn parse(hash: &str) -> Result<Self, MalformedAddress> {
        if hash.len() != CONTENT_ADDRESS_LEN {
            return Err(MalformedAddress::Length {
                actual: hash.chars().count(),
            });
        }
        for (position, byte) in hash.bytes().enumerate() {
            if !(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
                return Err(MalformedAddress::NotLowercaseHex { position });
            }
        }
        Ok(Self(hash.to_owned()))
    }

    /// The address's hex.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The two shard directory names this address is filed under.
    ///
    /// Infallible by construction: the invariant [`Self::parse`] enforces is exactly what makes
    /// these slices exist.
    pub fn shard(&self) -> [&str; SHARD_DEPTH] {
        let (first, rest) = self.0.split_at(SHARD_SEGMENT_LEN);
        let (second, _) = rest.split_at(SHARD_SEGMENT_LEN);
        [first, second]
    }

    /// The file name this address is stored under, `{hash}.bin`.
    pub fn file_name(&self) -> String {
        format!("{}{BLOB_SUFFIX}", self.0)
    }

    /// The address a `{hash}.bin` file name names, if it names one.
    ///
    /// The reverse of [`Self::file_name`], and the enumeration walk's only way to turn a
    /// directory entry into a blob — anything this rejects is debris, never an entry.
    pub fn from_file_name(name: &str) -> Option<Self> {
        Self::parse(name.strip_suffix(BLOB_SUFFIX)?).ok()
    }

    /// Whether this address really belongs under the shard directories `segments`.
    ///
    /// A file whose name is a valid address but whose path is a shard it does not derive is
    /// corruption, not a blob: serving it would answer one content address with another's bytes.
    pub fn is_filed_under(&self, segments: [&str; SHARD_DEPTH]) -> bool {
        self.shard() == segments
    }
}

impl fmt::Display for ContentAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Debug for ContentAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ContentAddress({})", self.0)
    }
}

/// The finalized store under `root`.
pub fn blobs_dir(root: &Path) -> PathBuf {
    root.join(BLOBS_DIR)
}

/// The live-upload directory under `root`.
pub fn incoming_dir(root: &Path) -> PathBuf {
    root.join(INCOMING_DIR)
}

/// The forensic-hold directory under `root`.
pub fn quarantine_dir(root: &Path) -> PathBuf {
    root.join(QUARANTINE_DIR)
}

/// The shard directory `address` is filed under: `{root}/blobs/{hash[0:2]}/{hash[2:4]}`.
pub fn shard_dir(root: &Path, address: &ContentAddress) -> PathBuf {
    let [first, second] = address.shard();
    blobs_dir(root).join(first).join(second)
}

/// The exact path a finalized blob is committed to.
///
/// `blobs/{hash[0:2]}/{hash[2:4]}/{hash}.bin` — **never** a flat `blobs/{hash}.bin`.
pub fn blob_path(root: &Path, address: &ContentAddress) -> PathBuf {
    shard_dir(root, address).join(address.file_name())
}

/// Whether `segment` is a shard directory name this layout could have created.
pub fn is_shard_segment(segment: &str) -> bool {
    segment.len() == SHARD_SEGMENT_LEN
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(seed: &str) -> ContentAddress {
        let mut hex = seed.to_owned();
        hex.push_str(&"0".repeat(CONTENT_ADDRESS_LEN - seed.len()));
        match ContentAddress::parse(&hex) {
            Ok(address) => address,
            Err(error) => panic!("the fixture must be an address: {error}"),
        }
    }

    #[test]
    fn a_blob_is_addressed_two_levels_deep_and_never_flat() {
        let root = Path::new("/srv/capsule/blobs-root");
        let address = address("abcdef12");

        assert_eq!(address.shard(), ["ab", "cd"]);
        assert_eq!(
            blob_path(root, &address),
            root.join("blobs")
                .join("ab")
                .join("cd")
                .join(format!("{address}.bin")),
        );
        assert_ne!(
            blob_path(root, &address),
            root.join("blobs").join(format!("{address}.bin")),
            "the flat layout is what the shard overturned"
        );
    }

    #[test]
    fn only_lowercase_hex_of_the_right_length_is_an_address() {
        assert!(ContentAddress::parse(&"a".repeat(64)).is_ok());
        assert_eq!(
            ContentAddress::parse(&"a".repeat(63)),
            Err(MalformedAddress::Length { actual: 63 })
        );
        assert_eq!(
            ContentAddress::parse(&"a".repeat(65)),
            Err(MalformedAddress::Length { actual: 65 })
        );
        assert_eq!(
            ContentAddress::parse(&format!("A{}", "a".repeat(63))),
            Err(MalformedAddress::NotLowercaseHex { position: 0 }),
            "case is the invariant, not a normalization"
        );
        assert_eq!(
            ContentAddress::parse(&format!("{}g", "a".repeat(63))),
            Err(MalformedAddress::NotLowercaseHex { position: 63 })
        );
        assert!(
            ContentAddress::parse(&format!("../{}", "a".repeat(61))).is_err(),
            "a traversal sequence is not hex, so it can never reach a path"
        );
    }

    #[test]
    fn a_file_name_round_trips_through_its_address() {
        let address = address("0f1e2d3c");
        assert_eq!(
            ContentAddress::from_file_name(&address.file_name()),
            Some(address)
        );
        assert_eq!(ContentAddress::from_file_name("readme.txt"), None);
        assert_eq!(ContentAddress::from_file_name(&"a".repeat(64)), None);
        assert_eq!(
            ContentAddress::from_file_name(&format!(".{}.tmp", "a".repeat(64))),
            None,
            "a crashed temp file is debris, not a blob"
        );
    }

    #[test]
    fn an_address_knows_the_shard_it_belongs_to() {
        let address = address("abcdef12");
        assert!(address.is_filed_under(["ab", "cd"]));
        assert!(!address.is_filed_under(["ab", "ce"]));
    }

    #[test]
    fn a_shard_segment_is_two_lowercase_hex_characters() {
        assert!(is_shard_segment("ab"));
        assert!(is_shard_segment("09"));
        assert!(!is_shard_segment("AB"));
        assert!(!is_shard_segment("a"));
        assert!(!is_shard_segment("abc"));
        assert!(!is_shard_segment(".."));
    }

    /// The layout is valid for a 64-character lowercase-hex address and for nothing else.
    ///
    /// If a future change re-pins the ciphertext digest, this fails and says what the change
    /// actually costs — a `.server/version` bump and a data move — rather than letting every
    /// stored path silently mean something different.
    #[test]
    fn digest_change_is_a_layout_change() {
        assert_eq!(
            CONTENT_ADDRESS_LEN, 64,
            "the shard is SHA-256 hex; changing the digest is a `.server/version` data move"
        );
        assert_eq!(SHARD_DEPTH, 2);
        assert_eq!(SHARD_SEGMENT_LEN, 2);
    }
}
