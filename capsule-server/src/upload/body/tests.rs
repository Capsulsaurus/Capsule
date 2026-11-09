use std::collections::VecDeque;
use std::time::Duration;

use super::*;

/// The default pace: 30 s stall bound, 1 KiB/s floor owed after a 30 s grace.
fn default_pace() -> ChunkPace {
    ChunkPace::default()
}

/// The millisecond before `at`.
fn just_before(at: Instant) -> Instant {
    at.checked_sub(Duration::from_millis(1))
        .expect("every instant here is past the clock's origin")
}

// ─── The meter: the cut decision over handed-in instants ────────────────────────────────────

#[test]
fn a_body_silent_for_the_stall_bound_is_cut_as_stalled() {
    let start = Instant::now();
    let mut meter = PaceMeter::start(default_pace(), start);
    meter.on_bytes(4096, start + Duration::from_secs(1));
    let quiet_until = start + Duration::from_secs(1) + default_pace().stall;
    assert_eq!(meter.verdict(just_before(quiet_until)), Ok(()));
    assert_eq!(meter.verdict(quiet_until), Err(Cut::Stalled));
    assert_eq!(meter.deadline(), quiet_until);
}

#[test]
fn a_body_that_never_sends_a_byte_is_cut_one_stall_bound_in() {
    let start = Instant::now();
    let meter = PaceMeter::start(default_pace(), start);
    assert_eq!(meter.deadline(), start + default_pace().stall);
    assert_eq!(
        meter.verdict(start + default_pace().stall),
        Err(Cut::Stalled)
    );
}

/// The acceptance case of #1041 at the decision layer: a body trickled at 2 KiB/s — twice the
/// floor — for ten minutes, five times the old 120 s total deadline, is never cut.
#[test]
fn a_body_trickled_above_the_floor_for_ten_minutes_is_never_cut() {
    let start = Instant::now();
    let mut meter = PaceMeter::start(default_pace(), start);
    for second in 1..=600 {
        let now = start + Duration::from_secs(second);
        assert_eq!(meter.verdict(now), Ok(()), "cut {second} s in");
        meter.on_bytes(2048, now);
    }
    assert_eq!(meter.received(), 600 * 2048);
}

/// A body that never stalls — a byte every twenty seconds — still falls behind the floor and is
/// cut as too slow, not as stalled.
#[test]
fn a_body_that_never_stalls_but_falls_below_the_floor_is_cut() {
    let start = Instant::now();
    let mut meter = PaceMeter::start(default_pace(), start);
    let mut cut = None;
    for tick in 1..=30 {
        let now = start + Duration::from_secs(20 * tick);
        if let Err(reason) = meter.verdict(now) {
            cut = Some((reason, tick));
            break;
        }
        meter.on_bytes(1, now);
    }
    let (reason, tick) = cut.expect("a byte every twenty seconds is far below 1 KiB/s");
    assert_eq!(reason, Cut::BelowMinimumRate);
    // The grace is one stall bound, so the floor is owed from 30 s: by 40 s, 10 KiB are owed and
    // two bytes arrived.
    assert_eq!(tick, 2, "cut at the first check past the grace");
}

/// The rate is owed only after the grace: a body that pauses for most of a stall bound and then
/// catches up is not cut for the pause.
#[test]
fn a_pause_inside_the_grace_is_not_cut_for_the_rate() {
    let start = Instant::now();
    let mut meter = PaceMeter::start(default_pace(), start);
    let pause = Duration::from_secs(29);
    assert_eq!(meter.verdict(start + pause), Ok(()));
    meter.on_bytes(64 * 1024, start + pause);
    assert_eq!(
        meter.verdict(start + pause + Duration::from_secs(20)),
        Ok(())
    );
}

#[test]
fn a_zero_floor_cuts_only_on_stall() {
    let start = Instant::now();
    let pace = ChunkPace::new(Duration::from_secs(30), 0);
    let mut meter = PaceMeter::start(pace, start);
    for tick in 1..=1000 {
        let now = start + Duration::from_secs(29 * tick);
        assert_eq!(meter.verdict(now), Ok(()), "cut {tick} ticks in");
        meter.on_bytes(1, now);
    }
}

#[test]
fn a_zero_stall_bound_is_cut_at_once() {
    let start = Instant::now();
    let meter = PaceMeter::start(ChunkPace::new(Duration::ZERO, 0), start);
    assert_eq!(meter.verdict(start), Err(Cut::Stalled));
}

/// The loop sleeps until the deadline and then asks for the verdict: the two never disagree, or
/// the loop would wake to an `Ok` and spin.
#[test]
fn the_verdict_is_cut_exactly_from_the_deadline() {
    let start = Instant::now();
    for (pace, bytes) in [
        (default_pace(), 0),
        (default_pace(), 5_000),
        (ChunkPace::new(Duration::from_secs(5), 3), 7),
        (ChunkPace::new(Duration::from_millis(300), 64 * 1024), 1536),
    ] {
        let mut meter = PaceMeter::start(pace, start);
        meter.on_bytes(bytes, start + Duration::from_millis(100));
        let deadline = meter.deadline();
        assert_eq!(
            meter.verdict(just_before(deadline)),
            Ok(()),
            "{pace:?} with {bytes} B"
        );
        assert!(meter.verdict(deadline).is_err(), "{pace:?} with {bytes} B");
    }
}

#[test]
fn zero_bytes_are_not_progress() {
    let start = Instant::now();
    let mut meter = PaceMeter::start(default_pace(), start);
    meter.on_bytes(0, start + Duration::from_secs(29));
    assert_eq!(
        meter.verdict(start + default_pace().stall),
        Err(Cut::Stalled)
    );
}

// ─── The read loop, over a scripted body on a paused clock ──────────────────────────────────

/// One step of a scripted body: what it yields, after how long.
enum Step {
    Data(Duration, &'static [u8]),
    Fail(Duration),
    /// Never yields again.
    Hang,
}

/// A body whose frames arrive on a schedule, read on tokio's clock.
struct Script(VecDeque<Step>);

impl Script {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self(steps.into_iter().collect())
    }
}

impl DataSource for Script {
    async fn next_data(&mut self) -> Option<Result<Bytes, TransportFailed>> {
        match self.0.pop_front()? {
            Step::Data(after, bytes) => {
                tokio::time::sleep(after).await;
                Some(Ok(Bytes::from_static(bytes)))
            }
            Step::Fail(after) => {
                tokio::time::sleep(after).await;
                Some(Err(TransportFailed))
            }
            Step::Hang => std::future::pending().await,
        }
    }
}

const PIECE: &[u8] = &[7; 2048];

/// The acceptance case of #1041 through the read loop: 2 KiB a second for ten minutes is
/// accepted whole, however far past the old 120 s total deadline it runs.
#[tokio::test(start_paused = true)]
async fn a_body_trickled_above_the_floor_past_the_old_deadline_is_read_whole() {
    let started = Instant::now();
    let script = Script::new((0..600).map(|_| Step::Data(Duration::from_secs(1), PIECE)));
    let read = read_paced(script, default_pace(), u64::MAX).await;
    let PacedRead::Whole(bytes) = read else {
        panic!("a steady trickle above the floor was cut: {read:?}");
    };
    assert_eq!(bytes.len(), 600 * PIECE.len());
    assert!(started.elapsed() >= Duration::from_secs(600));
}

#[tokio::test(start_paused = true)]
async fn a_body_that_stops_is_cut_one_stall_bound_after_its_last_byte() {
    let started = Instant::now();
    let script = Script::new([
        Step::Data(Duration::from_secs(1), PIECE),
        Step::Data(Duration::from_secs(1), PIECE),
        Step::Hang,
    ]);
    let read = read_paced(script, default_pace(), u64::MAX).await;
    assert_eq!(
        read,
        PacedRead::Cut {
            cut: Cut::Stalled,
            received: 2 * PIECE.len() as u64,
        }
    );
    let cut_at = Duration::from_secs(2) + default_pace().stall;
    assert!(
        started.elapsed() >= cut_at && started.elapsed() < cut_at + Duration::from_secs(1),
        "cut {:?} in, not one stall bound after the last byte",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn a_body_below_the_floor_is_cut_as_too_slow() {
    // A byte every ten seconds never stalls a 30 s bound, and is far below 1 KiB/s.
    let script = Script::new((0..100).map(|_| Step::Data(Duration::from_secs(10), b"x")));
    let read = read_paced(script, default_pace(), u64::MAX).await;
    assert!(
        matches!(
            read,
            PacedRead::Cut {
                cut: Cut::BelowMinimumRate,
                ..
            }
        ),
        "{read:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_zero_stall_bound_refuses_without_polling_the_body() {
    // A body that would fail at once if it were polled: the cut comes first.
    let script = Script::new([Step::Fail(Duration::ZERO)]);
    let read = read_paced(script, ChunkPace::new(Duration::ZERO, 0), u64::MAX).await;
    assert_eq!(
        read,
        PacedRead::Cut {
            cut: Cut::Stalled,
            received: 0,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_transport_failure_hands_back_what_arrived() {
    let script = Script::new([
        Step::Data(Duration::from_millis(10), b"cipher"),
        Step::Fail(Duration::from_millis(10)),
    ]);
    let read = read_paced(script, default_pace(), u64::MAX).await;
    assert_eq!(read, PacedRead::Failed(Bytes::from_static(b"cipher")));
}

#[tokio::test]
async fn a_body_within_the_cap_is_read_whole() {
    let read = read_paced(
        Body::from_bytes(Bytes::from_static(b"ciphertext")),
        default_pace(),
        64,
    )
    .await;
    assert_eq!(read, PacedRead::Whole(Bytes::from_static(b"ciphertext")));
}

#[tokio::test]
async fn a_body_past_the_cap_is_left_past_it_for_the_cap_to_refuse() {
    // Stopped on the frame that crossed the limit, not truncated to it: the body-size cap
    // inside must still see more than its limit to answer its own `413`.
    let read = read_paced(
        Body::from_bytes(Bytes::from_static(b"ciphertext")),
        default_pace(),
        4,
    )
    .await;
    let PacedRead::Whole(bytes) = read else {
        panic!("{read:?}");
    };
    assert!(bytes.len() > 4);
}

#[test]
fn the_body_rejections_keep_their_codes_through_the_conversion() {
    let unsupported = ChunkBodyRejection::from(OpaqueBodyRejection::UnsupportedMediaType {
        code: error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE,
    });
    assert!(matches!(
        unsupported,
        ChunkBodyRejection::UnsupportedMediaType { code } if code == error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE
    ));
    let unreadable = ChunkBodyRejection::from(OpaqueBodyRejection::Unreadable {
        code: error_codes::UPLOAD_MALFORMED_REQUEST,
    });
    assert!(matches!(
        unreadable,
        ChunkBodyRejection::Unreadable { code } if code == error_codes::UPLOAD_MALFORMED_REQUEST
    ));
}
