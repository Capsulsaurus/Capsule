//! [`InMemorySubscriptions`]: the deterministic [`SubscriptionStore`] double.
//!
//! One mutex, so [`SubscriptionStore::put`]'s read-decide-write is one critical section, as the
//! Postgres adapter's single upsert statement is.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use jiff::Timestamp;

use super::{PutOutcome, Subscription, SubscriptionStore};
use crate::federation::PeerId;
use crate::federation::pull::JobKey;
use crate::store::{AlbumId, StoreFuture, UserId};

/// The key a subscription is held under: subscriber, then peer, then album.
type Key = (UserId, PeerId, AlbumId);

/// The deterministic subscription store.
#[derive(Debug, Default)]
pub struct InMemorySubscriptions {
    rows: Mutex<BTreeMap<Key, Subscription>>,
}

impl InMemorySubscriptions {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> std::sync::MutexGuard<'_, BTreeMap<Key, Subscription>> {
        self.rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SubscriptionStore for InMemorySubscriptions {
    fn put(&self, subscription: Subscription) -> StoreFuture<'_, PutOutcome> {
        Box::pin(async move {
            let key = (
                subscription.subscriber.clone(),
                subscription.peer.clone(),
                subscription.album.clone(),
            );
            let mut rows = self.rows();
            let outcome = match rows.get_mut(&key) {
                None => {
                    rows.insert(key, subscription.clone());
                    PutOutcome::Created(subscription)
                }
                Some(held) if held.expires_at < subscription.expires_at => {
                    let replaced = Subscription {
                        subscribed_at: held.subscribed_at,
                        ..subscription
                    };
                    *held = replaced.clone();
                    PutOutcome::Replaced(replaced)
                }
                Some(held) => PutOutcome::Kept(held.clone()),
            };
            Ok(outcome)
        })
    }

    fn of_subscriber<'a>(&'a self, subscriber: &'a UserId) -> StoreFuture<'a, Vec<Subscription>> {
        Box::pin(async move {
            Ok(self
                .rows()
                .values()
                .filter(|row| row.subscriber == *subscriber)
                .cloned()
                .collect())
        })
    }

    fn remove<'a>(
        &'a self,
        subscriber: &'a UserId,
        peer: &'a PeerId,
        album: &'a AlbumId,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            Ok(self
                .rows()
                .remove(&(subscriber.clone(), peer.clone(), album.clone()))
                .is_some())
        })
    }

    fn of_job<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, Vec<Subscription>> {
        Box::pin(async move {
            Ok(self
                .rows()
                .values()
                .filter(|row| row.peer == job.peer && row.album == job.album)
                .cloned()
                .collect())
        })
    }

    fn live_jobs(&self, now: Timestamp) -> StoreFuture<'_, Vec<JobKey>> {
        Box::pin(async move {
            let jobs: BTreeSet<JobKey> = self
                .rows()
                .values()
                .filter(|row| row.is_live(now))
                .map(Subscription::job)
                .collect();
            Ok(jobs.into_iter().collect())
        })
    }
}
