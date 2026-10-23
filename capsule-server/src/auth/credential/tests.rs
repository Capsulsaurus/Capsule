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
