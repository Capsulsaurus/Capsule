//! [`InMemorySubscriptions`]: the deterministic [`SubscriptionStore`] double.
//!
//! One mutex, so [`SubscriptionStore::put`]'s read-decide-write is one critical section, as the
//! Postgres adapter's single upsert statement is, and [`SubscriptionStore::put_within`]'s count
//! is taken in the same section as the write it admits, as the Postgres adapter's table lock
//! makes it.

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

/// The key `subscription` is held under.
fn key_of(subscription: &Subscription) -> Key {
    (
        subscription.subscriber.clone(),
        subscription.peer.clone(),
        subscription.album.clone(),
    )
}

/// [`SubscriptionStore::put`]'s decision, inside the caller's critical section.
fn put_locked(rows: &mut BTreeMap<Key, Subscription>, subscription: Subscription) -> PutOutcome {
    let key = key_of(&subscription);
    match rows.get_mut(&key) {
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
    }
}

impl SubscriptionStore for InMemorySubscriptions {
    fn put(&self, subscription: Subscription) -> StoreFuture<'_, PutOutcome> {
        Box::pin(async move { Ok(put_locked(&mut self.rows(), subscription)) })
    }

    fn put_within(
        &self,
        subscription: Subscription,
        limit: u32,
    ) -> StoreFuture<'_, Option<PutOutcome>> {
        Box::pin(async move {
            let mut rows = self.rows();
            if !rows.contains_key(&key_of(&subscription)) {
                let held = rows
                    .keys()
                    .filter(|(subscriber, _, _)| *subscriber == subscription.subscriber)
                    .count();
                if held >= usize::try_from(limit).unwrap_or(usize::MAX) {
                    return Ok(None);
                }
            }
            Ok(Some(put_locked(&mut rows, subscription)))
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
