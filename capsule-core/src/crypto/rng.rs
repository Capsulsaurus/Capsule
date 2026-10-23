//! OS CSPRNG access. Every key, salt, and nonce is drawn here; Capsule never seeds
//! its own PRNG (SSoT: [Cryptography — Primitives § Randomness]).
//!
//! [Cryptography — Primitives § Randomness]: https://docs/design/cryptography/primitives/#randomness

/// Fill `buf` with cryptographically secure random bytes from the OS.
///
/// Panics only if the OS RNG is unavailable — an unrecoverable environment fault on
/// every platform Capsule targets, where continuing would be a security defect.
pub fn fill(buf: &mut [u8]) {
    getrandom::fill(buf).expect("OS CSPRNG (getrandom) must be available");
}

/// Draw a fresh `N`-byte random array (key, salt, or nonce prefix).
pub fn random_array<const N: usize>() -> [u8; N] {
    let mut a = [0u8; N];
    fill(&mut a);
    a
}

#[cfg(test)]
mod tests;
