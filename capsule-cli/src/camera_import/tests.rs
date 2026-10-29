//! The camera flow over a fake camera and a real workspace: what is received, what is imported,
//! what a second run skips, and every failure reported rather than dropped. The TCP bring-up and
//! the event-channel reader are exercised end to end by `tests/camera_import.rs`.

use std::collections::BTreeMap;
use std::io::Write;

use capsule_core::crypto::primitives::Argon2Params;
use capsule_library::import::{CameraError, CameraObject};

use super::*;
use crate::import_report::FileImportStatus;

const STORAGE: u32 = 0x0001_0001;
const EMPTY_SLOT: u32 = 0x0002_0001;
const FOLDER: u32 = 1;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("capsule-camera-import-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A library at a fast KDF cost, with its default album, as the CLI opens one.
fn workspace(scratch: &Scratch) -> Workspace {
    let lib = scratch.0.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let mut ws = Workspace::create_with_params(
        &lib,
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    ws
}

/// A camera with one card of files, an optional empty second slot, and objects it refuses.
#[derive(Default)]
struct FakeCamera {
    objects: BTreeMap<u32, (CameraObject, Vec<u8>)>,
    refused_reads: Vec<u32>,
    empty_slot: bool,
    disconnect_on: Option<u32>,
    reads: usize,
}

impl FakeCamera {
    fn file(mut self, handle: u32, name: &str, bytes: &[u8]) -> Self {
        let object = CameraObject {
            storage: STORAGE,
            handle,
            parent: FOLDER,
            filename: name.to_owned(),
            size: Some(bytes.len() as u64),
            captured_at: None,
            is_folder: false,
        };
        self.objects.insert(handle, (object, bytes.to_vec()));
        self
    }
}

impl CameraDevice for FakeCamera {
    fn supports_partial_reads(&self) -> bool {
        false
    }

    fn storage_ids(&mut self) -> Result<Vec<u32>, CameraError> {
        Ok(if self.empty_slot {
            vec![STORAGE, EMPTY_SLOT]
        } else {
            vec![STORAGE]
        })
    }

    fn object_handles(&mut self, storage: u32) -> Result<Vec<u32>, CameraError> {
        if storage == EMPTY_SLOT {
            return Err(CameraError::Refused {
                operation: "GetObjectHandles".into(),
                code: "Store_Not_Available".into(),
            });
        }
        Ok(self.objects.keys().copied().collect())
    }

    fn object(&mut self, handle: u32) -> Result<CameraObject, CameraError> {
        Ok(self.objects[&handle].0.clone())
    }

    fn read_object(
        &mut self,
        object: &CameraObject,
        _offset: u64,
        sink: &mut dyn Write,
    ) -> Result<u64, CameraError> {
        self.reads += 1;
        if self.disconnect_on == Some(object.handle) {
            return Err(CameraError::Disconnected("connection reset".into()));
        }
        if self.refused_reads.contains(&object.handle) {
            return Err(CameraError::Refused {
                operation: "GetObject".into(),
                code: "Access_Denied".into(),
            });
        }
        let bytes = &self.objects[&object.handle].1;
        sink.write_all(bytes).map_err(CameraError::Sink)?;
        Ok(bytes.len() as u64)
    }
}

fn card() -> FakeCamera {
    FakeCamera::default()
        .file(10, "DSC00001.JPG", b"first photo bytes")
        .file(11, "DSC00002.JPG", b"second photo bytes")
}

#[test]
fn a_card_is_received_and_imported_and_a_second_run_fetches_and_imports_nothing() {
    let scratch = Scratch::new();
    let mut ws = workspace(&scratch);
    let staging = scratch.0.join("library").join(STAGING_DIR).join("cam");

    let mut camera = card();
    let (receive, import) = receive_and_import(&mut ws, &mut camera, &staging).unwrap();
    assert_eq!(receive.received, 2, "{receive:?}");
    assert_eq!(receive.already_staged, 0);
    assert!(receive.failures.is_empty() && receive.storage_failures.is_empty());
    assert_eq!(receive.staging_root, staging.display().to_string());
    assert_eq!(import.imported, 2, "{import:?}");
    assert!(
        import
            .outcomes
            .iter()
            .all(|o| o.status == FileImportStatus::Imported)
    );
    assert_eq!(ws.asset_ids().len(), 2);

    let mut again = card();
    let (receive, import) = receive_and_import(&mut ws, &mut again, &staging).unwrap();
    assert_eq!(receive.received, 2);
    assert_eq!(receive.already_staged, 2, "nothing is fetched twice");
    assert_eq!(again.reads, 0);
    assert_eq!(import.imported, 0);
    assert_eq!(import.duplicates, 2, "{import:?}");
    assert_eq!(ws.asset_ids().len(), 2);
}

#[test]
fn a_refused_object_and_an_unlisted_slot_are_reported_and_the_rest_imports() {
    let scratch = Scratch::new();
    let mut ws = workspace(&scratch);
    let staging = scratch.0.join("staging");
    let mut camera = FakeCamera {
        refused_reads: vec![11],
        empty_slot: true,
        ..card()
    };
    let (receive, import) = receive_and_import(&mut ws, &mut camera, &staging).unwrap();
    assert_eq!(receive.received, 1);
    assert_eq!(
        receive.failures,
        [ReceiveFailure {
            storage: STORAGE,
            handle: 11,
            filename: Some("DSC00002.JPG".into()),
            error: "camera refused GetObject: Access_Denied".into(),
        }]
    );
    assert_eq!(
        receive.storage_failures,
        [StorageFailureReport {
            storage: Some(EMPTY_SLOT),
            error: "camera refused GetObjectHandles: Store_Not_Available".into(),
        }]
    );
    assert_eq!(import.imported, 1);
}

#[test]
fn a_lost_connection_ends_the_run_and_keeps_what_was_staged() {
    let scratch = Scratch::new();
    let mut ws = workspace(&scratch);
    let staging = scratch.0.join("staging");
    let mut camera = FakeCamera {
        disconnect_on: Some(11),
        ..card()
    };
    let error = receive_and_import(&mut ws, &mut camera, &staging)
        .unwrap_err()
        .to_string();
    assert!(error.contains("connection reset"), "{error}");
    assert!(error.contains("next run resumes it"), "{error}");
    assert!(
        ws.asset_ids().is_empty(),
        "nothing imports from a cut-off run"
    );

    // The next run fetches only what the first did not finish.
    let mut retry = card();
    let (receive, import) = receive_and_import(&mut ws, &mut retry, &staging).unwrap();
    assert_eq!(receive.already_staged, 1);
    assert_eq!(retry.reads, 1);
    assert_eq!(import.imported, 2);
}

#[test]
fn an_empty_camera_imports_nothing_and_stages_no_tree() {
    let scratch = Scratch::new();
    let mut ws = workspace(&scratch);
    let staging = scratch.0.join("staging");
    let (receive, import) =
        receive_and_import(&mut ws, &mut FakeCamera::default(), &staging).unwrap();
    assert_eq!(receive.received, 0);
    assert_eq!(import, ImportReport::default());
}

#[test]
fn an_address_takes_the_ptpip_port_unless_it_names_one() {
    assert_eq!(
        parse_address("192.168.1.20").unwrap(),
        "192.168.1.20:15740".parse().unwrap()
    );
    assert_eq!(
        parse_address(" 192.168.1.20:5000 ").unwrap(),
        "192.168.1.20:5000".parse().unwrap()
    );
    assert_eq!(
        parse_address("fe80::1").unwrap(),
        "[fe80::1]:15740".parse().unwrap()
    );
    assert_eq!(
        parse_address("[fe80::1]").unwrap(),
        "[fe80::1]:15740".parse().unwrap()
    );
    assert_eq!(
        parse_address("[fe80::1]:99").unwrap(),
        "[fe80::1]:99".parse().unwrap()
    );
    assert_eq!(parse_address("localhost:7").unwrap().port(), 7);
    assert_eq!(parse_address("localhost").unwrap().port(), DEFAULT_PORT);
    assert!(parse_address("  ").is_err());
}

#[test]
fn each_camera_stages_under_its_own_portable_directory() {
    let device = DeviceInfo {
        manufacturer: "Sony Corporation".into(),
        model: "ILCE-7M4".into(),
        serial_number: "0123/45:67".into(),
        ..DeviceInfo::default()
    };
    assert_eq!(camera_key(&device), "Sony_Corporation-ILCE-7M4-0123_45_67");
    assert_eq!(
        staging_root(Path::new("/lib"), &device),
        Path::new("/lib/.library/camera-staging/Sony_Corporation-ILCE-7M4-0123_45_67")
    );
    assert_eq!(camera_key(&DeviceInfo::default()), "camera");
    let dots = DeviceInfo {
        model: "..".into(),
        ..DeviceInfo::default()
    };
    assert_eq!(camera_key(&dots), "camera", "never a parent-directory name");
    let long = DeviceInfo {
        model: "M".repeat(500),
        ..DeviceInfo::default()
    };
    assert_eq!(camera_key(&long).len(), MAX_CAMERA_KEY);
}

#[test]
fn a_camera_named_like_a_windows_device_stages_under_a_repaired_name() {
    let named = |manufacturer: &str, model: &str| DeviceInfo {
        manufacturer: manufacturer.into(),
        model: model.into(),
        ..DeviceInfo::default()
    };
    assert_eq!(camera_key(&named("CON", "")), "_CON");
    assert_eq!(camera_key(&named(" nul ", "")), "_nul", "case-insensitive");
    assert_eq!(
        camera_key(&named("aux.", "")),
        "_aux",
        "a trailing dot is trimmed first"
    );
    assert_eq!(camera_key(&named("COM1", "")), "_COM1");
    assert_eq!(
        camera_key(&named("PRN.cam", "")),
        "_PRN.cam",
        "with an extension"
    );
    assert_eq!(
        camera_key(&named("CON", "X1")),
        "CON-X1",
        "only a whole device name is reserved"
    );
}

#[test]
fn the_summary_names_every_object_left_on_the_camera() {
    let report = CameraImportReport {
        camera: CameraSummary {
            address: "10.0.0.2:15740".into(),
            manufacturer: "Sony".into(),
            model: "ILCE-7M4".into(),
            serial_number: "1".into(),
        },
        initiator: Pairing {
            guid: uuid::Uuid::new_v4(),
            name: "Capsule".into(),
        },
        receive: ReceiveReport {
            staging_root: "/lib/.library/camera-staging/x".into(),
            received: 1,
            failures: vec![ReceiveFailure {
                storage: STORAGE,
                handle: 11,
                filename: Some("DSC00002.JPG".into()),
                error: "refused".into(),
            }],
            storage_failures: vec![StorageFailureReport {
                storage: None,
                error: "busy".into(),
            }],
            ..ReceiveReport::default()
        },
        import: ImportReport {
            imported: 1,
            ..ImportReport::default()
        },
        events: EventsReport::default(),
    };
    let lines = summary_lines(&report);
    assert!(lines[0].starts_with("Received 1 object(s)"), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("storages could not be listed: busy"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("DSC00002.JPG") && l.contains("refused"))
    );
    assert!(lines.iter().any(|l| l.contains("run the import again")));
    assert_eq!(
        lines.last().unwrap(),
        &import_report::summary_line(&report.import)
    );
}
