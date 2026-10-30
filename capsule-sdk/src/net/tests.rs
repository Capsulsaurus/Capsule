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

// ─── Per-request stall bounds ────────────────────────────────────────────────

/// Pull the next piece of `body` after `gap`, as an HTTP stack does once the link has taken the
/// piece before it.
async fn pull(body: &mut PacedBody, gap: Duration) -> Option<bytes::Bytes> {
    use http_body::Body as _;
    tokio::time::sleep(gap).await;
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *body).poll_frame(cx))
        .await
        .map(|frame| {
            let Ok(frame) = frame;
            frame.into_data().expect("a paced body yields only data")
        })
}

/// A body of `pieces` full pieces.
fn paced(pieces: usize, progress: &ExchangeProgress) -> PacedBody {
    PacedBody::new(
        bytes::Bytes::from(vec![9u8; pieces * PACED_PIECE]),
        progress.clone(),
    )
}

/// **Slow but steady.** Each piece leaves just inside the stall bound, and the server then takes
/// longer than the stall bound to answer, inside the response wait: twenty-odd stall bounds end
/// to end, and nothing is cut. Simulated clock; no wall-clock wait.
#[tokio::test(start_paused = true)]
async fn a_slow_but_steady_request_is_never_cut() {
    let bounds = RequestBounds::default();
    let progress = ExchangeProgress::waiting();
    let mut body = paced(20, &progress);
    let gap = Duration::from_secs(19);
    assert!(gap < bounds.stall);
    let started = tokio::time::Instant::now();
    let exchange = async move {
        let mut sent = 0;
        while let Some(piece) = pull(&mut body, gap).await {
            sent += piece.len();
        }
        // The server writes and fsyncs for longer than a stall bound before it answers.
        tokio::time::sleep(bounds.stall + Duration::from_secs(10)).await;
        sent
    };
    let sent = within_bounds(bounds, &progress, exchange)
        .await
        .expect("a live request is never cut");
    assert_eq!(sent, 20 * PACED_PIECE);
    assert!(
        started.elapsed() > bounds.stall * 15,
        "ran far past the stall bound"
    );
}

/// **Stalled.** Two pieces leave, then the link takes no more: the request is cut one stall
/// bound after the last piece, as a stall.
#[tokio::test(start_paused = true)]
async fn a_request_whose_body_stops_moving_is_cut_as_stalled() {
    let bounds = RequestBounds::default();
    let progress = ExchangeProgress::waiting();
    let mut body = paced(20, &progress);
    let started = tokio::time::Instant::now();
    let exchange = async move {
        pull(&mut body, Duration::from_secs(1)).await;
        pull(&mut body, Duration::from_secs(1)).await;
        std::future::pending::<()>().await;
    };
    assert_eq!(
        within_bounds(bounds, &progress, exchange).await,
        Err(RequestCut::Stalled)
    );
    let cut_at = Duration::from_secs(2) + bounds.stall;
    assert!(started.elapsed() >= cut_at);
    assert!(started.elapsed() < cut_at + Duration::from_secs(1));
}

/// **Never answered.** The whole body leaves and no response comes: the request is cut at the
/// response wait after the last piece, not at the shorter stall bound.
#[tokio::test(start_paused = true)]
async fn a_sent_request_with_no_answer_is_cut_at_the_response_wait() {
    let bounds = RequestBounds::default();
    let progress = ExchangeProgress::waiting();
    let mut body = paced(2, &progress);
    let started = tokio::time::Instant::now();
    let exchange = async move {
        while pull(&mut body, Duration::ZERO).await.is_some() {}
        std::future::pending::<()>().await;
    };
    assert_eq!(
        within_bounds(bounds, &progress, exchange).await,
        Err(RequestCut::NoResponse)
    );
    assert!(started.elapsed() >= bounds.response_wait);
    assert!(started.elapsed() < bounds.response_wait + Duration::from_secs(1));
}

/// A response body that stops arriving is a stall too.
#[tokio::test(start_paused = true)]
async fn a_response_that_stops_arriving_is_cut_as_stalled() {
    let bounds = RequestBounds::default();
    let progress = ExchangeProgress::waiting();
    progress.on_received();
    let exchange = std::future::pending::<()>();
    assert_eq!(
        within_bounds(bounds, &progress, exchange).await,
        Err(RequestCut::Stalled)
    );
}

#[test]
fn a_paced_body_advertises_its_exact_length() {
    use http_body::Body as _;
    let body = paced(3, &ExchangeProgress::waiting());
    assert_eq!(body.size_hint().exact(), Some(3 * PACED_PIECE as u64));
}

/// A piece pulled is the client's only sign the body is moving, so the slowest link it sees as
/// moving is one piece per stall bound. That must sit below the server's default 1 KiB/s
/// minimum sustained rate: the client never cuts as stalled a link the server would accept.
#[test]
fn one_piece_per_stall_bound_is_slower_than_the_server_rate_floor() {
    const SERVER_MIN_RATE_BYTES_PER_SECOND: f64 = 1024.0;
    let bounds = RequestBounds::default();
    let slowest_seen_moving = PACED_PIECE as f64 / bounds.stall.as_secs_f64();
    assert!(
        slowest_seen_moving < SERVER_MIN_RATE_BYTES_PER_SECOND,
        "{slowest_seen_moving} B/s"
    );
}

#[test]
fn the_response_wait_outlasts_the_stall_bound() {
    let bounds = RequestBounds::default();
    assert!(bounds.response_wait > bounds.stall);
    assert!(bounds.stall <= Duration::from_secs(30));
}
