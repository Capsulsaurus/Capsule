//! [`SplitCounters`] — the per-chunk throughput budgets in this process, everything else shared.
//!
//! # Why two budgets are counted locally (ADR-0015)
//!
//! [`CounterKey::UploadChunks`] and [`CounterKey::UploadChunksSource`] are hit twice for every
//! chunk an upload sends — the hottest path the server has — and they bound disk and CPU
//! throughput, not a secret. No security argument rests on them, unlike the replay nonces or
//! the enrollment and second-factor budgets, which must be one count across every replica.
//!
//! A chunk is only ever written by the process that owns the session's blob root, so today
//! every chunk that succeeds is counted by one process's counter and the local count is exact.
//! With several replicas sharing sessions, the worst case is an attacker spreading requests to
//! get one budget per replica; those landing on a replica that does not own the stage are
//! refused after their body is read, and every one of them is still behind authentication, the
//! shared per-create [`CounterKey::UploadSessions`] budget, the live-session cap, the in-flight
//! cap and the request body cap. When uploads run on more than one process per blob root these
//! two budgets move to the shared store, or routing pins an account to its owner.
//!
//! Every other key goes to `shared`, the deployment-wide store. [`is_process_local`] is the one
//! list, and a test pins it, so a budget cannot quietly become per-process.

use std::sync::Arc;

use jiff::Timestamp;

use super::{Budget, CounterKey, CounterStore, Verdict};
use crate::store::StoreFuture;

/// Whether `key` is counted per process rather than deployment-wide.
#[must_use]
pub fn is_process_local(key: &CounterKey) -> bool {
    matches!(
        key,
        CounterKey::UploadChunks(_) | CounterKey::UploadChunksSource(_)
    )
}

/// A [`CounterStore`] that sends the per-chunk throughput budgets to `local` and every other key
/// to `shared`.
#[derive(Debug, Clone)]
pub struct SplitCounters {
    local: Arc<dyn CounterStore>,
    shared: Arc<dyn CounterStore>,
}

impl SplitCounters {
    /// Route [`is_process_local`] keys to `local` and the rest to `shared`.
    pub fn new(local: Arc<dyn CounterStore>, shared: Arc<dyn CounterStore>) -> Self {
        Self { local, shared }
    }

    fn route(&self, key: &CounterKey) -> &dyn CounterStore {
        if is_process_local(key) {
            &*self.local
        } else {
            &*self.shared
        }
    }
}

impl CounterStore for SplitCounters {
    fn charge<'a>(
        &'a self,
        key: &'a CounterKey,
        units: u32,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        self.route(key).charge(key, units, budget, at)
    }

    fn peek<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        self.route(key).peek(key, budget, at)
    }

    fn reset<'a>(&'a self, key: &'a CounterKey) -> StoreFuture<'a, ()> {
        self.route(key).reset(key)
    }
}

#[cfg(test)]
mod tests;
