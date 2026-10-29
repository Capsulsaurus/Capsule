//! Discovery against a scripted responder: a [`Browser`] that replays timed answers on a fake
//! clock, so the window, withdrawals, re-resolution, address choice and the import's choice of
//! camera are proved without the real network. The live `mdns-sd` adapter is a field-for-field
//! conversion and is not exercised here.

use std::cell::Cell;
use std::collections::VecDeque;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::rc::Rc;

use super::*;

/// A fake clock the scripted responder advances.
#[derive(Clone)]
struct Clock {
    origin: Instant,
    elapsed: Rc<Cell<Duration>>,
}

impl Clock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            elapsed: Rc::new(Cell::new(Duration::ZERO)),
        }
    }

    fn now(&self) -> Instant {
        self.origin + self.elapsed.get()
    }

    fn advance_to(&self, at: Duration) {
        if at > self.elapsed.get() {
            self.elapsed.set(at);
        }
    }
}

/// A responder that answers with `script` (each event at its offset from the start), then
/// stays silent; or fails at an offset.
struct Scripted {
    clock: Clock,
    script: VecDeque<(Duration, Result<BrowseEvent, String>)>,
    waits: Vec<Duration>,
}

impl Scripted {
    fn new(clock: &Clock, script: Vec<(u64, BrowseEvent)>) -> Self {
        Self {
            clock: clock.clone(),
            script: script
                .into_iter()
                .map(|(ms, event)| (Duration::from_millis(ms), Ok(event)))
                .collect(),
            waits: Vec::new(),
        }
    }
}

impl Browser for Scripted {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<BrowseEvent>> {
        self.waits.push(timeout);
        let wake = self.clock.elapsed.get() + timeout;
        match self.script.front() {
            Some((at, _)) if *at <= wake => {
                let (at, event) = self.script.pop_front().expect("front exists");
                self.clock.advance_to(at);
                event.map(Some).map_err(|e| eyre!(e))
            }
            _ => {
                self.clock.advance_to(wake);
                Ok(None)
            }
        }
    }
}

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn v6(text: &str) -> IpAddr {
    IpAddr::V6(text.parse::<Ipv6Addr>().expect("an IPv6 literal"))
}

fn answer(name: &str, port: u16, addresses: Vec<IpAddr>) -> BrowseEvent {
    BrowseEvent::Resolved(ServiceAnswer {
        fullname: format!("{name}.{SERVICE_TYPE}"),
        host: format!("{}.local.", name.to_lowercase().replace(' ', "-")),
        port,
        addresses,
    })
}

fn removed(name: &str) -> BrowseEvent {
    BrowseEvent::Removed {
        fullname: format!("{name}.{SERVICE_TYPE}"),
    }
}

fn camera(name: &str, address: IpAddr, port: u16) -> DiscoveredCamera {
    DiscoveredCamera {
        name: name.to_owned(),
        host: format!("{}.local.", name.to_lowercase().replace(' ', "-")),
        address,
        port,
    }
}

const WINDOW: Duration = Duration::from_secs(3);

fn run(script: Vec<(u64, BrowseEvent)>) -> (Vec<DiscoveredCamera>, Scripted) {
    let clock = Clock::new();
    let mut responder = Scripted::new(&clock, script);
    let found = browse(&mut responder, WINDOW, || clock.now()).expect("browse");
    (found, responder)
}

#[test]
fn every_camera_answering_inside_the_window_is_offered_by_name() {
    let (found, _) = run(vec![
        (100, answer("Zeta Body", 15740, vec![v4(192, 168, 1, 20)])),
        (900, answer("EOS R5", 15740, vec![v4(192, 168, 1, 10)])),
    ]);
    assert_eq!(
        found,
        vec![
            camera("EOS R5", v4(192, 168, 1, 10), 15740),
            camera("Zeta Body", v4(192, 168, 1, 20), 15740),
        ]
    );
}

#[test]
fn an_answer_after_the_window_is_not_offered_and_the_wait_is_bounded() {
    let (found, responder) = run(vec![
        (2_999, answer("EOS R5", 15740, vec![v4(10, 0, 0, 2)])),
        (3_001, answer("Late", 15740, vec![v4(10, 0, 0, 3)])),
    ]);
    assert_eq!(found, vec![camera("EOS R5", v4(10, 0, 0, 2), 15740)]);
    assert!(
        responder.waits.iter().all(|wait| *wait <= WINDOW),
        "no single wait outlasts the window: {:?}",
        responder.waits
    );
    assert_eq!(
        responder.waits.last(),
        Some(&Duration::from_millis(1)),
        "the last wait is what was left of the window"
    );
}

#[test]
fn a_silent_network_is_an_empty_answer_after_one_wait() {
    let (found, responder) = run(Vec::new());
    assert!(found.is_empty());
    assert_eq!(responder.waits, vec![WINDOW]);
}

#[test]
fn a_withdrawn_camera_is_not_offered() {
    let (found, _) = run(vec![
        (100, answer("EOS R5", 15740, vec![v4(10, 0, 0, 2)])),
        (200, answer("Alpha", 15740, vec![v4(10, 0, 0, 3)])),
        (500, removed("EOS R5")),
    ]);
    assert_eq!(found, vec![camera("Alpha", v4(10, 0, 0, 3), 15740)]);
}

#[test]
fn a_camera_resolved_twice_is_offered_once_with_its_latest_records() {
    let (found, _) = run(vec![
        (100, answer("EOS R5", 15740, vec![v4(10, 0, 0, 2)])),
        (800, answer("EOS R5", 15741, vec![v4(10, 0, 0, 9)])),
    ]);
    assert_eq!(found, vec![camera("EOS R5", v4(10, 0, 0, 9), 15741)]);
}

#[test]
fn a_camera_whose_only_address_is_link_local_ipv6_is_not_offered() {
    let (found, _) = run(vec![
        (100, answer("Scoped", 15740, vec![v6("fe80::1")])),
        (200, answer("Empty", 15740, Vec::new())),
        (
            300,
            answer("Routable", 15740, vec![v6("fe80::2"), v6("2001:db8::5")]),
        ),
    ]);
    assert_eq!(found, vec![camera("Routable", v6("2001:db8::5"), 15740)]);
}

#[test]
fn a_camera_that_loses_its_last_address_is_withdrawn() {
    let (found, _) = run(vec![
        (100, answer("EOS R5", 15740, vec![v4(10, 0, 0, 2)])),
        (200, answer("EOS R5", 15740, vec![v6("fe80::1")])),
    ]);
    assert!(found.is_empty());
}

#[test]
fn a_failed_browse_fails_the_window() {
    let clock = Clock::new();
    let mut responder = Scripted::new(&clock, Vec::new());
    responder.script.push_back((
        Duration::from_millis(50),
        Err("the mDNS daemon ended".to_owned()),
    ));
    let error = browse(&mut responder, WINDOW, || clock.now()).expect_err("the browse failed");
    assert!(error.to_string().contains("daemon ended"), "{error}");
}

#[test]
fn ipv4_is_preferred_and_the_lowest_of_each_family_wins() {
    assert_eq!(
        connectable_address(&[v6("2001:db8::1"), v4(10, 0, 0, 9), v4(10, 0, 0, 2)]),
        Some(v4(10, 0, 0, 2))
    );
    assert_eq!(
        connectable_address(&[v6("2001:db8::9"), v6("2001:db8::1")]),
        Some(v6("2001:db8::1"))
    );
    assert_eq!(connectable_address(&[v6("fe80::1"), v6("::")]), None);
    assert_eq!(connectable_address(&[]), None);
}

#[test]
fn the_instance_name_drops_the_service_type() {
    assert_eq!(instance_name("EOS R5._ptp._tcp.local."), "EOS R5");
    assert_eq!(instance_name("a.b._ptp._tcp.local."), "a.b");
    assert_eq!(instance_name("odd-name"), "odd-name");
}

#[test]
fn an_import_takes_the_only_camera() {
    let only = camera("EOS R5", v4(10, 0, 0, 2), 15740);
    assert_eq!(
        choose(vec![only.clone()], WINDOW).expect("one camera"),
        only
    );
    assert_eq!(
        only.socket_addr(),
        "10.0.0.2:15740".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn an_import_with_no_camera_is_refused_and_points_at_address() {
    let error = choose(Vec::new(), WINDOW).expect_err("nothing to choose");
    let message = error.to_string();
    assert!(message.contains("within 3 s"), "{message}");
    assert!(message.contains("--address"), "{message}");
}

#[test]
fn an_import_never_guesses_between_cameras() {
    let error = choose(
        vec![
            camera("Alpha", v4(10, 0, 0, 3), 15740),
            camera("EOS R5", v4(10, 0, 0, 2), 15740),
            camera("Zeta", v6("2001:db8::5"), 15740),
        ],
        WINDOW,
    )
    .expect_err("three cameras");
    let message = error.to_string();
    assert!(message.starts_with("3 cameras answered"), "{message}");
    for line in [
        "  Alpha at 10.0.0.3:15740",
        "  EOS R5 at 10.0.0.2:15740",
        "  Zeta at [2001:db8::5]:15740",
    ] {
        assert!(message.contains(line), "{line} missing from:\n{message}");
    }
}

#[test]
fn the_summary_names_each_camera_or_the_empty_window() {
    let report = DiscoveryReport {
        service_type: SERVICE_TYPE,
        window_seconds: 3,
        cameras: vec![camera("EOS R5", v4(10, 0, 0, 2), 15740)],
    };
    assert_eq!(
        summary_lines(&report),
        vec![
            "1 camera(s) answered within 3 s:".to_owned(),
            "  EOS R5 (eos-r5.local.) at 10.0.0.2:15740".to_owned(),
        ]
    );
    let empty = DiscoveryReport {
        cameras: Vec::new(),
        ..report
    };
    assert_eq!(
        summary_lines(&empty),
        vec!["No camera answered on the network within 3 s.".to_owned()]
    );
}

#[test]
fn the_report_serializes_as_the_discover_json_contract() {
    let report = DiscoveryReport {
        service_type: SERVICE_TYPE,
        window_seconds: 3,
        cameras: vec![camera("EOS R5", v4(10, 0, 0, 2), 15740)],
    };
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::json!({
            "service_type": "_ptp._tcp.local.",
            "window_seconds": 3,
            "cameras": [{
                "name": "EOS R5",
                "host": "eos-r5.local.",
                "address": "10.0.0.2",
                "port": 15740,
            }],
        })
    );
}
