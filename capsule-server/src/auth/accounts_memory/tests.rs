use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use super::{Credentials, InMemoryAccounts, MAX_FAILED_ATTEMPTS};
use crate::auth::directory::{AccountDirectory, Authentication};
use crate::auth::profile::{AccountProfiles, PasswordChange, PasswordChanged, ProfileUpdate};
use crate::auth::registry::{AccountRegistry, Registration};
use crate::store::UserId;
use crate::store::memory::ManualClock;

const EMAIL: &str = "somebody@example.test";
const PASSWORD: &str = "correct horse battery staple";

fn user() -> UserId {
    UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f")
}

/// A fifteen-minute lockout window, as a deployment gets by default.
const WINDOW: SignedDuration = SignedDuration::from_mins(15);

/// A directory with one registered account, over a clock the test drives.
async fn seeded_on(clock: Arc<ManualClock>) -> InMemoryAccounts {
    let accounts = InMemoryAccounts::new(
        Credentials::with_reduced_cost().expect("the platform hashes"),
        clock,
        MAX_FAILED_ATTEMPTS,
        WINDOW,
    );
    assert_eq!(
        accounts
            .create(EMAIL, PASSWORD, &user(), Timestamp::UNIX_EPOCH)
            .await
            .expect("it writes"),
        Registration::Created(user())
    );
    accounts
}

/// The same, for a case with nothing to say about time.
async fn seeded() -> InMemoryAccounts {
    seeded_on(Arc::new(ManualClock::default())).await
}

/// Present a wrong password `times` times.
async fn fail(accounts: &InMemoryAccounts, times: u32) {
    for _ in 0..times {
        let _ = accounts.authenticate(EMAIL, "wrong").await;
    }
}

#[tokio::test]
async fn registering_then_signing_in_works() {
    // The whole reason this adapter exists rather than a fail-closed stub.
    let accounts = seeded().await;
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
}

#[tokio::test]
async fn no_plaintext_password_is_retained() {
    // The property the port is built on: the credential never rises above the adapter, and
    // it is not sitting in the adapter either.
    let accounts = seeded().await;
    let held = accounts
        .held()
        .by_email_mut(EMAIL)
        .cloned()
        .expect("it is held");
    let stored = held.stored.expect("a password account holds a credential");
    assert!(stored.starts_with("$argon2id$"), "{stored}");
    assert!(!stored.contains(PASSWORD));
}

#[tokio::test]
async fn a_taken_address_is_reported_and_nothing_is_written() {
    let accounts = seeded().await;
    let other = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70");
    assert_eq!(
        accounts
            .create(
                EMAIL,
                "a different password entirely",
                &other,
                Timestamp::UNIX_EPOCH
            )
            .await
            .expect("it answers"),
        Registration::AlreadyExists
    );
    // The first account's credential still works, so nothing was overwritten.
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
}

#[tokio::test]
async fn an_unknown_address_and_a_wrong_password_are_one_answer() {
    // The port collapses them into one value on purpose, so no caller *can* tell them apart.
    let accounts = seeded().await;
    assert_eq!(
        accounts
            .authenticate("nobody@example.test", PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Refused
    );
    assert_eq!(
        accounts
            .authenticate(EMAIL, "the wrong password")
            .await
            .expect("it answers"),
        Authentication::Refused
    );
}

#[tokio::test]
async fn enough_failures_lock_the_account_and_a_correct_password_is_told_so() {
    // `Locked` is the one refusal a *correct* password also receives, which is why it is a
    // separate value: a client showing "wrong password" here would send somebody round a
    // loop that cannot succeed.
    let accounts = seeded().await;
    for _ in 0..MAX_FAILED_ATTEMPTS {
        assert_eq!(
            accounts
                .authenticate(EMAIL, "wrong")
                .await
                .expect("it answers"),
            Authentication::Refused
        );
    }
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Locked
    );
}

#[tokio::test]
async fn a_lockout_decays_because_nothing_else_can_clear_it() {
    // There is no unlock operation on any surface, and `login`, `reauthenticate` and
    // `password` all refuse on `Locked` before they verify anything — so without a decay a
    // lockout is a permanently lost account rather than a throttle.
    let clock = Arc::new(ManualClock::default());
    let accounts = seeded_on(clock.clone()).await;
    fail(&accounts, MAX_FAILED_ATTEMPTS).await;
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Locked
    );

    // One second short of the window: still locked. The boundary is asserted because an
    // off-by-one here is a lockout that never engages.
    clock.advance(WINDOW - SignedDuration::from_secs(1));
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Locked
    );

    clock.advance(SignedDuration::from_secs(1));
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
}

#[tokio::test]
async fn attempts_during_a_lockout_do_not_extend_it() {
    // Deliberate, and the direction is not obvious. Extending the window on every attempt
    // would keep a live guessing run permanently locked out — and would hand anybody who can
    // reach the endpoint a way to keep *somebody else's* account locked forever by hammering
    // it, which is a denial of service on an account rather than a defence of it. So the
    // window runs from the last **counted** failure, and an attempt made while locked is
    // refused without being counted.
    //
    // What that costs is bounded and small: a run gets `MAX_FAILED_ATTEMPTS` guesses per
    // window and no more, which is four a minute at the default. What bounds an attacker
    // across *many* accounts is a rate limiter, and the counter port that would carry one
    // has no trusted client address to key on (`registry`, the disclosure section).
    let clock = Arc::new(ManualClock::default());
    let accounts = seeded_on(clock.clone()).await;
    fail(&accounts, MAX_FAILED_ATTEMPTS).await;
    for _ in 0..3 {
        clock.advance(SignedDuration::from_mins(1));
        fail(&accounts, 1).await;
    }
    // Still inside the window measured from the tenth *counted* failure: locked.
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Locked
    );
    // Past it: open, and the hammering did not move the deadline.
    clock.advance(WINDOW);
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
}

#[tokio::test]
async fn failures_spread_wider_than_the_window_never_accumulate() {
    // Ten mistypes over a year is not a guessing run, and counting them as one would lock an
    // account on a tenth attempt made months after the ninth.
    let clock = Arc::new(ManualClock::default());
    let accounts = seeded_on(clock.clone()).await;
    for _ in 0..MAX_FAILED_ATTEMPTS * 2 {
        fail(&accounts, 1).await;
        clock.advance(WINDOW + SignedDuration::from_secs(1));
    }
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
}

#[tokio::test]
async fn a_success_before_the_ceiling_clears_the_count() {
    let accounts = seeded().await;
    for _ in 0..MAX_FAILED_ATTEMPTS - 1 {
        let _ = accounts.authenticate(EMAIL, "wrong").await;
    }
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
    // Back to zero: the next wrong password does not tip an already-full counter over.
    let _ = accounts.authenticate(EMAIL, "wrong").await;
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
}

#[tokio::test]
async fn a_cancelled_attempt_keeps_its_charge() {
    // Decision 3 of #510: nothing refunds a charge when the request goes away, or
    // disconnecting mid-verification would be a free guess. The reservation here completes
    // inside the first poll, so a pending first poll is an attempt charged and verifying.
    let accounts = seeded().await;
    fail(&accounts, MAX_FAILED_ATTEMPTS - 1).await;

    let mut attempt = accounts.authenticate(EMAIL, PASSWORD);
    let pending =
        std::future::poll_fn(|cx| std::task::Poll::Ready(attempt.as_mut().poll(cx).is_pending()))
            .await;
    assert!(
        pending,
        "the attempt must still be verifying when it is dropped"
    );
    assert_eq!(accounts.recorded_failures(EMAIL), Some(MAX_FAILED_ATTEMPTS));
    drop(attempt);

    // The correct password that was cancelled counted: the account is now at the ceiling.
    assert_eq!(accounts.recorded_failures(EMAIL), Some(MAX_FAILED_ATTEMPTS));
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Locked
    );
}

#[tokio::test]
async fn a_password_change_clears_a_lockout() {
    // The port requires it: a change is a successful credential presentation.
    let accounts = seeded().await;
    for _ in 0..MAX_FAILED_ATTEMPTS {
        let _ = accounts.authenticate(EMAIL, "wrong").await;
    }
    assert_eq!(
        accounts
            .set_password(&user(), "a brand new password", Timestamp::UNIX_EPOCH)
            .await
            .expect("it writes"),
        PasswordChanged::Yes
    );
    assert_eq!(
        accounts
            .authenticate(EMAIL, "a brand new password")
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
    assert_eq!(
        accounts
            .authenticate(EMAIL, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Refused
    );
}

#[tokio::test]
async fn re_authentication_takes_the_account_from_the_credential_and_not_the_request() {
    let accounts = seeded().await;
    assert_eq!(
        accounts
            .authenticate_user(&user(), PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Granted(user())
    );
    let stranger = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e71");
    assert_eq!(
        accounts
            .authenticate_user(&stranger, PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Refused
    );
}

#[tokio::test]
async fn changing_a_password_for_an_absent_account_writes_nothing() {
    let accounts = seeded().await;
    let stranger = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e72");
    assert_eq!(
        accounts
            .set_password(&stranger, "irrelevant", Timestamp::UNIX_EPOCH)
            .await
            .expect("it answers"),
        PasswordChanged::NoSuchAccount
    );
}

#[tokio::test]
async fn a_profile_reads_back_what_registration_wrote_and_takes_an_edit() {
    let accounts = seeded().await;
    let profile = accounts
        .read(&user())
        .await
        .expect("it answers")
        .expect("the account exists");
    assert_eq!(profile.email.as_deref(), Some(EMAIL));
    assert_eq!(profile.display_name, None);
    assert_eq!(profile.created_at, Timestamp::UNIX_EPOCH);

    let updated = accounts
        .update(
            &user(),
            &ProfileUpdate {
                display_name: Some(Some("Ada Lovelace".to_owned())),
            },
        )
        .await
        .expect("it answers")
        .expect("the account exists");
    assert_eq!(updated.display_name.as_deref(), Some("Ada Lovelace"));

    // An absent field leaves the name alone; `Some(None)` clears it.
    let untouched = accounts
        .update(&user(), &ProfileUpdate::default())
        .await
        .expect("it answers")
        .expect("the account exists");
    assert_eq!(untouched.display_name.as_deref(), Some("Ada Lovelace"));

    let cleared = accounts
        .update(
            &user(),
            &ProfileUpdate {
                display_name: Some(None),
            },
        )
        .await
        .expect("it answers")
        .expect("the account exists");
    assert_eq!(cleared.display_name, None);
}

#[tokio::test]
async fn a_profile_for_an_absent_account_is_absent_and_not_an_error() {
    // Reachable with a perfectly valid credential: a session outlives the account row it
    // names if the account is deleted while a token is live.
    let accounts = seeded().await;
    let stranger = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e73");
    assert!(
        accounts
            .read(&stranger)
            .await
            .expect("it answers")
            .is_none()
    );
    assert!(
        accounts
            .update(&stranger, &ProfileUpdate::default())
            .await
            .expect("it answers")
            .is_none()
    );
}

#[tokio::test]
async fn addresses_are_compared_verbatim() {
    // Recorded as a test rather than left implicit: case folding is a normalization policy
    // this port does not describe, and #402's adapter has to make the same choice.
    let accounts = seeded().await;
    assert_eq!(
        accounts
            .authenticate("Somebody@Example.test", PASSWORD)
            .await
            .expect("it answers"),
        Authentication::Refused
    );
}
