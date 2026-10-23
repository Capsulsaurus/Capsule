//! The CLI's peer state: the inbox, the ledger file and the boot clock. The directory floor is
//! `peering_state`'s.
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
            "discard",
            "--library",
            "lib",
            "ab",
            "--json",
        ],
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

#[test]
fn the_peering_channel_legs_parse_and_name_exactly_one_destination() {
    let to = Uuid::from_u128(1).to_string();
    let asset = Uuid::from_u128(2).to_string();
    let delegate = |extra: &[&str]| {
        let mut args = vec![
            "capsule",
            "peer",
            "delegate",
            "--library",
            "lib",
            "--asset",
            &asset,
            "--json",
        ];
        args.extend_from_slice(extra);
        crate::cli::try_parse_from(false, &args).is_ok()
    };
    // Over the channel, `--to` is optional: the handshake names the delegate.
    assert!(delegate(&["--peer", "192.168.1.20:7420"]));
    assert!(delegate(&["--peer", "laptop.local:7420", "--to", &to]));
    // Files still need both the directory and the device.
    assert!(delegate(&["--out", "o", "--to", &to]));
    assert!(
        !delegate(&["--out", "o"]),
        "a file hand-off names its device"
    );
    assert!(!delegate(&[]), "a hand-off goes somewhere");
    assert!(
        !delegate(&["--peer", "192.168.1.20:7420", "--out", "o", "--to", &to]),
        "one destination, not two"
    );

    let accept = |extra: &[&str]| {
        let mut args = vec!["capsule", "peer", "accept", "--library", "lib", "--json"];
        args.extend_from_slice(extra);
        crate::cli::try_parse_from(false, &args).is_ok()
    };
    assert!(accept(&["--listen", "0.0.0.0:7420"]));
    assert!(accept(&["--listen", "[::]:7420", "--listen-timeout", "30"]));
    assert!(accept(&["--listen", "0.0.0.0:7420", "--from", "f"]));
    assert!(!accept(&["--listen", "not-an-address"]));
    assert!(!accept(&[
        "--listen",
        "0.0.0.0:7420",
        "--listen-timeout",
        "0"
    ]));
    assert!(
        !accept(&["--listen-timeout", "30"]),
        "a timeout without a listener is a mistake"
    );
}

#[test]
fn a_hand_off_over_the_channel_reports_no_file_and_whether_it_was_acknowledged() {
    let over_channel = serde_json::to_value(DelegateSummary {
        delegate: Some(Uuid::from_u128(1)),
        peer: Some("192.168.1.20:7420".into()),
        handed_off: vec![HandedOff {
            asset_id: Uuid::from_u128(2),
            file: None,
            hand_off: "ab".repeat(32),
            bytes: 10,
            acknowledged: true,
        }],
        failed: vec![PeerFailure::new(
            Uuid::from_u128(3).to_string(),
            "not_acknowledged",
            "the delegate ended the session",
        )],
    })
    .expect("serializes");
    assert_eq!(over_channel["peer"], "192.168.1.20:7420");
    assert!(over_channel["handed_off"][0].get("file").is_none());
    assert_eq!(over_channel["handed_off"][0]["acknowledged"], true);
    assert_eq!(over_channel["failed"][0]["kind"], "not_acknowledged");

    // A file hand-off keeps its shape: the file, the delegate, and no peer.
    let to_file = serde_json::to_value(DelegateSummary {
        delegate: Some(Uuid::from_u128(1)),
        peer: None,
        handed_off: vec![HandedOff {
            asset_id: Uuid::from_u128(2),
            file: Some("out/x.capsule-handoff".into()),
            hand_off: "ab".repeat(32),
            bytes: 10,
            acknowledged: false,
        }],
        failed: Vec::new(),
    })
    .expect("serializes");
    assert!(to_file.get("peer").is_none());
    assert_eq!(to_file["delegate"], Uuid::from_u128(1).to_string());
    assert_eq!(to_file["handed_off"][0]["file"], "out/x.capsule-handoff");

    // No channel was established: the delegate is unknown.
    let unreached = serde_json::to_value(DelegateSummary {
        delegate: None,
        peer: Some("192.0.2.1:7420".into()),
        handed_off: Vec::new(),
        failed: Vec::new(),
    })
    .expect("serializes");
    assert!(unreached["delegate"].is_null());

    let taken = serde_json::to_value(Accepted {
        file: None,
        peer: Some(Uuid::from_u128(4)),
        ack: HandOffAck {
            asset_id: Uuid::from_u128(2),
            hand_off: "ab".repeat(32),
            delegate_device: Uuid::from_u128(1),
        },
        ack_file: None,
        acknowledged: Some(false),
    })
    .expect("serializes");
    assert_eq!(taken["peer"], Uuid::from_u128(4).to_string());
    assert_eq!(taken["acknowledged"], false);
    assert!(taken.get("file").is_none() && taken.get("ack_file").is_none());
}

#[tokio::test]
async fn a_listener_binds_an_ephemeral_port_and_reports_it() {
    let listener = PeerListener::bind(
        "127.0.0.1:0".parse().expect("an address"),
        DEFAULT_LISTEN_TIMEOUT,
    )
    .await
    .expect("binds");
    assert_ne!(listener.local_addr().expect("bound").port(), 0);
    let taken = listener.local_addr().expect("bound");
    assert!(matches!(
        PeerListener::bind(taken, DEFAULT_LISTEN_TIMEOUT).await,
        Err(PeerError::Listen { .. })
    ));
}

/// A stand-in handshake: it completes when the peer sends one byte, and stalls until then.
async fn byte_handshake(
    mut tcp: TcpStream,
    source: SocketAddr,
) -> (SocketAddr, Result<u8, PeeringError>) {
    use tokio::io::AsyncReadExt as _;
    let byte = tcp
        .read_u8()
        .await
        .map_err(|error| PeeringError::Io(error.to_string()));
    (source, byte)
}

/// A peer that connects and then stalls its handshake holds only its own slot: a second peer's
/// handshake completes while the first is still pending, long before the listen deadline.
#[tokio::test]
async fn a_stalled_handshake_does_not_hold_the_listener() {
    use tokio::io::AsyncWriteExt as _;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = listener.local_addr().expect("bound");
    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_secs(30);
    let mut pending = FuturesUnordered::new();
    let mut refused = Vec::new();

    let _stalled = TcpStream::connect(address).await.expect("connects");
    let mut prompt = TcpStream::connect(address).await.expect("connects");
    let prompt_source = prompt.local_addr().expect("bound");
    prompt.write_u8(7).await.expect("writes");

    let admitted = next_session(
        &listener,
        "the listener",
        deadline,
        &byte_handshake,
        &mut pending,
        &mut refused,
    )
    .await;
    assert!(
        matches!(admitted, Admitted::Session(source, 7) if source == prompt_source),
        "{admitted:?}"
    );
    assert_eq!(pending.len(), 1, "the stalled handshake is still pending");
    assert!(refused.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "admitted without waiting out the stalled peer"
    );
}

/// The listen deadline bounds a handshake as well as the wait for a connection: a peer that
/// connects and never completes its handshake ends the wait at the deadline.
#[tokio::test]
async fn the_listen_deadline_bounds_a_stalled_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = listener.local_addr().expect("bound");
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    let mut pending = FuturesUnordered::new();
    let mut refused = Vec::new();

    let _stalled = TcpStream::connect(address).await.expect("connects");
    let admitted = tokio::time::timeout(
        Duration::from_secs(10),
        next_session(
            &listener,
            "the listener",
            deadline,
            &byte_handshake,
            &mut pending,
            &mut refused,
        ),
    )
    .await
    .expect("the deadline ends the wait");
    assert!(matches!(admitted, Admitted::TimedOut), "{admitted:?}");
    assert!(tokio::time::Instant::now() >= deadline);
    assert_eq!(pending.len(), 1, "the handshake was started, then cut off");
}

/// A handshake that fails is recorded under its source and the listener keeps going.
#[tokio::test]
async fn a_failed_handshake_is_recorded_and_the_next_one_admitted() {
    use tokio::io::AsyncWriteExt as _;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = listener.local_addr().expect("bound");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut pending = FuturesUnordered::new();
    let mut refused = Vec::new();

    // Closes without a byte: its handshake fails.
    drop(TcpStream::connect(address).await.expect("connects"));
    let later = async {
        // Only once the failed handshake has had time to end.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut good = TcpStream::connect(address).await.expect("connects");
        good.write_u8(9).await.expect("writes");
        good
    };
    let (admitted, _good) = tokio::join!(
        next_session(
            &listener,
            "the listener",
            deadline,
            &byte_handshake,
            &mut pending,
            &mut refused,
        ),
        later
    );
    assert!(matches!(admitted, Admitted::Session(_, 9)), "{admitted:?}");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].kind, "peering_io");
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
        PeeringError::SelfPeer(device),
        PeeringError::HybridCheckFailed,
        PeeringError::Discovery("mdns".into()),
        PeeringError::Codec("hello".into()),
        PeeringError::Crypto(capsule_core::crypto::CryptoError::Auth("sig")),
        PeeringError::Backup(capsule_core::backup::BackupError::Format("tar".into())),
        PeeringError::Transfer("budget".into()),
        PeeringError::Timeout("reading a frame"),
        PeeringError::PeerRefused {
            kind: "revoked_device".into(),
            message: "revoked".into(),
        },
        PeeringError::NotLan(std::net::IpAddr::from([8, 8, 8, 8])),
        PeeringError::Directory("unreachable".into()),
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
        DelegationError::WrongOrigin {
            origin: device,
            peer: Uuid::nil(),
        },
    ]);
    errors
}

#[test]
fn every_delegation_and_peering_error_has_a_distinct_stable_kind() {
    let errors = every_delegation_error();
    let mut kinds: Vec<&str> = errors.iter().map(delegation_kind).collect();
    // The names the CLI gives failures that are not a `DelegationError`.
    kinds.extend([
        "bundle",
        "read",
        "ack_malformed",
        "copy_missing",
        "peer_mismatch",
        "not_acknowledged",
    ]);
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
fn a_permanently_refused_held_hand_off_is_quarantined_and_reported_once() {
    let asset = Uuid::from_u128(3);
    for error in [
        // The origin was revoked after the hand-off was accepted: often a lost device, whose
        // original may survive only in this copy.
        DelegationError::Peering(PeeringError::RevokedDevice(Uuid::from_u128(9))),
        // A stale album epoch, a closed album or withdrawn write access at finalization.
        rejected(400, capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED),
        // The create-time twin of a closed album.
        rejected(403, capsule_i18n::error_codes::UPLOAD_ALBUM_ACCESS_DENIED),
        // A conflicting provenance chain.
        rejected(409, capsule_i18n::error_codes::UPLOAD_STALE_REVIVAL),
    ] {
        let (_dir, state, address) = holding_one();
        let inbox = state.inbox();
        let courier = refused(&inbox, &address, asset, &error);
        assert_eq!(courier.outcome, "quarantined", "{error:?}");
        assert_eq!(courier.kind, Some(delegation_kind(&error)));
        assert_eq!(courier.code.as_deref(), error.error_code());
        assert!(
            inbox.addresses().expect("lists").is_empty(),
            "no later `peer accept` retries it: {error:?}"
        );
        assert_eq!(
            inbox.load_quarantined(&address).expect("reads"),
            Some(b"a held hand-off".to_vec()),
            "the bytes are kept: {error:?}"
        );
        let summary = AcceptSummary {
            accepted: Vec::new(),
            refused: Vec::new(),
            uploads: vec![courier],
        };
        assert!(
            summary.has_failures(),
            "the run that quarantines it says so"
        );
        let json = serde_json::to_value(&summary).expect("serializes");
        assert_eq!(json["uploads"][0]["outcome"], "quarantined");
        assert_eq!(json["uploads"][0]["asset_id"], asset.to_string());

        // `peer status` lists it once, with the stable kind, from a fresh read of the state.
        let listed = quarantined(&PeerState::new(state.root.clone()).inbox()).expect("lists");
        assert_eq!(listed.len(), 1, "{listed:?}");
        let view = serde_json::to_value(&listed[0]).expect("serializes");
        assert_eq!(view["hand_off"], address);
        assert_eq!(view["asset_id"], asset.to_string());
        assert_eq!(view["kind"], delegation_kind(&error));
        assert_eq!(
            view["code"],
            error
                .error_code()
                .map_or(serde_json::Value::Null, serde_json::Value::from)
        );
        assert_eq!(view["message"], error.to_string());
        assert_eq!(
            fs::read(view["file"].as_str().expect("a path")).expect("the file is there"),
            b"a held hand-off"
        );
    }
}

#[test]
fn the_quarantine_survives_a_reopen_and_only_discard_deletes_it() {
    let (_dir, state, address) = holding_one();
    let error = DelegationError::Peering(PeeringError::RevokedDevice(Uuid::from_u128(9)));
    let courier = refused(&state.inbox(), &address, Uuid::from_u128(3), &error);
    assert_eq!(courier.outcome, "quarantined");

    // A held copy and a name that is no content address are not discardable.
    let held = "cd".repeat(32);
    state
        .inbox()
        .persist(&held, b"still held")
        .expect("persists");
    for name in [held.as_str(), "../../etc/passwd", ""] {
        assert!(
            matches!(discard(&state, name), Err(PeerError::NotQuarantined(_))),
            "{name:?}"
        );
    }
    assert_eq!(state.inbox().addresses().expect("lists"), vec![held]);

    let reopened = PeerState::new(state.root.clone());
    let discarded = discard(&reopened, &address).expect("discards");
    assert_eq!(
        serde_json::to_value(&discarded).expect("serializes"),
        serde_json::json!({
            "hand_off": address,
            "asset_id": Uuid::from_u128(3).to_string(),
            "kind": "revoked_device",
        })
    );
    assert!(quarantined(&reopened.inbox()).expect("lists").is_empty());
    assert_eq!(
        reopened.inbox().load_quarantined(&address).expect("reads"),
        None
    );
    assert!(matches!(
        discard(&reopened, &address),
        Err(PeerError::NotQuarantined(_))
    ));
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
        assert!(quarantined(&inbox).expect("lists").is_empty());
    }
}
