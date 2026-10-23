use jiff::Timestamp;

use super::{
    ActivateOutcome, BeginOutcome, ConsumeOutcome, EnrollmentState, InMemoryTotp, TotpCodes,
    TotpEnrollment, TotpStore,
};
use crate::store::UserId;

fn user() -> UserId {
    UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f")
}

fn pending() -> TotpEnrollment {
    TotpEnrollment {
        user_id: user(),
        secret: TotpCodes::generate_secret(),
        state: EnrollmentState::Pending,
        last_step: None,
        enrolled_at: Timestamp::UNIX_EPOCH,
        activated_at: None,
    }
}

#[tokio::test]
async fn an_abandoned_pending_enrollment_is_replaced_without_ceremony() {
    let store = InMemoryTotp::new();
    assert_eq!(
        store.begin(pending()).await.expect("it writes"),
        BeginOutcome::Started
    );
    let second = pending();
    let expected = second.secret.clone();
    assert_eq!(
        store.begin(second).await.expect("it writes"),
        BeginOutcome::Started
    );
    let held = store
        .read(&user())
        .await
        .expect("it answers")
        .expect("it is held");
    assert_eq!(held.secret, expected);
}

#[tokio::test]
async fn an_active_enrollment_is_never_silently_replaced() {
    // The one refusal `begin` makes, and the reason it exists: an enroll that overwrote an
    // active secret would let a stolen session swap the factor for one the attacker holds
    // without ever presenting a code.
    let store = InMemoryTotp::new();
    store.begin(pending()).await.expect("it writes");
    store
        .activate(&user(), 7, Timestamp::UNIX_EPOCH)
        .await
        .expect("it writes");
    assert_eq!(
        store.begin(pending()).await.expect("it answers"),
        BeginOutcome::AlreadyActive
    );
}

#[tokio::test]
async fn only_a_pending_enrollment_activates_and_the_confirming_code_is_spent() {
    let store = InMemoryTotp::new();
    assert_eq!(
        store
            .activate(&user(), 7, Timestamp::UNIX_EPOCH)
            .await
            .expect("it answers"),
        ActivateOutcome::NotPending,
        "there is nothing to confirm"
    );
    store.begin(pending()).await.expect("it writes");
    assert_eq!(
        store
            .activate(&user(), 7, Timestamp::UNIX_EPOCH)
            .await
            .expect("it writes"),
        ActivateOutcome::Activated
    );
    assert_eq!(
        store
            .activate(&user(), 8, Timestamp::UNIX_EPOCH)
            .await
            .expect("it answers"),
        ActivateOutcome::NotPending,
        "an active enrollment is not pending"
    );
    // The code that confirmed the enrollment must not also complete a sign-in.
    assert_eq!(
        store.consume(&user(), 7).await.expect("it answers"),
        ConsumeOutcome::Replayed
    );
}

#[tokio::test]
async fn a_step_at_or_below_the_last_used_one_is_a_replay() {
    // RFC 6238 §5.2: a code is accepted at most once, and a code is valid for three steps
    // with drift — so re-typing it twelve seconds later is a real attack.
    let store = InMemoryTotp::new();
    store.begin(pending()).await.expect("it writes");
    assert_eq!(
        store.consume(&user(), 10).await.expect("it answers"),
        ConsumeOutcome::Fresh
    );
    assert_eq!(
        store.consume(&user(), 10).await.expect("it answers"),
        ConsumeOutcome::Replayed
    );
    assert_eq!(
        store.consume(&user(), 9).await.expect("it answers"),
        ConsumeOutcome::Replayed
    );
    assert_eq!(
        store.consume(&user(), 11).await.expect("it answers"),
        ConsumeOutcome::Fresh
    );
}

#[tokio::test]
async fn consuming_against_nothing_says_so_rather_than_accepting() {
    let store = InMemoryTotp::new();
    assert_eq!(
        store.consume(&user(), 1).await.expect("it answers"),
        ConsumeOutcome::NotEnrolled
    );
}

#[tokio::test]
async fn disabling_reports_whether_there_was_anything_to_disable() {
    let store = InMemoryTotp::new();
    assert!(!store.disable(&user()).await.expect("it answers"));
    store.begin(pending()).await.expect("it writes");
    assert!(store.disable(&user()).await.expect("it answers"));
    assert!(store.read(&user()).await.expect("it answers").is_none());
}
