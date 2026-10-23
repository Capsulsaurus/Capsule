use std::sync::Arc;

use super::super::conformance::{self, Harness};
use super::{InMemoryCapabilities, InMemoryPeers};
use crate::federation::{CapabilityStore, PeerStore};
use crate::store::memory::ManualClock;

#[derive(Debug)]
struct MemoryHarness {
    clock: Arc<ManualClock>,
    capabilities: InMemoryCapabilities,
    peers: InMemoryPeers,
}

impl Harness for MemoryHarness {
    fn capabilities(&self) -> &dyn CapabilityStore {
        &self.capabilities
    }

    fn peers(&self) -> &dyn PeerStore {
        &self.peers
    }

    fn clock(&self) -> &ManualClock {
        &self.clock
    }
}

#[tokio::test]
async fn the_in_memory_stores_conform() {
    let clock = Arc::new(ManualClock::default());
    let harness = MemoryHarness {
        capabilities: InMemoryCapabilities::new(clock.clone()),
        peers: InMemoryPeers::new(),
        clock,
    };
    conformance::run_all(&harness).await;
}
