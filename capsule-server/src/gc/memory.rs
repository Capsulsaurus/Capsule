//! A deterministic in-memory [`CollectionStore`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use jiff::Timestamp;

use super::CollectionStore;
use crate::blob::ContentAddress;
use crate::store::StoreFuture;

/// The marks, in memory.
#[derive(Debug, Default)]
pub struct InMemoryCollection {
    marks: Mutex<BTreeMap<ContentAddress, Timestamp>>,
}

impl InMemoryCollection {
    /// An empty set of marks.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl CollectionStore for InMemoryCollection {
    fn mark(&self, address: &ContentAddress, at: Timestamp) -> StoreFuture<'_, bool> {
        let address = address.clone();
        Box::pin(async move { Ok(lock(&self.marks).insert(address, at).is_none()) })
    }

    fn unmark<'a>(&'a self, address: &'a ContentAddress) -> StoreFuture<'a, bool> {
        Box::pin(async move { Ok(lock(&self.marks).remove(address).is_some()) })
    }

    fn marked_since<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move { Ok(lock(&self.marks).get(address).copied()) })
    }

    fn marks(&self) -> StoreFuture<'_, Vec<(ContentAddress, Timestamp)>> {
        Box::pin(async move {
            Ok(lock(&self.marks)
                .iter()
                .map(|(address, at)| (address.clone(), *at))
                .collect())
        })
    }
}
