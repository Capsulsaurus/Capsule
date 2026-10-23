use std::sync::Arc;

use jiff::SignedDuration;

use super::{Harness, run_all};
use crate::auth::accounts_memory::{InMemoryAccounts, MAX_FAILED_ATTEMPTS};
use crate::auth::credential::Credentials;
use crate::auth::directory::AccountDirectory;
use crate::auth::oidc::FederatedAccounts;
use crate::auth::profile::{AccountProfiles, PasswordChange};
use crate::auth::registry::AccountRegistry;
use crate::store::StoreFuture;
use crate::store::memory::ManualClock;

/// A fifteen-minute lockout window, as a deployment gets by default.
const WINDOW: SignedDuration = SignedDuration::from_mins(15);

/// The deterministic double, on a clock the suite drives.
#[derive(Debug)]
struct MemoryHarness {
    accounts: InMemoryAccounts,
    clock: Arc<ManualClock>,
}

impl Harness for MemoryHarness {
    fn registry(&self) -> &dyn AccountRegistry {
        &self.accounts
    }

    fn directory(&self) -> &dyn AccountDirectory {
        &self.accounts
    }

    fn profiles(&self) -> &dyn AccountProfiles {
        &self.accounts
    }

    fn passwords(&self) -> &dyn PasswordChange {
        &self.accounts
    }

    fn federated(&self) -> &dyn FederatedAccounts {
        &self.accounts
    }

    fn lockout_attempts(&self) -> u32 {
        MAX_FAILED_ATTEMPTS
    }

    fn lockout_window(&self) -> SignedDuration {
        WINDOW
    }

    fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.clock.advance(by);
            Ok(())
        })
    }

    fn recorded_failures<'a>(&'a self, address: &'a str) -> StoreFuture<'a, Option<u32>> {
        Box::pin(async move { Ok(self.accounts.recorded_failures(address)) })
    }
}

fn harness() -> MemoryHarness {
    let clock = Arc::new(ManualClock::default());
    MemoryHarness {
        accounts: InMemoryAccounts::new(
            Credentials::with_reduced_cost().expect("the platform hashes"),
            clock.clone(),
            MAX_FAILED_ATTEMPTS,
            WINDOW,
        ),
        clock,
    }
}

/// The development profile's adapter is only a legitimate stand-in for Postgres to the
/// extent it passes this.
///
/// One test rather than one per case, unlike [`crate::store::conformance`]'s: every case here
/// registers an account, which costs an Argon2id hash at the server-side parameter set, and
/// sixteen fresh harnesses would pay for sixteen decoys on top. The suite shares one harness
/// and every case scopes its own addresses, which is what makes that safe.
#[tokio::test]
async fn the_in_memory_account_store_conforms() {
    run_all(&harness()).await;
}
