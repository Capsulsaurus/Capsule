//! Staged upload scheduler tests. Each staged Validation bullet in
//! the transfer upload doc gets a deterministic test: ladder order (by observed
//! session sequence), the `awaiting-original` derivation, the release gate (the
//! core [`ReleaseGate`] refuses while T2 is not durable), resume-from-server-truth
//! (re-derive the queue from the feed after a "kill"), and the staged × streaming
//! exclusion (proven by construction in `capsule-core`; wired here).

use std::sync::Mutex;

use capsule_core::crypto::hash::Hash32;
use capsule_library::library::{
    BlobRole, BlobVerdict, ReleaseDecision, ReleaseGate, RetainReason, StorageVerdict,
    StorageVerifier, VerifierError,
};
use uuid::Uuid;

use super::*;
use crate::sync::{BlobManifest, BlobRef, ChangeKind, FeedEntry, OriginalAvailability};

// ── A recording sink: returns Uploaded, remembering every (asset, blob) it saw ──

#[derive(Default)]
struct RecordingSink {
    seen: Mutex<Vec<(String, TierBlob)>>,
}

impl TierSink for RecordingSink {
    async fn open_session(
        &self,
        asset_id: &str,
        blob: &TierBlob,
    ) -> Result<TierSessionOutcome, StagedError> {
        self.seen
            .lock()
            .unwrap()
            .push((asset_id.to_string(), blob.clone()));
        Ok(TierSessionOutcome::Uploaded {
            session_id: format!("session-{}", blob.hash),
        })
    }
}

/// A three-tier bundle: T0 index (manifest + metadata), T1 preview (thumb +
/// preview), T2 original — supplied deliberately out of ladder order.
fn three_tier_asset(id: &str) -> StagedAsset {
    StagedAsset::new(
        id,
        vec![
            TierBlob::new(UploadTier::Original, format!("{id}-orig"), 4_000_000),
            TierBlob::new(UploadTier::Index, format!("{id}-manifest"), 512),
            TierBlob::new(UploadTier::Preview, format!("{id}-thumb"), 8_000),
            TierBlob::new(UploadTier::Index, format!("{id}-metadata"), 2_048),
            TierBlob::new(UploadTier::Preview, format!("{id}-preview"), 90_000),
        ],
    )
}

/// **Staged ladder order (unit).** Under `staged`, sessions open strictly T0 → T1 →
/// T2 per asset, and T2 (with T1) only under the large-reconciliation criteria; the
/// ladder is proven by the observed session sequence.
#[tokio::test]
async fn staged_opens_tiers_strictly_in_ladder_order() {
    let asset = three_tier_asset("a");

    // Unmetered Wi-Fi: the whole ladder opens, strictly T0 → T1 → T2 (intra-tier
    // order preserved: manifest before metadata, thumb before preview).
    let sink = RecordingSink::default();
    let report = StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Unmetered)
        .run(std::slice::from_ref(&asset), &sink)
        .await
        .unwrap();
    assert_eq!(
        report.tier_sequence(),
        vec![
            UploadTier::Index,
            UploadTier::Index,
            UploadTier::Preview,
            UploadTier::Preview,
            UploadTier::Original,
        ],
        "unmetered staged opens the full ladder in order"
    );
    let hashes: Vec<_> = report.opened.iter().map(|o| o.hash.as_str()).collect();
    assert_eq!(
        hashes,
        vec!["a-manifest", "a-metadata", "a-thumb", "a-preview", "a-orig"],
        "intra-tier order is preserved"
    );

    // Metered link: only the T0 index escapes; T1/T2 are deferred (large- and
    // small-reconciliation both require a non-metered link).
    let sink = RecordingSink::default();
    let report = StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Metered)
        .run(std::slice::from_ref(&asset), &sink)
        .await
        .unwrap();
    assert_eq!(
        report.tier_sequence(),
        vec![UploadTier::Index, UploadTier::Index],
        "metered staged opens only the T0 index — the ladder prefix"
    );

    // Force-sync on the metered link overrides the criteria: the whole ladder opens.
    let sink = RecordingSink::default();
    let report = StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Metered)
        .with_force_sync(true)
        .run(std::slice::from_ref(&asset), &sink)
        .await
        .unwrap();
    assert_eq!(report.opened.len(), 5, "force-sync opens every tier");
    assert_eq!(report.tier_sequence().last(), Some(&UploadTier::Original));
}

/// The permitted set is always a ladder **prefix**: the tier gate is monotone, so
/// T2 never opens before T1, which never opens before T0. Checked directly on
/// `plan_sessions` for every connection class.
#[test]
fn staged_plan_is_always_a_ladder_prefix() {
    let asset = three_tier_asset("p");
    for class in [
        ConnectionClass::Unmetered,
        ConnectionClass::Metered,
        ConnectionClass::Constrained,
        ConnectionClass::Adverse,
        ConnectionClass::Offline,
    ] {
        let planned = StagedScheduler::new(UploadPolicy::Staged, class).plan_sessions(&asset);
        // The planned tiers are non-decreasing and contain no gap (a tier present
        // implies every lower tier present).
        let tiers: Vec<UploadTier> = planned.iter().map(|b| b.tier).collect();
        assert!(tiers.windows(2).all(|w| w[0] <= w[1]), "{class:?} in order");
        if tiers.contains(&UploadTier::Preview) {
            assert!(tiers.contains(&UploadTier::Index), "{class:?} T1 ⇒ T0");
        }
        if tiers.contains(&UploadTier::Original) {
            assert!(tiers.contains(&UploadTier::Preview), "{class:?} T2 ⇒ T1");
        }
    }
    // Offline opens nothing at all — not even T0.
    let offline =
        StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Offline).plan_sessions(&asset);
    assert!(offline.is_empty(), "offline opens no session");
}

/// Under `full`, every session opens eagerly regardless of the connection class —
/// today's all-or-nothing behavior on one code path with `staged`.
#[tokio::test]
async fn full_opens_every_tier_eagerly_even_on_metered() {
    let asset = three_tier_asset("f");
    let sink = RecordingSink::default();
    let report = StagedScheduler::new(UploadPolicy::Full, ConnectionClass::Metered)
        .run(std::slice::from_ref(&asset), &sink)
        .await
        .unwrap();
    assert_eq!(report.opened.len(), 5, "full opens all five blobs");
    assert_eq!(
        report.tier_sequence().last(),
        Some(&UploadTier::Original),
        "full opens T2 even on a metered link"
    );
}

/// **awaiting-original semantics (unit).** Visibility is present with
/// `original_held = false` (the entry is on the feed → T0 finalized); the derived
/// badge is `AwaitingOriginal`, and it flips to `Held` when `original_held` flips.
/// The derivation is pure — never a stored second source of truth.
#[test]
fn awaiting_original_is_derived_from_original_held() {
    let mut entry = feed_entry(
        "z", /* original_held */ false, /* with_original_ref */ true,
    );
    assert!(entry.is_awaiting_original());
    assert_eq!(
        entry.original_availability(),
        OriginalAvailability::AwaitingOriginal
    );

    // The T2 finalization flips the fact; the derived badge follows with no other change.
    entry.original_held = true;
    assert!(!entry.is_awaiting_original());
    assert_eq!(entry.original_availability(), OriginalAvailability::Held);
}

/// `held_from_feed` reads server truth off the feed entry: derivatives are always
/// held; the original is held **iff** `original_held`, so an awaiting-original
/// asset's T2 stays outstanding (never a dangling reference).
#[test]
fn held_from_feed_excludes_the_original_while_awaiting() {
    let awaiting = feed_entry("z", false, true);
    let held = held_from_feed(&awaiting);
    assert!(held.contains("z-thumb"), "derivative held");
    assert!(
        !held.contains("z-orig"),
        "the original is NOT held while awaiting-original"
    );

    let landed = feed_entry("z", true, true);
    let held = held_from_feed(&landed);
    assert!(
        held.contains("z-orig"),
        "the original becomes held once original_held flips"
    );
}

/// A derivative manifest the feed inlines is held under the digest of its bytes — the address
/// the push names it by — so re-running a push against an unchanged library does not re-open a
/// session for it (#556).
#[test]
fn held_from_feed_holds_each_inlined_derivative_manifest_by_its_digest() {
    let entry = feed_entry("m", false, false);
    let held = held_from_feed(&entry);
    let digest = capsule_core::crypto::hash::hash_bytes(b"m-thumb-manifest").to_hex();
    assert!(
        held.contains(&digest),
        "the manifest is held by its address"
    );
    assert_eq!(
        held.len(),
        2,
        "the derivative and its manifest, and nothing else"
    );
}

/// **Staged resume from server truth (smoke).** Kill the client mid-ladder; on
/// restart the tier queue re-derives from the feed and re-uploads only the missing
/// tiers — never the ones the server already holds.
#[tokio::test]
async fn resume_re_derives_only_missing_tiers_from_server_truth() {
    let asset = three_tier_asset("r");

    // First window on a metered link only got the T0 index up. The client is then
    // killed — no local queue survives. On restart it pulls the feed: T0 blobs are
    // held (the entry exists), T1/T2 are not (derivatives absent, original_held = false).
    let mut entry = feed_entry("r", false, false);
    entry.blobs.derivatives = vec![]; // only the index landed; no derivative refs yet
    // The two T0 index blobs are held server-side (metadata finalization).
    let mut held = held_from_feed(&entry);
    held.insert("r-manifest".to_string());
    held.insert("r-metadata".to_string());

    let remaining = remaining_tiers(&asset, &held);
    assert_eq!(
        remaining.blobs.iter().map(|b| b.tier).collect::<Vec<_>>(),
        vec![
            UploadTier::Preview,
            UploadTier::Preview,
            UploadTier::Original
        ],
        "only the un-held T1 + T2 tiers remain"
    );

    // Now on unmetered Wi-Fi the resumed queue drives exactly those missing tiers.
    let sink = RecordingSink::default();
    let report = StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Unmetered)
        .run(&[remaining], &sink)
        .await
        .unwrap();
    let hashes: Vec<_> = report.opened.iter().map(|o| o.hash.as_str()).collect();
    assert_eq!(
        hashes,
        vec!["r-thumb", "r-preview", "r-orig"],
        "resume re-uploads only the missing tiers, in ladder order"
    );
    // The already-held index tiers were never re-opened.
    assert!(
        report.opened.iter().all(|o| o.tier != UploadTier::Index),
        "no held tier is re-uploaded"
    );
}

/// **Staged release gate (unit).** Under `staged`, every release path refuses while
/// T2 is not durable: the same core [`ReleaseGate`] that always governed release
/// sees the original blob as not-yet-stored (awaiting-original) and returns
/// `Retain`, then `Release` once T2 lands and the verdict turns durable.
#[test]
fn staged_release_gate_refuses_until_t2_is_durable() {
    let asset_id = Uuid::now_v7();
    let blob = Hash32([7u8; 32]);

    // T2 pending: the server does not yet hold the original → not durable → retain.
    let pending = StagedVerifier { t2_durable: false };
    let decision = ReleaseGate::new(&pending).may_release(asset_id, &[blob], true);
    assert_eq!(
        decision,
        ReleaseDecision::Retain(RetainReason::NotDurable),
        "no release while the original is still awaiting-original"
    );

    // T2 landed + durable + receipt verified: release is now safe.
    let durable = StagedVerifier { t2_durable: true };
    let decision = ReleaseGate::new(&durable).may_release(asset_id, &[blob], true);
    assert_eq!(
        decision,
        ReleaseDecision::Release,
        "release unlocks once T2 durable"
    );
}

/// A storage verifier whose original-blob durability tracks whether T2 has landed —
/// the staged-upload framing of the release gate's input.
struct StagedVerifier {
    t2_durable: bool,
}

impl StorageVerifier for StagedVerifier {
    fn verify(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<StorageVerdict, VerifierError> {
        let blobs = blob_hashes
            .iter()
            .map(|h| BlobVerdict {
                hash: *h,
                role: BlobRole::Original,
                stored: self.t2_durable,
                indexed: self.t2_durable,
                retrievable: self.t2_durable,
            })
            .collect();
        Ok(StorageVerdict {
            asset_id,
            durable: self.t2_durable,
            blobs,
            checked_at: "2026-07-10T00:00:00Z".into(),
        })
    }

    fn receipt_verified(&self, _: Uuid, _: &[Hash32]) -> Result<bool, VerifierError> {
        Ok(true)
    }
}

// ── Feed-entry fixture ─────────────────────────────────────────────────────────

/// A minimal sync [`FeedEntry`] for an asset: a held thumbnail derivative and,
/// optionally, an original ref whose held-ness is governed by `original_held`.
fn feed_entry(id: &str, original_held: bool, with_original_ref: bool) -> FeedEntry {
    FeedEntry {
        album_id: b"album".to_vec(),
        sync_seq: 1,
        protocol_version: "2026-07-10".to_string(),
        kind: ChangeKind::Created,
        asset_id: id.as_bytes().to_vec(),
        manifest_cbor: vec![],
        metadata_blob: vec![],
        blobs: BlobManifest {
            original: with_original_ref.then(|| BlobRef {
                ciphertext_hash: format!("{id}-orig"),
                role: "original".to_string(),
                size: 4_000_000,
            }),
            derivatives: vec![BlobRef {
                ciphertext_hash: format!("{id}-thumb"),
                role: "derivative".to_string(),
                size: 8_000,
            }],
        },
        derivative_manifests: vec![format!("{id}-thumb-manifest").into_bytes()],
        original_held,
        purged: false,
        changed_at: "1970-01-01T00:00:00Z".to_string(),
    }
}

// ── Running under a changing path ──

mod under_path {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::net::path::{
        PathCost, PathInterface, PathMonitor, PathSample, PathUpdate, ThermalState, TierAdmission,
    };
    use crate::upload::queue::LaneWindows;

    fn monitor() -> PathMonitor {
        PathMonitor::new(PathSample::desktop(), LaneWindows::default())
    }

    fn metered() -> PathSample {
        PathSample {
            cost: PathCost::Metered,
            interface: PathInterface::Cellular,
            ..PathSample::desktop()
        }
    }

    /// Set when the future holding it is dropped: the in-flight work was cancelled.
    struct Cancelled(Arc<AtomicBool>);

    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_plan_scheduler_gates_on_the_whole_plan() {
        let monitor = monitor();
        monitor.observe(PathSample {
            thermal: ThermalState::Serious,
            ..PathSample::desktop()
        });
        let scheduler = StagedScheduler::from_plan(&monitor.plan(), UploadPolicy::Full);
        // An unmetered class alone would open the original; the heat holds it back, and
        // force-sync does not overrule the device's own limits.
        assert!(scheduler.tier_opens_now(UploadTier::Index));
        assert!(!scheduler.tier_opens_now(UploadTier::Original));
        assert!(
            !scheduler
                .with_force_sync(true)
                .tier_opens_now(UploadTier::Original)
        );
    }

    #[test]
    fn a_class_scheduler_keeps_its_old_gate() {
        for class in [
            ConnectionClass::Unmetered,
            ConnectionClass::Metered,
            ConnectionClass::Constrained,
            ConnectionClass::Adverse,
            ConnectionClass::Offline,
        ] {
            for force_sync in [false, true] {
                let scheduler =
                    StagedScheduler::new(UploadPolicy::Staged, class).with_force_sync(force_sync);
                for tier in [UploadTier::Index, UploadTier::Preview, UploadTier::Original] {
                    assert_eq!(
                        scheduler.tier_opens_now(tier),
                        class.permits_tier(tier, force_sync),
                        "{class:?} {tier:?} force_sync={force_sync}"
                    );
                }
            }
            // Full still opens everything eagerly on a class-only scheduler.
            assert!(
                StagedScheduler::new(UploadPolicy::Full, class)
                    .tier_opens_now(UploadTier::Original)
            );
        }
    }

    #[test]
    fn narrowing_is_closing_a_tier_that_was_open() {
        let open = StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Unmetered);
        let index = StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Metered);
        let none = StagedScheduler::new(UploadPolicy::Staged, ConnectionClass::Offline);
        assert!(open.narrows_to(&index));
        assert!(index.narrows_to(&none));
        assert!(!index.narrows_to(&open));
        assert!(!open.narrows_to(&open));
    }

    /// **Acceptance: a changed path generation cancels forbidden work within one scheduling
    /// tick.** The attempt running under the unmetered plan never finishes on its own; the
    /// metered path closes the original tier, and the same wake that delivers the change drops
    /// it and re-runs under an index-only scheduler. The clock is paused and never advanced, so
    /// no timer or later tick is involved.
    #[tokio::test(start_paused = true)]
    async fn a_new_generation_cancels_forbidden_work_within_one_tick() {
        let monitor = monitor();
        let cancelled = Arc::new(AtomicBool::new(false));
        let schedulers = Mutex::new(Vec::new());
        let start = tokio::time::Instant::now();

        let run = run_under_path(&monitor, UploadPolicy::Full, false, |scheduler| {
            schedulers.lock().unwrap().push(scheduler);
            let guard = Cancelled(cancelled.clone());
            async move {
                if scheduler.tier_opens_now(UploadTier::Original) {
                    let _guard = guard;
                    std::future::pending::<&str>().await
                } else {
                    "index only"
                }
            }
        });
        let flip = async {
            tokio::task::yield_now().await;
            assert!(
                !cancelled.load(Ordering::SeqCst),
                "nothing cancels before the change"
            );
            let update = monitor.observe(metered());
            assert!(matches!(update, PathUpdate::NewPath(_)));
        };
        let (out, ()) = tokio::join!(run, flip);

        assert_eq!(out, "index only");
        assert!(
            cancelled.load(Ordering::SeqCst),
            "the in-flight attempt was dropped"
        );
        assert_eq!(tokio::time::Instant::now(), start, "no time passed");
        let schedulers = schedulers.lock().unwrap();
        assert_eq!(schedulers.len(), 2, "one cancel, one re-plan");
        assert!(schedulers[0].tier_opens_now(UploadTier::Original));
        assert!(schedulers[1].tier_opens_now(UploadTier::Index));
        assert!(!schedulers[1].tier_opens_now(UploadTier::Original));
    }

    /// A same-path change that closes a tier cancels just the same.
    #[tokio::test(start_paused = true)]
    async fn a_critical_thermal_state_cancels_everything() {
        let monitor = monitor();
        let admissions = Mutex::new(Vec::new());
        let run = run_under_path(&monitor, UploadPolicy::Staged, false, |scheduler| {
            let opens = scheduler.tier_opens_now(UploadTier::Index);
            admissions.lock().unwrap().push(opens);
            async move {
                if opens {
                    std::future::pending::<()>().await;
                }
            }
        });
        let flip = async {
            tokio::task::yield_now().await;
            monitor.observe(PathSample {
                thermal: ThermalState::Critical,
                ..PathSample::desktop()
            });
        };
        tokio::join!(run, flip);
        assert_eq!(*admissions.lock().unwrap(), vec![true, false]);
        assert_eq!(monitor.plan().admission, TierAdmission::Nothing);
    }

    /// A new path that closes nothing lets the running work finish where it is.
    #[tokio::test(start_paused = true)]
    async fn a_new_generation_that_closes_nothing_runs_on() {
        let monitor = monitor();
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let mut finished = Some(finished);
        let attempts = Mutex::new(0_u32);
        let run = run_under_path(&monitor, UploadPolicy::Full, false, |_| {
            *attempts.lock().unwrap() += 1;
            let finished = finished.take();
            async move {
                if let Some(finished) = finished {
                    let _ = finished.await;
                }
                "finished"
            }
        });
        let flip = async {
            tokio::task::yield_now().await;
            let update = monitor.observe(PathSample {
                interface: PathInterface::Wifi,
                ..PathSample::desktop()
            });
            assert!(matches!(update, PathUpdate::NewPath(_)));
            tokio::task::yield_now().await;
            finish.send(()).unwrap();
        };
        let (out, ()) = tokio::join!(run, flip);
        assert_eq!(out, "finished");
        assert_eq!(
            *attempts.lock().unwrap(),
            1,
            "the attempt was never cancelled"
        );
    }
}
