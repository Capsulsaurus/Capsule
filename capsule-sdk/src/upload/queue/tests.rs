//! Tests for the per-server upload queue: lanes, the two in-flight windows and the advice that
//! lowers them, the shared byte budget reserved per range, the lane retry budget, round-robin
//! fairness, tier order inside a lane, stall reclamation, deferral, and rebuild on resume. The
//! last tests drive the queue through a simulated clock over a lossy, high-latency link.

use super::*;

const STALL: Duration = Duration::from_secs(30);

fn key(name: &str) -> ServerKey {
    ServerKey::new(format!("https://{name}"))
}

fn job(tier: UploadTier, name: &'static str) -> QueuedJob<&'static str> {
    QueuedJob::new(tier, name)
}

/// A queue whose lanes open with sessions window `sessions` and ranges window one.
fn queue_of(budget: u64, sessions: u32) -> UploadQueue<&'static str> {
    UploadQueue::new(budget, STALL).with_windows(LaneWindows::new(sessions, 1))
}

/// Drain every admissible grant at `now`, completing none.
fn admit_all(queue: &mut UploadQueue<&'static str>, now: Instant) -> Vec<Grant<&'static str>> {
    std::iter::from_fn(|| queue.next(now)).collect()
}

/// Advice as a server would send it.
fn advice(header: &str) -> ServerAdvice {
    ServerAdvice::parse(Some(header))
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
    let mut queue = queue_of(1 << 30, 1);
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
    queue.enqueue(&a, job(UploadTier::Index, "a2"));

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
    let mut queue = queue_of(1 << 30, 8);
    queue.enqueue(&a, job(UploadTier::Original, "orig"));
    queue.enqueue(&a, job(UploadTier::Preview, "prev"));
    queue.enqueue(&a, job(UploadTier::Index, "idx1"));
    queue.enqueue(&a, job(UploadTier::Index, "idx2"));

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
    let mut queue = queue_of(1 << 30, 8);
    for name in ["a1", "a2", "a3"] {
        queue.enqueue(&a, job(UploadTier::Index, name));
    }
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    for name in ["c1", "c2"] {
        queue.enqueue(&c, job(UploadTier::Index, name));
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
    let mut queue = queue_of(1 << 30, 1);
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
    queue.enqueue(&a, job(UploadTier::Index, "a2"));
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    queue.enqueue(&b, job(UploadTier::Index, "b2"));

    let a1 = queue.next(now).expect("a1");
    assert_eq!(a1.job, "a1");
    queue.complete(a1.id, now);
    // `a` was served last, so `b` goes next even though `a` is free again.
    assert_eq!(queue.next(now).expect("b1").job, "b1");
    assert_eq!(queue.next(now).expect("a2").job, "a2");
}

/// **A blob's size reserves nothing; its ranges do.** Admitting a session takes none of the
/// budget, every range in flight holds its length, and every lane's ranges share one budget.
#[test]
fn every_lane_shares_one_byte_budget_reserved_per_range() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(100, STALL).with_windows(LaneWindows::new(8, 4));
    queue.enqueue(&a, job(UploadTier::Original, "a-4GiB-original"));
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    queue.enqueue(&b, job(UploadTier::Index, "b2"));

    let a1 = queue.next(now).expect("a session");
    let b1 = queue.next(now).expect("b1");
    let b2 = queue.next(now).expect("b2");
    assert_eq!(queue.in_use(), 0, "admitting a session reserves nothing");

    let a_range = queue.reserve_range(a1.id, 60, now).expect("60 fits");
    assert_eq!(queue.in_use(), 60);
    assert_eq!(
        queue.reserve_range(b1.id, 60, now),
        Err(RangeRefused::BudgetFull),
        "another lane's range does not fit the 40 bytes left"
    );
    assert_eq!(
        queue.reserve_range(b2.id, 40, now),
        Err(RangeRefused::BudgetFull),
        "40 would fit, but b1 waits for the budget first"
    );

    assert!(queue.release_range(a_range, now));
    assert!(
        !queue.release_range(a_range, now),
        "a range is released once"
    );
    assert_eq!(queue.in_use(), 0);
    queue
        .reserve_range(b1.id, 60, now)
        .expect("fits once a's range returned");
    let b_range = queue
        .reserve_range(b2.id, 40, now)
        .expect("40 fits once b1, ahead of it, is served");
    assert_eq!(queue.in_use(), 100);

    assert!(queue.complete(b2.id, now), "completing releases its ranges");
    assert!(!queue.release_range(b_range, now));
    assert_eq!(queue.in_use(), 60);
}

#[test]
fn a_range_larger_than_the_budget_runs_once_the_budget_is_free() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL).with_windows(LaneWindows::new(2, 2));
    queue.enqueue(&a, job(UploadTier::Original, "orig"));
    let orig = queue.next(now).expect("orig");

    let small = queue.reserve_range(orig.id, 10, now).expect("small");
    assert_eq!(
        queue.reserve_range(orig.id, 500, now),
        Err(RangeRefused::BudgetFull),
        "a huge range waits for the whole budget"
    );
    queue.release_range(small, now);
    queue
        .reserve_range(orig.id, 500, now)
        .expect("the huge range runs alone");
    assert_eq!(
        queue.in_use(),
        100,
        "its reservation is clamped to the budget"
    );
}

/// **The budget serves waiting ranges first-come.** A holder of the whole budget that releases
/// its range and asks for the next one at once goes behind the other lane's T0 range that was
/// already waiting, so one lane's originals never starve another lane's T0 work; and a range
/// clamped to the whole budget keeps its place at the head until the budget drains for it.
#[test]
fn waiting_ranges_are_served_first_come() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = UploadQueue::new(100, STALL).with_windows(LaneWindows::new(4, 1));
    queue.enqueue(&a, job(UploadTier::Original, "a-original"));
    queue.enqueue(&b, job(UploadTier::Index, "b-index"));
    let original = queue.next(now).expect("a's original");
    let index = queue.next(now).expect("b's index");

    let held = queue
        .reserve_range(original.id, 500, now)
        .expect("the clamped range takes the whole budget");
    assert_eq!(
        queue.reserve_range(index.id, 10, now),
        Err(RangeRefused::BudgetFull)
    );
    assert_eq!(queue.waiting_for_budget(), 1);

    assert!(queue.release_range(held, now));
    assert_eq!(
        queue.reserve_range(original.id, 500, now),
        Err(RangeRefused::BudgetFull),
        "the holder asks again behind the range already waiting"
    );
    assert_eq!(queue.waiting_for_budget(), 2);
    let small = queue
        .reserve_range(index.id, 10, now)
        .expect("the first in line is served first");
    assert_eq!(queue.waiting_for_budget(), 1);

    assert_eq!(
        queue.reserve_range(original.id, 500, now),
        Err(RangeRefused::BudgetFull),
        "the clamped range waits for the whole budget"
    );
    assert!(queue.release_range(small, now));
    queue
        .reserve_range(original.id, 500, now)
        .expect("the budget drained for the head of the line");
    assert_eq!(queue.waiting_for_budget(), 0);
    assert_eq!(queue.in_use(), 100);
}

/// A grant that leaves flight while it waits leaves the budget's line with it, so the line
/// never waits on a session that will not ask again.
#[test]
fn a_waiting_grant_that_leaves_flight_leaves_the_line() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL).with_windows(LaneWindows::new(4, 1));
    for name in ["holder", "gone", "next"] {
        queue.enqueue(&a, job(UploadTier::Original, name));
    }
    let holder = queue.next(now).expect("holder");
    let gone = queue.next(now).expect("gone");
    let next = queue.next(now).expect("next");

    let held = queue.reserve_range(holder.id, 100, now).expect("held");
    assert!(queue.reserve_range(gone.id, 10, now).is_err());
    assert!(queue.reserve_range(next.id, 10, now).is_err());
    assert_eq!(queue.waiting_for_budget(), 2);

    assert!(queue.requeue(gone.id));
    assert_eq!(queue.waiting_for_budget(), 1);
    assert!(queue.release_range(held, now));
    queue
        .reserve_range(next.id, 10, now)
        .expect("the requeued grant no longer holds the line");
}

/// **P bounds the ranges each session holds in flight**, independently per session.
#[test]
fn a_session_holds_at_most_its_ranges_window() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(1 << 30, STALL).with_windows(LaneWindows::new(2, 2));
    queue.enqueue(&a, job(UploadTier::Original, "x"));
    queue.enqueue(&a, job(UploadTier::Original, "y"));
    let x = queue.next(now).expect("x");
    let y = queue.next(now).expect("y");
    assert_eq!(x.ranges, 2);

    let first = queue.reserve_range(x.id, 4096, now).expect("first");
    queue.reserve_range(x.id, 4096, now).expect("second");
    assert_eq!(
        queue.reserve_range(x.id, 4096, now),
        Err(RangeRefused::WindowFull)
    );
    assert_eq!(queue.ranges_in_flight(x.id), 2);
    queue
        .reserve_range(y.id, 4096, now)
        .expect("another session has its own window");

    queue.release_range(first, now);
    queue
        .reserve_range(x.id, 4096, now)
        .expect("a returned range frees a slot");

    assert!(queue.complete(x.id, now));
    assert_eq!(
        queue.reserve_range(x.id, 4096, now),
        Err(RangeRefused::NotInFlight)
    );
    assert_eq!(queue.ranges_in_flight(x.id), 0);
}

/// **Advice only lowers the windows.** `sessions` narrows W while it is fresh and stops binding
/// once its `ttl` runs out; `ranges` narrows P for the sessions granted after it, and a
/// session's own advice narrows its P further but never widens it. Advice above the client's own
/// caps changes nothing.
#[test]
fn advice_lowers_the_windows_and_never_raises_them() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(1 << 30, STALL).with_windows(LaneWindows::new(4, 4));
    for name in ["s1", "s2", "s3", "s4", "s5", "s6"] {
        queue.enqueue(&a, job(UploadTier::Index, name));
    }
    assert_eq!(queue.sessions_window(&a, now), 4);

    queue.set_advice(&a, advice("sessions=2, ranges=3, ttl=60"), now);
    assert_eq!(queue.sessions_window(&a, now), 2);
    let granted = admit_all(&mut queue, now);
    assert_eq!(granted.len(), 2, "W is the advised two");
    assert!(granted.iter().all(|grant| grant.ranges == 3));

    // The session's own create narrows its P; a wider answer later never widens it.
    assert_eq!(
        queue.set_session_ranges(granted[0].id, &advice("ranges=1")),
        Some(1)
    );
    assert_eq!(
        queue.set_session_ranges(granted[0].id, &advice("ranges=16")),
        Some(1)
    );
    queue
        .reserve_range(granted[0].id, 1, now)
        .expect("one range");
    assert_eq!(
        queue.reserve_range(granted[0].id, 1, now),
        Err(RangeRefused::WindowFull)
    );

    // Once the advice is older than its ttl, W is the lane's own cap again.
    let stale = now + Duration::from_secs(60);
    assert_eq!(queue.sessions_window(&a, stale), 4);
    assert_eq!(admit_all(&mut queue, stale).len(), 2);

    // Advice above the client's caps is clamped to them.
    let b = key("b");
    queue.set_windows(&b, LaneWindows::new(3, 2));
    queue.set_advice(&b, advice("sessions=16, ranges=16"), now);
    assert_eq!(queue.sessions_window(&b, now), 3);
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    assert_eq!(queue.next(now).expect("b1").ranges, 2);
    assert_eq!(
        queue.set_session_ranges(GrantId(u64::MAX), &advice("ranges=1")),
        None,
        "an unknown grant"
    );
}

/// **W never exceeds the account's live-session cap**, whatever the client's caps or advice.
#[test]
fn the_live_session_cap_bounds_the_sessions_window() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = queue_of(1 << 30, 8);
    for name in ["a1", "a2", "a3", "a4"] {
        queue.enqueue(&a, job(UploadTier::Index, name));
    }
    queue.set_session_cap(&a, 3);
    assert_eq!(queue.sessions_window(&a, now), 3);
    assert_eq!(admit_all(&mut queue, now).len(), 3);
    queue.set_session_cap(&a, 0);
    assert_eq!(queue.sessions_window(&a, now), 1, "never below one");
    assert_eq!(
        queue.sessions_window(&key("unknown"), now),
        8,
        "a server with no lane answers the queue's own cap"
    );
}

/// The caps a lane holds are clamped into the compiled bounds.
#[test]
fn lane_windows_are_clamped_into_the_compiled_bounds() {
    assert_eq!(LaneWindows::new(0, 0), LaneWindows::new(1, 1));
    let widest = LaneWindows::new(u32::MAX, u32::MAX);
    assert_eq!(widest.sessions, MAX_SESSIONS_WINDOW);
    assert_eq!(widest.ranges, MAX_RANGES_WINDOW);
    assert_eq!(
        LaneWindows::default(),
        LaneWindows::new(DEFAULT_SESSIONS_WINDOW, DEFAULT_RANGES_WINDOW)
    );
}

/// **One retry budget per lane.** Retries spend it whichever job fails; once it is spent a
/// failing job leaves the queue instead of running again, and other lanes keep their own.
#[test]
fn a_lane_retry_budget_bounds_every_jobs_retries() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = queue_of(1 << 30, 4).with_retry_budget(2);
    for name in ["a1", "a2", "a3"] {
        queue.enqueue(&a, job(UploadTier::Index, name));
    }
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    let lane = queue.lane_retries(&a);
    assert!(lane.same_as(&queue.lane_retries(&a)), "one budget per lane");
    assert!(!lane.same_as(&queue.lane_retries(&b)));

    let granted = admit_all(&mut queue, now);
    let on_a: Vec<_> = granted.iter().filter(|g| g.server == a).collect();
    assert_eq!(queue.retry(on_a[0].id), RetryAdmission::Requeued);
    assert_eq!(queue.retry(on_a[1].id), RetryAdmission::Requeued);
    assert_eq!(
        queue.retry(on_a[2].id),
        RetryAdmission::BudgetSpent(job(UploadTier::Index, "a3"))
    );
    assert_eq!(queue.retry(on_a[2].id), RetryAdmission::NotInFlight);
    assert_eq!(queue.queued(&a), 2, "the two paid retries are queued again");
    assert_eq!(lane.remaining(), 0);

    let b1 = granted.iter().find(|g| g.server == b).expect("b1");
    assert_eq!(
        queue.retry(b1.id),
        RetryAdmission::Requeued,
        "b's budget is its own"
    );

    // A requeue is an interruption, not a failure: it charges nothing.
    let again = queue.next(now).expect("a job again");
    assert!(queue.requeue(again.id));
    assert_eq!(lane.remaining(), 0);
}

#[test]
fn a_deferred_lane_is_passed_over_until_its_deadline() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = queue_of(1 << 30, 1);
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
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
fn a_stalled_lane_gives_up_every_range_it_holds() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = UploadQueue::new(100, STALL).with_windows(LaneWindows::new(1, 2));
    queue.enqueue(&a, job(UploadTier::Index, "a1"));

    let a1 = queue.next(now).expect("a1");
    let a_range = queue.reserve_range(a1.id, 60, now).expect("a range");
    queue.reserve_range(a1.id, 40, now).expect("another range");
    assert_eq!(queue.in_use(), 100);

    // Short of the bound, progress keeps the reservations.
    let later = now + STALL / 2;
    assert!(queue.progress(a1.id, later));
    assert!(queue.reclaim_stalled(later + STALL / 2).is_empty());

    let stalled_at = later + STALL;
    assert_eq!(queue.reclaim_stalled(stalled_at), vec![a1.id]);
    assert_eq!(queue.in_use(), 0, "every range reservation is revoked");
    assert!(!queue.release_range(a_range, stalled_at));
    assert_eq!(queue.queued(&a), 1, "a1 is back on its lane");
    assert!(
        !queue.progress(a1.id, stalled_at),
        "a revoked grant is gone"
    );
    assert!(!queue.complete(a1.id, stalled_at));
    assert_eq!(
        queue.lane_retries(&a).remaining(),
        DEFAULT_LANE_RETRY_BUDGET,
        "a stall charges no retry"
    );
}

/// A lane still moving keeps its sessions while a stalled one gives its budget up.
#[test]
fn a_stalled_lane_gives_its_budget_to_the_lanes_still_moving() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = queue_of(100, 1);
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    let a1 = queue.next(now).expect("a1");
    queue.reserve_range(a1.id, 100, now).expect("a1's range");
    let b1 = queue.next(now).expect("b1");

    let stalled_at = now + STALL;
    // b reported progress; a did not.
    assert!(queue.progress(b1.id, now + STALL / 2));
    assert_eq!(queue.reclaim_stalled(stalled_at), vec![a1.id]);
    queue
        .reserve_range(b1.id, 100, stalled_at)
        .expect("b takes the budget a gave up");
    assert_eq!(queue.in_flight(&b), 1);
    assert_eq!(queue.in_flight(&a), 0);
    assert_eq!(queue.next(stalled_at), None, "a waits out one stall bound");
}

#[test]
fn a_reclaimed_lane_is_served_again_after_one_stall_bound() {
    let now = Instant::now();
    let a = key("a");
    let mut queue = queue_of(100, 1);
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
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
    let mut queue = queue_of(1 << 30, 4);
    for name in ["a1", "a2", "a3", "a4", "a5"] {
        queue.enqueue(&a, job(UploadTier::Index, name));
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
    let mut queue = queue_of(100, 1);
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
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
    let mut queue = queue_of(100, 1);
    let retry_after = now + Duration::from_secs(60);
    queue.defer(&a, retry_after);
    let superseded = queue.rebuild_lane(&a, [job(UploadTier::Index, "a1")]);
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
    let mut queue = queue_of(1 << 30, 1);
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
    queue.enqueue(&a, job(UploadTier::Index, "a2"));
    let a1 = queue.next(now).expect("a1");
    queue.reserve_range(a1.id, 5, now).expect("a range");
    assert!(queue.requeue(a1.id));
    assert_eq!(queue.in_use(), 0, "a requeue releases the ranges");
    assert!(!queue.requeue(a1.id), "a grant is requeued once");
    assert_eq!(queue.next(now).expect("a1 again").job, "a1");
}

#[test]
fn a_window_bounds_each_lane_independently() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = queue_of(1 << 30, 1);
    queue.set_windows(&a, LaneWindows::new(2, 1));
    for name in ["a1", "a2", "a3"] {
        queue.enqueue(&a, job(UploadTier::Index, name));
    }
    for name in ["b1", "b2"] {
        queue.enqueue(&b, job(UploadTier::Index, name));
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
    queue.set_windows(&a, LaneWindows::new(0, 0));
    queue.enqueue(&a, job(UploadTier::Index, "empty"));
    queue.enqueue(&a, job(UploadTier::Index, "empty2"));
    let first = queue.next(now).expect("one session");
    assert_eq!(
        queue.next(now),
        None,
        "the window is one, not zero or unbounded"
    );
    queue
        .reserve_range(first.id, 4096, now)
        .expect("a range clamped to the one-byte budget");
    assert_eq!(queue.in_use(), 1);
}

#[test]
fn rebuild_replaces_a_lane_from_its_own_server_truth_only() {
    let now = Instant::now();
    let (a, b) = (key("a"), key("b"));
    let mut queue = queue_of(100, 1);
    queue.set_windows(&a, LaneWindows::new(3, 1));
    queue.enqueue(&a, job(UploadTier::Index, "a-stale-1"));
    queue.enqueue(&a, job(UploadTier::Index, "a-stale-2"));
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    queue.defer(&a, now + Duration::from_secs(60));
    let in_flight = queue.next(now).expect("b1 (a is deferred)");
    assert_eq!(in_flight.job, "b1");
    queue
        .reserve_range(in_flight.id, 10, now)
        .expect("b1's range");

    let stale = queue
        .next(now + Duration::from_secs(60))
        .expect("a-stale-1");
    queue.reserve_range(stale.id, 30, now).expect("a's range");
    assert_eq!(queue.in_use(), 40);

    let superseded = queue.rebuild_lane(
        &a,
        [
            job(UploadTier::Original, "a-orig"),
            job(UploadTier::Index, "a-idx"),
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
    queue.enqueue(&b, job(UploadTier::Index, "b1"));
    queue.enqueue(&a, job(UploadTier::Index, "a1"));
    queue.enqueue(&b, job(UploadTier::Index, "b2"));
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
    let mut queue = queue_of(100, 2);
    queue.enqueue(&a, job(UploadTier::Index, "x-idx@a"));
    queue.enqueue(&a, job(UploadTier::Index, "y-idx@a"));
    queue.enqueue(&a, job(UploadTier::Original, "x-orig@a"));
    queue.enqueue(&b, job(UploadTier::Index, "x-idx@b"));
    queue.enqueue(&b, job(UploadTier::Preview, "y-prev@b"));
    let running = queue.next(now).expect("x-idx@a");
    assert_eq!(running.job, "x-idx@a");
    queue.reserve_range(running.id, 10, now).expect("a range");
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
    let mut queue = queue_of(100, 1);
    queue.enqueue(&a, job(UploadTier::Index, "x"));
    queue.enqueue(&a, job(UploadTier::Index, "y"));
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

// ─── Simulated clock ─────────────────────────────────────────────────────────
//
// A discrete-event model of a device driving the queue over real-looking links, in simulated
// time: nothing sleeps, so a minute of transfer is a microsecond of test. Every request is one
// range: it reserves its bytes, takes two round trips on its own connection (one connection's
// throughput is bounded by its round trip, however much bandwidth the path has), and on a lossy
// link is lost now and then, which costs a stall bound before the client gives up on it. A lost
// request fails its session, which asks the queue for a retry against the lane's budget.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

/// One server as the model sees it.
#[derive(Debug, Clone, Copy)]
struct Link {
    rtt: Duration,
    /// Requests lost per thousand; a thousand is a server that fails everything.
    loss_per_mille: u64,
    /// How long a lost request costs before the client gives up on it: the stall bound.
    lost_after: Duration,
}

/// A deterministic `xorshift64*` stream, so every run of the model draws the same losses.
struct Rng(u64);

impl Rng {
    fn chance(&mut self, per_mille: u64) -> bool {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d) % 1000 < per_mille
    }
}

/// A request in flight: when it returns, for which session and range, and whether it was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Arrival {
    at: Duration,
    seq: u64,
    grant: GrantId,
    range: RangeId,
    lost: bool,
}

#[derive(Debug, Default)]
struct Tally {
    requests: u32,
    retries: u32,
    completed: u32,
    failed: u32,
}

/// The device and its servers in simulated time.
struct Sim {
    queue: UploadQueue<usize>,
    base: Instant,
    now: Duration,
    rng: Rng,
    links: Vec<(ServerKey, Link)>,
    /// Which link each job runs against, and the ranges it has left to send.
    job_link: Vec<usize>,
    remaining: Vec<u32>,
    range_len: u64,
    /// Sessions in flight: their job, P, and ranges outstanding.
    sessions: BTreeMap<GrantId, (usize, u32, u32)>,
    arrivals: BinaryHeap<Reverse<Arrival>>,
    seq: u64,
    peak_in_use: u64,
    tally: Vec<Tally>,
}

impl Sim {
    /// `jobs` sessions of `ranges` ranges each on every link, under `windows`, sharing `budget`.
    fn new(
        links: &[Link],
        jobs: usize,
        ranges: u32,
        range_len: u64,
        budget: u64,
        windows: LaneWindows,
        retry_budget: u32,
    ) -> Self {
        let mut queue = UploadQueue::new(budget, Duration::from_secs(3600))
            .with_windows(windows)
            .with_retry_budget(retry_budget);
        let links: Vec<_> = links
            .iter()
            .enumerate()
            .map(|(at, link)| (key(&format!("server-{at}")), *link))
            .collect();
        let mut job_link = Vec::new();
        for (at, (server, _)) in links.iter().enumerate() {
            for _ in 0..jobs {
                queue.enqueue(server, QueuedJob::new(UploadTier::Original, job_link.len()));
                job_link.push(at);
            }
        }
        Self {
            queue,
            base: Instant::now(),
            now: Duration::ZERO,
            rng: Rng(0x9e37_79b9_7f4a_7c15),
            remaining: vec![ranges; job_link.len()],
            job_link,
            range_len,
            sessions: BTreeMap::new(),
            arrivals: BinaryHeap::new(),
            seq: 0,
            peak_in_use: 0,
            tally: links.iter().map(|_| Tally::default()).collect(),
            links,
        }
    }

    fn instant(&self) -> Instant {
        self.base + self.now
    }

    /// Admit every session the windows allow, then send every range their P and the byte
    /// budget allow.
    fn fill(&mut self) {
        let now = self.instant();
        while let Some(grant) = self.queue.next(now) {
            self.sessions.insert(grant.id, (grant.job, grant.ranges, 0));
        }
        let ids: Vec<GrantId> = self.sessions.keys().copied().collect();
        for id in ids {
            loop {
                let (job, window, outstanding) = self.sessions[&id];
                if outstanding >= window || outstanding >= self.remaining[job] {
                    break;
                }
                let Ok(range) = self.queue.reserve_range(id, self.range_len, now) else {
                    break;
                };
                assert!(
                    self.queue.in_use() <= self.queue.budget(),
                    "the byte budget is never exceeded"
                );
                self.peak_in_use = self.peak_in_use.max(self.queue.in_use());
                let link_at = self.job_link[job];
                let link = self.links[link_at].1;
                let lost = self.rng.chance(link.loss_per_mille);
                let took = if lost { link.lost_after } else { 2 * link.rtt };
                self.tally[link_at].requests += 1;
                self.seq += 1;
                self.arrivals.push(Reverse(Arrival {
                    at: self.now + took,
                    seq: self.seq,
                    grant: id,
                    range,
                    lost,
                }));
                self.sessions.get_mut(&id).expect("in flight").2 += 1;
            }
        }
    }

    /// Run until every job completed or failed; the simulated time it took.
    fn run(mut self) -> (Duration, Vec<Tally>, u64) {
        loop {
            self.fill();
            let Some(Reverse(arrival)) = self.arrivals.pop() else {
                break;
            };
            self.now = arrival.at;
            let now = self.instant();
            let Some(&(job, _, _)) = self.sessions.get(&arrival.grant) else {
                continue;
            };
            let link_at = self.job_link[job];
            if arrival.lost {
                self.sessions.remove(&arrival.grant);
                match self.queue.retry(arrival.grant) {
                    RetryAdmission::Requeued => self.tally[link_at].retries += 1,
                    RetryAdmission::BudgetSpent(_) => self.tally[link_at].failed += 1,
                    RetryAdmission::NotInFlight => unreachable!("the session was in flight"),
                }
                continue;
            }
            assert!(self.queue.release_range(arrival.range, now));
            self.queue
                .lane_retries(&self.links[link_at].0)
                .record_success();
            self.remaining[job] -= 1;
            let session = self.sessions.get_mut(&arrival.grant).expect("in flight");
            session.2 -= 1;
            if self.remaining[job] == 0 && session.2 == 0 {
                self.sessions.remove(&arrival.grant);
                assert!(self.queue.complete(arrival.grant, now));
                self.tally[link_at].completed += 1;
            }
        }
        assert!(self.queue.is_empty(), "every job completed or failed");
        assert_eq!(self.queue.in_use(), 0);
        (self.now, self.tally, self.peak_in_use)
    }
}

/// A 600 ms, 3 %-lossy path with a 5 s stall bound: the issue's high-latency mobile link.
const LOSSY_HIGH_RTT: Link = Link {
    rtt: Duration::from_millis(600),
    loss_per_mille: 30,
    lost_after: Duration::from_secs(5),
};

/// **W sessions in flight are W× the throughput** on a lossy, high-RTT link. Thirty-two assets
/// of seven two-round-trip sessions each (about 8 s per asset) take minutes one session at a
/// time; with W sessions open at once, each on its own connection, the same work finishes close
/// to W times sooner, because round trips, not bandwidth, bound each connection.
#[test]
fn w_sessions_in_flight_give_w_times_the_throughput_on_a_lossy_high_rtt_link() {
    const SESSIONS: usize = 32 * 7;
    let run = |sessions| {
        Sim::new(
            &[LOSSY_HIGH_RTT],
            SESSIONS,
            2,
            1 << 20,
            64 << 20,
            LaneWindows::new(sessions, 1),
            DEFAULT_LANE_RETRY_BUDGET,
        )
        .run()
    };
    let (sequential, tally, _) = run(1);
    assert_eq!(tally[0].completed as usize, SESSIONS);
    assert!(tally[0].retries > 0, "the link lost requests");
    assert!(
        sequential >= 4 * LOSSY_HIGH_RTT.rtt * SESSIONS as u32,
        "one session at a time costs every round trip: {sequential:?}"
    );
    for sessions in [2, 4, 8] {
        let (windowed, tally, _) = run(sessions);
        assert_eq!(tally[0].completed as usize, SESSIONS);
        let speedup = sequential.as_secs_f64() / windowed.as_secs_f64();
        assert!(
            speedup >= 0.8 * f64::from(sessions),
            "W={sessions}: {sequential:?} -> {windowed:?} is only {speedup:.2}x"
        );
    }
}

/// **The budget never exceeds its bound.** Three lanes of four sessions with four ranges each
/// could hold 48 ranges in flight; a budget of five ranges holds five, saturated, and every job
/// still finishes.
#[test]
fn the_byte_budget_bounds_every_range_in_flight_across_lanes() {
    let link = Link {
        rtt: Duration::from_millis(80),
        loss_per_mille: 0,
        lost_after: Duration::from_secs(5),
    };
    let range = 1 << 20;
    let (_, tally, peak) = Sim::new(
        &[link, link, link],
        10,
        6,
        range,
        5 * range,
        LaneWindows::new(4, 4),
        DEFAULT_LANE_RETRY_BUDGET,
    )
    .run();
    assert!(tally.iter().all(|lane| lane.completed == 10));
    assert_eq!(
        peak,
        5 * range,
        "the budget is saturated, and never exceeded"
    );
}

/// **A failing server costs at most its lane's retry budget**, and the healthy lane beside it
/// finishes every job. Without a lane budget each of the twenty jobs would take its own full
/// retry ladder.
#[test]
fn a_failing_server_costs_at_most_its_lanes_retry_budget() {
    let failing = Link {
        loss_per_mille: 1000,
        ..LOSSY_HIGH_RTT
    };
    let healthy = Link {
        loss_per_mille: 0,
        ..LOSSY_HIGH_RTT
    };
    let budget = 5;
    let (_, tally, _) = Sim::new(
        &[failing, healthy],
        20,
        3,
        1 << 20,
        64 << 20,
        LaneWindows::new(4, 1),
        budget,
    )
    .run();
    assert_eq!(
        tally[0].retries, budget,
        "the failing lane spent its budget"
    );
    assert_eq!(
        tally[0].failed, 20,
        "then every job failed rather than retried"
    );
    assert_eq!(
        tally[0].requests,
        20 + budget,
        "one request per job plus the budget"
    );
    assert_eq!(tally[1].completed, 20, "the healthy lane is untouched");
    assert_eq!(tally[1].retries, 0);
}
