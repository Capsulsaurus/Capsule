use super::*;

fn device(id: &str, cohort: Option<&str>, created: i64, last: i64, current: bool) -> DeviceEntry {
    DeviceEntry {
        id: id.to_string(),
        created_at: created,
        last_active_at: last,
        user_agent: None,
        ip_address: None,
        is_current: current,
        cohort_hash: cohort.map(str::to_string),
        device_id: None,
    }
}

/// The same fixture, but for a client that asserted a hardware device id.
fn device_with_id(
    id: &str,
    device_id: &str,
    cohort: Option<&str>,
    created: i64,
    last: i64,
    current: bool,
) -> DeviceEntry {
    DeviceEntry {
        device_id: Some(device_id.to_string()),
        ..device(id, cohort, created, last, current)
    }
}

#[test]
fn reinstall_two_sessions_one_cohort_group_previously_used() {
    // A reinstall: a new device_id (new session) with the SAME cohort hash. The
    // two sessions must collapse into ONE group in the "previously used" state.
    let listing = SessionListing {
        devices: vec![
            device("sess-old", Some("cohortA"), 100, 150, false),
            device("sess-new", Some("cohortA"), 200, 250, true),
        ],
        cohorts: vec![CohortMapEntry {
            cohort_hash: "cohortA".into(),
            first_seen: 100,
            last_seen: 250,
        }],
    };
    let view = DevicesView::from_listing(&listing);
    assert_eq!(view.groups.len(), 1, "one physical device ⇒ one group");
    let g = &view.groups[0];
    assert_eq!(g.cohort_hash.as_deref(), Some("cohortA"));
    assert_eq!(g.sessions.len(), 2);
    assert_eq!(g.recognition, Recognition::PreviouslyUsed);
    assert!(g.contains_current);
    // The current session asserts "this device".
    assert_eq!(g.label_key(), keys::LABEL_THIS_DEVICE);
    // The recognition itself (ignoring current-device badge) is "previously used".
    assert_eq!(g.recognition.label_key(), keys::LABEL_PREVIOUSLY_USED);
    assert_eq!(g.first_seen, 100);
    assert_eq!(g.last_seen, 250);
}

#[test]
fn durable_map_outlives_session_previously_used_from_single_session() {
    // Only one live session, but the durable map remembers an earlier (expired)
    // one via an older first_seen ⇒ "previously used".
    let listing = SessionListing {
        devices: vec![device("sess-1", Some("cohortB"), 500, 550, true)],
        cohorts: vec![CohortMapEntry {
            cohort_hash: "cohortB".into(),
            first_seen: 100, // older than the live session's creation
            last_seen: 550,
        }],
    };
    let view = DevicesView::from_listing(&listing);
    let g = view.current().unwrap();
    assert_eq!(g.recognition, Recognition::PreviouslyUsed);
    assert_eq!(g.first_seen, 100);
}

#[test]
fn fresh_single_session_is_new() {
    let listing = SessionListing {
        devices: vec![device("sess-1", Some("cohortC"), 100, 120, true)],
        cohorts: vec![CohortMapEntry {
            cohort_hash: "cohortC".into(),
            first_seen: 100,
            last_seen: 120,
        }],
    };
    let view = DevicesView::from_listing(&listing);
    let g = view.current().unwrap();
    assert_eq!(g.recognition, Recognition::New);
    assert_eq!(g.label_key(), keys::LABEL_THIS_DEVICE); // current badge
    assert_eq!(g.recognition.label_key(), keys::LABEL_NEW_DEVICE);
}

#[test]
fn cohortless_sessions_stand_alone() {
    let listing = SessionListing {
        devices: vec![
            device("no-cohort-1", None, 100, 110, true),
            device("no-cohort-2", None, 200, 210, false),
        ],
        cohorts: vec![],
    };
    let view = DevicesView::from_listing(&listing);
    assert_eq!(view.groups.len(), 2, "ungrouped sessions never merge");
    assert!(view.groups.iter().all(|g| g.cohort_hash.is_none()));
    // Current device sorts first.
    assert!(view.groups[0].contains_current);
}

#[test]
fn support_bundle_round_trips() {
    let listing = SessionListing {
        devices: vec![
            device("sess-old", Some("cohortA"), 100, 150, false),
            device("sess-new", Some("cohortA"), 200, 250, true),
        ],
        cohorts: vec![CohortMapEntry {
            cohort_hash: "cohortA".into(),
            first_seen: 100,
            last_seen: 250,
        }],
    };
    let view = DevicesView::from_listing(&listing);
    let bundle = view.current().unwrap().support_bundle();

    assert_eq!(bundle.cohort_hash.as_deref(), Some("cohortA"));
    assert_eq!(bundle.sessions.len(), 2);
    assert_eq!(bundle.sessions[0].session_id, "sess-old");
    assert_eq!(bundle.sessions[0].first_seen, 100);
    assert_eq!(bundle.sessions[0].last_seen, 150);

    // Serializable and round-trippable for attaching to a bug report.
    let json = serde_json::to_string(&bundle).unwrap();
    let back: SupportBundle = serde_json::from_str(&json).unwrap();
    assert_eq!(bundle, back);
    // `device_id` is `Option`, serialized with `skip_serializing_if` semantics: a
    // session whose client asserted none is omitted from the bundle, not rendered null.
    assert!(!json.contains("device_id"));
}

/// The authentication doc's support-bundle contract: each session line carries the
/// `(device_id, session_id)` **pair**. A listing that exposed only the session
/// id would leave `device_id` hard-coded `None` here, and the pair could never be reported.
#[test]
fn support_bundle_carries_the_device_id_session_id_pair() {
    let listing = SessionListing {
        devices: vec![
            device_with_id(
                "sess-old",
                "018f2b1c-0000-7000-8000-000000000001",
                Some("cohortA"),
                100,
                150,
                false,
            ),
            device_with_id(
                "sess-new",
                "018f2b1c-0000-7000-8000-000000000002",
                Some("cohortA"),
                200,
                250,
                true,
            ),
        ],
        cohorts: vec![CohortMapEntry {
            cohort_hash: "cohortA".into(),
            first_seen: 100,
            last_seen: 250,
        }],
    };
    let bundle = DevicesView::from_listing(&listing)
        .current()
        .unwrap()
        .support_bundle();

    let pairs: Vec<(Option<&str>, &str)> = bundle
        .sessions
        .iter()
        .map(|s| (s.device_id.as_deref(), s.session_id.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (Some("018f2b1c-0000-7000-8000-000000000001"), "sess-old"),
            (Some("018f2b1c-0000-7000-8000-000000000002"), "sess-new"),
        ]
    );

    // Two devices sharing one cohort still report distinct device ids — that is the
    // whole point of the pair: the cohort groups them, the device id separates them.
    assert_ne!(bundle.sessions[0].device_id, bundle.sessions[1].device_id);

    let json = serde_json::to_string(&bundle).unwrap();
    assert!(json.contains("device_id"));
    let back: SupportBundle = serde_json::from_str(&json).unwrap();
    assert_eq!(bundle, back);
}

#[test]
fn cohortless_support_bundle_omits_hash() {
    let listing = SessionListing {
        devices: vec![device("solo", None, 100, 110, true)],
        cohorts: vec![],
    };
    let view = DevicesView::from_listing(&listing);
    let bundle = view.current().unwrap().support_bundle();
    assert!(bundle.cohort_hash.is_none());
    let json = serde_json::to_string(&bundle).unwrap();
    assert!(!json.contains("cohort_hash"));
    let back: SupportBundle = serde_json::from_str(&json).unwrap();
    assert_eq!(bundle, back);
}
