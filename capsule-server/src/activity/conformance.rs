//! The one suite every [`ActivityStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **The log is the owner's, newest first, in the order it was recorded** — not re-sorted by
//!   the events' instants — and only to the account it belongs to.
//! - **Every field reads back exactly as recorded**, every kind and every absent field included.
//! - **The retention bound holds on both axes.** A read never returns an event older than its
//!   `since`; an append deletes the account's events older than its `retain_from` and every
//!   event beyond the newest [`MAX_EVENTS_PER_ACCOUNT`], and touches no other account's log.
//! - **Append-only.** A duplicate `event_id` is refused and the first event stands.
//!
//! # Reusing a harness
//!
//! Every case scopes its own account, so cases may share one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};

use super::{ActivityEvent, ActivityKind, ActivityStore, MAX_EVENTS_PER_ACCOUNT};
use crate::store::{SessionId, StoreError, UserId};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The activity store under test.
    fn activity(&self) -> &dyn ActivityStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming activity store must succeed at {doing}: {error}"),
    }
}

/// An instant `secs` seconds after a fixed origin.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// Long before every instant a case records: a `since` or `retain_from` that keeps everything.
fn dawn() -> Timestamp {
    Timestamp::UNIX_EPOCH
}

/// `case`'s account.
fn user(case: &str) -> UserId {
    UserId::new(format!("{case}-user"))
}

/// Event `n` of `case`, a sign-out at `secs` with every optional field set.
fn event(case: &str, n: usize, secs: i64) -> ActivityEvent {
    ActivityEvent {
        event_id: format!("{case}-event-{n:05}"),
        user_id: user(case),
        kind: ActivityKind::SignOut,
        at: at(secs),
        request_id: Some(format!("{case}-request-{n}")),
        session_id: Some(SessionId::new(format!("{case}-session"))),
        subject: Some(format!("{case}-subject-{n}")),
    }
}

/// Record `event`, keeping everything.
async fn record(h: &dyn Harness, event: ActivityEvent) {
    ok(
        h.activity().record(event, dawn()).await,
        "record an activity event",
    );
}

/// Every event of `case`'s account the store holds, newest first.
async fn everything(h: &dyn Harness, case: &str) -> Vec<ActivityEvent> {
    ok(
        h.activity()
            .recent(&user(case), dawn(), MAX_EVENTS_PER_ACCOUNT * 2)
            .await,
        "read an activity log",
    )
}

/// The ids of `events`, in order.
fn ids(events: &[ActivityEvent]) -> Vec<&str> {
    events.iter().map(|event| event.event_id.as_str()).collect()
}

/// An account nothing was recorded for reads an empty log.
pub async fn an_untouched_account_has_an_empty_log(h: &dyn Harness) {
    assert!(everything(h, "untouched").await.is_empty());
}

/// A recorded event reads back with every field as recorded.
pub async fn an_event_reads_back_exactly_as_recorded(h: &dyn Harness) {
    let recorded = event("exact", 1, 10);
    record(h, recorded.clone()).await;
    assert_eq!(everything(h, "exact").await, vec![recorded]);
}

/// Every kind and every absent optional field survives the round trip.
pub async fn every_kind_and_absent_field_round_trips(h: &dyn Harness) {
    let mut recorded = Vec::new();
    for (n, kind) in ActivityKind::ALL.into_iter().enumerate() {
        let event = ActivityEvent {
            kind,
            request_id: None,
            session_id: None,
            subject: None,
            ..event("kinds", n, i64::try_from(n).expect("a small index"))
        };
        record(h, event.clone()).await;
        recorded.push(event);
    }
    recorded.reverse();
    assert_eq!(everything(h, "kinds").await, recorded);
}

/// An instant with sub-second precision reads back to the microsecond.
pub async fn instants_read_back_to_the_microsecond(h: &dyn Harness) {
    let recorded = ActivityEvent {
        at: at(0) + SignedDuration::from_micros(123_456),
        ..event("micros", 1, 0)
    };
    record(h, recorded.clone()).await;
    assert_eq!(everything(h, "micros").await, vec![recorded]);
}

/// The log reads newest first in the order events were recorded, not by their instants.
pub async fn the_log_is_newest_first_in_recording_order(h: &dyn Harness) {
    // Recorded with instants that tie and go backwards: the order is the recording order.
    record(h, event("order", 1, 30)).await;
    record(h, event("order", 2, 10)).await;
    record(h, event("order", 3, 10)).await;
    assert_eq!(
        ids(&everything(h, "order").await),
        [
            "order-event-00003",
            "order-event-00002",
            "order-event-00001"
        ]
    );
}

/// A log holds only its own account's events.
pub async fn a_log_is_scoped_to_its_account(h: &dyn Harness) {
    record(h, event("mine", 1, 1)).await;
    record(h, event("theirs", 1, 1)).await;
    assert_eq!(ids(&everything(h, "mine").await), ["mine-event-00001"]);
    assert_eq!(ids(&everything(h, "theirs").await), ["theirs-event-00001"]);
}

/// A read returns at most `limit` events, the newest.
pub async fn a_read_returns_the_newest_limit_events(h: &dyn Harness) {
    for n in 1..=5 {
        record(
            h,
            event("limit", n, i64::try_from(n).expect("a small index")),
        )
        .await;
    }
    let read = ok(
        h.activity().recent(&user("limit"), dawn(), 2).await,
        "read an activity log",
    );
    assert_eq!(ids(&read), ["limit-event-00005", "limit-event-00004"]);
}

/// A read never returns an event recorded before its `since`, deleted or not.
pub async fn a_read_excludes_events_before_its_since(h: &dyn Harness) {
    record(h, event("since", 1, 10)).await;
    record(h, event("since", 2, 20)).await;
    let read = ok(
        h.activity().recent(&user("since"), at(20), 10).await,
        "read an activity log",
    );
    assert_eq!(ids(&read), ["since-event-00002"]);
}

/// An append deletes its account's events older than `retain_from`, and nobody else's.
pub async fn an_append_deletes_its_accounts_expired_events(h: &dyn Harness) {
    record(h, event("expiry", 1, 10)).await;
    record(h, event("expiry", 2, 20)).await;
    record(h, event("bystander", 1, 10)).await;
    ok(
        h.activity().record(event("expiry", 3, 30), at(20)).await,
        "record an activity event",
    );
    assert_eq!(
        ids(&everything(h, "expiry").await),
        ["expiry-event-00003", "expiry-event-00002"]
    );
    assert_eq!(
        ids(&everything(h, "bystander").await),
        ["bystander-event-00001"]
    );
}

/// An append keeps at most [`MAX_EVENTS_PER_ACCOUNT`] events, deleting the oldest.
pub async fn an_append_keeps_at_most_the_bound(h: &dyn Harness) {
    let over = MAX_EVENTS_PER_ACCOUNT + 3;
    for n in 1..=over {
        record(h, event("bound", n, 1)).await;
    }
    let held = everything(h, "bound").await;
    assert_eq!(held.len(), MAX_EVENTS_PER_ACCOUNT);
    assert_eq!(
        held.first().map(|event| event.event_id.clone()),
        Some(format!("bound-event-{over:05}"))
    );
    assert_eq!(
        held.last().map(|event| event.event_id.clone()),
        Some("bound-event-00004".to_owned())
    );
}

/// A duplicate `event_id` is refused, and the first event stands.
pub async fn a_duplicate_event_id_is_refused_and_the_first_stands(h: &dyn Harness) {
    let first = event("duplicate", 1, 1);
    record(h, first.clone()).await;
    let second = ActivityEvent {
        kind: ActivityKind::PasswordChanged,
        ..first.clone()
    };
    match h.activity().record(second, dawn()).await {
        Err(StoreError::Rejected { .. }) => {}
        other => panic!("a duplicate event id must be refused, got {other:?}"),
    }
    assert_eq!(everything(h, "duplicate").await, vec![first]);
}

/// The whole suite, in one pass on one store.
pub async fn run_all(h: &dyn Harness) {
    an_untouched_account_has_an_empty_log(h).await;
    an_event_reads_back_exactly_as_recorded(h).await;
    every_kind_and_absent_field_round_trips(h).await;
    instants_read_back_to_the_microsecond(h).await;
    the_log_is_newest_first_in_recording_order(h).await;
    a_log_is_scoped_to_its_account(h).await;
    a_read_returns_the_newest_limit_events(h).await;
    a_read_excludes_events_before_its_since(h).await;
    an_append_deletes_its_accounts_expired_events(h).await;
    an_append_keeps_at_most_the_bound(h).await;
    a_duplicate_event_id_is_refused_and_the_first_stands(h).await;
}

#[cfg(test)]
mod tests;
