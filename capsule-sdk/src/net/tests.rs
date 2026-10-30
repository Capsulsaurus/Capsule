use super::*;

/// **Class detection (networking Validation).** Mocked OS signals per platform
/// map to the expected class; `constrained` and `metered` are distinguished.
#[test]
fn signals_map_to_the_expected_class() {
    // Desktop / wired default and Android NOT_METERED → unmetered.
    assert_eq!(
        ConnectionClass::from_signals(PathSignals::unmetered()),
        ConnectionClass::Unmetered
    );

    // No path → offline (regardless of the other signals).
    assert_eq!(
        ConnectionClass::from_signals(PathSignals::offline()),
        ConnectionClass::Offline
    );

    // iOS isExpensive → metered.
    assert_eq!(
        ConnectionClass::from_signals(PathSignals {
            has_path: true,
            is_expensive: true,
            not_metered_capability: false,
            is_constrained: false,
        }),
        ConnectionClass::Metered
    );

    // Android without NET_CAPABILITY_NOT_METERED → metered even when not
    // flagged expensive.
    assert_eq!(
        ConnectionClass::from_signals(PathSignals {
            has_path: true,
            is_expensive: false,
            not_metered_capability: false,
            is_constrained: false,
        }),
        ConnectionClass::Metered
    );

    // Data saver → constrained, and it wins over a metered link (distinct from
    // metered).
    let constrained = ConnectionClass::from_signals(PathSignals {
        has_path: true,
        is_expensive: true,
        not_metered_capability: false,
        is_constrained: true,
    });
    assert_eq!(constrained, ConnectionClass::Constrained);
    assert_ne!(constrained, ConnectionClass::Metered);
}

/// **Adverse promotion/demotion (networking Validation).** An injected
/// reset/stall pattern promotes to `adverse`; a sustained clean run demotes.
#[test]
fn adverse_promotes_on_resets_and_demotes_after_a_clean_run() {
    let mut detector = AdverseDetector::new(AdverseThresholds {
        window: 8,
        promote_at: 3,
        demote_after_clean: 4,
    });
    let base = ConnectionClass::Metered;

    // Two bad outcomes: not yet promoted.
    detector.record(TransferSignal::Reset);
    detector.record(TransferSignal::Stall);
    assert!(!detector.is_adverse());
    assert_eq!(detector.classify(base), ConnectionClass::Metered);

    // The third bad outcome inside the window trips promotion; the effective
    // class becomes adverse regardless of the metered base.
    detector.record(TransferSignal::BlackHole);
    assert!(detector.is_adverse());
    assert_eq!(detector.classify(base), ConnectionClass::Adverse);

    // A short clean streak does not yet demote.
    detector.record(TransferSignal::Clean);
    detector.record(TransferSignal::Clean);
    detector.record(TransferSignal::Clean);
    assert!(detector.is_adverse());

    // The fourth consecutive clean transfer demotes back to the base class.
    detector.record(TransferSignal::Clean);
    assert!(!detector.is_adverse());
    assert_eq!(detector.classify(base), ConnectionClass::Metered);
}

/// A reset mid clean-streak resets the demotion counter (the streak must be
/// *consecutive*).
#[test]
fn a_reset_breaks_the_demotion_streak() {
    let mut detector = AdverseDetector::new(AdverseThresholds {
        window: 6,
        promote_at: 2,
        demote_after_clean: 3,
    });
    detector.record(TransferSignal::Reset);
    detector.record(TransferSignal::Reset);
    assert!(detector.is_adverse());

    detector.record(TransferSignal::Clean);
    detector.record(TransferSignal::Clean);
    detector.record(TransferSignal::Reset); // breaks the streak
    detector.record(TransferSignal::Clean);
    detector.record(TransferSignal::Clean);
    assert!(detector.is_adverse(), "streak was not yet 3 consecutive");
    detector.record(TransferSignal::Clean);
    assert!(!detector.is_adverse());
}

/// Offline is never promoted to adverse — there is no path to be flaky on.
#[test]
fn offline_is_never_promoted_to_adverse() {
    let mut detector = AdverseDetector::with_defaults();
    for _ in 0..8 {
        detector.record(TransferSignal::BlackHole);
    }
    assert!(detector.is_adverse());
    assert_eq!(
        detector.classify(ConnectionClass::Offline),
        ConnectionClass::Offline
    );
}

/// **Connection-class matrix.** The staged-upload tier gates and the
/// reconciliation gates take exactly the expected value on every class — the
/// single table that downstream sync/cache consumers read.
#[test]
fn connection_class_tier_and_reconciliation_matrix() {
    use ConnectionClass::{Adverse, Constrained, Metered, Offline, Unmetered};
    use UploadTier::{Index, Original, Preview};

    // (class, T0, T1, T2, small, large) with force_sync = false.
    let matrix = [
        (Unmetered, true, true, true, true, true),
        (Metered, true, false, false, false, false),
        (Constrained, true, false, false, false, false),
        (Adverse, true, false, false, false, false),
        (Offline, false, false, false, false, false),
    ];
    for (class, t0, t1, t2, small, large) in matrix {
        assert_eq!(class.permits_tier(Index, false), t0, "{class:?} T0");
        assert_eq!(class.permits_tier(Preview, false), t1, "{class:?} T1");
        assert_eq!(class.permits_tier(Original, false), t2, "{class:?} T2");
        assert_eq!(
            class.permits_reconciliation(ReconciliationClass::Small, false),
            small,
            "{class:?} small"
        );
        assert_eq!(
            class.permits_reconciliation(ReconciliationClass::Large, false),
            large,
            "{class:?} large"
        );
    }
}

/// Force-sync overrides the metered/Wi-Fi criteria for above-index tiers and
/// large reconciliations, but never resurrects an offline path.
#[test]
fn force_sync_overrides_metered_but_not_offline() {
    assert!(ConnectionClass::Metered.permits_tier(UploadTier::Original, true));
    assert!(ConnectionClass::Constrained.permits_tier(UploadTier::Preview, true));
    assert!(ConnectionClass::Adverse.permits_reconciliation(ReconciliationClass::Large, true));

    assert!(!ConnectionClass::Offline.permits_tier(UploadTier::Index, true));
    assert!(!ConnectionClass::Offline.permits_reconciliation(ReconciliationClass::Small, true));
}

/// The cache-eviction byte budget scales monotonically with re-fetch cost:
/// unmetered retains the least, offline the most.
#[test]
fn cache_budget_is_monotonic_in_refetch_cost() {
    const BASE: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB base
    let unmetered = ConnectionClass::Unmetered.cache_retention_budget(BASE);
    let metered = ConnectionClass::Metered.cache_retention_budget(BASE);
    let constrained = ConnectionClass::Constrained.cache_retention_budget(BASE);
    let adverse = ConnectionClass::Adverse.cache_retention_budget(BASE);
    let offline = ConnectionClass::Offline.cache_retention_budget(BASE);

    assert_eq!(unmetered, BASE);
    assert_eq!(metered, constrained);
    assert!(unmetered <= metered);
    assert!(metered <= adverse);
    assert!(adverse <= offline);
    // Saturating: an absurd base never wraps.
    assert_eq!(
        ConnectionClass::Offline.cache_retention_budget(u64::MAX),
        u64::MAX
    );
}

/// **Adverse switches fetches to bounded windows.** The behavioral promotion
/// (already covered above) feeds directly into the transfer-window rule: only
/// the promoted `adverse` class bounds the `Range` window; every other class
/// fetches the whole remainder. The networking-doc linkage "promotes to
/// `adverse` and switches fetches to bounded windows" in one assertion.
#[test]
fn adverse_bounds_the_transfer_window() {
    assert_eq!(
        ConnectionClass::Adverse.range_window(),
        Some(ADVERSE_RANGE_WINDOW)
    );
    for class in [
        ConnectionClass::Unmetered,
        ConnectionClass::Metered,
        ConnectionClass::Constrained,
        ConnectionClass::Offline,
    ] {
        assert_eq!(class.range_window(), None, "{class:?} is open-ended");
    }

    // The promotion path itself lands on the bounded window: a reset burst
    // promotes a metered base to adverse, whose window is now bounded.
    let mut detector = AdverseDetector::with_defaults();
    for _ in 0..3 {
        detector.record(TransferSignal::Reset);
    }
    assert_eq!(
        detector.classify(ConnectionClass::Metered),
        ConnectionClass::Adverse
    );
    assert_eq!(
        detector.classify(ConnectionClass::Metered).range_window(),
        Some(ADVERSE_RANGE_WINDOW)
    );
}

/// **Backoff discipline (networking Validation).** For every policy class the
/// retry sequence stays within its bounds, is jittered, honors a server
/// `Retry-After` as a floor, and gives up after a bounded number of attempts —
/// no configuration can produce an unbounded hot loop.
#[test]
fn backoff_stays_bounded_jitters_and_gives_up() {
    for class in [
        RetryClass::Interactive,
        RetryClass::BulkTransfer,
        RetryClass::ControlCeremony,
    ] {
        let policy = class.policy();

        // Full jitter at fraction 1.0 rides the ceiling exactly: the sequence
        // is non-decreasing and never exceeds max_delay.
        let mut engine = RetryEngine::deterministic(class, 1.0);
        let mut prev = Duration::ZERO;
        let mut retries = 0u32;
        while let RetryDecision::Retry { after } = engine.next_backoff(None) {
            assert!(after <= policy.max_delay, "{class:?} within cap");
            assert!(after >= prev, "{class:?} ceiling non-decreasing");
            prev = after;
            retries += 1;
            assert!(retries <= policy.max_retries, "{class:?} bounded");
        }
        assert_eq!(
            retries, policy.max_retries,
            "{class:?} gives up at the bound"
        );

        // Full jitter at fraction 0.0 floors at zero but STILL gives up: even a
        // zero-delay config cannot hot-loop.
        let mut zero = RetryEngine::deterministic(class, 0.0);
        let mut count = 0u32;
        while let RetryDecision::Retry { after } = zero.next_backoff(None) {
            assert_eq!(after, Duration::ZERO);
            count += 1;
            assert!(count <= policy.max_retries + 1, "{class:?} cannot hot-loop");
        }
        assert_eq!(count, policy.max_retries);

        // A server Retry-After beyond the cap is honored as a floor.
        let mut hinted = RetryEngine::deterministic(class, 0.0);
        let hint = policy.max_delay + Duration::from_secs(5);
        match hinted.next_backoff(Some(hint)) {
            RetryDecision::Retry { after } => {
                assert_eq!(after, hint, "{class:?} honors Retry-After");
            }
            RetryDecision::GiveUp => panic!("{class:?} should retry first"),
        }
    }
}

/// Full jitter is a *uniform spread over `[0, ceiling]`*, not a fixed value:
/// a seeded production engine at the capped ceiling produces varied waits, all
/// within the cap. Deterministic via the fixed seed.
#[test]
fn full_jitter_spreads_within_the_ceiling() {
    let policy = RetryClass::BulkTransfer.policy();
    let mut engine = RetryEngine::seeded(RetryClass::BulkTransfer, 0xC0FF_EE00_1234_5678);
    // Drive past the doubling so the ceiling is pinned at max_delay, then the
    // only variation is jitter.
    let mut seen = Vec::new();
    for _ in 0..policy.max_retries {
        if let RetryDecision::Retry { after } = engine.next_backoff(None) {
            assert!(after <= policy.max_delay);
            seen.push(after);
        }
    }
    let min = seen.iter().min().copied().unwrap_or_default();
    let max = seen.iter().max().copied().unwrap_or_default();
    assert!(min < max, "full jitter must spread, not fix: {seen:?}");
}

/// Resetting the engine after real progress restores the full budget — a
/// long-lived resumable transfer earns a fresh backoff per independent stall.
#[test]
fn reset_restores_the_retry_budget() {
    let mut engine = RetryEngine::deterministic(RetryClass::BulkTransfer, 0.5);
    for _ in 0..3 {
        let _ = engine.next_backoff(None);
    }
    assert_eq!(engine.attempts(), 3);
    engine.reset();
    assert_eq!(engine.attempts(), 0);
}

/// The stall detector cuts on **no-bytes-for-T** against a mock clock — never a
/// wall-clock sleep. Progress rearms it; a gap at or beyond the bound trips it.
#[test]
fn stall_detector_cuts_on_no_bytes_for_t() {
    let cfg = StallConfig::new(Duration::from_millis(500));
    let mut stall = StallDetector::new(cfg, 0);

    // Within the bound: live but slow, not a stall.
    assert!(!stall.is_stalled(499));
    // At the bound: cut.
    assert!(stall.is_stalled(500));
    assert!(stall.is_stalled(10_000));

    // Bytes arrive at t=600: the timer rearms from there.
    stall.on_progress(600);
    assert!(!stall.is_stalled(1000)); // only 400 ms since progress
    assert!(stall.is_stalled(1100)); // 500 ms since progress → cut
}

/// `dial_client` builds a rustls client with the bounded connect timeout — the
/// Happy-Eyeballs-at-dial posture (address racing itself is the stack's).
#[test]
fn dial_client_builds() {
    assert!(dial_client().is_ok());
}

/// Every SDK transport carries the three build constants the server's gate reads.
#[test]
fn the_handshake_headers_are_the_build_constants() {
    let headers = protocol_headers();
    assert_eq!(
        headers
            .get("x-capsule-protocol")
            .and_then(|v| v.to_str().ok()),
        Some(capsule_core::crypto::primitives::PROTOCOL_VERSION)
    );
    assert_eq!(
        headers
            .get("x-capsule-crypto-suite")
            .and_then(|v| v.to_str().ok()),
        Some(
            capsule_core::crypto::primitives::CRYPTO_SUITE_ID
                .to_string()
                .as_str()
        )
    );
    assert_eq!(
        headers
            .get("x-capsule-sidecar-schema")
            .and_then(|v| v.to_str().ok()),
        Some(
            capsule_core::sidecar::SIDECAR_SCHEMA_V1
                .to_string()
                .as_str()
        )
    );
    assert_eq!(headers.len(), 3);
    assert!(http_client().is_ok());
}

/// **The retry classification table** (networking doc, Retry Policy Classes). Transport
/// failures, every `5xx`, `408`, `425` and `429` are retryable; a contract `4xx` is terminal
/// unless the recovery matrix names an action for its code; nothing else is a retryable failure.
#[test]
fn errors_are_classified_before_any_retry() {
    use RetryVerdict::{Recover, Retryable, Terminal};

    assert_eq!(RetryVerdict::of_transport(), Retryable);
    let table: &[(u16, bool, RetryVerdict)] = &[
        // The transient set, whatever the code says.
        (408, false, Retryable),
        (408, true, Retryable),
        (425, false, Retryable),
        (429, false, Retryable),
        (429, true, Retryable),
        (500, false, Retryable),
        (502, false, Retryable),
        (503, false, Retryable),
        (504, false, Retryable),
        (507, false, Retryable),
        (599, false, Retryable),
        // Contract `4xx`: an action from the recovery matrix, or nothing.
        (400, true, Recover),
        (400, false, Terminal),
        (403, false, Terminal),
        (404, true, Recover),
        (404, false, Terminal),
        (409, true, Recover),
        (409, false, Terminal),
        (413, false, Terminal),
        (415, false, Terminal),
        (426, false, Terminal),
        (499, false, Terminal),
        // Not failures this table retries.
        (200, false, Terminal),
        (204, true, Terminal),
        (301, false, Terminal),
        (600, false, Terminal),
    ];
    for &(status, names_action, expected) in table {
        let verdict = RetryVerdict::of_status(status, names_action);
        assert_eq!(verdict, expected, "{status} (action named: {names_action})");
        assert_eq!(verdict.permits_retry(), expected != Terminal, "{status}");
    }
}

/// **A failing server costs at most the lane's retry budget.** Every retry spends one token
/// whichever item takes it; once the budget is spent nothing more is retried until successes
/// earn tokens back, one per [`LANE_SUCCESSES_PER_RETRY`], never above the capacity.
#[test]
fn a_lane_retry_budget_bounds_the_retries_of_every_item() {
    let mut budget = LaneRetryBudget::new(3);
    assert_eq!(budget.capacity(), 3);
    let spent = (0..10).filter(|_| budget.try_spend()).count();
    assert_eq!(spent, 3, "ten failing items share three retries");
    assert_eq!(budget.remaining(), 0);

    for _ in 0..LANE_SUCCESSES_PER_RETRY - 1 {
        budget.record_success();
    }
    assert!(!budget.try_spend(), "not yet earned back");
    budget.record_success();
    assert_eq!(budget.remaining(), 1, "earned back by successes");

    for _ in 0..100 {
        budget.record_success();
    }
    assert_eq!(budget.remaining(), 3, "never above the capacity");

    let mut never = LaneRetryBudget::new(0);
    assert!(!never.try_spend(), "a zero budget never retries");
    assert_eq!(
        LaneRetryBudget::default().capacity(),
        DEFAULT_LANE_RETRY_BUDGET
    );
}

/// Every clone of a [`LaneRetries`] is the one budget: what one holder spends, another cannot.
#[test]
fn a_shared_lane_budget_is_one_pool() {
    let lane = LaneRetries::new(2);
    let other_item = lane.clone();
    assert!(lane.same_as(&other_item));
    assert!(!lane.same_as(&LaneRetries::new(2)));
    assert!(lane.try_spend());
    assert!(other_item.try_spend());
    assert!(!lane.try_spend(), "the clone spent the last retry");
    assert_eq!(other_item.remaining(), 0);
    for _ in 0..LANE_SUCCESSES_PER_RETRY {
        other_item.record_success();
    }
    assert_eq!(lane.remaining(), 1);
}

/// **Parallelism is connections, not streams.** `W` requests in flight at once through one SDK
/// client ride `W` separate HTTP/1.1 connections: the server below answers none of them until
/// all `W` have arrived, each on a connection of its own, so a client that multiplexed them onto
/// one connection or sent them one at a time never gets an answer and the test times out.
#[tokio::test]
async fn concurrent_requests_ride_separate_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const W: usize = 4;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let all_arrived = Arc::new(tokio::sync::Barrier::new(W));
    let server = tokio::spawn(async move {
        let mut connections = Vec::new();
        for _ in 0..W {
            let (mut socket, _) = listener.accept().await.unwrap();
            let all_arrived = all_arrived.clone();
            connections.push(tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0_u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let read = socket.read(&mut buf).await.unwrap();
                    assert_ne!(read, 0, "the client closed before sending a request");
                    head.extend_from_slice(&buf[..read]);
                }
                assert!(
                    head.starts_with(b"GET / HTTP/1.1\r\n"),
                    "an HTTP/1.1 request"
                );
                all_arrived.wait().await;
                socket
                    .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n")
                    .await
                    .unwrap();
            }));
        }
        for connection in connections {
            connection.await.unwrap();
        }
    });

    let client = http_client().unwrap();
    let url = format!("http://{addr}/");
    let requests: Vec<_> = (0..W)
        .map(|_| {
            let (client, url) = (client.clone(), url.clone());
            tokio::spawn(async move { client.get(url).send().await })
        })
        .collect();
    let answered = tokio::time::timeout(Duration::from_secs(20), async {
        for request in requests {
            let response = request.await.unwrap().unwrap();
            assert_eq!(response.status().as_u16(), 204);
        }
        server.await.unwrap();
    })
    .await;
    assert!(
        answered.is_ok(),
        "{W} concurrent requests did not ride {W} connections"
    );
}
