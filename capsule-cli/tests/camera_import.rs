//! The acceptance for `capsule camera import` and `capsule camera identity`: the real binary
//! against a scripted PTP/IP camera on loopback.
//!
//! The camera is a responder thread speaking the protocol through `ptpip`'s own packet and
//! dataset codecs: it accepts the command and event channels, records the initiator identity it
//! is shown, serves one card of two files, and sends a keep-alive probe on the event channel that
//! the CLI's event reader must answer. Each `capsule` run is its own process, sharing only the
//! library directory and the redirected config directory, so the persisted initiator identity is
//! proved stable across processes the way a camera that pairs would see it.
//!
//! The account is seeded in-process at a fast Argon2id cost, as `import_round_trip.rs` explains:
//! every spawned process still runs the full recorded unlock.

use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

use capsule_core::crypto::primitives::Argon2Params;
use capsule_library::lifecycle::Workspace;
use ptpip::codec::Writer;
use ptpip::packet::{Packet, read_packet, write_packet};
use ptpip::{
    DeviceInfo, ObjectFormat, ObjectHandle, ObjectInfo, OperationCode, ResponseCode, StorageId,
};
use serde_json::Value;

const PASSPHRASE: &str = "camera-import-passphrase";
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};
const STORAGE: u32 = 0x0001_0001;
const FOLDER: u32 = 1;
/// The EXIF JPEG object format.
const EXIF_JPEG: ObjectFormat = ObjectFormat(0x3801);
const CARD: [(u32, &str, &[u8]); 2] = [
    (10, "DSC00001.JPG", b"first photo bytes"),
    (11, "DSC00002.JPG", b"second photo bytes"),
];

// ── Scratch plumbing ─────────────────────────────────────────────────────────

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-camera-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self(path)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn path(p: &Path) -> &str {
    p.to_str().expect("scratch paths are UTF-8")
}

/// Run the real binary once, with the CLI's config, data and cache directories under `home`.
fn spawn(home: &Path, args: &[&str], passphrase_stdin: bool) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_capsule"))
        .args(args)
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "off")
        .env("LC_ALL", "en_US.UTF-8")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn capsule");
    if passphrase_stdin {
        child
            .stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(format!("{PASSPHRASE}\n").as_bytes())
            .expect("write passphrase");
    }
    child.wait_with_output().expect("wait for capsule")
}

fn json_stdout(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "capsule failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is one JSON document")
}

struct Fixture {
    _scratch: ScratchDir,
    home: PathBuf,
    library: PathBuf,
}

/// A library from a real `capsule library init`, with its account seeded at a fast cost.
fn fixture() -> Fixture {
    let scratch = ScratchDir::new();
    let home = scratch.0.join("home");
    let library = scratch.0.join("library");
    std::fs::create_dir_all(&home).expect("create scratch home");
    let out = spawn(
        &home,
        &["library", "init", path(&library), "--name", "Camera"],
        false,
    );
    assert!(out.status.success(), "{out:?}");
    drop(Workspace::open(&library, PASSPHRASE.as_bytes(), FAST_KDF).expect("seed the account"));
    Fixture {
        _scratch: scratch,
        home,
        library,
    }
}

// ── The scripted camera ──────────────────────────────────────────────────────

/// What the camera saw of one session.
#[derive(Debug)]
struct Seen {
    /// The initiator GUID from `InitCommandRequest`.
    guid: [u8; 16],
    /// The initiator name.
    name: String,
    /// Whether the event-channel probe was answered.
    probe_answered: bool,
    /// The `GetObject` handles requested.
    fetched: Vec<u32>,
}

fn u32_array(values: &[u32]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32_array(values);
    w.into_bytes()
}

fn object_info(handle: u32) -> ObjectInfo {
    let (_, name, bytes) = CARD
        .iter()
        .find(|(h, _, _)| *h == handle)
        .expect("a handle on the card");
    ObjectInfo {
        storage_id: StorageId(STORAGE),
        format: EXIF_JPEG,
        protection_status: 0,
        compressed_size: bytes.len() as u32,
        thumb_format: 0,
        thumb_compressed_size: 0,
        thumb_pix_width: 0,
        thumb_pix_height: 0,
        image_pix_width: 0,
        image_pix_height: 0,
        image_bit_depth: 0,
        parent: ObjectHandle(FOLDER),
        association_type: 0,
        association_desc: 0,
        sequence_number: 0,
        filename: (*name).to_owned(),
        capture_date: "20260102T030405".into(),
        modification_date: String::new(),
        keywords: String::new(),
    }
}

/// Answer `transaction` with a data phase carrying `bytes`, then `OK`.
fn send_data(command: &mut TcpStream, transaction: u32, bytes: Vec<u8>) {
    write_packet(
        command,
        &Packet::StartData {
            transaction,
            total: bytes.len() as u64,
        },
    )
    .expect("start data");
    write_packet(
        command,
        &Packet::EndData {
            transaction,
            payload: bytes,
        },
    )
    .expect("end data");
    send_ok(command, transaction);
}

fn send_ok(command: &mut TcpStream, transaction: u32) {
    write_packet(
        command,
        &Packet::OperationResponse {
            code: ResponseCode::OK,
            transaction,
            params: vec![],
        },
    )
    .expect("response");
}

/// A camera that serves one session on `listener` and reports what it saw.
fn camera(listener: TcpListener) -> JoinHandle<Seen> {
    std::thread::spawn(move || {
        let timeout = Some(Duration::from_secs(30));
        let (mut command, _) = listener.accept().expect("command channel");
        command.set_read_timeout(timeout).expect("timeout");
        let Packet::InitCommandRequest { guid, name, .. } =
            read_packet(&mut command).expect("init command")
        else {
            panic!("expected InitCommandRequest");
        };
        write_packet(
            &mut command,
            &Packet::InitCommandAck {
                connection: 3,
                guid: [0xC0; 16],
                name: "ILCE-7M4".into(),
                version: 0x0001_0000,
            },
        )
        .expect("ack");
        let (mut event, _) = listener.accept().expect("event channel");
        event.set_read_timeout(timeout).expect("timeout");
        assert_eq!(
            read_packet(&mut event).expect("init event"),
            Packet::InitEventRequest { connection: 3 }
        );
        write_packet(&mut event, &Packet::InitEventAck).expect("event ack");
        // A keep-alive probe the CLI's event reader must answer, and an event it must accept.
        write_packet(&mut event, &Packet::ProbeRequest).expect("probe");
        write_packet(
            &mut event,
            &Packet::Event {
                code: 0x4002,
                transaction: 0,
                params: vec![10],
            },
        )
        .expect("event");

        let device = DeviceInfo {
            manufacturer: "Sony Corporation".into(),
            model: "ILCE-7M4".into(),
            serial_number: "0042".into(),
            operations_supported: vec![
                OperationCode::GET_DEVICE_INFO.0,
                OperationCode::OPEN_SESSION.0,
                OperationCode::CLOSE_SESSION.0,
                OperationCode::GET_STORAGE_IDS.0,
                OperationCode::GET_OBJECT_HANDLES.0,
                OperationCode::GET_OBJECT_INFO.0,
                OperationCode::GET_OBJECT.0,
            ],
            ..DeviceInfo::default()
        };
        let mut fetched = Vec::new();
        loop {
            let Packet::OperationRequest {
                code,
                transaction,
                params,
                ..
            } = read_packet(&mut command).expect("operation request")
            else {
                panic!("expected an operation request");
            };
            match code {
                OperationCode::GET_DEVICE_INFO => {
                    send_data(&mut command, transaction, device.encode().expect("encode"));
                }
                OperationCode::OPEN_SESSION => send_ok(&mut command, transaction),
                OperationCode::GET_STORAGE_IDS => {
                    send_data(&mut command, transaction, u32_array(&[STORAGE]));
                }
                OperationCode::GET_OBJECT_HANDLES => {
                    let handles: Vec<u32> = CARD.iter().map(|(h, _, _)| *h).collect();
                    send_data(&mut command, transaction, u32_array(&handles));
                }
                OperationCode::GET_OBJECT_INFO => {
                    let info = object_info(params[0]).encode().expect("encode");
                    send_data(&mut command, transaction, info);
                }
                OperationCode::GET_OBJECT => {
                    fetched.push(params[0]);
                    let (_, _, data) = CARD
                        .iter()
                        .find(|(h, _, _)| *h == params[0])
                        .expect("a handle on the card");
                    send_data(&mut command, transaction, data.to_vec());
                }
                OperationCode::CLOSE_SESSION => {
                    send_ok(&mut command, transaction);
                    break;
                }
                other => panic!("the camera does not implement {other}"),
            }
        }
        let probe_answered = matches!(read_packet(&mut event), Ok(Packet::ProbeResponse));
        Seen {
            guid,
            name,
            probe_answered,
            fetched,
        }
    })
}

fn listen() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    (listener, addr)
}

fn camera_import(fixture: &Fixture, addr: SocketAddr) -> Output {
    spawn(
        &fixture.home,
        &[
            "camera",
            "import",
            "--address",
            &addr.to_string(),
            "--library",
            path(&fixture.library),
            "--passphrase-stdin",
            "--json",
        ],
        true,
    )
}

// ── The tests ────────────────────────────────────────────────────────────────

#[test]
fn a_camera_is_imported_under_a_stable_identity_and_a_second_run_fetches_nothing() {
    let fixture = fixture();

    let (listener, addr) = listen();
    let seen = camera(listener);
    let report = json_stdout(&camera_import(&fixture, addr));
    let seen = seen.join().expect("camera thread");

    assert_eq!(report["camera"]["model"], "ILCE-7M4");
    assert_eq!(report["camera"]["address"], addr.to_string());
    assert_eq!(report["receive"]["received"], 2, "{report:#}");
    assert_eq!(report["receive"]["failures"], Value::Array(vec![]));
    assert_eq!(report["import"]["imported"], 2, "{report:#}");
    assert_eq!(report["events"]["probes"], 1, "{report:#}");
    assert_eq!(report["events"]["events"], 1, "{report:#}");
    assert!(seen.probe_answered, "the event reader answered the probe");
    assert_eq!(seen.fetched, [10, 11]);
    let staging = fixture
        .library
        .join(".library/camera-staging/Sony_Corporation-ILCE-7M4-0042");
    assert_eq!(report["receive"]["staging_root"], path(&staging));

    // The identity the camera saw is the one `camera identity` reports, and it persists.
    let identity = json_stdout(&spawn(
        &fixture.home,
        &["camera", "identity", "--json"],
        false,
    ));
    assert_eq!(identity["created"], false, "the import created it");
    let guid: uuid::Uuid =
        serde_json::from_value(identity["guid"].clone()).expect("the GUID is a UUID");
    assert_eq!(seen.guid, *guid.as_bytes());
    assert_eq!(identity["name"], seen.name.as_str());
    assert_eq!(report["initiator"]["guid"], identity["guid"]);

    // A second run presents the same identity, fetches nothing, and imports only duplicates.
    let (listener, addr) = listen();
    let again = camera(listener);
    let report = json_stdout(&camera_import(&fixture, addr));
    let again = again.join().expect("camera thread");
    assert_eq!(
        again.guid, seen.guid,
        "a paired camera recognises the client"
    );
    assert!(again.fetched.is_empty(), "nothing is fetched twice");
    assert_eq!(report["receive"]["already_staged"], 2, "{report:#}");
    assert_eq!(report["import"]["imported"], 0);
    assert_eq!(report["import"]["duplicates"], 2, "{report:#}");
}

#[test]
fn the_identity_is_created_once_and_renamed_without_a_new_guid() {
    let scratch = ScratchDir::new();
    let home = scratch.0.join("home");
    std::fs::create_dir_all(&home).expect("home");

    let first = json_stdout(&spawn(&home, &["camera", "identity", "--json"], false));
    assert_eq!(first["created"], true);
    assert!(
        first["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("Capsule"))
    );

    let renamed = json_stdout(&spawn(
        &home,
        &["camera", "identity", "--name", "Darkroom", "--json"],
        false,
    ));
    assert_eq!(renamed["guid"], first["guid"]);
    assert_eq!(renamed["name"], "Darkroom");

    let blank = spawn(&home, &["camera", "identity", "--name", " "], false);
    assert!(!blank.status.success(), "a blank name is refused");
}

#[test]
fn an_unreachable_camera_fails_the_run_and_imports_nothing() {
    let fixture = fixture();
    let (listener, addr) = listen();
    drop(listener);
    let out = camera_import(&fixture, addr);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Could not open a PTP/IP session with the camera"),
        "stderr:\n{stderr}"
    );
    assert!(
        !fixture.library.join(".library/camera-staging").exists(),
        "nothing was staged"
    );
}
