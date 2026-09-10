//! The registry's remaining records, and the rule a peer reads them by.

use jiff::{SignedDuration, Timestamp};

use super::revocation::{
    MAX_STALENESS, MAX_TOKEN_TTL, PublishedRevocations, RevocationVerdict, RevokedToken,
    check_revocation,
};
use super::{
    AnnouncementError, DEFAULT_ANNOUNCEMENT_WINDOW, DeprecationAnnouncement, ProtocolWindow,
    ServerInfo,
};

fn window() -> ProtocolWindow {
    ProtocolWindow {
        min: "2026-01-01".to_owned(),
        max: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
    }
}

fn info() -> ServerInfo {
    ServerInfo::new(
        "https://capsule.example",
        "https://capsule.example/v1",
        window(),
        vec![7u8; 32],
    )
}

fn at(days: i64) -> Timestamp {
    crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(days * 24))
}

#[test]
fn a_deployment_that_does_not_federate_publishes_no_federation_endpoint() {
    // Absence is the record saying so. A published endpoint that answers nothing invites every
    // peer to fail against it, and "federation is off" is a normal deployment.
    let plain = info();
    assert_eq!(plain.federation_url(), None);

    let federating = info().with_federation("https://capsule.example/federation");
    assert_eq!(
        federating.federation_url(),
        Some("https://capsule.example/federation")
    );
}

#[test]
fn a_deprecation_announced_inside_the_window_is_refused() {
    // The record's whole purpose is the notice period. A server could publish a well-formed
    // announcement for next Tuesday and be conforming to the shape while breaking the promise.
    let mut server = info();
    let announced_at = at(0);

    let error = server
        .announce(DeprecationAnnouncement {
            min_protocol_version: "2026-06-01".to_owned(),
            announced_at,
            cutoff: at(30),
            detail_url: None,
        })
        .expect_err("thirty days is inside the ninety-day window");

    assert!(matches!(
        error,
        AnnouncementError::InsideWindow {
            lead_days: 30,
            window_days: 90,
            ..
        }
    ));
    assert!(server.deprecations().is_empty());
}

#[test]
fn a_deprecation_announced_ahead_of_the_window_is_published() {
    let mut server = info();
    server
        .announce(DeprecationAnnouncement {
            min_protocol_version: "2026-06-01".to_owned(),
            announced_at: at(0),
            cutoff: at(120),
            detail_url: Some("https://capsule.example/deprecation".to_owned()),
        })
        .expect("one hundred and twenty days clears the window");

    assert_eq!(server.deprecations().len(), 1);
    assert_eq!(server.deprecations()[0].cutoff, at(120));
}

#[test]
fn the_announcement_window_is_deployment_configurable() {
    // Ninety days is a default, not the policy stated as a constant: schema-rules.md says
    // "deployment-configurable" and a deployment with one internal client is entitled to less.
    let mut server = info().with_announcement_window(SignedDuration::from_hours(7 * 24));
    server
        .announce(DeprecationAnnouncement {
            min_protocol_version: "2026-06-01".to_owned(),
            announced_at: at(0),
            cutoff: at(30),
            detail_url: None,
        })
        .expect("thirty days clears a seven-day window");

    assert_eq!(server.deprecations().len(), 1);
    assert_eq!(DEFAULT_ANNOUNCEMENT_WINDOW.as_hours(), 90 * 24);
}

#[test]
fn a_cutoff_in_the_past_is_refused_as_such() {
    // Distinguished from `InsideWindow` because the operator error is different: one is a date
    // typed too soon, the other is a date typed for last year.
    let mut server = info().with_announcement_window(SignedDuration::ZERO);
    let error = server
        .announce(DeprecationAnnouncement {
            min_protocol_version: "2026-06-01".to_owned(),
            announced_at: at(10),
            cutoff: at(5),
            detail_url: None,
        })
        .expect_err("a cutoff before its own announcement is refused");

    assert!(matches!(error, AnnouncementError::AlreadyPassed { .. }));
}

#[test]
fn a_listed_token_is_refused() {
    let now = at(0);
    let list = PublishedRevocations {
        generated_at: now,
        revoked: vec![RevokedToken {
            jti: "revoked-one".to_owned(),
            expires_at: crate::store::deadline(now, SignedDuration::from_hours(1)),
        }],
    };

    let verdict = check_revocation(
        &list,
        "revoked-one",
        crate::store::deadline(now, SignedDuration::from_hours(1)),
        now,
    );
    assert_eq!(verdict, RevocationVerdict::Revoked);
    assert!(!verdict.accepts());
}

#[test]
fn an_unlisted_token_is_honored_while_the_list_is_fresh() {
    let now = at(0);
    let list = PublishedRevocations {
        generated_at: now,
        revoked: Vec::new(),
    };

    let verdict = check_revocation(
        &list,
        "unlisted",
        crate::store::deadline(now, SignedDuration::from_hours(1)),
        crate::store::deadline(now, SignedDuration::from_mins(14)),
    );
    assert_eq!(verdict, RevocationVerdict::Honored);
    assert!(verdict.accepts());
}

#[test]
fn a_list_past_the_staleness_bound_stops_honoring_anything() {
    // The rule revocation depends on. Without it, revocation is defeated by making the list
    // unreachable — which is a capability any network position between two servers has.
    let now = at(0);
    let list = PublishedRevocations {
        generated_at: now,
        revoked: Vec::new(),
    };
    let later = crate::store::deadline(now, MAX_STALENESS + SignedDuration::from_secs(1));

    let verdict = check_revocation(
        &list,
        "unlisted",
        crate::store::deadline(now, SignedDuration::from_hours(2)),
        later,
    );
    assert_eq!(verdict, RevocationVerdict::Stale);
    assert!(!verdict.accepts());
}

#[test]
fn a_token_is_honored_right_up_to_the_staleness_bound() {
    // The 15 minutes are a *permitted* latency, not a margin to be conservative inside: a
    // verifier that refused at 14 minutes would multiply every peer's fetch rate for nothing.
    let now = at(0);
    let list = PublishedRevocations {
        generated_at: now,
        revoked: Vec::new(),
    };

    let verdict = check_revocation(
        &list,
        "unlisted",
        crate::store::deadline(now, SignedDuration::from_hours(2)),
        crate::store::deadline(now, MAX_STALENESS),
    );
    assert_eq!(verdict, RevocationVerdict::Honored);
}

#[test]
fn an_expired_token_is_refused_whatever_the_list_says() {
    // Checked before anything else: no list, fresh or stale, can rehabilitate a token past its
    // own `exp`, and reporting `Stale` for one would send a peer to refetch for no reason.
    let now = at(0);
    let stale = PublishedRevocations {
        generated_at: Timestamp::UNIX_EPOCH,
        revoked: Vec::new(),
    };
    let later = crate::store::deadline(now, SignedDuration::from_hours(48));

    let verdict = check_revocation(
        &stale,
        "unlisted",
        crate::store::deadline(now, MAX_TOKEN_TTL),
        later,
    );
    assert_eq!(verdict, RevocationVerdict::Expired);
}
