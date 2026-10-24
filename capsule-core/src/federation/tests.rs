use super::*;
use crate::ambient::testing::TestIds;

fn dev(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn asset(id: u128, capture: i64) -> AggregateAsset {
    AggregateAsset {
        asset_id: Uuid::from_u128(id),
        capture_timestamp: capture,
    }
}

/// A member constituent asserting `group_id`, reachable and unblocked, with the given assets.
fn member(
    album: Uuid,
    server: &str,
    group_id: Uuid,
    name: &str,
    ts: &str,
    device: Uuid,
    assets: Vec<AggregateAsset>,
) -> Constituent {
    Constituent {
        album_id: album,
        home_server: server.to_string(),
        is_member: true,
        assertion: Some(AlbumGroupAssertion::new(group_id, name, ts, device, vec![])),
        reachable: true,
        blocked: false,
        assets,
    }
}

// ── Assertion write / sign / merge ──────────────────────────────────────────────────

#[test]
fn assertion_signs_and_verifies_and_tamper_is_caught() {
    let ik = HybridSigningKey::generate();
    let gid = mint_group_id(TestIds::new(113).ids());
    let mut a = AlbumGroupAssertion::new(gid, "Trip", "2026-07-10T10:00:00Z", dev(1), vec![]);
    assert!(
        !a.verify(&ik.verifying_key()),
        "unsigned assertion does not verify"
    );
    a.sign(&ik);
    assert!(a.verify(&ik.verifying_key()));
    // A rename mutates the covered bytes: the stale signature must no longer verify.
    a.rename("Renamed", "2026-07-10T11:00:00Z", dev(1));
    assert!(!a.verify(&ik.verifying_key()));
    a.sign(&ik);
    assert!(a.verify(&ik.verifying_key()));
    assert_eq!(a.name(), Some("Renamed"));
}

/// **LWW rename convergence** (Validation bullet): two participants concurrently rename the
/// group on their own constituents; merging their assertions in *either* order converges to
/// the same name.
#[test]
fn rename_converges_by_lww_regardless_of_order() {
    let gid = mint_group_id(TestIds::new(114).ids());
    // Alice's assertion, renamed later-in-time than Bob's → Alice's name must win.
    let mut alice =
        AlbumGroupAssertion::new(gid, "Alice-name", "2026-07-10T12:00:00Z", dev(1), vec![]);
    let mut bob = AlbumGroupAssertion::new(gid, "Bob-name", "2026-07-10T11:00:00Z", dev(2), vec![]);

    let mut ab = alice.clone();
    assert!(ab.merge(&bob));
    let mut ba = bob.clone();
    assert!(ba.merge(&alice));
    assert_eq!(ab.name(), ba.name(), "merge converges regardless of order");
    assert_eq!(ab.name(), Some("Alice-name"), "later (ts,device) wins");

    // Idempotent: re-merging changes nothing.
    alice.merge(&bob);
    bob.merge(&alice);
    assert_eq!(alice.name(), Some("Alice-name"));
    assert_eq!(bob.name(), Some("Alice-name"));
}

#[test]
fn merge_unions_hints_and_refuses_a_different_group() {
    let gid = mint_group_id(TestIds::new(115).ids());
    let other_gid = mint_group_id(TestIds::new(116).ids());
    let mut a = AlbumGroupAssertion::new(
        gid,
        "G",
        "2026-07-10T10:00:00Z",
        dev(1),
        vec![MemberHint {
            album_id: dev(0xA),
            home_server: "alice.tld".into(),
        }],
    );
    let b = AlbumGroupAssertion::new(
        gid,
        "G",
        "2026-07-10T10:00:00Z",
        dev(2),
        vec![MemberHint {
            album_id: dev(0xB),
            home_server: "bob.tld".into(),
        }],
    );
    assert!(a.merge(&b));
    assert_eq!(a.member_hint.len(), 2, "hints unioned");
    // A merge across a different group id is refused (no cross-talk).
    let wrong = AlbumGroupAssertion::new(other_gid, "X", "2026-07-10T13:00:00Z", dev(3), vec![]);
    assert!(!a.merge(&wrong));
    assert_eq!(a.name(), Some("G"), "the wrong-group merge changed nothing");
}

/// The seal opacity is exercised end-to-end in `lifecycle.rs`; here we assert the signing
/// bytes *do* commit to the group facts (so a tamper is caught), while the sealed wire the
/// server stores carries them only inside the AEAD ciphertext.
#[test]
fn signing_bytes_commit_to_group_facts() {
    let gid = mint_group_id(TestIds::new(117).ids());
    let a = AlbumGroupAssertion::new(gid, "Secret-Trip", "2026-07-10T10:00:00Z", dev(1), vec![]);
    let bytes = a.signing_bytes();
    assert!(
        bytes.windows(11).any(|w| w == b"Secret-Trip"),
        "signing bytes commit to the group name"
    );
}

// ── Invite → assertion ──────────────────────────────────────────────────────────────

#[test]
fn invite_seeds_a_joiner_assertion_with_siblings() {
    let gid = mint_group_id(TestIds::new(118).ids());
    let creator_album = dev(0xC);
    let invite = AlbumGroupInvite {
        group_id: gid,
        group_name: "Reunion".into(),
        siblings: vec![MemberHint {
            album_id: creator_album,
            home_server: "creator.tld".into(),
        }],
    };
    let own = dev(0x99);
    let a = invite.to_assertion(own, "joiner.tld", "2026-07-10T10:00:00Z", dev(7));
    assert!(a.asserts(gid));
    assert_eq!(a.name(), Some("Reunion"));
    // Carries the creator sibling plus the joiner's own constituent as advisory hints.
    assert!(a.member_hint.iter().any(|h| h.album_id == creator_album));
    assert!(a.member_hint.iter().any(|h| h.album_id == own));
}

// ── Aggregate view rendering ─────────────────────────────────────────────────────────

/// **Composition** (Validation bullet): N constituents asserting one group compose into one
/// capture-time-ordered view; the group name is the LWW convergence across them.
#[test]
fn composition_merges_and_orders_by_capture_time() {
    let gid = mint_group_id(TestIds::new(119).ids());
    let alice = member(
        dev(1),
        "alice.tld",
        gid,
        "Alice-name",
        "2026-07-10T12:00:00Z",
        dev(1),
        vec![asset(0x10, 300), asset(0x11, 100)],
    );
    let bob = member(
        dev(2),
        "bob.tld",
        gid,
        "Bob-name",
        "2026-07-10T11:00:00Z",
        dev(2),
        vec![asset(0x20, 200)],
    );
    let view = render_aggregate(gid, &[alice, bob], None);

    // Every constituent's assets appear, ordered by capture time (asset_id tiebreak).
    let order: Vec<i64> = view.entries.iter().map(|e| e.capture_timestamp).collect();
    assert_eq!(order, vec![100, 200, 300]);
    assert_eq!(view.entries.len(), 3);
    // Both origins are present and reachable → not partial.
    assert_eq!(view.origins.len(), 2);
    assert!(!view.partial);
    // Group name is the LWW convergence (Alice's later stamp wins).
    assert_eq!(view.group_name.as_deref(), Some("Alice-name"));
    // Cover falls back to the newest included asset.
    assert_eq!(view.cover, Some(Uuid::from_u128(0x10)));

    // Idempotent / order-independent (grouping-convergence): re-rendering with the
    // constituents in the opposite order yields an identical view.
    let alice2 = member(
        dev(1),
        "alice.tld",
        gid,
        "Alice-name",
        "2026-07-10T12:00:00Z",
        dev(1),
        vec![asset(0x11, 100), asset(0x10, 300)],
    );
    let bob2 = member(
        dev(2),
        "bob.tld",
        gid,
        "Bob-name",
        "2026-07-10T11:00:00Z",
        dev(2),
        vec![asset(0x20, 200)],
    );
    assert_eq!(render_aggregate(gid, &[bob2, alice2], None), view);
}

/// **Injection refusal** (Validation bullet): an album that does NOT assert the group, and a
/// stranger's album (not a member), cannot be pulled into the view.
#[test]
fn injection_is_refused_by_the_inclusion_rule() {
    let gid = mint_group_id(TestIds::new(120).ids());
    let other_gid = mint_group_id(TestIds::new(121).ids());

    let real = member(
        dev(1),
        "alice.tld",
        gid,
        "G",
        "2026-07-10T10:00:00Z",
        dev(1),
        vec![asset(0x10, 100)],
    );
    // (a) A member album that asserts a DIFFERENT group — must not be pulled in.
    let wrong_group = member(
        dev(2),
        "bob.tld",
        other_gid,
        "Other",
        "2026-07-10T10:00:00Z",
        dev(2),
        vec![asset(0x20, 200)],
    );
    // (b) A stranger's album that asserts the RIGHT group but the user is NOT a member of
    //     (its assertion would be undecryptable in reality) — cannot inject itself.
    let stranger = Constituent {
        is_member: false,
        ..member(
            dev(3),
            "mallory.tld",
            gid,
            "G",
            "2026-07-10T10:00:00Z",
            dev(3),
            vec![asset(0x30, 50)],
        )
    };
    // (c) A member album with NO assertion at all — not part of the group.
    let no_assertion = Constituent {
        assertion: None,
        ..member(
            dev(4),
            "carol.tld",
            gid,
            "G",
            "2026-07-10T10:00:00Z",
            dev(4),
            vec![asset(0x40, 75)],
        )
    };

    let view = render_aggregate(gid, &[real, wrong_group, stranger, no_assertion], None);
    // Only the real member's single asset is admitted.
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].asset_id, Uuid::from_u128(0x10));
    assert_eq!(view.origins.len(), 1);
    assert_eq!(view.origins[0].home_server, "alice.tld");
}

/// **Partial view** (Validation bullet): one origin unreachable → its entries still render
/// (from the local index) flagged degraded, the aggregate is `partial`, and nothing is removed.
#[test]
fn partial_view_degrades_visibly_without_removal() {
    let gid = mint_group_id(TestIds::new(122).ids());
    let reachable = member(
        dev(1),
        "alice.tld",
        gid,
        "G",
        "2026-07-10T10:00:00Z",
        dev(1),
        vec![asset(0x10, 100)],
    );
    let down = Constituent {
        reachable: false,
        ..member(
            dev(2),
            "bob.tld",
            gid,
            "G",
            "2026-07-10T10:00:00Z",
            dev(2),
            vec![asset(0x20, 200)],
        )
    };
    let view = render_aggregate(gid, &[reachable, down], None);

    // Both assets still present — the unreachable origin's entry is not removed.
    assert_eq!(view.entries.len(), 2);
    let bob_entry = view
        .entries
        .iter()
        .find(|e| e.home_server == "bob.tld")
        .expect("bob's entry survives");
    assert!(
        bob_entry.degraded,
        "the unreachable origin's entry is flagged degraded"
    );
    let alice_entry = view
        .entries
        .iter()
        .find(|e| e.home_server == "alice.tld")
        .unwrap();
    assert!(!alice_entry.degraded);
    // The aggregate reports itself partial, with a per-origin indicator.
    assert!(view.partial);
    let bob_origin = view
        .origins
        .iter()
        .find(|o| o.home_server == "bob.tld")
        .unwrap();
    assert!(!bob_origin.reachable);
}

/// **Per-origin moderation drop**: blocking a server drops its constituent from the viewer's
/// aggregate, independently of the others (which are unaffected).
#[test]
fn moderation_drops_a_single_origin() {
    let gid = mint_group_id(TestIds::new(123).ids());
    let alice = member(
        dev(1),
        "alice.tld",
        gid,
        "G",
        "2026-07-10T10:00:00Z",
        dev(1),
        vec![asset(0x10, 100)],
    );
    let blocked = Constituent {
        blocked: true,
        ..member(
            dev(2),
            "spam.tld",
            gid,
            "G",
            "2026-07-10T10:00:00Z",
            dev(2),
            vec![asset(0x20, 200)],
        )
    };
    let view = render_aggregate(gid, &[alice, blocked], None);
    assert_eq!(view.entries.len(), 1, "the blocked origin is dropped");
    assert_eq!(view.origins.len(), 1);
    assert_eq!(view.origins[0].home_server, "alice.tld");
}

#[test]
fn cover_override_is_honored_only_when_it_names_an_included_asset() {
    let gid = mint_group_id(TestIds::new(124).ids());
    let alice = member(
        dev(1),
        "alice.tld",
        gid,
        "G",
        "2026-07-10T10:00:00Z",
        dev(1),
        vec![asset(0x10, 300), asset(0x11, 100)],
    );
    // Override to the older asset — honored because it is included.
    let view = render_aggregate(
        gid,
        std::slice::from_ref(&alice),
        Some(Uuid::from_u128(0x11)),
    );
    assert_eq!(view.cover, Some(Uuid::from_u128(0x11)));
    // An override that names an asset NOT in the aggregate falls back to the newest.
    let view = render_aggregate(gid, &[alice], Some(Uuid::from_u128(0xDEAD)));
    assert_eq!(view.cover, Some(Uuid::from_u128(0x10)));
}

#[test]
fn empty_aggregate_is_well_formed() {
    let gid = mint_group_id(TestIds::new(125).ids());
    let view = render_aggregate(gid, &[], None);
    assert!(view.entries.is_empty());
    assert!(view.origins.is_empty());
    assert_eq!(view.cover, None);
    assert_eq!(view.group_name, None);
    assert!(!view.partial);
}
