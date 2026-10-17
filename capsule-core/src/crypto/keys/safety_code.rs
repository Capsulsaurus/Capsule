//! The cross-device-add **safety code** and the enrollee's **key commitment** (SSoT:
//! [Device Enrollment § Cross-Device Add], step 4).
//!
//! The enrollment relay is a dumb pipe: anyone holding the channel handle may post in either
//! direction, so a relay (or anyone on it) can substitute the keys each device advertises. The
//! defence is a short code both devices derive from what they *received* and the user compares
//! by eye. If nothing was substituted, both sides hash the same two key sets and the codes match;
//! a man in the middle leaves each side hashing a different pair, and the codes diverge.
//!
//! ## Why a commitment rides alongside the code
//!
//! A short code alone is grindable. A man in the middle chooses the keys it shows *both* sides,
//! so it needs only `code(A, M1) == code(M2, B)` — a birthday search over the code space, cheap
//! for any code short enough for a human to compare. The ceremony therefore orders the reveal:
//! the enrollee sends [`key_commitment`] of its keys first, the initiator then reveals its own,
//! and only then does the enrollee reveal the keys it committed to (which the initiator checks
//! against the commitment with [`commitment_matches`]).
//!
//! Ordering alone is not enough, because device keys are **static**: the initiator's are already
//! published in the directory, and an enrollee's become known the first time it reveals them. A
//! man in the middle that can predict one side's input finishes the other side first, then
//! grinds offline for the keys it commits to. So each side's input also carries a **fresh
//! per-ceremony random nonce** (the `nonce` field of the enrollment payloads), and the code and
//! the commitment both cover it. The initiator's nonce is unknown until after the man in the
//! middle has committed toward the initiator; the enrollee's is hidden under its commitment until
//! after the man in the middle has revealed toward the enrollee. Every input it controls is then
//! fixed before it learns an input it needs, so it gets exactly one blind guess at the code:
//! `1 / 10^SAFETY_CODE_DIGITS`.
//!
//! ## Inputs are bytes, by design
//!
//! Both functions take each side's **canonical advertised-key bytes**, not a typed bundle: the
//! bundle and its canonical CBOR belong to the enrollment payload encoding
//! (`lifecycle::enrollment`), and this module must agree with any implementation that produces
//! the same bytes. Each input is length-prefixed and the two are role-ordered (initiator first),
//! so no split of one concatenation can be passed off as another and swapping the roles changes
//! the code.
//!
//! [Device Enrollment § Cross-Device Add]: https://docs/design/device-enrollment/#cross-device-add

use sha2::{Digest, Sha256};

/// Domain separator for the safety code — distinct from the commitment's so a commitment can
/// never be presented as a code or vice versa.
const SAFETY_CODE_DOMAIN: &[u8] = b"capsule/enrollment/safety-code/v1";

/// Domain separator for the enrollee's key commitment.
const COMMITMENT_DOMAIN: &[u8] = b"capsule/enrollment/key-commitment/v1";

/// How many decimal digits a safety code carries. Under the commit-then-reveal ordering over
/// nonce-bearing inputs a man in the middle gets one blind guess, so twelve digits bound it at one
/// in 10^12.
pub const SAFETY_CODE_DIGITS: usize = 12;

/// The digits per displayed chunk. Both devices render the code in the same chunked,
/// fixed-length shape so the human comparison is chunk-by-chunk rather than one long run.
pub const SAFETY_CODE_CHUNK: usize = 4;

/// The length of a [`key_commitment`].
pub const KEY_COMMITMENT_LEN: usize = 32;

/// Feed one length-prefixed field into the hash, so field boundaries are unambiguous.
fn absorb(hasher: &mut Sha256, field: &[u8]) {
    let len = u64::try_from(field.len()).expect("a slice length fits in u64");
    hasher.update(len.to_be_bytes());
    hasher.update(field);
}

/// The safety code both devices display, over the **initiator's** and the **enrollee's**
/// canonical advertised-key bytes, in that order.
///
/// Each input must include that side's fresh per-ceremony nonce: over static keys alone the code
/// is grindable whatever the reveal order (see the module docs).
///
/// The result is [`SAFETY_CODE_DIGITS`] decimal digits in groups of [`SAFETY_CODE_CHUNK`]
/// separated by single spaces (e.g. `0412 7730 9958`), zero-padded so it is always the same
/// length. Deterministic: the same two inputs always give the same code, on every platform.
pub fn safety_code(a_keys: &[u8], b_keys: &[u8]) -> String {
    let mut hasher = Sha256::new();
    absorb(&mut hasher, SAFETY_CODE_DOMAIN);
    absorb(&mut hasher, a_keys);
    absorb(&mut hasher, b_keys);
    let digest = hasher.finalize();

    // The first 8 digest bytes reduced mod 10^12. The modulo bias is below 10^12 / 2^64
    // (about 5e-8) — far beneath the guessing bound the code exists to provide.
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    let modulus = 10u64.pow(u32::try_from(SAFETY_CODE_DIGITS).expect("digit count fits in u32"));
    let value = u64::from_be_bytes(head) % modulus;
    let digits = format!("{value:0SAFETY_CODE_DIGITS$}");

    let chunks: Vec<&str> = digits
        .as_bytes()
        .chunks(SAFETY_CODE_CHUNK)
        .map(|chunk| std::str::from_utf8(chunk).expect("ASCII digits"))
        .collect();
    let code = chunks.join(" ");
    tracing::debug!(
        a_len = a_keys.len(),
        b_len = b_keys.len(),
        "enrollment: derived a safety code"
    );
    code
}

/// The enrollee's commitment to its canonical advertised-key bytes, sent before the initiator
/// reveals its own keys (see the module docs for why the order matters).
pub fn key_commitment(keys: &[u8]) -> [u8; KEY_COMMITMENT_LEN] {
    let mut hasher = Sha256::new();
    absorb(&mut hasher, COMMITMENT_DOMAIN);
    absorb(&mut hasher, keys);
    hasher.finalize().into()
}

/// Whether revealed key bytes are the ones a commitment was made to. A mismatch is the abort
/// path: the enrollee revealed keys it did not commit to, or the commitment was substituted.
pub fn commitment_matches(commitment: &[u8; KEY_COMMITMENT_LEN], keys: &[u8]) -> bool {
    let matches = key_commitment(keys) == *commitment;
    if !matches {
        tracing::warn!("enrollment: revealed keys do not match the enrollee's commitment");
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code is fixed-length, chunked, digits-only — the shape both devices render.
    #[test]
    fn the_code_is_twelve_digits_in_three_chunks() {
        let code = safety_code(b"initiator keys", b"enrollee keys");
        let chunks: Vec<&str> = code.split(' ').collect();
        assert_eq!(chunks.len(), SAFETY_CODE_DIGITS / SAFETY_CODE_CHUNK);
        for chunk in &chunks {
            assert_eq!(chunk.len(), SAFETY_CODE_CHUNK);
            assert!(chunk.bytes().all(|b| b.is_ascii_digit()), "{code}");
        }
        assert_eq!(code.len(), SAFETY_CODE_DIGITS + chunks.len() - 1);
    }

    /// A pinned vector: any change to the construction (domain, framing, reduction, format) is a
    /// cross-device incompatibility — two builds would show different codes for the same keys —
    /// so it must fail here rather than in a ceremony.
    #[test]
    fn the_code_is_pinned() {
        assert_eq!(safety_code(b"a", b"b"), PINNED_AB);
        assert_eq!(safety_code(b"", b""), PINNED_EMPTY);
    }
    // Computed independently of this module: SHA-256 over the three `u64`-big-endian
    // length-prefixed fields (domain, a, b), first 8 bytes big-endian mod 10^12, zero-padded.
    const PINNED_AB: &str = "0161 2210 7184";
    const PINNED_EMPTY: &str = "4811 6247 6570";

    /// Both devices agree when nothing was substituted, and a substituted key on either side
    /// diverges the code — the MITM case the check exists to catch.
    #[test]
    fn a_substituted_key_on_either_side_changes_the_code() {
        let honest = safety_code(b"device-a keys", b"device-b keys");
        assert_eq!(honest, safety_code(b"device-a keys", b"device-b keys"));
        assert_ne!(honest, safety_code(b"device-a keys", b"device-m keys"));
        assert_ne!(honest, safety_code(b"device-m keys", b"device-b keys"));
    }

    /// Roles are ordered: the same two key sets with initiator and enrollee swapped give a
    /// different code, so a relay cannot reflect one side's keys back as the other's.
    #[test]
    fn swapping_the_roles_changes_the_code() {
        assert_ne!(safety_code(b"one", b"two"), safety_code(b"two", b"one"));
    }

    /// Length-prefixing keeps field boundaries unambiguous: moving bytes across the boundary
    /// between the two inputs changes the code even though the concatenation is identical.
    #[test]
    fn moving_bytes_across_the_boundary_changes_the_code() {
        assert_ne!(safety_code(b"ab", b"c"), safety_code(b"a", b"bc"));
        assert_ne!(key_commitment(b"abc"), key_commitment(b"ab"));
    }

    /// The commitment opens to exactly the keys it was made over.
    #[test]
    fn a_commitment_opens_only_to_its_own_keys() {
        let commitment = key_commitment(b"device-b keys");
        assert!(commitment_matches(&commitment, b"device-b keys"));
        assert!(!commitment_matches(&commitment, b"device-m keys"));
        assert!(!commitment_matches(
            &[0u8; KEY_COMMITMENT_LEN],
            b"device-b keys"
        ));
    }
}
