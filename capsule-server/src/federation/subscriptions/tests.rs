use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::store::memory::ManualClock;

#[tokio::test]
async fn the_in_memory_store_conforms() {
    conformance::run_all(&InMemorySubscriptions::new()).await;
}

fn subscription(subscriber: &str, album: &str, expires_at: Timestamp) -> Subscription {
    Subscription {
        subscriber: UserId::new(subscriber),
        peer: PeerId::new("home.test"),
        album: AlbumId::new(album),
        scope: Scope::Read,
        jti: format!("{subscriber}-{album}"),
        token: CapabilityToken::new("header.payload.signature"),
        issued_at: Timestamp::UNIX_EPOCH,
        expires_at,
        min_protocol_version: "2026-06-01".to_owned(),
        subscribed_at: Timestamp::UNIX_EPOCH,
        updated_at: Timestamp::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn the_job_source_lists_the_live_jobs_at_the_clocks_now() {
    let clock = Arc::new(ManualClock::default());
    let store = Arc::new(InMemorySubscriptions::new());
    let hour =
        |hours| crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(hours));
    store
        .put(subscription("u", "a", hour(1)))
        .await
        .expect("it records");
    store
        .put(subscription("v", "a", hour(3)))
        .await
        .expect("it records");
    store
        .put(subscription("u", "b", hour(2)))
        .await
        .expect("it records");
    let source = SubscriptionJobs::new(store, clock.clone());

    let job = |album: &str| JobKey::new(PeerId::new("home.test"), AlbumId::new(album));
    assert_eq!(
        source.jobs().await.expect("it lists"),
        vec![job("a"), job("b")]
    );
    clock.advance(SignedDuration::from_hours(2));
    assert_eq!(
        source.jobs().await.expect("it lists"),
        vec![job("a")],
        "the source reads the clock on every round"
    );
    clock.advance(SignedDuration::from_hours(1));
    assert!(source.jobs().await.expect("it lists").is_empty());
}

#[test]
fn nothing_prints_the_token() {
    let held = subscription("u", "a", Timestamp::UNIX_EPOCH);
    let rendered = format!("{held:?} {:?}", PutOutcome::Kept(held.clone()));
    assert!(!rendered.contains(held.token.expose()), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}

#[test]
fn a_subscription_is_live_strictly_before_its_expiry() {
    let expires_at = crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(1));
    let held = subscription("u", "a", expires_at);
    assert!(held.is_live(Timestamp::UNIX_EPOCH));
    assert!(!held.is_live(expires_at));
    assert_eq!(
        held.job(),
        JobKey::new(PeerId::new("home.test"), AlbumId::new("a"))
    );
}
