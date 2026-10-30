//! Tests for the per-server upload queue: lanes, the shared byte budget, round-robin
//! fairness, tier order inside a lane, stall reclamation, deferral, and rebuild on resume.

use super::*;

const STALL: Duration = Duration::from_secs(30);

fn key(name: &str) -> ServerKey {
    ServerKey::new(format!("https://{name}"))
}

fn job(tier: UploadTier, bytes: u64, name: &'static str) -> QueuedJob<&'static str> {
    QueuedJob::new(tier, bytes, name)
}

/// Drain every admissible grant at `now`, completing none.
fn admit_all(queue: &mut UploadQueue<&'static str>, now: Instant) -> Vec<Grant<&'static str>> {
    std::iter::from_fn(|| queue.next(now)).collect()
}

#[test]
fn an_empty_queue_admits_nothing() {
    let mut queue: UploadQueue<&str> = UploadQueue::new(1024, STALL);
    assert!(queue.is_empty());
    assert_eq!(queue.next(Instant::now()), None);
}

#[test]
fn a_single_lane_with_window_one_runs_one_job_at_a_time_in_order() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(1 << 30, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 10, "a1"));
    queue.enqueue(&a, job(UploadTier::Index, 10, "a2"));

    let first = queue.next(now).expect("first job admitted");
    assert_eq!(first.job, "a1");
    assert_eq!(queue.next(now), None, "the window of one is full");

    assert!(queue.complete(first.id, now));
    assert_eq!(queue.next(now).expect("second job admitted").job, "a2");
}

#[test]
fn tier_order_holds_inside_a_lane_whatever_the_enqueue_order() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(1 << 30, STALL);
    queue.set_window(&a, 8);
    queue.enqueue(&a, job(UploadTier::Original, 1, "orig"));
    queue.enqueue(&a, job(UploadTier::Preview, 1, "prev"));
    queue.enqueue(&a, job(UploadTier::Index, 1, "idx1"));
    queue.enqueue(&a, job(UploadTier::Index, 1, "idx2"));

    let order: Vec<_> = admit_all(&mut queue, now)
        .into_iter()
        .map(|grant| grant.job)
        .collect();
    assert_eq!(order, ["idx1", "idx2", "prev", "orig"]);
}

#[test]
fn lanes_are_served_round_robin() {
    let now = Instant::now();
    let (a, b, c) = (key("a"), key("b"), key("c"));
    let mut queue = UploadQueue::new(1 << 30, STALL);
    for server in [&a, &b, &c] {
        queue.set_window(server, 8);
    }
    for name in ["a1", "a2", "a3"] {
        queue.enqueue(&a, job(UploadTier::Index, 1, name));
    }
    queue.enqueue(&b, job(UploadTier::Index, 1, "b1"));
    for name in ["c1", "c2"] {
        queue.enqueue(&c, job(UploadTier::Index, 1, name));
    }

    let order: Vec<_> = admit_all(&mut queue, now)
        .into_iter()
        .map(|grant| grant.job)
        .collect();
    assert_eq!(order, ["a1", "b1", "c1", "a2", "c2", "a3"]);
}

#[test]
fn round_robin_resumes_after_the_lane_served_last() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(1 << 30, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 1, "a1"));
    queue.enqueue(&a, job(UploadTier::Index, 1, "a2"));
    queue.enqueue(&b, job(UploadTier::Index, 1, "b1"));
    queue.enqueue(&b, job(UploadTier::Index, 1, "b2"));

    let a1 = queue.next(now).expect("a1");
    assert_eq!(a1.job, "a1");
    queue.complete(a1.id, now);
    // `a` was served last, so `b` goes next even though `a` is free again.
    assert_eq!(queue.next(now).expect("b1").job, "b1");
    assert_eq!(queue.next(now).expect("a2").job, "a2");
}

#[test]
fn every_lane_shares_one_byte_budget() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(100, STALL);
    queue.set_window(&a, 8);
    queue.set_window(&b, 8);
    queue.enqueue(&a, job(UploadTier::Index, 60, "a1"));
    queue.enqueue(&b, job(UploadTier::Index, 60, "b1"));
    queue.enqueue(&b, job(UploadTier::Index, 40, "b2"));

    let a1 = queue.next(now).expect("a1 fits");
    assert_eq!(a1.reserved, 60);
    assert_eq!(queue.in_use(), 60);
    // b1 does not fit the 40 bytes left, and `b` never skips its head for b2.
    assert_eq!(queue.next(now), None);

    assert!(queue.complete(a1.id, now));
    assert_eq!(queue.in_use(), 0);
    let b1 = queue.next(now).expect("b1 fits once a1 released");
    assert_eq!(b1.job, "b1");
    let b2 = queue.next(now).expect("b2 fits beside b1");
    assert_eq!(b2.job, "b2");
    assert_eq!(queue.in_use(), 100);
}

#[test]
fn a_job_larger_than_the_budget_runs_once_the_budget_is_free() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL);
    queue.set_window(&a, 2);
    queue.enqueue(&a, job(UploadTier::Index, 10, "small"));
    queue.enqueue(&a, job(UploadTier::Original, 500, "huge"));

    let small = queue.next(now).expect("small");
    assert_eq!(queue.next(now), None, "huge waits for the whole budget");
    queue.complete(small.id, now);
    let huge = queue.next(now).expect("huge runs alone");
    assert_eq!(
        huge.reserved, 100,
        "its reservation is clamped to the budget"
    );
    assert_eq!(queue.in_use(), 100);
}

#[test]
fn a_deferred_lane_is_passed_over_until_its_deadline() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(1 << 30, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 1, "a1"));
    queue.enqueue(&b, job(UploadTier::Index, 1, "b1"));
    queue.defer(&a, now + Duration::from_secs(10));
    // An earlier deadline never shortens the wait the server asked for.
    queue.defer(&a, now + Duration::from_secs(1));

    assert_eq!(queue.next(now).expect("b1").job, "b1");
    assert_eq!(queue.next(now + Duration::from_secs(5)), None);
    assert_eq!(
        queue
            .next(now + Duration::from_secs(10))
            .expect("a1 after Retry-After")
            .job,
        "a1"
    );
}

#[test]
fn a_stalled_lane_gives_its_budget_to_the_lanes_still_moving() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(100, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 100, "a1"));
    queue.enqueue(&b, job(UploadTier::Index, 100, "b1"));

    let a1 = queue.next(now).expect("a1 holds the whole budget");
    assert_eq!(queue.next(now), None);

    // Short of the bound, progress keeps the reservation.
    let later = now + STALL / 2;
    assert!(queue.progress(a1.id, later));
    assert!(queue.reclaim_stalled(later + STALL / 2).is_empty());

    let stalled_at = later + STALL;
    assert_eq!(queue.reclaim_stalled(stalled_at), vec![a1.id]);
    assert_eq!(queue.in_use(), 0);
    assert_eq!(queue.queued(&a), 1, "a1 is back on its lane");
    assert!(
        !queue.progress(a1.id, stalled_at),
        "a revoked grant is gone"
    );
    assert!(!queue.complete(a1.id, stalled_at));

    // `a` waits out one stall bound, so `b` takes the budget.
    assert_eq!(queue.next(stalled_at).expect("b1").job, "b1");
    let b1 = queue.in_flight(&b);
    assert_eq!(b1, 1);
}

#[test]
fn a_reclaimed_lane_is_served_again_after_one_stall_bound() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 10, "a1"));
    let a1 = queue.next(now).expect("a1");
    let stalled_at = now + STALL;
    assert_eq!(queue.reclaim_stalled(stalled_at), vec![a1.id]);
    assert_eq!(queue.next(stalled_at), None);
    let again = queue.next(stalled_at + STALL).expect("a1 again");
    assert_eq!(again.job, "a1");
    assert_ne!(again.id, a1.id, "a re-admitted job gets a fresh grant");
}

#[test]
fn reclaim_keeps_the_original_order_of_revoked_jobs() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(1 << 30, STALL);
    queue.set_window(&a, 4);
    for name in ["a1", "a2", "a3"] {
        queue.enqueue(&a, job(UploadTier::Index, 1, name));
    }
    for name in ["a4", "a5"] {
        queue.enqueue(&a, job(UploadTier::Index, 1, name));
    }
    let granted = admit_all(&mut queue, now);
    assert_eq!(granted.len(), 4);
    // Finishing a job out of order must not reorder the ones still in flight.
    assert!(queue.complete(granted[0].id, now));
    assert_eq!(queue.reclaim_stalled(now + STALL).len(), 3);

    let order: Vec<_> = admit_all(&mut queue, now + 2 * STALL)
        .into_iter()
        .map(|grant| grant.job)
        .collect();
    assert_eq!(order, ["a2", "a3", "a4", "a5"]);
}

#[test]
fn reclaim_never_shortens_a_longer_server_deadline() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 10, "a1"));
    let a1 = queue.next(now).expect("a1");
    let retry_after = now + 10 * STALL;
    queue.defer(&a, retry_after);

    assert_eq!(queue.reclaim_stalled(now + STALL), vec![a1.id]);
    assert_eq!(
        queue.next(now + 2 * STALL),
        None,
        "the server's Retry-After outlasts the stall hold-off"
    );
    assert_eq!(queue.next(retry_after).expect("a1 again").job, "a1");
}

#[test]
fn rebuild_keeps_a_running_server_deadline() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL);
    let retry_after = now + Duration::from_secs(60);
    queue.defer(&a, retry_after);
    let superseded = queue.rebuild_lane(&a, [job(UploadTier::Index, 10, "a1")]);
    assert!(superseded.is_empty());

    assert_eq!(
        queue.next(now),
        None,
        "a rebuilt lane still waits out Retry-After"
    );
    assert_eq!(queue.next(retry_after).expect("a1").job, "a1");
}

#[test]
fn a_requeued_job_returns_to_the_front_of_its_tier() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(1 << 30, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 5, "a1"));
    queue.enqueue(&a, job(UploadTier::Index, 5, "a2"));
    let a1 = queue.next(now).expect("a1");
    assert!(queue.requeue(a1.id));
    assert_eq!(queue.in_use(), 0);
    assert!(!queue.requeue(a1.id), "a grant is requeued once");
    assert_eq!(queue.next(now).expect("a1 again").job, "a1");
}

#[test]
fn a_window_bounds_each_lane_independently() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(1 << 30, STALL);
    queue.set_window(&a, 2);
    for name in ["a1", "a2", "a3"] {
        queue.enqueue(&a, job(UploadTier::Index, 1, name));
    }
    for name in ["b1", "b2"] {
        queue.enqueue(&b, job(UploadTier::Index, 1, name));
    }
    let order: Vec<_> = admit_all(&mut queue, now)
        .into_iter()
        .map(|grant| grant.job)
        .collect();
    assert_eq!(order, ["a1", "b1", "a2"]);
    assert_eq!(queue.in_flight(&a), 2);
    assert_eq!(queue.in_flight(&b), 1);
}

#[test]
fn a_zero_window_or_budget_is_raised_to_one() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(0, STALL);
    assert_eq!(queue.budget(), 1);
    queue.set_window(&a, 0);
    queue.enqueue(&a, job(UploadTier::Index, 0, "empty"));
    queue.enqueue(&a, job(UploadTier::Index, 0, "empty2"));
    assert!(queue.next(now).is_some());
    assert_eq!(
        queue.next(now),
        None,
        "the window is one, not zero or unbounded"
    );
}

#[test]
fn rebuild_replaces_a_lane_from_its_own_server_truth_only() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(100, STALL);
    queue.set_window(&a, 3);
    queue.enqueue(&a, job(UploadTier::Index, 30, "a-stale-1"));
    queue.enqueue(&a, job(UploadTier::Index, 30, "a-stale-2"));
    queue.enqueue(&b, job(UploadTier::Index, 10, "b1"));
    queue.defer(&a, now + Duration::from_secs(60));
    let in_flight = queue.next(now).expect("b1 (a is deferred)");
    assert_eq!(in_flight.job, "b1");

    let stale = queue
        .next(now + Duration::from_secs(60))
        .expect("a-stale-1");
    assert_eq!(queue.in_use(), 40);

    let superseded = queue.rebuild_lane(
        &a,
        [
            job(UploadTier::Original, 20, "a-orig"),
            job(UploadTier::Index, 20, "a-idx"),
        ],
    );
    assert_eq!(superseded, vec![stale.id]);
    assert_eq!(queue.in_use(), 10, "only b1's reservation is left");
    assert_eq!(queue.queued(&a), 2);
    assert_eq!(queue.queued(&b), 0);
    assert_eq!(queue.in_flight(&b), 1, "b's lane is untouched");
    assert!(
        !queue.complete(stale.id, now),
        "a superseded grant is ignored"
    );

    let order: Vec<_> = admit_all(&mut queue, now)
        .into_iter()
        .map(|grant| grant.job)
        .collect();
    assert_eq!(
        order,
        ["a-idx", "a-orig"],
        "the rebuilt lane keeps tier order and its window"
    );
}

#[test]
fn servers_lists_lanes_in_round_robin_order() {
    let (a, b) = (key("a"), key("b"));
    let mut queue: UploadQueue<&str> = UploadQueue::new(1, STALL);
    queue.enqueue(&b, job(UploadTier::Index, 1, "b1"));
    queue.enqueue(&a, job(UploadTier::Index, 1, "a1"));
    queue.enqueue(&b, job(UploadTier::Index, 1, "b2"));
    let servers: Vec<_> = queue.servers().map(ServerKey::as_str).collect();
    assert_eq!(servers, ["https://b", "https://a"]);
    assert_eq!(a.to_string(), "https://a");
    assert!(!queue.is_empty());
}

/// **A delete withdraws its asset's jobs from every lane.** Queued jobs of the asset are
/// dropped on both servers, one in flight is superseded with its reservation released, and
/// every other job keeps its place and runs in tier order.
#[test]
fn withdraw_removes_one_assets_jobs_from_every_lane() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(100, STALL);
    queue.set_window(&a, 2);
    queue.set_window(&b, 2);
    queue.enqueue(&a, job(UploadTier::Index, 10, "x-idx@a"));
    queue.enqueue(&a, job(UploadTier::Index, 10, "y-idx@a"));
    queue.enqueue(&a, job(UploadTier::Original, 10, "x-orig@a"));
    queue.enqueue(&b, job(UploadTier::Index, 10, "x-idx@b"));
    queue.enqueue(&b, job(UploadTier::Preview, 10, "y-prev@b"));
    let running = queue.next(now).expect("x-idx@a");
    assert_eq!(running.job, "x-idx@a");
    assert_eq!(queue.in_use(), 10);

    let superseded = queue.withdraw(|name| name.starts_with("x-"));

    assert_eq!(superseded, vec![running.id]);
    assert_eq!(
        queue.in_use(),
        0,
        "the superseded grant's bytes are released"
    );
    assert!(
        !queue.complete(running.id, now),
        "a superseded grant is ignored"
    );
    assert_eq!(queue.queued(&a), 1);
    assert_eq!(queue.queued(&b), 1);
    let mut order: Vec<_> = admit_all(&mut queue, now)
        .into_iter()
        .map(|grant| grant.job)
        .collect();
    order.sort_unstable();
    assert_eq!(order, ["y-idx@a", "y-prev@b"]);
    assert!(
        queue.withdraw(|name| name.starts_with("x-")).is_empty(),
        "withdrawing again finds nothing"
    );
}

/// Withdrawing keeps a lane's pause: a server's `Retry-After` still binds what is left.
#[test]
fn withdraw_keeps_a_lanes_pause() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL);
    queue.enqueue(&a, job(UploadTier::Index, 10, "x"));
    queue.enqueue(&a, job(UploadTier::Index, 10, "y"));
    let retry_after = now + Duration::from_secs(60);
    queue.defer(&a, retry_after);

    assert!(queue.withdraw(|name| *name == "x").is_empty());

    assert_eq!(
        queue.next(now),
        None,
        "the lane still waits out Retry-After"
    );
    assert_eq!(queue.next(retry_after).expect("y").job, "y");
}
