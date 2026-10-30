//! The one suite every [`SubscriptionStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **The longer-lived token wins.** A put under a held key replaces the token only when the
//!   posted one expires later, and never moves `subscribed_at`; a replayed older invite changes
//!   nothing.
//! - **Jobs are distinct and live.** Two subscribers to one album are one job, and a job whose
//!   every token has lapsed is not listed.
//! - **Every read is ordered**, so the worker visits jobs in a stable order and a subscriber's
//!   list does not shuffle between calls.
//!
//! # Reusing a harness
//!
//! Every case scopes its own subscribers, peers and albums by a case name, so cases may share
//! one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};

use super::{CapabilityToken, PutOutcome, Subscription, SubscriptionStore};
use crate::federation::PeerId;
use crate::federation::capability::Scope;
use crate::federation::pull::JobKey;
use crate::store::{AlbumId, StoreError, UserId, deadline};

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming subscription store must succeed at {doing}: {error}"),
    }
}

/// An instant `hours` past an epoch every case shares.
fn at(hours: i64) -> Timestamp {
    deadline(
        Timestamp::from_second(1_767_225_600).expect("a representable instant"),
        SignedDuration::from_hours(hours),
    )
}

/// A subscription of `case`'s `subscriber` to `album` on `peer`, issued at hour `issued` and
/// expiring at hour `expires`, recorded at hour `issued`.
fn subscription(
    case: &str,
    subscriber: &str,
    peer: &str,
    album: &str,
    issued: i64,
    expires: i64,
) -> Subscription {
    Subscription {
        subscriber: UserId::new(format!("{case}-{subscriber}")),
        peer: PeerId::new(format!("{peer}.{case}.test")),
        album: AlbumId::new(format!("{case}-{album}")),
        scope: Scope::Read,
        jti: format!("{case}-{subscriber}-{peer}-{album}-{issued}-{expires}"),
        token: CapabilityToken::new(format!("token-{case}-{issued}-{expires}")),
        issued_at: at(issued),
        expires_at: at(expires),
        min_protocol_version: "2026-06-01".to_owned(),
        subscribed_at: at(issued),
        updated_at: at(issued),
    }
}

/// Run every case against `store`.
pub async fn run_all(store: &dyn SubscriptionStore) {
    a_first_put_creates_and_reads_back(store).await;
    a_longer_lived_token_replaces_and_keeps_subscribed_at(store).await;
    an_older_or_equal_token_is_kept(store).await;
    subscribers_are_listed_in_order_and_kept_apart(store).await;
    remove_forgets_exactly_one_key(store).await;
    live_jobs_are_distinct_ordered_and_live(store).await;
    of_job_lists_every_subscriber_of_one_album(store).await;
}

async fn a_first_put_creates_and_reads_back(store: &dyn SubscriptionStore) {
    let posted = subscription("create", "u", "home", "a", 0, 6);
    let outcome = ok(store.put(posted.clone()).await, "a first put");
    assert_eq!(outcome, PutOutcome::Created(posted.clone()));
    let listed = ok(store.of_subscriber(&posted.subscriber).await, "listing");
    assert_eq!(listed, vec![posted], "every field reads back as written");
}

async fn a_longer_lived_token_replaces_and_keeps_subscribed_at(store: &dyn SubscriptionStore) {
    let first = subscription("replace", "u", "home", "a", 0, 6);
    ok(store.put(first.clone()).await, "a first put");
    let fresher = subscription("replace", "u", "home", "a", 3, 9);
    let outcome = ok(store.put(fresher.clone()).await, "a fresher put");
    let expected = Subscription {
        subscribed_at: first.subscribed_at,
        ..fresher
    };
    assert_eq!(outcome, PutOutcome::Replaced(expected.clone()));
    assert_eq!(
        ok(store.of_subscriber(&first.subscriber).await, "listing"),
        vec![expected],
        "the token moved and the subscription's age did not"
    );
}

async fn an_older_or_equal_token_is_kept(store: &dyn SubscriptionStore) {
    let held = subscription("keep", "u", "home", "a", 3, 9);
    ok(store.put(held.clone()).await, "a first put");
    for (issued, expires) in [(0, 6), (4, 9)] {
        let replayed = subscription("keep", "u", "home", "a", issued, expires);
        let outcome = ok(store.put(replayed).await, "a replayed put");
        assert_eq!(
            outcome,
            PutOutcome::Kept(held.clone()),
            "a token that does not outlive the held one changes nothing"
        );
    }
    assert_eq!(
        ok(store.of_subscriber(&held.subscriber).await, "listing"),
        vec![held]
    );
}

async fn subscribers_are_listed_in_order_and_kept_apart(store: &dyn SubscriptionStore) {
    let b = subscription("order", "u", "beta", "a", 0, 6);
    let a2 = subscription("order", "u", "alpha", "b", 0, 6);
    let a1 = subscription("order", "u", "alpha", "a", 0, 6);
    let other = subscription("order", "v", "alpha", "a", 0, 6);
    for posted in [&b, &a2, &a1, &other] {
        ok(store.put(posted.clone()).await, "a put");
    }
    assert_eq!(
        ok(store.of_subscriber(&a1.subscriber).await, "listing"),
        vec![a1, a2, b],
        "ordered by peer and then album, and nobody else's"
    );
    assert!(
        ok(
            store.of_subscriber(&UserId::new("order-nobody")).await,
            "listing an account with none"
        )
        .is_empty()
    );
}

async fn remove_forgets_exactly_one_key(store: &dyn SubscriptionStore) {
    let gone = subscription("remove", "u", "home", "a", 0, 6);
    let kept = subscription("remove", "u", "home", "b", 0, 6);
    let theirs = subscription("remove", "v", "home", "a", 0, 6);
    for posted in [&gone, &kept, &theirs] {
        ok(store.put(posted.clone()).await, "a put");
    }
    assert!(ok(
        store
            .remove(&gone.subscriber, &gone.peer, &gone.album)
            .await,
        "a removal"
    ));
    assert!(
        !ok(
            store
                .remove(&gone.subscriber, &gone.peer, &gone.album)
                .await,
            "a repeated removal"
        ),
        "a key already forgotten is answered as absent"
    );
    assert_eq!(
        ok(store.of_subscriber(&gone.subscriber).await, "listing"),
        vec![kept]
    );
    assert_eq!(
        ok(store.of_subscriber(&theirs.subscriber).await, "listing"),
        vec![theirs],
        "another subscriber's subscription to the same album stays"
    );
}

async fn live_jobs_are_distinct_ordered_and_live(store: &dyn SubscriptionStore) {
    // Two subscribers to one album, one album whose only token lapses at hour 6, and one live
    // album on a peer that sorts first.
    let shared_u = subscription("jobs", "u", "home", "a", 0, 9);
    let shared_v = subscription("jobs", "v", "home", "a", 0, 6);
    let lapsing = subscription("jobs", "u", "home", "b", 0, 6);
    let first = subscription("jobs", "u", "early", "c", 0, 9);
    for posted in [&shared_u, &shared_v, &lapsing, &first] {
        ok(store.put(posted.clone()).await, "a put");
    }

    let scoped = |jobs: Vec<JobKey>| -> Vec<JobKey> {
        jobs.into_iter()
            .filter(|job| job.peer.as_str().ends_with(".jobs.test"))
            .collect()
    };
    assert_eq!(
        scoped(ok(store.live_jobs(at(1)).await, "listing jobs")),
        vec![first.job(), shared_u.job(), lapsing.job()],
        "each album once, in job order"
    );
    assert_eq!(
        scoped(ok(store.live_jobs(at(6)).await, "listing jobs")),
        vec![first.job(), shared_u.job()],
        "a token is not live at the instant it expires, and one live token keeps a job"
    );
    assert!(
        scoped(ok(store.live_jobs(at(9)).await, "listing jobs")).is_empty(),
        "a job with no live token is not a job"
    );
}

async fn of_job_lists_every_subscriber_of_one_album(store: &dyn SubscriptionStore) {
    let v = subscription("of-job", "v", "home", "a", 0, 6);
    let u = subscription("of-job", "u", "home", "a", 0, 9);
    let elsewhere = subscription("of-job", "u", "home", "b", 0, 9);
    for posted in [&v, &u, &elsewhere] {
        ok(store.put(posted.clone()).await, "a put");
    }
    assert_eq!(
        ok(
            store.of_job(&u.job()).await,
            "listing a job's subscriptions"
        ),
        vec![u, v],
        "ordered by subscriber, and only this album's"
    );
}
