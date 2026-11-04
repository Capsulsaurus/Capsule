//! Check 8: every stored pulled quota total equals its derivation (#1183).
//!
//! The in-memory ledger derives its pulled total and cannot drift, so drift is injected through
//! a double that answers the audit with what a ledger written behind its schema would. The
//! Postgres ledger's own drift, written with SQL behind its triggers, is reported by this check
//! in `quota/postgres/tests.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::quota::{
    ChargeOutcome, ExpiredReservation, InMemoryQuota, PulledDrift, QuotaLimits, StoredUsage,
};
use crate::store::StoreFuture;

/// A quota ledger whose audit names `drift`, and that counts every reconcile it is asked for.
#[derive(Debug, Default)]
struct DriftedQuota {
    inner: InMemoryQuota,
    drift: Vec<PulledDrift>,
    reconciles: AtomicUsize,
}

impl QuotaStore for DriftedQuota {
    fn usage<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, StoredUsage> {
        self.inner.usage(user)
    }

    fn charge<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome> {
        self.inner.charge(user, address, size, at, limits)
    }

    fn reserve<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        until: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome> {
        self.inner.reserve(user, address, size, at, until, limits)
    }

    fn settle<'a>(&'a self, address: &'a ContentAddress) -> StoreFuture<'a, bool> {
        self.inner.settle(address)
    }

    fn expired_reservations(
        &self,
        now: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<ExpiredReservation>> {
        self.inner.expired_reservations(now, limit)
    }

    fn extend_reservation<'a>(
        &'a self,
        address: &'a ContentAddress,
        until: Timestamp,
    ) -> StoreFuture<'a, bool> {
        self.inner.extend_reservation(address, until)
    }

    fn release_expired<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        now: Timestamp,
    ) -> StoreFuture<'a, bool> {
        self.inner.release_expired(user, address, now)
    }

    fn release<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, bool> {
        self.inner.release(user, address)
    }

    fn release_attribution<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<(UserId, u64)>> {
        self.inner.release_attribution(address)
    }

    fn suspend<'a>(&'a self, user: &'a UserId, at: Timestamp) -> StoreFuture<'a, bool> {
        self.inner.suspend(user, at)
    }

    fn unsuspend<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, bool> {
        self.inner.unsuspend(user)
    }

    fn pulled_drift(&self) -> StoreFuture<'_, Vec<PulledDrift>> {
        let drift = self.drift.clone();
        Box::pin(async move { Ok(drift) })
    }

    fn reconcile_pulled(&self) -> StoreFuture<'_, Vec<PulledDrift>> {
        self.reconciles.fetch_add(1, Ordering::SeqCst);
        let drift = self.drift.clone();
        Box::pin(async move { Ok(drift) })
    }
}

#[tokio::test]
async fn a_ledger_that_derives_its_pulled_total_is_clean() {
    let h = Harness::new();
    h.publish("quota-clean").await;
    let context = h
        .context
        .clone()
        .with_quotas(Arc::new(InMemoryQuota::new()));
    let report = scrub(&context, Depth::Structural).await.expect("a pass");
    assert!(report.is_clean(), "found {:?}", report.findings);
}

#[tokio::test]
async fn each_drifted_account_is_its_own_finding_and_nothing_is_rebuilt() {
    let h = Harness::new();
    h.publish("quota-drift").await;
    let quotas = Arc::new(DriftedQuota {
        drift: vec![
            PulledDrift {
                user: UserId::new("drifted-refs"),
                stored: 9,
                derived: 9,
                holdings: true,
            },
            PulledDrift {
                user: UserId::new("drifted-total"),
                stored: 14,
                derived: 9,
                holdings: false,
            },
        ],
        ..DriftedQuota::default()
    });
    let before = h.snapshot().await;

    let context = h.context.clone().with_quotas(quotas.clone());
    let report = scrub(&context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.findings,
        vec![
            Finding::PulledQuotaDrift {
                user: UserId::new("drifted-refs"),
                stored: 9,
                derived: 9,
                holdings: true,
            },
            Finding::PulledQuotaDrift {
                user: UserId::new("drifted-total"),
                stored: 14,
                derived: 9,
                holdings: false,
            },
        ]
    );
    assert_eq!(report.counts(), vec![("pulled_quota_drift", 2)]);
    assert!(!report.is_clean());
    assert_eq!(
        quotas.reconciles.load(Ordering::SeqCst),
        0,
        "the scrub names drift and never rebuilds it"
    );
    assert_eq!(h.snapshot().await, before, "the scrub changed the store");
}
