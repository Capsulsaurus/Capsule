use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::store::memory::ManualClock;

/// A store that refuses every write, for the "recording never refuses the act" rule.
#[derive(Debug)]
struct Refusing;

impl ActivityStore for Refusing {
    fn record(&self, _: ActivityEvent, _: Timestamp) -> StoreFuture<'_, ()> {
        Box::pin(async {
            Err(StoreError::Unavailable {
                store: "activity",
                detail: "down".to_owned(),
            })
        })
    }

    fn recent<'a>(
        &'a self,
        _: &'a UserId,
        _: Timestamp,
        _: usize,
    ) -> StoreFuture<'a, Vec<ActivityEvent>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// A context over a fresh in-memory log and a clock the test moves.
fn context() -> (ActivityContext, ManualClock) {
    let clock = ManualClock::new(Timestamp::UNIX_EPOCH + SignedDuration::from_hours(24 * 365));
    let context = ActivityContext::new(Arc::new(InMemoryActivity::new()), Arc::new(clock.clone()));
    (context, clock)
}

#[test]
fn every_kind_round_trips_through_its_token_and_nothing_else_decodes() {
    for kind in ActivityKind::ALL {
        assert_eq!(ActivityKind::from_token(kind.as_str()), Some(kind));
    }
    assert_eq!(ActivityKind::from_token("sign_in"), None);
    assert_eq!(ActivityKind::from_token(""), None);
}

#[test]
fn every_token_is_distinct() {
    let tokens: std::collections::BTreeSet<_> =
        ActivityKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(tokens.len(), ActivityKind::ALL.len());
}

#[tokio::test]
async fn a_recorded_event_carries_the_clock_the_session_and_the_subject() {
    let (activity, clock) = context();
    let user = UserId::new("alice");
    let session = SessionId::new("session-1");
    activity
        .record(
            &user,
            ActivityKind::SessionRevoked,
            Some(&session),
            Some("session-2"),
        )
        .await;

    let events = activity.recent(&user, 10).await.expect("an in-memory read");
    let [event] = events.as_slice() else {
        panic!("one event, got {events:?}");
    };
    assert_eq!(event.kind, ActivityKind::SessionRevoked);
    assert_eq!(event.at, clock.now());
    assert_eq!(event.session_id.as_ref(), Some(&session));
    assert_eq!(event.subject.as_deref(), Some("session-2"));
    // Outside a request span there is no request to name.
    assert_eq!(event.request_id, None);
    let id = uuid::Uuid::parse_str(&event.event_id).expect("a UUID event id");
    assert_eq!(id.get_version_num(), 7);
}

#[tokio::test]
async fn a_read_hides_events_past_the_retention_window() {
    let (activity, clock) = context();
    let user = UserId::new("alice");
    activity
        .record(&user, ActivityKind::PasswordChanged, None, None)
        .await;
    clock.advance(RETENTION);
    assert_eq!(activity.recent(&user, 10).await.expect("a read").len(), 1);
    clock.advance(SignedDuration::from_secs(1));
    assert!(activity.recent(&user, 10).await.expect("a read").is_empty());
}

#[tokio::test]
async fn an_append_after_the_window_deletes_what_expired() {
    let (activity, clock) = context();
    let user = UserId::new("alice");
    activity
        .record(&user, ActivityKind::TotpEnabled, None, None)
        .await;
    clock.advance(RETENTION + SignedDuration::from_secs(1));
    activity
        .record(&user, ActivityKind::TotpDisabled, None, None)
        .await;
    let held = activity
        .store()
        .recent(&user, Timestamp::MIN, MAX_EVENTS_PER_ACCOUNT)
        .await
        .expect("a read");
    assert_eq!(
        held.iter().map(|event| event.kind).collect::<Vec<_>>(),
        [ActivityKind::TotpDisabled]
    );
}

#[tokio::test]
async fn a_read_is_clamped_to_the_bound() {
    let (activity, _clock) = context();
    let user = UserId::new("alice");
    for _ in 0..3 {
        activity
            .record(&user, ActivityKind::SignOut, None, None)
            .await;
    }
    assert_eq!(
        activity
            .recent(&user, usize::MAX)
            .await
            .expect("a read")
            .len(),
        3
    );
}

#[tokio::test]
async fn a_store_that_cannot_record_does_not_fail_the_caller() {
    let activity = ActivityContext::new(Arc::new(Refusing), Arc::new(ManualClock::default()));
    // Returns `()`: there is nothing for a caller to refuse on.
    activity
        .record(&UserId::new("alice"), ActivityKind::SignOut, None, None)
        .await;
}
