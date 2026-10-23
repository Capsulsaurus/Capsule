//! The one suite every [`TotpStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **An active factor is never silently replaced.** `begin` over an active enrollment answers
//!   `AlreadyActive` and changes nothing; over a pending one it replaces it.
//! - **Activation is pending-only and spends its code.** The confirming step is recorded, so the
//!   code that confirmed an enrollment cannot also complete a sign-in.
//! - **Consumption is a compare-and-set.** A step at or below the highest one used is a replay,
//!   and two consumptions racing on one step admit exactly one — RFC 6238 §5.2.
//! - **Absence is an outcome, not an error.** No enrollment is `NotPending`, `NotEnrolled`,
//!   `false` or `None`, as each operation says.
//! - **Accounts are isolated**, and what is read back is what was written — the secret
//!   verbatim, the instants at no coarser than microsecond precision and never later.
//!
//! # Reusing a harness
//!
//! Every case scopes its own accounts, so cases may share one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};

use super::{
    ActivateOutcome, BeginOutcome, ConsumeOutcome, EnrollmentState, TotpEnrollment, TotpSecret,
    TotpStore,
};
use crate::auth::directory::DirectoryError;
use crate::store::UserId;

/// The store under test.
pub trait Harness: Send + Sync {
    /// The enrollment store under test.
    fn enrollments(&self) -> &dyn TotpStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, DirectoryError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming TOTP store must succeed at {doing}: {error}"),
    }
}

/// `case`'s account `name`.
fn user(case: &str, name: &str) -> UserId {
    UserId::new(format!("{case}-{name}"))
}

/// A whole-microsecond instant `secs` seconds after a fixed origin.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// A fresh pending enrollment for `user` under `secret`.
fn pending(user: &UserId, secret: &str) -> TotpEnrollment {
    TotpEnrollment {
        user_id: user.clone(),
        secret: TotpSecret::new(secret),
        state: EnrollmentState::Pending,
        last_step: None,
        enrolled_at: at(0),
        activated_at: None,
    }
}

/// `user`'s enrollment, if any.
async fn read(h: &dyn Harness, user: &UserId) -> Option<TotpEnrollment> {
    ok(h.enrollments().read(user).await, "read an enrollment")
}

/// Begin `record`, expecting the call to succeed.
async fn begin(h: &dyn Harness, record: TotpEnrollment) -> BeginOutcome {
    ok(h.enrollments().begin(record).await, "begin an enrollment")
}

/// Consume `step` for `user`.
async fn consume(h: &dyn Harness, user: &UserId, step: u64) -> ConsumeOutcome {
    ok(h.enrollments().consume(user, step).await, "consume a step")
}

// ===========================================================================================
// Beginning
// ===========================================================================================

/// A first `begin` starts a pending enrollment that reads back exactly, secret included.
pub async fn a_first_begin_starts_a_pending_enrollment(h: &dyn Harness) {
    let alice = user("begin", "alice");
    let record = pending(&alice, "JBSWY3DPEHPK3PXP");
    assert_eq!(begin(h, record.clone()).await, BeginOutcome::Started);
    let held = read(h, &alice).await.expect("the enrollment is held");
    assert_eq!(held, record);
    assert_eq!(held.secret.expose_base32(), "JBSWY3DPEHPK3PXP");
}

/// A pending enrollment is replaced by the next `begin`, without ceremony.
pub async fn a_pending_enrollment_is_replaced(h: &dyn Harness) {
    let alice = user("replace", "alice");
    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    let mut again = pending(&alice, "BBBBBBBBBBBBBBBB");
    again.enrolled_at = at(60);
    assert_eq!(begin(h, again.clone()).await, BeginOutcome::Started);
    assert_eq!(read(h, &alice).await, Some(again));
}

/// An active enrollment refuses `begin`, and the refusal changes nothing.
pub async fn an_active_enrollment_refuses_begin_and_changes_nothing(h: &dyn Harness) {
    let alice = user("active", "alice");
    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 100, at(30)).await,
            "activate"
        ),
        ActivateOutcome::Activated
    );
    let before = read(h, &alice).await.expect("held");
    let mut attacker = pending(&alice, "ZZZZZZZZZZZZZZZZ");
    attacker.enrolled_at = at(90);
    assert_eq!(
        begin(h, attacker).await,
        BeginOutcome::AlreadyActive,
        "an active factor must never be replaced by a begin"
    );
    assert_eq!(
        read(h, &alice).await,
        Some(before),
        "a refused begin changes nothing"
    );
}

// ===========================================================================================
// Activating
// ===========================================================================================

/// Activation promotes a pending enrollment, stamps it, and spends the confirming step.
pub async fn activation_promotes_and_spends_its_step(h: &dyn Harness) {
    let alice = user("activate", "alice");
    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 100, at(30)).await,
            "activate"
        ),
        ActivateOutcome::Activated
    );
    let held = read(h, &alice).await.expect("held");
    assert_eq!(held.state, EnrollmentState::Active);
    assert_eq!(held.activated_at, Some(at(30)));
    assert_eq!(held.last_step, Some(100));
    assert_eq!(held.secret.expose_base32(), "AAAAAAAAAAAAAAAA");
    assert_eq!(
        consume(h, &alice, 100).await,
        ConsumeOutcome::Replayed,
        "the code that confirmed an enrollment must not also complete a sign-in"
    );
    assert_eq!(consume(h, &alice, 101).await, ConsumeOutcome::Fresh);
}

/// Activating an active enrollment, or an account with none, is `NotPending` and changes nothing.
pub async fn activation_is_pending_only(h: &dyn Harness) {
    let alice = user("notpending", "alice");
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 1, at(1)).await,
            "activate an unknown account"
        ),
        ActivateOutcome::NotPending
    );
    assert_eq!(read(h, &alice).await, None, "nothing was created");

    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 100, at(30)).await,
            "activate"
        ),
        ActivateOutcome::Activated
    );
    let before = read(h, &alice).await.expect("held");
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 200, at(60)).await,
            "activate again"
        ),
        ActivateOutcome::NotPending
    );
    assert_eq!(
        read(h, &alice).await,
        Some(before),
        "a second activation must not move the step or the instant"
    );
}

// ===========================================================================================
// Consuming
// ===========================================================================================

/// A step is fresh only above every step used before; at or below is a replay.
pub async fn consumption_is_a_compare_and_set(h: &dyn Harness) {
    let alice = user("consume", "alice");
    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    assert_eq!(
        consume(h, &alice, 0).await,
        ConsumeOutcome::Fresh,
        "on a fresh enrollment every step is new, step zero included"
    );
    assert_eq!(consume(h, &alice, 0).await, ConsumeOutcome::Replayed);
    assert_eq!(consume(h, &alice, 5).await, ConsumeOutcome::Fresh);
    assert_eq!(consume(h, &alice, 5).await, ConsumeOutcome::Replayed);
    assert_eq!(
        consume(h, &alice, 4).await,
        ConsumeOutcome::Replayed,
        "a step below the highest used is a replay too"
    );
    assert_eq!(consume(h, &alice, 6).await, ConsumeOutcome::Fresh);
    assert_eq!(read(h, &alice).await.expect("held").last_step, Some(6));
}

/// Consuming for an account with no enrollment is `NotEnrolled`.
pub async fn consumption_without_an_enrollment_is_not_enrolled(h: &dyn Harness) {
    let nobody = user("noenrol", "nobody");
    assert_eq!(consume(h, &nobody, 7).await, ConsumeOutcome::NotEnrolled);
    assert_eq!(read(h, &nobody).await, None, "nothing was created");
}

/// Consumptions racing on one step admit exactly one.
pub async fn racing_consumptions_admit_exactly_one(h: &dyn Harness) {
    let alice = user("race", "alice");
    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    let store = h.enrollments();
    let (first, second, third, fourth) = tokio::join!(
        store.consume(&alice, 42),
        store.consume(&alice, 42),
        store.consume(&alice, 42),
        store.consume(&alice, 42),
    );
    let outcomes = [
        ok(first, "consume a"),
        ok(second, "consume b"),
        ok(third, "consume c"),
        ok(fourth, "consume d"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == ConsumeOutcome::Fresh)
            .count(),
        1,
        "one code, one acceptance: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, ConsumeOutcome::Fresh | ConsumeOutcome::Replayed)),
        "{outcomes:?}"
    );
}

// ===========================================================================================
// Disabling
// ===========================================================================================

/// Disabling removes the enrollment once; afterwards the account is unenrolled and may begin.
pub async fn disabling_removes_the_enrollment_once(h: &dyn Harness) {
    let alice = user("disable", "alice");
    assert!(
        !ok(h.enrollments().disable(&alice).await, "disable nothing"),
        "disabling an account with no enrollment answers false"
    );
    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 100, at(30)).await,
            "activate"
        ),
        ActivateOutcome::Activated
    );
    assert!(ok(h.enrollments().disable(&alice).await, "disable"));
    assert_eq!(read(h, &alice).await, None);
    assert!(!ok(h.enrollments().disable(&alice).await, "disable again"));
    assert_eq!(consume(h, &alice, 101).await, ConsumeOutcome::NotEnrolled);
    assert_eq!(
        begin(h, pending(&alice, "BBBBBBBBBBBBBBBB")).await,
        BeginOutcome::Started,
        "a disabled factor no longer blocks a fresh enrollment"
    );
}

// ===========================================================================================
// Isolation and fidelity
// ===========================================================================================

/// One account's enrollment, activation, consumption and removal never touch another's.
pub async fn enrollments_do_not_leak_between_accounts(h: &dyn Harness) {
    let alice = user("isolated", "alice");
    let bob = user("isolated", "bob");
    assert_eq!(
        begin(h, pending(&alice, "AAAAAAAAAAAAAAAA")).await,
        BeginOutcome::Started
    );
    assert_eq!(read(h, &bob).await, None);
    assert_eq!(
        begin(h, pending(&bob, "BBBBBBBBBBBBBBBB")).await,
        BeginOutcome::Started
    );
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 100, at(30)).await,
            "activate alice"
        ),
        ActivateOutcome::Activated
    );
    assert_eq!(consume(h, &alice, 101).await, ConsumeOutcome::Fresh);
    let bobs = read(h, &bob).await.expect("bob's is held");
    assert_eq!(bobs, pending(&bob, "BBBBBBBBBBBBBBBB"));
    assert!(ok(h.enrollments().disable(&alice).await, "disable alice"));
    assert_eq!(read(h, &bob).await, Some(bobs));
}

/// Instants come back at microsecond precision or finer, and never later than they went in.
pub async fn the_instants_round_trip_to_the_microsecond(h: &dyn Harness) {
    let alice = user("precise", "alice");
    let enrolled = Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    let activated = Timestamp::from_nanosecond(1_700_000_030_987_654_321).expect("an instant");
    let mut record = pending(&alice, "AAAAAAAAAAAAAAAA");
    record.enrolled_at = enrolled;
    assert_eq!(begin(h, record).await, BeginOutcome::Started);
    assert_eq!(
        ok(
            h.enrollments().activate(&alice, 1, activated).await,
            "activate"
        ),
        ActivateOutcome::Activated
    );
    let held = read(h, &alice).await.expect("held");
    let activated_held = held.activated_at.expect("activated");
    for (stored, given) in [(held.enrolled_at, enrolled), (activated_held, activated)] {
        assert!(stored <= given, "a stored instant must never move later");
        assert!(
            given.duration_since(stored) < SignedDuration::from_micros(1),
            "a stored instant may lose at most sub-microsecond precision: {stored} vs {given}"
        );
    }
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    a_first_begin_starts_a_pending_enrollment(h).await;
    a_pending_enrollment_is_replaced(h).await;
    an_active_enrollment_refuses_begin_and_changes_nothing(h).await;
    activation_promotes_and_spends_its_step(h).await;
    activation_is_pending_only(h).await;
    consumption_is_a_compare_and_set(h).await;
    consumption_without_an_enrollment_is_not_enrolled(h).await;
    racing_consumptions_admit_exactly_one(h).await;
    disabling_removes_the_enrollment_once(h).await;
    enrollments_do_not_leak_between_accounts(h).await;
    the_instants_round_trip_to_the_microsecond(h).await;
}

#[cfg(test)]
mod tests;
