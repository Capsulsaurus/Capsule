use std::net::IpAddr;
use std::sync::Arc;

use jiff::Timestamp;

use super::*;
use crate::counter::{CounterStore, InMemoryCounters};
use crate::store::memory::ManualClock;
use crate::store::{StoreError, StoreFuture};

/// A counter store that cannot be reached.
#[derive(Debug)]
struct DownCounters;

fn down<T>() -> StoreFuture<'static, T> {
    Box::pin(async {
        Err(StoreError::Unavailable {
            store: "counters",
            detail: "down".to_owned(),
        })
    })
}

impl CounterStore for DownCounters {
    fn hit<'a>(&'a self, _: &'a CounterKey, _: Budget, _: Timestamp) -> StoreFuture<'a, Verdict> {
        down()
    }

    fn peek<'a>(&'a self, _: &'a CounterKey, _: Budget, _: Timestamp) -> StoreFuture<'a, Verdict> {
        down()
    }

    fn reset<'a>(&'a self, _: &'a CounterKey) -> StoreFuture<'a, ()> {
        down()
    }
}

fn counters(store: Arc<dyn CounterStore>) -> CounterContext {
    CounterContext::new(store, Arc::new(ManualClock::default()))
}

fn client(address: &str) -> ClientAddr {
    ClientAddr::new(Some(address.parse::<IpAddr>().expect("a test address")))
}

fn id(tag: u16) -> String {
    format!("{tag:032x}")
}

#[tokio::test]
async fn an_unreachable_limiter_refuses_rather_than_admitting() {
    let counters = counters(Arc::new(DownCounters));
    let refused = throttle(&counters, client("203.0.113.9"), &id(1)).await;
    assert!(
        matches!(refused, Err(ShareRejection::Unavailable { .. })),
        "{refused:?}"
    );
}

#[tokio::test]
async fn the_source_budget_trips_without_spending_the_links() {
    let counters = counters(Arc::new(InMemoryCounters::new()));
    let sweeper = client("203.0.113.9");
    for tag in 0..u16::try_from(budgets::SHARE_SOURCE.limit).expect("fits") {
        throttle(&counters, sweeper, &id(tag))
            .await
            .expect("admitted");
    }

    let refused = throttle(&counters, sweeper, &id(9_999)).await;
    assert!(
        matches!(refused, Err(ShareRejection::RateLimited { code, .. }) if code == error_codes::SHARE_RATE_LIMITED),
        "{refused:?}"
    );

    // The refused request charged nothing against the link it named.
    let link = counters
        .peek(&CounterKey::ShareLink(id(9_999)), budgets::SHARE_LINK)
        .await
        .expect("peek");
    assert!(link.admits(), "{link:?}");
    throttle(&counters, client("198.51.100.7"), &id(9_999))
        .await
        .expect("another source is admitted");
}
