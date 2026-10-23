//! The CLI's peer state: the inbox, the directory floor, the ledger file and the boot clock.
//! The flows over a real server are E2E case 15.

use super::*;

/// A scratch directory removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-cli-peer-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("a scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn state() -> (Scratch, PeerState) {
    let dir = Scratch::new();
    let state = PeerState::new(dir.path().join("peer"));
    (dir, state)
}

#[test]
fn the_inbox_holds_each_hand_off_under_its_address_until_removed() {
    let (_dir, state) = state();
    let inbox = state.inbox();
    assert!(
        inbox
            .addresses()
            .expect("an absent inbox is empty")
            .is_empty()
    );
    let a = "aa".repeat(32);
    let b = "bb".repeat(32);
    inbox.persist(&b, b"second").expect("persists");
    inbox.persist(&a, b"first").expect("persists");
    assert_eq!(
        inbox.addresses().expect("lists"),
        vec![a.clone(), b.clone()]
    );
    assert_eq!(inbox.load(&a).expect("reads"), Some(b"first".to_vec()));
    inbox.remove(&a).expect("removes");
    inbox.remove(&a).expect("removing twice is not an error");
    assert_eq!(inbox.load(&a).expect("reads"), None);
    assert_eq!(inbox.addresses().expect("lists"), vec![b]);
}

#[test]
fn the_inbox_refuses_anything_but_a_content_address_as_a_name() {
    let (_dir, state) = state();
    let inbox = state.inbox();
    for name in ["", "../escape", "a/b", "not hex"] {
        assert!(inbox.persist(name, b"x").is_err(), "{name:?}");
        assert!(inbox.load(name).is_err(), "{name:?}");
    }
}

#[test]
fn the_directory_floor_only_rises_and_survives_a_reopen() {
    let (_dir, state) = state();
    let versions = state.directory_versions();
    assert_eq!(versions.highest().expect("reads"), 0);
    versions.record(7).expect("records");
    versions.record(3).expect("a lower version is not an error");
    assert_eq!(versions.highest().expect("reads"), 7);
    assert_eq!(state.directory_versions().highest().expect("reads"), 7);
}

#[test]
fn the_ledger_file_round_trips_and_starts_empty() {
    let (_dir, state) = state();
    assert_eq!(
        state.load_ledger().expect("absent is empty"),
        OriginLedger::new()
    );
    let ledger = OriginLedger::new();
    state.save_ledger(&ledger).expect("writes");
    assert_eq!(state.load_ledger().expect("reads"), ledger);
}

#[test]
fn a_durable_write_replaces_the_file_whole_and_leaves_no_temporary_behind() {
    let dir = Scratch::new();
    let path = dir.path().join("nested").join("file.json");
    write_durably(&path, b"one").expect("writes");
    write_durably(&path, b"two").expect("overwrites");
    assert_eq!(fs::read(&path).expect("reads"), b"two");
    let names: Vec<_> = fs::read_dir(path.parent().expect("a parent"))
        .expect("lists")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("file.json")]);
}

#[test]
fn the_boot_clock_names_one_boot_and_never_runs_backwards_within_it() {
    let clock = SystemBootClock::default();
    let epoch = clock.epoch();
    let first = clock.now_ms();
    let second = clock.now_ms();
    assert_eq!(clock.epoch(), epoch);
    assert!(second >= first);
    // Two clocks in one boot agree on the boot where the platform names one.
    if SystemBootClock::linux().is_some() {
        assert_eq!(SystemBootClock::default().epoch(), epoch);
    }
}

#[test]
fn every_retain_reason_has_a_distinct_stable_name() {
    let names = [
        kept_because(&RetainReason::NotDurable),
        kept_because(&RetainReason::ReceiptMissing),
        kept_because(&RetainReason::VerifyUnavailable),
        kept_because(&RetainReason::ReceiptUnavailable),
        kept_because(&RetainReason::ReadBackMismatch),
    ];
    let mut unique = names.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len());
}

#[test]
fn the_peer_commands_parse_with_json() {
    let to = Uuid::from_u128(1).to_string();
    let asset = Uuid::from_u128(2).to_string();
    for args in [
        vec![
            "capsule",
            "peer",
            "delegate",
            "--library",
            "lib",
            "--to",
            &to,
            "--asset",
            &asset,
            "--out",
            "out",
            "--json",
        ],
        vec![
            "capsule",
            "peer",
            "accept",
            "--library",
            "lib",
            "--from",
            "f",
            "--json",
        ],
        vec!["capsule", "peer", "accept", "--library", "lib"],
        vec![
            "capsule",
            "peer",
            "status",
            "--library",
            "lib",
            "--ack",
            "a",
            "--json",
        ],
    ] {
        assert!(
            crate::cli::try_parse_from(false, &args).is_ok(),
            "{args:?} parses"
        );
    }
    // An asset is required: a hand-off of nothing is a mistake, not a no-op.
    assert!(
        crate::cli::try_parse_from(
            false,
            [
                "capsule",
                "peer",
                "delegate",
                "--library",
                "lib",
                "--to",
                &to,
                "--out",
                "o"
            ]
        )
        .is_err()
    );
}
