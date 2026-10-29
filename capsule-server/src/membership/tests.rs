use super::*;

fn record(version: u64, epoch: u64, document: &[u8]) -> RosterRecord {
    RosterRecord {
        album_id: AlbumId::new("album"),
        roster_version: version,
        amk_epoch: epoch,
        attested_by_device: Uuid::from_u128(1),
        received_at: Timestamp::UNIX_EPOCH,
        document: document.to_vec(),
    }
}

/// [`precheck`] on an album with no live members, so no roster can be a removal.
fn decide(held: Option<&RosterRecord>, incoming: &RosterRecord) -> Option<RosterOutcome> {
    precheck(held, &[], incoming, &[])
}

fn member(name: &str) -> (UserId, MemberRole) {
    (UserId::new(name), MemberRole::Reader)
}

#[test]
fn a_removal_at_the_held_epoch_is_refused_and_one_that_advances_it_is_not() {
    let held = record(1, 3, b"a");
    let live = [UserId::new("bob"), UserId::new("carol")];
    assert_eq!(
        precheck(Some(&held), &live, &record(2, 3, b"b"), &[member("bob")]),
        Some(RosterOutcome::EpochNotAdvanced {
            current_version: 1,
            stored: 3,
            removed: 1,
        })
    );
    assert_eq!(
        precheck(Some(&held), &live, &record(2, 3, b"b"), &[]),
        Some(RosterOutcome::EpochNotAdvanced {
            current_version: 1,
            stored: 3,
            removed: 2,
        })
    );
    assert_eq!(
        precheck(Some(&held), &live, &record(2, 4, b"b"), &[member("bob")]),
        None,
        "a removal that rides a rotation is applied"
    );
}

#[test]
fn an_unchanged_or_growing_roster_at_the_held_epoch_is_applied() {
    let held = record(1, 3, b"a");
    let live = [UserId::new("bob")];
    // A role change, and an addition: neither drops anyone, so neither needs a rotation.
    assert_eq!(
        precheck(
            Some(&held),
            &live,
            &record(2, 3, b"b"),
            &[(UserId::new("bob"), MemberRole::Writer)]
        ),
        None
    );
    assert_eq!(
        precheck(
            Some(&held),
            &live,
            &record(2, 3, b"b"),
            &[member("bob"), member("dave")]
        ),
        None
    );
}

#[test]
fn the_older_refusals_are_decided_before_the_removal_rule() {
    let held = record(2, 3, b"a");
    let live = [UserId::new("bob")];
    // A replay of the held bytes is a replay, whoever the caller's list omits.
    assert_eq!(
        precheck(Some(&held), &live, &record(2, 3, b"a"), &[]),
        Some(RosterOutcome::Replayed(held.clone()))
    );
    // A behind client is stale, and a regression is a regression, not a non-advance.
    assert_eq!(
        precheck(Some(&held), &live, &record(1, 3, b"b"), &[]),
        Some(RosterOutcome::Stale { current_version: 2 })
    );
    assert_eq!(
        precheck(Some(&held), &live, &record(3, 2, b"b"), &[]),
        Some(RosterOutcome::EpochRegressed {
            current_version: 2,
            stored: 3
        })
    );
}

#[test]
fn the_first_roster_is_always_applied() {
    assert_eq!(decide(None, &record(1, 1, b"a")), None);
    // Even a version 0 or an epoch 0: monotonicity is against the *held* roster only.
    assert_eq!(decide(None, &record(0, 0, b"a")), None);
}

#[test]
fn the_same_version_is_a_replay_on_identical_bytes_and_stale_otherwise() {
    let held = record(1, 1, b"a");
    assert_eq!(
        decide(Some(&held), &record(1, 1, b"a")),
        Some(RosterOutcome::Replayed(held.clone()))
    );
    assert_eq!(
        decide(Some(&held), &record(1, 1, b"b")),
        Some(RosterOutcome::Stale { current_version: 1 })
    );
}

#[test]
fn a_lower_version_is_stale_whatever_its_bytes_or_epoch() {
    let held = record(2, 2, b"a");
    assert_eq!(
        decide(Some(&held), &record(1, 9, b"a")),
        Some(RosterOutcome::Stale { current_version: 2 })
    );
}

#[test]
fn a_newer_version_with_a_lower_epoch_is_a_regression() {
    let held = record(1, 3, b"a");
    assert_eq!(
        decide(Some(&held), &record(2, 2, b"b")),
        Some(RosterOutcome::EpochRegressed {
            current_version: 1,
            stored: 3
        })
    );
    // Equal is fine: a roster may change without a key rotation.
    assert_eq!(decide(Some(&held), &record(2, 3, b"b")), None);
    assert_eq!(decide(Some(&held), &record(2, 4, b"b")), None);
}

#[test]
fn a_version_past_the_window_is_a_leap_whatever_it_holds() {
    // The wedge: one publish at the ceiling of the type, which nothing could ever supersede.
    let held = record(2, 1, b"a");
    assert_eq!(
        decide(Some(&held), &record(u64::MAX, 1, b"b")),
        Some(RosterOutcome::VersionLeap {
            current_version: 2,
            max_version: 2 + MAX_ROSTER_VERSION_STEP,
        })
    );
    // The edge of the window is inside it; one past it is not.
    assert_eq!(
        decide(Some(&held), &record(2 + MAX_ROSTER_VERSION_STEP, 1, b"b")),
        None
    );
    assert_eq!(
        decide(Some(&held), &record(3 + MAX_ROSTER_VERSION_STEP, 1, b"b")),
        Some(RosterOutcome::VersionLeap {
            current_version: 2,
            max_version: 2 + MAX_ROSTER_VERSION_STEP,
        })
    );
}

#[test]
fn the_window_binds_the_first_roster_too_against_a_held_version_of_zero() {
    assert_eq!(
        decide(None, &record(MAX_ROSTER_VERSION_STEP, 0, b"a")),
        None
    );
    assert_eq!(
        decide(None, &record(u64::MAX, 0, b"a")),
        Some(RosterOutcome::VersionLeap {
            current_version: 0,
            max_version: MAX_ROSTER_VERSION_STEP,
        })
    );
}

#[test]
fn nothing_above_the_storable_ceiling_is_ever_accepted() {
    // The ceiling is what a BIGINT holds and what a generated client can decode. It binds
    // the first roster and every later one, and it is the reason a held version above it
    // cannot exist.
    assert_eq!(
        decide(None, &record(MAX_ROSTER_VERSION + 1, 0, b"a")),
        Some(RosterOutcome::VersionLeap {
            current_version: 0,
            max_version: MAX_ROSTER_VERSION_STEP,
        })
    );
    let held = record(MAX_ROSTER_VERSION - 1, 1, b"a");
    assert_eq!(
        decide(Some(&held), &record(MAX_ROSTER_VERSION, 1, b"b")),
        None
    );
    assert_eq!(
        decide(Some(&held), &record(MAX_ROSTER_VERSION + 1, 1, b"b")),
        Some(RosterOutcome::VersionLeap {
            current_version: MAX_ROSTER_VERSION - 1,
            // Clamped: `held + 16` would be past what an adapter can store.
            max_version: MAX_ROSTER_VERSION,
        })
    );
}

#[test]
fn the_window_never_names_a_ceiling_a_client_could_not_decode() {
    // Every `max_version` the route can render is inside the range the generated clients
    // lower integers into (`i64`), whatever the held version is — including the values that
    // cannot occur, so the property does not depend on the ceiling being enforced elsewhere.
    for held_version in [0, 1, MAX_ROSTER_VERSION - 1, MAX_ROSTER_VERSION, u64::MAX] {
        let held = record(held_version, 1, b"a");
        let Some(RosterOutcome::VersionLeap {
            current_version,
            max_version,
        }) = decide(Some(&held), &record(u64::MAX, 1, b"b"))
        else {
            continue;
        };
        assert!(max_version <= MAX_ROSTER_VERSION, "{max_version}");
        assert!(i64::try_from(max_version).is_ok(), "{max_version}");
        assert_eq!(current_version, held_version);
    }
}

#[test]
fn a_held_version_at_the_top_of_the_type_is_unreachable_and_refuses_everything() {
    // The degenerate case the clamp exists to make unreachable, pinned at the exact
    // boundary rather than near it. A store that somehow held `u64::MAX` would refuse every
    // publish — including `u64::MAX` itself, which is *not* treated as a replay, because the
    // ceiling is decided before the version comparison. That state cannot arise: no roster
    // above `MAX_ROSTER_VERSION` is ever applied (the case above), so no held record can
    // carry one. The behaviour is stated here so it is a decision rather than an accident.
    let held = record(u64::MAX, 1, b"a");
    let refusal = Some(RosterOutcome::VersionLeap {
        current_version: u64::MAX,
        max_version: MAX_ROSTER_VERSION,
    });
    assert_eq!(decide(Some(&held), &record(u64::MAX, 1, b"a")), refusal);
    assert_eq!(decide(Some(&held), &record(u64::MAX, 1, b"b")), refusal);
    // And a version inside the storable range is stale against it, not a leap.
    assert_eq!(
        decide(Some(&held), &record(5, 1, b"b")),
        Some(RosterOutcome::Stale {
            current_version: u64::MAX
        })
    );
}

#[test]
fn the_role_tokens_round_trip_and_nothing_else_parses() {
    for role in [MemberRole::Reader, MemberRole::Writer] {
        assert_eq!(role_from_token(role_token(role)), Some(role));
    }
    assert_eq!(role_from_token("admin"), None);
}

#[test]
fn a_roster_records_debug_shows_the_documents_length_not_its_bytes() {
    let rendered = format!("{:?}", record(1, 1, b"secret-bytes"));
    assert!(rendered.contains("document_len: 12"), "{rendered}");
    assert!(!rendered.contains("secret"), "{rendered}");
}
