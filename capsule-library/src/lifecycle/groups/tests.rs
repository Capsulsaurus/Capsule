use tempfile::TempDir;

use super::super::fast_workspace;

// ── Aggregated federated albums ──────────────────────────────────────

/// The op-path write half is **server-opaque**: the sealed group-assertion operation the
/// server would store carries no plaintext group facts — they live only inside the AMK
/// AEAD. A member re-derives the AMK and opens it; a non-member cannot.
#[test]
fn album_group_assertion_op_is_amk_sealed_and_opaque() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Alice trip").unwrap();

    let invite = ws
        .create_album_group(album, "Summer-Roadtrip-Secret", "alice.tld")
        .unwrap();
    let wire = ws.seal_album_group_op(&album).unwrap();

    // The server-stored ciphertext never exposes the group id or name in the clear.
    let gid_bytes = invite.group_id.as_bytes();
    assert!(
        !wire.windows(gid_bytes.len()).any(|w| w == gid_bytes),
        "group_id must not appear in the sealed op"
    );
    assert!(
        !wire
            .windows(b"Summer-Roadtrip-Secret".len())
            .any(|w| w == b"Summer-Roadtrip-Secret"),
        "group name must not appear in the sealed op"
    );

    // A member (holds the AMK) re-opens it to the exact reconciled assertion.
    let opened = ws.open_album_group_op(&album, &wire).unwrap();
    assert_eq!(opened.group_id, invite.group_id);
    assert_eq!(opened.name(), Some("Summer-Roadtrip-Secret"));
    assert!(opened.verify(&ws.user_ik_public()));

    // A non-member workspace (different account → different album keys) cannot open it: it
    // does not even hold the album, which is exactly why a stranger can never inject.
    let lib2 = TempDir::new().unwrap();
    let ws2 = fast_workspace(lib2.path());
    assert!(ws2.open_album_group_op(&album, &wire).is_err());
}

/// End-to-end composition + injection-refusal + partial-view + leave through the workspace:
/// two constituents assert the same group and compose into one capture-time-ordered view; a
/// third member album that never asserts the group is refused; an unreachable origin degrades
/// visibly without removal; leaving removes the assertion so the constituent drops out.
#[test]
fn album_group_view_composes_refuses_degrades_and_leaves() {
    let alice_lib = TempDir::new().unwrap();
    let bob_lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("p.jpg");
    std::fs::write(&img, b"\xFF\xD8\xFF group photo").unwrap();

    // Alice creates the group on her constituent and imports one asset.
    let mut alice = fast_workspace(alice_lib.path());
    let a_album = alice.create_album("Alice").unwrap();
    let a_other = alice.create_album("Alice-unrelated").unwrap(); // a member album NOT in the group.
    alice.import_asset(a_album, &img).unwrap();
    alice.import_asset(a_other, &img).unwrap();
    let invite = alice
        .create_album_group(a_album, "Trip", "alice.tld")
        .unwrap();

    // Bob joins the group on his own constituent (the invite rides as data) and imports.
    let mut bob = fast_workspace(bob_lib.path());
    let b_album = bob.create_album("Bob").unwrap();
    bob.import_asset(b_album, &img).unwrap();
    bob.join_album_group(b_album, &invite, "bob.tld").unwrap();

    // Alice reads Bob's assertion off the feed and folds it in (LWW name + hint union).
    let bob_assertion = bob.album_group_assertion(&b_album).unwrap().clone();
    assert!(alice.merge_album_group_assertion(b_album, &bob_assertion));

    // Alice renders the aggregate over her constituents + Bob's (a remote constituent she is
    // a member of). Bob's origin is momentarily unreachable → partial view.
    let a_c = alice.local_constituent(a_album, "alice.tld", true, false);
    let a_other_c = alice.local_constituent(a_other, "alice.tld", true, false); // no assertion.
    let bob_remote = capsule_core::federation::Constituent {
        album_id: b_album,
        home_server: "bob.tld".into(),
        is_member: true, // Alice holds Bob's AMK via the album invite.
        assertion: Some(bob_assertion),
        reachable: false, // bob.tld is down.
        blocked: false,
        assets: vec![capsule_core::federation::AggregateAsset {
            asset_id: bob.asset_ids()[0],
            capture_timestamp: 10,
        }],
    };
    let view = capsule_core::federation::render_aggregate(
        invite.group_id,
        &[a_c, a_other_c, bob_remote],
        None,
    );

    // Composition: Alice's group asset + Bob's asset; the unrelated album is refused.
    assert_eq!(
        view.entries.len(),
        2,
        "only the two group constituents compose"
    );
    assert!(view.entries.iter().any(|e| e.home_server == "alice.tld"));
    assert!(view.entries.iter().any(|e| e.home_server == "bob.tld"));
    // Partial view: Bob's origin is degraded but not removed.
    assert!(view.partial);
    assert!(
        view.entries
            .iter()
            .find(|e| e.home_server == "bob.tld")
            .unwrap()
            .degraded
    );
    // Group name converges by LWW across the two assertions.
    assert_eq!(view.group_name.as_deref(), Some("Trip"));

    // Leave: Alice removes her assertion → her constituent no longer admits into the group.
    assert!(alice.leave_album_group(a_album, false).unwrap());
    assert!(alice.album_group_assertion(&a_album).is_none());
    let a_c_after = alice.local_constituent(a_album, "alice.tld", true, false);
    assert!(
        !a_c_after.admits(invite.group_id),
        "a constituent with no assertion drops out of the group"
    );
}
