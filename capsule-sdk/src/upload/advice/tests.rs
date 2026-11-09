use super::*;

const FRESH: Duration = Duration::ZERO;
/// The client's own windows in these cases.
const OWN_SESSIONS: u32 = 8;
const OWN_RANGES: u32 = 4;

/// What one header reads as, through every accessor, at receipt.
#[derive(Debug, PartialEq, Eq)]
struct Read {
    chunk: Option<u64>,
    sessions: u32,
    ranges: u32,
    ttl: Duration,
}

fn read(header: Option<&str>) -> Read {
    let advice = ServerAdvice::parse(header);
    Read {
        chunk: advice.chunk(FRESH),
        sessions: advice.sessions_window(OWN_SESSIONS, FRESH),
        ranges: advice.ranges_window(OWN_RANGES),
        ttl: advice.ttl(),
    }
}

fn expect(chunk: Option<u64>, sessions: u32, ranges: u32, ttl: u64) -> Read {
    Read {
        chunk,
        sessions,
        ranges,
        ttl: Duration::from_secs(ttl),
    }
}

#[test]
fn every_member_is_read_and_clamped_into_the_clients_bounds() {
    let cases: &[(&str, Option<&str>, Read)] = &[
        (
            "the server's canonical form",
            Some("chunk=262144, sessions=4, ranges=1, ttl=300"),
            expect(Some(262_144), 4, 1, 300),
        ),
        (
            "no optional whitespace",
            Some("chunk=1048576,sessions=2,ranges=2,ttl=60"),
            expect(Some(1_048_576), 2, 2, 60),
        ),
        (
            "a chunk off the 4 KiB grid is aligned down",
            Some("chunk=10000"),
            expect(Some(8192), OWN_SESSIONS, OWN_RANGES, 300),
        ),
        (
            "a chunk below the protocol minimum is raised to it",
            Some("chunk=1"),
            expect(Some(PROTOCOL_MIN_CHUNK), OWN_SESSIONS, OWN_RANGES, 300),
        ),
        (
            "a chunk past the protocol maximum is cut to it",
            Some("chunk=999999999999999"),
            expect(Some(PROTOCOL_MAX_CHUNK), OWN_SESSIONS, OWN_RANGES, 300),
        ),
        (
            "a negative chunk is the protocol minimum",
            Some("chunk=-4096"),
            expect(Some(PROTOCOL_MIN_CHUNK), OWN_SESSIONS, OWN_RANGES, 300),
        ),
        (
            "a window advice can only lower the client's own",
            Some("sessions=64, ranges=64"),
            expect(None, OWN_SESSIONS, OWN_RANGES, 300),
        ),
        (
            "a zero window is one, never a stall",
            Some("sessions=0, ranges=0"),
            expect(None, 1, 1, 300),
        ),
        (
            "a ttl past the compiled ceiling is cut to it",
            Some("chunk=8192, ttl=999999"),
            expect(Some(8192), OWN_SESSIONS, OWN_RANGES, 3600),
        ),
        (
            "parameters on a member are ignored",
            Some("chunk=8192;unit=bytes, sessions=2"),
            expect(Some(8192), 2, OWN_RANGES, 300),
        ),
        (
            "a repeated member's last value wins",
            Some("sessions=2, sessions=3"),
            expect(None, 3, OWN_RANGES, 300),
        ),
    ];
    for (case, header, want) in cases {
        assert_eq!(&read(*header), want, "{case}");
    }
}

#[test]
fn unknown_and_unusable_members_are_ignored() {
    let cases: &[(&str, &str, Read)] = &[
        (
            "an unknown member, which a newer server may add",
            "chunk=8192, burst=9, sessions=2",
            expect(Some(8192), 2, OWN_RANGES, 300),
        ),
        (
            "a decimal is not an integer",
            "chunk=8192.5, sessions=2",
            expect(None, 2, OWN_RANGES, 300),
        ),
        (
            "a bare key is a boolean, not an integer",
            "chunk, sessions=2",
            expect(None, 2, OWN_RANGES, 300),
        ),
        (
            "a string value is not an integer",
            r#"chunk="8192", sessions=2"#,
            expect(None, 2, OWN_RANGES, 300),
        ),
        (
            "a comma inside a string cannot smuggle a member in",
            r#"note="x, chunk=8192", sessions=2"#,
            expect(None, 2, OWN_RANGES, 300),
        ),
        (
            "a comma inside an inner list cannot smuggle a member in",
            "note=(a, chunk=8192), sessions=2",
            expect(None, 2, OWN_RANGES, 300),
        ),
        (
            "an uppercase key is not a key",
            "CHUNK=8192, sessions=2",
            expect(None, 2, OWN_RANGES, 300),
        ),
        (
            "more than fifteen digits is not an integer",
            "chunk=1234567890123456, sessions=2",
            expect(None, 2, OWN_RANGES, 300),
        ),
        (
            "an unusable ttl is the default",
            "chunk=8192, ttl=soon",
            expect(Some(8192), OWN_SESSIONS, OWN_RANGES, 300),
        ),
    ];
    for (case, header, want) in cases {
        assert_eq!(&read(Some(header)), want, "{case}");
    }
}

#[test]
fn absent_or_unreadable_advice_is_the_clients_own_defaults() {
    let defaults = Read {
        chunk: None,
        sessions: OWN_SESSIONS,
        ranges: OWN_RANGES,
        ttl: Duration::ZERO,
    };
    let oversized = format!("chunk=8192, {}", "x=1, ".repeat(400));
    let cases: &[(&str, Option<&str>)] = &[
        ("no header", None),
        ("an oversized header", Some(&oversized)),
    ];
    for (case, header) in cases {
        assert_eq!(read(*header), defaults, "{case}");
    }
    for (case, header) in [("an empty header", ""), ("garbage", ";;==,,\"")] {
        let read = read(Some(header));
        assert_eq!(read.chunk, None, "{case}");
        assert_eq!(read.sessions, OWN_SESSIONS, "{case}");
        assert_eq!(read.ranges, OWN_RANGES, "{case}");
    }
    assert_eq!(ServerAdvice::default(), ServerAdvice::absent());
}

#[test]
fn advice_older_than_its_ttl_is_dropped_except_ranges() {
    let advice = ServerAdvice::parse(Some("chunk=8192, sessions=2, ranges=1, ttl=60"));
    let cases = [
        ("at receipt", Duration::ZERO, true),
        ("a moment before the ttl", Duration::from_secs(59), true),
        ("at the ttl", Duration::from_secs(60), false),
        ("long after", Duration::from_secs(3600), false),
    ];
    for (case, age, fresh) in cases {
        assert_eq!(advice.is_fresh(age), fresh, "{case}");
        assert_eq!(advice.chunk(age), fresh.then_some(8192), "{case}");
        assert_eq!(
            advice.sessions_window(OWN_SESSIONS, age),
            if fresh { 2 } else { OWN_SESSIONS },
            "{case}"
        );
        assert_eq!(
            advice.ranges_window(OWN_RANGES),
            1,
            "{case}: a session's ranges is fixed for its life"
        );
    }
}

#[test]
fn a_zero_ttl_holds_for_no_time_at_all() {
    let advice = ServerAdvice::parse(Some("chunk=8192, ttl=0"));
    assert_eq!(advice.chunk(Duration::ZERO), None);
}

#[test]
fn a_session_expiry_is_read_as_seconds_remaining() {
    let cases: &[(&str, Option<&str>, Option<&str>, SessionExpiry)] = &[
        (
            "both present",
            Some("86400"),
            Some("3600"),
            SessionExpiry {
                expires_in: Some(Duration::from_hours(24)),
                idle_expires_in: Some(Duration::from_secs(3600)),
            },
        ),
        (
            "an idle expiry past the cap is cut back to it",
            Some("60"),
            Some("3600"),
            SessionExpiry {
                expires_in: Some(Duration::from_secs(60)),
                idle_expires_in: Some(Duration::from_secs(60)),
            },
        ),
        (
            "an older server sends neither",
            None,
            None,
            SessionExpiry::default(),
        ),
        (
            "a malformed value is absent",
            Some("-5"),
            Some("soon"),
            SessionExpiry::default(),
        ),
        (
            "one without the other",
            None,
            Some("120"),
            SessionExpiry {
                expires_in: None,
                idle_expires_in: Some(Duration::from_secs(120)),
            },
        ),
    ];
    for (case, cap, idle, want) in cases {
        assert_eq!(&SessionExpiry::parse(*cap, *idle), want, "{case}");
    }
}
