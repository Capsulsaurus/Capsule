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

    fn count_expired(&self, _: Timestamp) -> StoreFuture<'_, u64> {
        Box::pin(async { Ok(0) })
    }

    fn delete_expired(&self, _: Timestamp, _: usize) -> StoreFuture<'_, u64> {
        Box::pin(async { Ok(0) })
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

/// A store whose writes never finish, for the bound on writes in flight.
#[derive(Debug)]
struct Stalled;

impl ActivityStore for Stalled {
    fn record(&self, _: ActivityEvent, _: Timestamp) -> StoreFuture<'_, ()> {
        Box::pin(std::future::pending())
    }

    fn recent<'a>(
        &'a self,
        _: &'a UserId,
        _: Timestamp,
        _: usize,
    ) -> StoreFuture<'a, Vec<ActivityEvent>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn count_expired(&self, _: Timestamp) -> StoreFuture<'_, u64> {
        Box::pin(async { Ok(0) })
    }

    fn delete_expired(&self, _: Timestamp, _: usize) -> StoreFuture<'_, u64> {
        Box::pin(async { Ok(0) })
    }
}

#[tokio::test]
async fn a_detached_kind_is_written_once_per_account_per_window() {
    let (activity, clock) = context();
    let alice = UserId::new("alice");
    let refused = ActivityKind::SignInPasswordRefused;

    assert_eq!(activity.record_detached(&alice, refused), Detached::Started);
    clock.advance(DETACHED_COALESCE_WINDOW - SignedDuration::from_secs(1));
    assert_eq!(
        activity.record_detached(&alice, refused),
        Detached::Coalesced
    );
    activity.settled().await;
    assert_eq!(
        activity.recent(&alice, 10).await.expect("a read").len(),
        1,
        "a second refusal inside the window is counted, not written"
    );

    clock.advance(SignedDuration::from_secs(1));
    assert_eq!(
        activity.record_detached(&alice, refused),
        Detached::Started,
        "the window is measured from the event that opened it"
    );
    activity.settled().await;
    assert_eq!(activity.recent(&alice, 10).await.expect("a read").len(), 2);
}

#[tokio::test]
async fn windows_are_per_account_and_per_kind() {
    let (activity, _clock) = context();
    let alice = UserId::new("alice");
    let bob = UserId::new("bob");

    assert_eq!(
        activity.record_detached(&alice, ActivityKind::SignInPasswordRefused),
        Detached::Started
    );
    assert_eq!(
        activity.record_detached(&alice, ActivityKind::SignInLocked),
        Detached::Started,
        "a lockout is not coalesced into a wrong password"
    );
    assert_eq!(
        activity.record_detached(&bob, ActivityKind::SignInPasswordRefused),
        Detached::Started,
        "one account's flood does not silence another's log"
    );
    activity.settled().await;
    assert_eq!(activity.recent(&alice, 10).await.expect("a read").len(), 2);
    assert_eq!(activity.recent(&bob, 10).await.expect("a read").len(), 1);
}

#[tokio::test]
async fn a_write_past_the_in_flight_bound_is_dropped_and_opens_no_window() {
    let activity = ActivityContext::new(Arc::new(Stalled), Arc::new(ManualClock::default()));
    let refused = ActivityKind::SignInPasswordRefused;
    for account in 0..MAX_DETACHED_IN_FLIGHT {
        assert_eq!(
            activity.record_detached(&UserId::new(format!("user-{account}")), refused),
            Detached::Started
        );
    }
    let last = UserId::new("one too many");
    assert_eq!(activity.record_detached(&last, refused), Detached::Dropped);
    assert_eq!(
        activity.record_detached(&last, refused),
        Detached::Dropped,
        "a dropped event opened no window, so the next attempt is not coalesced into it"
    );
}

#[test]
fn closed_windows_are_swept_once_the_set_outgrows_its_bound() {
    let mut windows = Windows::default();
    let at = Timestamp::UNIX_EPOCH;
    for account in 0..MIN_SWEEP_AT {
        assert_eq!(
            windows.admit(
                &UserId::new(format!("user-{account}")),
                ActivityKind::SignInLocked,
                None,
                at
            ),
            Admission::Opened { subject: None }
        );
    }
    assert_eq!(
        windows.open.len(),
        MIN_SWEEP_AT,
        "nothing is swept up to the bound"
    );
    assert_eq!(
        windows.admit(
            &UserId::new("early"),
            ActivityKind::SignInLocked,
            None,
            at + SignedDuration::from_mins(30)
        ),
        Admission::Opened { subject: None }
    );
    assert_eq!(
        windows.sweeps, 0,
        "no sweep runs before any window can have closed"
    );

    let later = at + DETACHED_COALESCE_WINDOW;
    assert_eq!(
        windows.admit(
            &UserId::new("late"),
            ActivityKind::SignInLocked,
            None,
            later
        ),
        Admission::Opened { subject: None }
    );
    assert_eq!(windows.sweeps, 1);
    assert_eq!(
        windows.open.len(),
        2,
        "only the window still open and the one opened just now survive"
    );
}

#[test]
fn the_accumulator_opens_no_window_past_its_bound_and_still_coalesces_the_open_ones() {
    let mut windows = Windows::default();
    let at = Timestamp::UNIX_EPOCH;
    let served = ActivityKind::ShareLinkServed;
    let owner = |n: usize| UserId::new(format!("owner-{n}"));
    for n in 0..MAX_OPEN_WINDOWS {
        assert_eq!(
            windows.admit(&owner(n), served, Some("link"), at),
            Admission::Opened {
                subject: Some("link".to_owned())
            }
        );
    }
    let late = UserId::new("late");
    let half_way = at + SignedDuration::from_mins(30);
    for _ in 0..1_000 {
        assert_eq!(
            windows.admit(&late, served, Some("link"), half_way),
            Admission::Full,
            "nothing can have closed, so the set stays at its bound"
        );
    }
    assert_eq!(
        windows.sweeps, 0,
        "a full set whose windows all still hold refuses without rescanning itself"
    );
    assert_eq!(windows.open.len(), MAX_OPEN_WINDOWS);
    assert_eq!(
        windows.admit(&owner(0), served, Some("link"), half_way),
        Admission::Coalesced,
        "a window already open still absorbs its link's serves"
    );

    let later = at + DETACHED_COALESCE_WINDOW;
    assert_eq!(
        windows.admit(&late, served, Some("link"), later),
        Admission::Opened {
            subject: Some("link".to_owned())
        },
        "once the open windows close, one sweep makes room"
    );
    assert_eq!(windows.sweeps, 1);
    assert_eq!(windows.open.len(), 1);
}

#[test]
fn a_dropped_named_window_gives_its_name_back() {
    let mut windows = Windows::default();
    let at = Timestamp::UNIX_EPOCH;
    let owner = UserId::new("owner");
    let served = ActivityKind::ShareLinkServed;
    for link in 0..MAX_NAMED_SERVED_PER_ACCOUNT {
        windows.admit(&owner, served, Some(&format!("link-{link}")), at);
    }
    windows.withdraw(&owner, served, Some("link-0".to_owned()));
    assert_eq!(
        windows.admit(&owner, served, Some("link-0"), at),
        Admission::Opened {
            subject: Some("link-0".to_owned())
        },
        "a window whose event was not written does not count against the account"
    );
}

#[tokio::test]
async fn an_account_names_a_bounded_number_of_links_and_peers_per_window() {
    let (activity, clock) = context();
    let owner = UserId::new("owner");
    for link in 0..MAX_NAMED_SERVED_PER_ACCOUNT - 1 {
        assert_eq!(
            activity.record_served(&owner, Served::ShareLink(&format!("link-{link}"))),
            Detached::Started
        );
    }
    assert_eq!(
        activity.record_served(&owner, Served::Peer("peer-a.test")),
        Detached::Started,
        "links and peers share the account's names"
    );
    for link in 100..140 {
        let expected = if link == 100 {
            Detached::Started
        } else {
            Detached::Coalesced
        };
        assert_eq!(
            activity.record_served(&owner, Served::ShareLink(&format!("link-{link}"))),
            expected,
            "every link past the bound is counted into one unnamed event"
        );
    }
    assert_eq!(
        activity.record_served(&owner, Served::Peer("peer-b.test")),
        Detached::Started,
        "each served kind has an unnamed window of its own"
    );
    assert_eq!(
        activity.record_served(&owner, Served::Peer("peer-c.test")),
        Detached::Coalesced
    );
    assert_eq!(
        activity.record_served(&owner, Served::ShareLink("link-0")),
        Detached::Coalesced,
        "a link named before the bound keeps its own window"
    );
    assert_eq!(
        activity.record_served(&UserId::new("other"), Served::ShareLink("link-x")),
        Detached::Started,
        "one account's links spend no other account's names"
    );
    activity.settled().await;

    let events = activity.recent(&owner, 1_000).await.expect("a read");
    assert_eq!(events.len(), MAX_NAMED_SERVED_PER_ACCOUNT + 2);
    let unnamed: Vec<ActivityKind> = events
        .iter()
        .filter(|event| event.subject.is_none())
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        unnamed,
        [ActivityKind::PeerServed, ActivityKind::ShareLinkServed]
    );

    clock.advance(DETACHED_COALESCE_WINDOW);
    assert_eq!(
        activity.record_served(&owner, Served::ShareLink("link-120")),
        Detached::Started,
        "the next window names links again"
    );
    activity.settled().await;
    let newest = activity.recent(&owner, 1).await.expect("a read");
    assert_eq!(newest[0].subject.as_deref(), Some("link-120"));
}

#[tokio::test]
async fn a_full_serving_set_never_drops_a_sign_in_event() {
    let (activity, clock) = context();
    {
        let mut served = lock(&activity.served_windows);
        let at = clock.now();
        for n in 0..MAX_OPEN_WINDOWS {
            served.admit(
                &UserId::new(format!("owner-{n}")),
                ActivityKind::ShareLinkServed,
                Some("link"),
                at,
            );
        }
    }
    assert_eq!(
        activity.record_served(&UserId::new("late"), Served::ShareLink("link")),
        Detached::Dropped,
        "the serving set is at its bound"
    );
    assert_eq!(
        activity.record_detached(&UserId::new("alice"), ActivityKind::SignInPasswordRefused),
        Detached::Started,
        "sign-in windows are not serving's to spend"
    );
    activity.settled().await;
}

#[tokio::test]
async fn serving_is_one_event_per_link_or_peer_per_window_naming_only_it() {
    let (activity, clock) = context();
    let owner = UserId::new("owner");

    assert_eq!(
        activity.record_served(&owner, Served::ShareLink("link-a")),
        Detached::Started
    );
    for _ in 0..9 {
        assert_eq!(
            activity.record_served(&owner, Served::ShareLink("link-a")),
            Detached::Coalesced,
            "every later blob the link serves in the window is counted, not written"
        );
    }
    assert_eq!(
        activity.record_served(&owner, Served::ShareLink("link-b")),
        Detached::Started,
        "each link has a window of its own"
    );
    assert_eq!(
        activity.record_served(&owner, Served::Peer("link-a")),
        Detached::Started,
        "a peer is not coalesced into a link that happens to share its identifier"
    );
    assert_eq!(
        activity.record_served(&owner, Served::Peer("other.test")),
        Detached::Started
    );
    assert_eq!(
        activity.record_served(&owner, Served::Peer("other.test")),
        Detached::Coalesced
    );
    activity.settled().await;

    let events = activity.recent(&owner, 100).await.expect("a read");
    let seen: Vec<(ActivityKind, Option<&str>)> = events
        .iter()
        .map(|event| (event.kind, event.subject.as_deref()))
        .collect();
    assert_eq!(
        seen,
        [
            (ActivityKind::PeerServed, Some("other.test")),
            (ActivityKind::PeerServed, Some("link-a")),
            (ActivityKind::ShareLinkServed, Some("link-b")),
            (ActivityKind::ShareLinkServed, Some("link-a")),
        ]
    );
    assert!(
        events.iter().all(|event| event.session_id.is_none()),
        "nobody's session acted: the reader holds none of the owner's"
    );

    clock.advance(DETACHED_COALESCE_WINDOW);
    assert_eq!(
        activity.record_served(&owner, Served::ShareLink("link-a")),
        Detached::Started,
        "the next window writes the link again"
    );
    activity.settled().await;
    assert_eq!(activity.recent(&owner, 100).await.expect("a read").len(), 5);
}
