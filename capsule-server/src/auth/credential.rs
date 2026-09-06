//! [`Credentials`] — the one place this server hashes and checks a password.
//!
//! # Why a helper and not a method on each adapter
//!
//! Three ports oblige their adapter to own credential verification end to end:
//! [`AccountDirectory`](super::AccountDirectory) verifies,
//! [`AccountRegistry`](super::AccountRegistry) hashes, and
//! [`PasswordChange`](super::PasswordChange) re-hashes. Their docs say why — a password hash
//! that crossed the port boundary would be a secret in a type that does not know it is one, and
//! it would put Argon2id's parameters in the routing layer, where a second call site can get
//! them subtly wrong.
//!
//! What that leaves is an obligation each *adapter* has to discharge identically. The in-memory
//! adapter beside this file discharges it, and the Postgres adapter (#402) discharges the same
//! one; written twice they would be two answers to a question with one right answer, and the
//! first divergence would be a parameter set — the thing that is invisible until somebody's
//! password is cheap to crack. So the algorithm is here, once, and an adapter owns *where the
//! hash is kept* rather than *what a hash is*.
//!
//! # Argon2id, at the crate's own defaults
//!
//! `Argon2::default()` is Argon2id, version 0x13, m=19456 KiB, t=2, p=1 — the parameter set the
//! RustCrypto crate publishes as its recommendation. Not the tiered parameters
//! [`capsule_core::crypto::pwkdf`] uses: those describe a *key derivation* that has to run on
//! the weakest device that will ever unwrap the blob, and this is a server-side verification
//! whose cost is paid on the server. The two are deliberately unrelated numbers, and the
//! parameters ride inside every PHC string this writes, so raising them is not a flag day.
//!
//! # The timing-equalized miss
//!
//! [`AccountDirectory`](super::AccountDirectory)'s contract is that no caller can tell an
//! unknown account from a wrong password. Returning early for an unknown address would leak
//! that difference in the response *time* whatever the body said, so an adapter must still do
//! the work — [`Credentials::absorb_miss`] is that work, verifying against a decoy hash
//! computed once at construction and discarding the answer.

use std::fmt;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};

/// How many bytes of salt every hash carries.
///
/// Sixteen, which is what the PHC specification recommends and what `Argon2::default()` would
/// have generated. Longer buys nothing: the salt is a uniqueness device, not a secret.
const SALT_LEN: usize = 16;

/// The password the decoy hash is built over.
///
/// A constant, and it does not matter what it is: [`Credentials::absorb_miss`] never compares
/// against it successfully, and the only property required of the decoy is that verifying
/// against it costs what verifying against a real hash costs.
const DECOY_PASSWORD: &[u8] = b"capsule/decoy/there-is-no-such-account";

/// Something went wrong hashing or reading a credential.
///
/// Never carries the password, the hash, or any part of either: this error is logged, and a
/// library that put a PHC string in a log line would put every account's salt there with it.
#[derive(Debug, thiserror::Error)]
#[error("a stored credential could not be processed: {detail}")]
pub struct CredentialError {
    /// The algorithm's own description of the failure.
    pub detail: String,
}

/// Hashing and checking passwords, at one parameter set.
///
/// `Debug` is hand-written and names the algorithm rather than the state, because the state
/// includes a hash.
#[derive(Clone)]
pub struct Credentials {
    argon: Argon2<'static>,
    /// A real Argon2id hash of a password nobody has, so a lookup that found nothing can cost
    /// what a lookup that found something costs.
    decoy: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("algorithm", &"argon2id")
            .finish_non_exhaustive()
    }
}

impl Credentials {
    /// A verifier at the crate's default Argon2id parameters.
    ///
    /// Pays for one hash — the decoy — so that no request ever has to.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] if the platform cannot produce a hash at all, which is a
    /// startup failure rather than a request failure: a server that cannot hash a password
    /// cannot authenticate anybody and must refuse to start.
    pub fn new() -> Result<Self, CredentialError> {
        let argon = Argon2::default();
        let decoy = hash_with(&argon, DECOY_PASSWORD)?;
        Ok(Self { argon, decoy })
    }

    /// The PHC string to store for `password`.
    ///
    /// A fresh random salt every time, so two accounts with the same password have different
    /// stored hashes and a stolen table cannot be attacked once for both.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] if the hash cannot be computed.
    pub fn hash(&self, password: &str) -> Result<String, CredentialError> {
        hash_with(&self.argon, password.as_bytes())
    }

    /// Whether `password` is the one `stored` was made from.
    ///
    /// A wrong password is `Ok(false)`, not an error: a refused credential is a normal answer to
    /// a normal question, and modelling it as a failure is what leads to a `?` that turns a
    /// sign-in rejection into a 500.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] only when `stored` is not a PHC string this server can read —
    /// a corrupted row, not a wrong password.
    pub fn verify(&self, password: &str, stored: &str) -> Result<bool, CredentialError> {
        let parsed = PasswordHash::new(stored).map_err(|error| CredentialError {
            detail: format!("the stored hash is not a readable PHC string ({error})"),
        })?;
        match self.argon.verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(error) => Err(CredentialError {
                detail: error.to_string(),
            }),
        }
    }

    /// Spend what a verification costs, having found no account to verify against.
    ///
    /// The timing-equalized miss; see the module docs. The result is deliberately discarded —
    /// there is nothing to learn from it, and a caller that branched on it would be branching
    /// on whether a decoy password happens to be somebody's.
    pub fn absorb_miss(&self, password: &str) {
        let _ = self.verify(password, &self.decoy);
    }
}

/// Hash `password` under `argon` with a fresh salt.
fn hash_with(argon: &Argon2<'static>, password: &[u8]) -> Result<String, CredentialError> {
    let mut bytes = [0u8; SALT_LEN];
    // `ring`'s CSPRNG rather than `password_hash`'s optional `rand` feature: it is already this
    // crate's source of randomness and its key generator, so one binary has one CSPRNG.
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).map_err(
        |error| CredentialError {
            detail: format!("the platform could not produce a salt ({error})"),
        },
    )?;
    let salt = SaltString::encode_b64(&bytes).map_err(|error| CredentialError {
        detail: format!("the salt could not be encoded ({error})"),
    })?;
    Ok(argon
        .hash_password(password, &salt)
        .map_err(|error| CredentialError {
            detail: error.to_string(),
        })?
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::Credentials;

    /// One instance for the whole module: `Credentials::new` pays for an Argon2id hash, and
    /// paying for it once per test is the difference between a fast suite and a slow one.
    fn credentials() -> Credentials {
        Credentials::new().expect("the platform hashes")
    }

    #[test]
    fn the_password_it_hashed_is_the_password_it_accepts() {
        let credentials = credentials();
        let stored = credentials
            .hash("correct horse battery staple")
            .expect("it hashes");
        assert!(
            credentials
                .verify("correct horse battery staple", &stored)
                .expect("it reads")
        );
    }

    #[test]
    fn a_wrong_password_is_a_refusal_and_not_an_error() {
        // The distinction the port is built on: a refused credential is an answer, so a route
        // cannot accidentally `?` it into a 500.
        let credentials = credentials();
        let stored = credentials
            .hash("correct horse battery staple")
            .expect("it hashes");
        assert!(
            !credentials
                .verify("Correct Horse Battery Staple", &stored)
                .expect("it reads")
        );
    }

    #[test]
    fn the_stored_hash_is_a_phc_string_naming_argon2id_and_never_the_password() {
        let credentials = credentials();
        let stored = credentials
            .hash("a password worth protecting")
            .expect("it hashes");
        assert!(stored.starts_with("$argon2id$"), "{stored}");
        assert!(!stored.contains("a password worth protecting"), "{stored}");
    }

    #[test]
    fn two_accounts_with_one_password_do_not_share_a_hash() {
        // A fresh salt per hash, which is what stops one offline attack from covering both.
        let credentials = credentials();
        let first = credentials.hash("shared").expect("it hashes");
        let second = credentials.hash("shared").expect("it hashes");
        assert_ne!(first, second);
        assert!(credentials.verify("shared", &first).expect("it reads"));
        assert!(credentials.verify("shared", &second).expect("it reads"));
    }

    #[test]
    fn a_corrupted_stored_hash_is_a_fault_and_not_a_refusal() {
        // It must not read as "your password is wrong", which would send somebody round a loop
        // that cannot succeed.
        let credentials = credentials();
        assert!(credentials.verify("anything", "not a PHC string").is_err());
    }

    #[test]
    fn absorbing_a_miss_costs_what_a_verification_costs() {
        // Not a timing assertion — those are flaky by nature. What is asserted is that the
        // decoy is a real hash the verifier reads, so the work actually happens: a decoy that
        // failed to parse would return in microseconds and leak the account oracle the port
        // exists to close.
        let credentials = credentials();
        credentials.absorb_miss("anything at all");
        assert!(
            !credentials
                .verify("anything at all", &credentials.decoy)
                .expect("the decoy parses")
        );
    }

    #[test]
    fn debug_names_the_algorithm_and_prints_no_hash() {
        let credentials = credentials();
        let rendered = format!("{credentials:?}");
        assert!(rendered.contains("argon2id"), "{rendered}");
        assert!(!rendered.contains('$'), "{rendered}");
    }
}
