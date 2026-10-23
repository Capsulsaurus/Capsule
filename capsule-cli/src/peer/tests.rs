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

// ─── The `--json` failure contract ────────────────────────────────────────────

fn rejected(status: u16, code: &str) -> DelegationError {
    DelegationError::Upload(capsule_sdk::upload::UploadError::Rejected {
        status,
        code: Some(code.to_owned()),
        message: "refused".into(),
    })
}

/// One of every [`DelegationError`] variant, and through `Peering` one of every
/// [`PeeringError`] variant.
fn every_delegation_error() -> Vec<DelegationError> {
    use capsule_sdk::peering::{UpdateDevice, VersionWindow};
    let device = Uuid::from_u128(7);
    let window = |v: &str| VersionWindow {
        min: v.into(),
        max: v.into(),
    };
    let peering = [
        PeeringError::Tls("handshake".into()),
        PeeringError::Io("reset".into()),
        PeeringError::VersionDisjoint {
            ours: window("2026-01-01"),
            theirs: window("2027-01-01"),
            update: UpdateDevice::ThisDevice,
        },
        PeeringError::ForeignIdentity,
        PeeringError::DirectoryRollback {
            pinned: 4,
            highest: 5,
        },
        PeeringError::PeerDirectoryStale { theirs: 4, ours: 5 },
        PeeringError::RateLimited(std::net::IpAddr::from([127, 0, 0, 1])),
        PeeringError::OutOfScope(capsule_sdk::fetch::FetchScope::MetadataOnly),
        PeeringError::UnknownDevice(device),
        PeeringError::RevokedDevice(device),
        PeeringError::HybridCheckFailed,
        PeeringError::Discovery("mdns".into()),
        PeeringError::Codec("hello".into()),
        PeeringError::Crypto(capsule_core::crypto::CryptoError::Auth("sig")),
        PeeringError::Backup(capsule_core::backup::BackupError::Format("tar".into())),
        PeeringError::Transfer("budget".into()),
    ];
    let mut errors: Vec<DelegationError> =
        peering.into_iter().map(DelegationError::Peering).collect();
    errors.extend([
        DelegationError::SelfDelegation,
        DelegationError::Malformed("short".into()),
        DelegationError::UnsupportedFormat { found: 9 },
        DelegationError::WrongDelegate { addressed: device },
        DelegationError::ProtocolUnsupported {
            bundle: "2099-01-01".into(),
            ours: "2026-01-01".into(),
        },
        DelegationError::Tampered("a member".into()),
        DelegationError::Inbox("disk".into()),
        rejected(400, capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED),
        DelegationError::UnknownDelegation(device),
        DelegationError::AckMismatch,
    ]);
    errors
}

#[test]
fn every_delegation_and_peering_error_has_a_distinct_stable_kind() {
    let errors = every_delegation_error();
    let mut kinds: Vec<&str> = errors.iter().map(delegation_kind).collect();
    // The names the CLI gives failures that are not a `DelegationError`.
    kinds.extend(["bundle", "read", "ack_malformed", "copy_missing"]);
    let total = kinds.len();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(kinds.len(), total, "{kinds:?}");
    assert!(
        kinds
            .iter()
            .all(|kind| kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')),
        "{kinds:?}"
    );
}

#[test]
fn a_failure_item_carries_its_kind_and_the_servers_code() {
    let error = rejected(400, capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED);
    let json = serde_json::to_value(PeerFailure::delegation("asset", &error)).expect("serializes");
    assert_eq!(json["subject"], "asset");
    assert_eq!(json["kind"], "upload");
    assert_eq!(json["code"], "error.upload.envelope_rejected");
    assert_eq!(json["message"], error.to_string());

    let revoked = DelegationError::Peering(PeeringError::RevokedDevice(Uuid::from_u128(7)));
    let json = serde_json::to_value(PeerFailure::delegation("f", &revoked)).expect("serializes");
    assert_eq!(json["kind"], "revoked_device");
    assert_eq!(json["code"], serde_json::Value::Null);

    let json = serde_json::to_value(PeerFailure::new("f", "read", "gone")).expect("serializes");
    assert_eq!(json["kind"], "read");
    assert_eq!(json["message"], "gone");
}

#[test]
fn an_upgrade_refusal_carries_the_protocol_code() {
    let error = DelegationError::Upload(capsule_sdk::upload::UploadError::UpgradeRequired {
        reason: capsule_sdk::net::negotiation::UpgradeReason::Refused,
        min: None,
        max: None,
        min_client_build: None,
        message: "update".into(),
    });
    let json = serde_json::to_value(PeerFailure::delegation("a", &error)).expect("serializes");
    assert_eq!(json["kind"], "upload");
    assert_eq!(json["code"], "error.protocol.version_unsupported");
}

#[test]
fn a_settled_hand_off_reports_no_failure_fields() {
    let removed = serde_json::to_value(Courier::settled(Uuid::nil(), "aa", "removed", None))
        .expect("serializes");
    assert_eq!(removed["outcome"], "removed");
    for field in ["kind", "code", "message"] {
        assert!(removed.get(field).is_none(), "{field}");
    }
    let kept = serde_json::to_value(Courier::settled(
        Uuid::nil(),
        "aa",
        "kept",
        Some(kept_because(&RetainReason::NotDurable)),
    ))
    .expect("serializes");
    assert_eq!(kept["kind"], "not_durable");
}

// ─── A held hand-off that can never succeed ───────────────────────────────────

/// An inbox holding one hand-off, and its address.
fn holding_one() -> (Scratch, PeerState, String) {
    let (dir, state) = state();
    let address = "ab".repeat(32);
    state
        .inbox()
        .persist(&address, b"a held hand-off")
        .expect("persists");
    (dir, state, address)
}

#[test]
fn a_permanently_refused_held_hand_off_is_discarded_and_reported_once() {
    let asset = Uuid::from_u128(3);
    for error in [
        // The origin was revoked after the hand-off was accepted.
        DelegationError::Peering(PeeringError::RevokedDevice(Uuid::from_u128(9))),
        // A create sealed under an album epoch the album has moved past.
        rejected(400, capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED),
        // A conflicting provenance chain.
        rejected(409, capsule_i18n::error_codes::UPLOAD_STALE_REVIVAL),
    ] {
        let (_dir, state, address) = holding_one();
        let inbox = state.inbox();
        let courier = refused(&inbox, &address, asset, &error);
        assert_eq!(courier.outcome, "discarded", "{error:?}");
        assert_eq!(courier.kind, Some(delegation_kind(&error)));
        assert_eq!(courier.code.as_deref(), error.error_code());
        assert!(
            inbox.addresses().expect("lists").is_empty(),
            "the copy is gone, so no later run reports it: {error:?}"
        );
        let summary = AcceptSummary {
            accepted: Vec::new(),
            refused: Vec::new(),
            uploads: vec![courier],
        };
        assert!(summary.has_failures(), "the run that discards it says so");
        let json = serde_json::to_value(&summary).expect("serializes");
        assert_eq!(json["uploads"][0]["outcome"], "discarded");
        assert_eq!(json["uploads"][0]["asset_id"], asset.to_string());
    }
}

#[test]
fn a_transiently_failed_held_hand_off_is_kept_for_the_next_run() {
    for error in [
        DelegationError::Upload(capsule_sdk::upload::UploadError::Transport("reset".into())),
        rejected(403, capsule_i18n::error_codes::QUOTA_EXCEEDED),
        rejected(503, capsule_i18n::error_codes::UPLOAD_UNAVAILABLE),
        DelegationError::Peering(PeeringError::DirectoryRollback {
            pinned: 4,
            highest: 5,
        }),
    ] {
        let (_dir, state, address) = holding_one();
        let inbox = state.inbox();
        let courier = refused(&inbox, &address, Uuid::nil(), &error);
        assert_eq!(courier.outcome, "failed", "{error:?}");
        assert_eq!(courier.kind, Some(delegation_kind(&error)));
        assert_eq!(courier.code.as_deref(), error.error_code());
        assert_eq!(inbox.addresses().expect("lists"), vec![address]);
    }
}
