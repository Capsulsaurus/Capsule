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

fn source() -> CounterKey {
    CounterKey::DropSource("203.0.113.9".to_owned())
}

#[tokio::test]
async fn an_unreachable_source_limiter_refuses_rather_than_admitting() {
    let counters = counters(Arc::new(DownCounters));
    let refused = charge_drop_limiter(&counters, &source(), budgets::DROP_SOURCE).await;
    assert!(
        matches!(refused, Err(DropRejection::Unavailable { .. })),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_spent_source_budget_is_a_rate_limit_with_a_retry_instant() {
    let counters = counters(Arc::new(InMemoryCounters::new()));
    for _ in 0..budgets::DROP_SOURCE.limit {
        charge_drop_limiter(&counters, &source(), budgets::DROP_SOURCE)
            .await
            .expect("admitted");
    }
    let refused = charge_drop_limiter(&counters, &source(), budgets::DROP_SOURCE).await;
    assert!(
        matches!(
            refused,
            Err(DropRejection::RateLimited { code, retry_after })
                if code == error_codes::DROP_RATE_LIMITED && retry_after > 0
        ),
        "{refused:?}"
    );
}
