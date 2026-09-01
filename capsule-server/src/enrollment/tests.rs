//! The freshness gate, which is the only part of this module that decides anything.

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::store::memory::{InMemoryChannels, InMemoryEnrollments, ManualClock};

fn context() -> EnrollmentContext {
    let clock: Arc<dyn crate::store::Clock> = Arc::new(ManualClock::default());
    EnrollmentContext::new(
        Arc::new(InMemoryEnrollments::new(
            Arc::clone(&clock),
            SignedDuration::from_mins(10),
        )),
        Arc::new(InMemoryChannels::new(
            Arc::clone(&clock),
            SignedDuration::from_mins(10),
        )),
        clock,
    )
}

fn at(mins: i64) -> Timestamp {
    crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_mins(mins))
}

#[test]
fn a_recent_credential_opens_the_window_and_an_old_one_does_not() {
    let context = context();
    assert!(context.is_fresh(at(0), at(0)));
    assert!(context.is_fresh(at(0), at(4)));
    assert!(
        context.is_fresh(at(0), at(5)),
        "the window is a permission, not a margin to be conservative inside"
    );
    assert!(!context.is_fresh(at(0), at(6)));
}

#[test]
fn the_window_is_deployment_configurable() {
    let context = context().with_fresh_auth_window(SignedDuration::from_mins(1));
    assert!(context.is_fresh(at(0), at(1)));
    assert!(!context.is_fresh(at(0), at(2)));
    assert_eq!(FRESH_AUTH_WINDOW.as_mins(), 5);
}

#[test]
fn a_future_authentication_is_still_fresh() {
    // Clock skew between the store and the request should never *close* the window: the failure
    // mode of refusing a legitimate add is worse than the failure mode of honouring one a few
    // seconds early, and a negative duration is under any positive bound anyway.
    let context = context();
    assert!(context.is_fresh(at(10), at(0)));
}
