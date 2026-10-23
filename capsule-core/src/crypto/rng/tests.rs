use super::*;

#[test]
fn fills_exact_length() {
    let mut buf = [0u8; 48];
    fill(&mut buf);
    // Astronomically unlikely to be all-zero; guards against a no-op stub.
    assert!(buf.iter().any(|&b| b != 0));
}

#[test]
fn draws_are_distinct() {
    let a = random_array::<32>();
    let b = random_array::<32>();
    assert_ne!(a, b, "two CSPRNG draws must not collide");
}
