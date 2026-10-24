use std::collections::BTreeMap;

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::domain::StackType;
use tempfile::TempDir;

use super::*;
use crate::db::DatabaseDriver;
use crate::import::planner::{ImportConfig, ImportDecision, plan};

const STORAGE: u32 = 0x0001_0001;
const DCIM_100: u32 = 0x10;
const DCIM_101: u32 = 0x11;

/// What the fake does when `read_object` reaches one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Refuse the read.
    Refuse,
    /// Deliver this many bytes, then drop the connection.
    DisconnectAfter(usize),
    /// Deliver one byte fewer than declared.
    Short,
    /// Deliver every byte, then corrupt the staged file behind the sink's back.
    CorruptStaged,
}

/// An in-memory camera.
#[derive(Default)]
struct FakeCamera {
    /// Storage id per handle, plus the record and bytes.
    objects: BTreeMap<u32, (CameraObject, Vec<u8>)>,
    partial: bool,
    faults: BTreeMap<u32, Fault>,
    unreadable_records: Vec<u32>,
    /// `(handle, offset)` of every `read_object` call.
    reads: Vec<(u32, u64)>,
    staging_root: PathBuf,
}

impl FakeCamera {
    fn new(staging_root: &Path) -> Self {
        Self {
            staging_root: staging_root.to_path_buf(),
            ..Self::default()
        }
    }

    fn file(mut self, handle: u32, parent: u32, name: &str, bytes: &[u8]) -> Self {
        self.objects.insert(
            handle,
            (
                CameraObject {
                    storage: STORAGE,
                    handle,
                    parent,
                    filename: name.into(),
                    size: Some(bytes.len() as u64),
                    captured_at: None,
                    is_folder: false,
                },
                bytes.to_vec(),
            ),
        );
        self
    }

    fn folder(mut self, handle: u32) -> Self {
        self.objects.insert(
            handle,
            (
                CameraObject {
                    storage: STORAGE,
                    handle,
                    parent: 0,
                    filename: format!("{handle}MSDCF"),
                    size: Some(0),
                    captured_at: None,
                    is_folder: true,
                },
                Vec::new(),
            ),
        );
        self
    }
}

impl CameraDevice for FakeCamera {
    fn supports_partial_reads(&self) -> bool {
        self.partial
    }

    fn storage_ids(&mut self) -> Result<Vec<u32>, CameraError> {
        Ok(vec![STORAGE])
    }

    fn object_handles(&mut self, storage: u32) -> Result<Vec<u32>, CameraError> {
        assert_eq!(storage, STORAGE);
        // Deliberately out of order: the adapter owns the ordering.
        Ok(self.objects.keys().rev().copied().collect())
    }

    fn object(&mut self, handle: u32) -> Result<CameraObject, CameraError> {
        if self.unreadable_records.contains(&handle) {
            return Err(CameraError::Refused {
                operation: "GetObjectInfo".into(),
                code: "Invalid_ObjectHandle".into(),
            });
        }
        Ok(self.objects[&handle].0.clone())
    }

    fn read_object(
        &mut self,
        object: &CameraObject,
        offset: u64,
        sink: &mut dyn Write,
    ) -> Result<u64, CameraError> {
        self.reads.push((object.handle, offset));
        let bytes = &self.objects[&object.handle].1[offset as usize..];
        let write =
            |sink: &mut dyn Write, part: &[u8]| sink.write_all(part).map_err(CameraError::Sink);
        match self.faults.get(&object.handle).copied() {
            Some(Fault::Refuse) => Err(CameraError::Refused {
                operation: "GetObject".into(),
                code: "Access_Denied".into(),
            }),
            Some(Fault::DisconnectAfter(n)) => {
                write(sink, &bytes[..n])?;
                // A real session would never complete this transaction.
                self.faults.remove(&object.handle);
                Err(CameraError::Disconnected("connection reset".into()))
            }
            Some(Fault::Short) => {
                write(sink, &bytes[..bytes.len() - 1])?;
                Ok(bytes.len() as u64 - 1)
            }
            Some(Fault::CorruptStaged) => {
                write(sink, bytes)?;
                sink.flush().map_err(CameraError::Sink)?;
                let partial = staging::partial_path(&self.staging_root, object);
                let mut staged = fs::read(&partial).expect("partial exists");
                staged[0] ^= 0xFF;
                fs::write(&partial, staged).expect("corrupt partial");
                Ok(bytes.len() as u64)
            }
            None => {
                write(sink, bytes)?;
                Ok(bytes.len() as u64)
            }
        }
    }
}

fn shot(n: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(n))
        .collect()
}

#[test]
fn receives_every_file_in_handle_order_and_skips_folders() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera = FakeCamera::new(tmp.path())
        .folder(DCIM_100)
        .file(0x30, DCIM_100, "DSC00002.JPG", &shot(2, 300))
        .file(0x20, DCIM_100, "DSC00001.JPG", &shot(1, 200));

    let receipt = adapter.receive(&mut camera).unwrap();

    assert!(receipt.failures.is_empty());
    let handles: Vec<u32> = receipt.received.iter().map(|r| r.object.handle).collect();
    assert_eq!(handles, vec![0x20, 0x30]);
    for received in &receipt.received {
        let bytes = fs::read(&received.path).unwrap();
        assert_eq!(received.sha256, hash_bytes(&bytes));
        assert_eq!(received.size, bytes.len() as u64);
        assert!(received.path.starts_with(&receipt.objects_root));
        assert!(!received.already_staged);
    }
    assert_eq!(receipt.received[0].sha256, hash_bytes(&shot(1, 200)));
    assert_eq!(camera.reads, vec![(0x20, 0), (0x30, 0)]);
    // Nothing is left half-written.
    assert_eq!(
        fs::read_dir(staging::partial_dir(tmp.path()))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn enumeration_orders_by_storage_then_handle_then_capture_date() {
    let a = CameraObject {
        storage: 2,
        handle: 1,
        parent: 0,
        filename: "a".into(),
        size: Some(1),
        captured_at: None,
        is_folder: false,
    };
    let b = CameraObject {
        storage: 1,
        handle: 9,
        ..a.clone()
    };
    let c = CameraObject {
        storage: 1,
        handle: 3,
        ..a.clone()
    };

    struct Listing(Vec<CameraObject>);
    impl CameraDevice for Listing {
        fn supports_partial_reads(&self) -> bool {
            false
        }
        fn storage_ids(&mut self) -> Result<Vec<u32>, CameraError> {
            Ok(vec![2, 1, 2])
        }
        fn object_handles(&mut self, storage: u32) -> Result<Vec<u32>, CameraError> {
            Ok(self
                .0
                .iter()
                .filter(|o| o.storage == storage)
                .map(|o| o.handle)
                .collect())
        }
        fn object(&mut self, handle: u32) -> Result<CameraObject, CameraError> {
            Ok(self.0.iter().find(|o| o.handle == handle).unwrap().clone())
        }
        fn read_object(
            &mut self,
            _: &CameraObject,
            _: u64,
            _: &mut dyn Write,
        ) -> Result<u64, CameraError> {
            unreachable!("enumeration only")
        }
    }

    let mut failures = Vec::new();
    let listed = enumerate(&mut Listing(vec![a, b, c]), &mut failures).unwrap();
    let order: Vec<(u32, u32)> = listed.iter().map(|o| (o.storage, o.handle)).collect();
    assert_eq!(order, vec![(1, 3), (1, 9), (2, 1)]);
    assert!(failures.is_empty());
}

#[test]
fn a_refused_object_is_a_failure_and_the_run_continues() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera = FakeCamera::new(tmp.path())
        .file(0x20, DCIM_100, "DSC00001.JPG", &shot(1, 50))
        .file(0x21, DCIM_100, "DSC00002.JPG", &shot(2, 50));
    camera.faults.insert(0x20, Fault::Refuse);

    let receipt = adapter.receive(&mut camera).unwrap();

    assert_eq!(receipt.received.len(), 1);
    assert_eq!(receipt.received[0].object.handle, 0x21);
    assert_eq!(receipt.failures.len(), 1);
    assert_eq!(receipt.failures[0].handle, 0x20);
    assert_eq!(
        receipt.failures[0].filename.as_deref(),
        Some("DSC00001.JPG")
    );
    assert!(matches!(
        receipt.failures[0].error,
        CameraError::Refused { .. }
    ));
}

#[test]
fn an_unreadable_record_is_reported_not_dropped() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera = FakeCamera::new(tmp.path())
        .file(0x20, DCIM_100, "DSC00001.JPG", &shot(1, 50))
        .file(0x21, DCIM_100, "DSC00002.JPG", &shot(2, 50));
    camera.unreadable_records.push(0x21);

    let receipt = adapter.receive(&mut camera).unwrap();

    assert_eq!(receipt.received.len(), 1);
    assert_eq!(receipt.failures.len(), 1);
    assert_eq!(receipt.failures[0].handle, 0x21);
    assert_eq!(receipt.failures[0].filename, None);
}

#[test]
fn a_short_transfer_fails_the_object_and_leaves_nothing_staged() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera =
        FakeCamera::new(tmp.path()).file(0x20, DCIM_100, "DSC00001.ARW", &shot(1, 100));
    camera.faults.insert(0x20, Fault::Short);

    let receipt = adapter.receive(&mut camera).unwrap();

    assert!(receipt.received.is_empty());
    assert!(matches!(
        receipt.failures[0].error,
        CameraError::SizeMismatch {
            declared: 100,
            received: 99,
            ..
        }
    ));
    assert_eq!(
        fs::read_dir(staging::partial_dir(tmp.path()))
            .unwrap()
            .count(),
        0
    );
    assert!(!adapter.objects_root().join("00010001").exists());
}

#[test]
fn staged_bytes_that_do_not_rehash_to_the_receipt_fail_the_object() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera =
        FakeCamera::new(tmp.path()).file(0x20, DCIM_100, "DSC00001.ARW", &shot(1, 100));
    camera.faults.insert(0x20, Fault::CorruptStaged);

    let receipt = adapter.receive(&mut camera).unwrap();

    assert!(receipt.received.is_empty());
    match &receipt.failures[0].error {
        CameraError::IntegrityMismatch {
            received, staged, ..
        } => {
            assert_eq!(*received, hash_bytes(&shot(1, 100)));
            assert_ne!(received, staged);
        }
        other => panic!("expected an integrity mismatch, got {other:?}"),
    }
    assert!(!adapter.objects_root().join("00010001").exists());
}

#[test]
fn a_dropped_connection_halts_and_the_next_run_resumes_the_partial_object() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let raw = shot(7, 1000);
    let mut camera = FakeCamera::new(tmp.path())
        .file(0x20, DCIM_100, "DSC00001.ARW", &raw)
        .file(0x21, DCIM_100, "DSC00002.ARW", &shot(8, 10));
    camera.partial = true;
    camera.faults.insert(0x20, Fault::DisconnectAfter(400));

    let halted = adapter.receive(&mut camera).unwrap_err();
    assert!(matches!(halted, CameraError::Disconnected(_)));
    assert!(halted.is_fatal());

    let receipt = adapter.receive(&mut camera).unwrap();

    let resumed = &receipt.received[0];
    assert_eq!(resumed.resumed_from, 400);
    assert_eq!(resumed.size, 1000);
    assert_eq!(resumed.sha256, hash_bytes(&raw));
    assert_eq!(fs::read(&resumed.path).unwrap(), raw);
    assert_eq!(camera.reads, vec![(0x20, 0), (0x20, 400), (0x21, 0)]);
}

#[test]
fn without_partial_reads_an_interrupted_object_starts_over() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let raw = shot(7, 1000);
    let mut camera = FakeCamera::new(tmp.path()).file(0x20, DCIM_100, "DSC00001.ARW", &raw);
    camera.faults.insert(0x20, Fault::DisconnectAfter(400));

    adapter.receive(&mut camera).unwrap_err();
    let receipt = adapter.receive(&mut camera).unwrap();

    assert_eq!(receipt.received[0].resumed_from, 0);
    assert_eq!(receipt.received[0].sha256, hash_bytes(&raw));
    assert_eq!(camera.reads, vec![(0x20, 0), (0x20, 0)]);
}

#[test]
fn a_finished_object_is_never_fetched_again() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera = FakeCamera::new(tmp.path()).file(0x20, DCIM_100, "DSC00001.JPG", &shot(1, 64));

    let first = adapter.receive(&mut camera).unwrap();
    let second = adapter.receive(&mut camera).unwrap();

    assert_eq!(camera.reads, vec![(0x20, 0)]);
    assert!(second.received[0].already_staged);
    assert_eq!(second.received[0].sha256, first.received[0].sha256);
}

#[test]
fn a_staged_file_of_the_wrong_size_is_fetched_again() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera = FakeCamera::new(tmp.path()).file(0x20, DCIM_100, "DSC00001.JPG", &shot(1, 64));
    let first = adapter.receive(&mut camera).unwrap();
    fs::write(&first.received[0].path, b"truncated").unwrap();

    let second = adapter.receive(&mut camera).unwrap();

    assert!(!second.received[0].already_staged);
    assert_eq!(fs::read(&second.received[0].path).unwrap(), shot(1, 64));
}

#[test]
fn a_staging_failure_halts_the_run() {
    let tmp = TempDir::new().unwrap();
    // A file where the staging root's `partial/` directory must go.
    fs::write(tmp.path().join("partial"), b"in the way").unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera = FakeCamera::new(tmp.path()).file(0x20, DCIM_100, "DSC00001.JPG", &shot(1, 8));

    let error = adapter.receive(&mut camera).unwrap_err();
    assert!(matches!(error, CameraError::Staging { .. }));
    assert!(error.is_fatal());
}

#[test]
fn extract_feeds_the_scanner_so_raw_and_jpeg_stack_and_entries_are_sorted() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    let mut camera = FakeCamera::new(tmp.path())
        .file(0x20, DCIM_100, "DSC00001.ARW", &shot(1, 80))
        .file(0x21, DCIM_100, "DSC00001.JPG", &shot(2, 40))
        .file(0x22, DCIM_101, "DSC00001.JPG", &shot(3, 40));
    let receipt = adapter.receive(&mut camera).unwrap();

    let extracted = adapter
        .extract(std::slice::from_ref(&receipt.objects_root))
        .unwrap();

    assert_eq!(adapter.provider(), ImportProvider::Camera);
    assert_eq!(extracted.entries.len(), 2, "one stack and one single");
    let paths: Vec<&PathBuf> = extracted
        .entries
        .iter()
        .map(SourceEntry::primary_path)
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
    let stacked = extracted
        .entries
        .iter()
        .find(|e| e.candidate.source_paths.len() == 2)
        .expect("RAW+JPEG stack");
    assert_eq!(stacked.candidate.stack_type, Some(StackType::RawJpeg));
    assert!(
        extracted
            .entries
            .iter()
            .all(|e| e.metadata == ExtractedMetadata::default())
    );
}

#[test]
fn extract_refuses_no_parts_and_a_missing_root() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path());
    assert!(matches!(adapter.extract(&[]), Err(AdapterError::NoParts)));
    assert!(matches!(
        adapter.extract(&[tmp.path().join("absent")]),
        Err(AdapterError::Io { .. })
    ));
}

#[test]
fn reimporting_a_camera_is_deduplicated_by_content() {
    let tmp = TempDir::new().unwrap();
    let adapter = CameraAdapter::new(tmp.path().join("stage"));
    let jpeg = shot(1, 120);
    let mut camera =
        FakeCamera::new(&tmp.path().join("stage")).file(0x20, DCIM_100, "DSC00001.JPG", &jpeg);
    let receipt = adapter.receive(&mut camera).unwrap();
    let scan = adapter
        .extract(std::slice::from_ref(&receipt.objects_root))
        .unwrap()
        .to_scan_result();

    let db = DatabaseDriver::open_in_memory().unwrap();
    let first = plan(&scan, &db, &ImportConfig::default()).unwrap();
    assert_eq!(first.counts.to_import, 1);

    db.insert_asset(&crate::db::rows::AssetRow {
        uuid: "already-imported".to_string(),
        asset_type: "photo".to_string(),
        capture_timestamp: 1,
        capture_utc: None,
        capture_tz_source: None,
        import_timestamp: 1,
        hash_sha256: receipt.received[0].sha256.to_hex(),
        width: None,
        height: None,
        duration_ms: None,
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: None,
        rating: 0,
        is_deleted: false,
        deleted_at: None,
        is_hidden: false,
    })
    .unwrap();

    let again = plan(&scan, &db, &ImportConfig::default()).unwrap();
    assert_eq!(again.counts.duplicates, 1);
    assert!(matches!(
        again.actions[0].1,
        ImportDecision::SkipDuplicate { .. }
    ));
}

#[test]
fn fatal_and_per_object_errors_are_told_apart() {
    assert!(CameraError::Disconnected(String::new()).is_fatal());
    assert!(CameraError::Sink(io::Error::other("disk")).is_fatal());
    assert!(
        !CameraError::Refused {
            operation: String::new(),
            code: String::new()
        }
        .is_fatal()
    );
    assert!(
        !CameraError::ResumeUnsupported {
            handle: 1,
            offset: 1
        }
        .is_fatal()
    );
}
