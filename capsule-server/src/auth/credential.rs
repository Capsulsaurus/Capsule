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
//!
//! # Off the async runtime
//!
//! Every Argon2id computation a request pays for runs on Tokio's blocking pool
//! (`spawn_blocking`), never on an async worker (#510). A hash costs tens of milliseconds of pure
//! CPU, and a worker spent on one is a worker every other request on it waits behind — a handful
//! of concurrent sign-ins would stall the whole server, health checks included. So the three
//! request-path operations ([`Credentials::hash`], [`Credentials::verify`],
//! [`Credentials::absorb_miss`]) are `async` and the synchronous primitives under them are
//! private: there is no way left to call Argon2id inline from a handler by accident.
//!
//! [`Credentials::new`] is the one exception, deliberately: it runs once at startup, before the
//! server accepts anything, and a composition root that cannot hash should fail there.

use std::fmt;
use std::time::Instant;

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

    /// A verifier at the smallest Argon2id cost the crate accepts (8 KiB, one pass), for tests.
    ///
    /// `#[cfg(test)]`, so it does not exist in a production build and [`crate::boot`] cannot
    /// reach it: the one constructor a running server uses is [`Credentials::new`]. The suites
    /// that exercise an account store hash hundreds of passwords, and at production cost that is
    /// minutes of CPU that proves nothing about the store.
    #[cfg(test)]
    pub(crate) fn with_reduced_cost() -> Result<Self, CredentialError> {
        use argon2::{Algorithm, Params, Version};

        let params = Params::new(8, 1, 1, None).map_err(|error| CredentialError {
            detail: error.to_string(),
        })?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let decoy = hash_with(&argon, DECOY_PASSWORD)?;
        Ok(Self { argon, decoy })
    }

    /// The PHC string to store for `password`.
    ///
    /// A fresh random salt every time, so two accounts with the same password have different
    /// stored hashes and a stolen table cannot be attacked once for both.
    ///
    /// Runs on the blocking pool; see the module docs.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] if the hash cannot be computed.
    pub async fn hash(&self, password: &str) -> Result<String, CredentialError> {
        let argon = self.argon.clone();
        let password = password.to_owned();
        off_runtime("hash", move || hash_with(&argon, password.as_bytes())).await
    }

    /// Whether `password` is the one `stored` was made from.
    ///
    /// A wrong password is `Ok(false)`, not an error: a refused credential is a normal answer to
    /// a normal question, and modelling it as a failure is what leads to a `?` that turns a
    /// sign-in rejection into a 500. Runs on the blocking pool; see the module docs.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] only when `stored` is not a PHC string this server can read —
    /// a corrupted row, not a wrong password — or when the blocking task could not finish.
    pub async fn verify(&self, password: &str, stored: &str) -> Result<bool, CredentialError> {
        let this = self.clone();
        let password = password.to_owned();
        let stored = stored.to_owned();
        off_runtime("verify", move || this.verify_now(&password, &stored)).await
    }

    /// Spend what a verification costs, having found no account to verify against.
    ///
    /// The timing-equalized miss; see the module docs. The result is deliberately discarded —
    /// there is nothing to learn from it, and a caller that branched on it would be branching
    /// on whether a decoy password happens to be somebody's. Runs on the blocking pool, like the
    /// verification it stands in for, so the two cost the same *where* they are paid as well.
    pub async fn absorb_miss(&self, password: &str) {
        let this = self.clone();
        let password = password.to_owned();
        let _ = off_runtime("absorb_miss", move || {
            this.verify_now(&password, &this.decoy)
        })
        .await;
    }

    /// [`Self::verify`], on the calling thread.
    fn verify_now(&self, password: &str, stored: &str) -> Result<bool, CredentialError> {
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
}

/// Run one Argon2id computation on Tokio's blocking pool and wait for it.
///
/// `operation` names it in the trace line, which carries the time it took — the number an
/// operator reads when sign-in latency moves. A task that panicked or was cancelled is a
/// [`CredentialError`], so a caller reaches no decision rather than a wrong one.
async fn off_runtime<T, F>(operation: &'static str, work: F) -> Result<T, CredentialError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CredentialError> + Send + 'static,
{
    let started = Instant::now();
    let outcome = tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CredentialError {
            detail: format!("the credential task did not finish ({error})"),
        })?;
    tracing::trace!(
        operation,
        elapsed_ms = started.elapsed().as_millis(),
        ok = outcome.is_ok(),
        "an Argon2id computation finished on the blocking pool"
    );
    outcome
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::Credentials;

    /// One instance for the whole module: `Credentials::new` pays for an Argon2id hash, and
    /// paying for it once per test is the difference between a fast suite and a slow one.
    fn credentials() -> Credentials {
        Credentials::new().expect("the platform hashes")
    }

    #[tokio::test]
    async fn the_password_it_hashed_is_the_password_it_accepts() {
        let credentials = credentials();
        let stored = credentials
            .hash("correct horse battery staple")
            .await
            .expect("it hashes");
        assert!(
            credentials
                .verify("correct horse battery staple", &stored)
                .await
                .expect("it reads")
        );
    }

    #[tokio::test]
    async fn a_wrong_password_is_a_refusal_and_not_an_error() {
        // The distinction the port is built on: a refused credential is an answer, so a route
        // cannot accidentally `?` it into a 500.
        let credentials = credentials();
        let stored = credentials
            .hash("correct horse battery staple")
            .await
            .expect("it hashes");
        assert!(
            !credentials
                .verify("Correct Horse Battery Staple", &stored)
                .await
                .expect("it reads")
        );
    }

    #[tokio::test]
    async fn the_stored_hash_is_a_phc_string_naming_argon2id_and_never_the_password() {
        let credentials = credentials();
        let stored = credentials
            .hash("a password worth protecting")
            .await
            .expect("it hashes");
        assert!(stored.starts_with("$argon2id$"), "{stored}");
        assert!(!stored.contains("a password worth protecting"), "{stored}");
    }

    #[tokio::test]
    async fn two_accounts_with_one_password_do_not_share_a_hash() {
        // A fresh salt per hash, which is what stops one offline attack from covering both.
        let credentials = credentials();
        let first = credentials.hash("shared").await.expect("it hashes");
        let second = credentials.hash("shared").await.expect("it hashes");
        assert_ne!(first, second);
        assert!(
            credentials
                .verify("shared", &first)
                .await
                .expect("it reads")
        );
        assert!(
            credentials
                .verify("shared", &second)
                .await
                .expect("it reads")
        );
    }

    #[tokio::test]
    async fn a_corrupted_stored_hash_is_a_fault_and_not_a_refusal() {
        // It must not read as "your password is wrong", which would send somebody round a loop
        // that cannot succeed.
        let credentials = credentials();
        assert!(
            credentials
                .verify("anything", "not a PHC string")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn absorbing_a_miss_costs_what_a_verification_costs() {
        // Not a timing assertion — those are flaky by nature. What is asserted is that the
        // decoy is a real hash the verifier reads, so the work actually happens: a decoy that
        // failed to parse would return in microseconds and leak the account oracle the port
        // exists to close.
        let credentials = credentials();
        credentials.absorb_miss("anything at all").await;
        assert!(
            !credentials
                .verify("anything at all", &credentials.decoy)
                .await
                .expect("the decoy parses")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn verification_leaves_the_async_worker_free() {
        // The property #510 is about, observed rather than timed: on a single-threaded runtime
        // a verification that ran inline would hold the only worker, so once the spawned task
        // had been polled at all it would already be finished. Off the runtime, it is polled,
        // parks on the blocking pool, and this task gets the worker back with the hash still in
        // flight.
        let credentials = credentials();
        let stored = credentials.hash("in flight").await.expect("it hashes");
        let started = Arc::new(AtomicBool::new(false));
        let verifying = tokio::spawn({
            let credentials = credentials.clone();
            let started = Arc::clone(&started);
            async move {
                started.store(true, Ordering::SeqCst);
                credentials.verify("in flight", &stored).await
            }
        });
        for _ in 0..100 {
            if started.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            started.load(Ordering::SeqCst),
            "the verifying task was polled"
        );
        assert!(
            !verifying.is_finished(),
            "a verification must not hold the async worker for the length of a hash"
        );
        assert!(
            verifying
                .await
                .expect("the task finished")
                .expect("it reads")
        );
    }

    #[tokio::test]
    async fn the_production_constructor_keeps_the_documented_argon2id_parameters() {
        let stored = Credentials::new()
            .expect("the platform hashes")
            .hash("pw")
            .await
            .expect("it hashes");
        assert!(
            stored.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{stored}"
        );
    }

    #[tokio::test]
    async fn the_reduced_cost_constructor_is_cheaper_and_still_verifies() {
        let credentials = Credentials::with_reduced_cost().expect("the platform hashes");
        let stored = credentials.hash("pw").await.expect("it hashes");
        assert!(
            stored.starts_with("$argon2id$v=19$m=8,t=1,p=1$"),
            "{stored}"
        );
        assert!(credentials.verify("pw", &stored).await.expect("it reads"));
        assert!(!credentials.verify("no", &stored).await.expect("it reads"));
    }

    #[test]
    fn debug_names_the_algorithm_and_prints_no_hash() {
        let credentials = credentials();
        let rendered = format!("{credentials:?}");
        assert!(rendered.contains("argon2id"), "{rendered}");
        assert!(!rendered.contains('$'), "{rendered}");
    }
}
