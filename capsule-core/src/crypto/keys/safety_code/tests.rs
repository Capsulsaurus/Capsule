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
