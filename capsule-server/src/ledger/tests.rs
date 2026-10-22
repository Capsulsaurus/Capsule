use std::sync::Arc;

use capsule_core::crypto::keys::HybridSigningKey;

use super::conformance::{self, Harness};
use super::*;
use crate::attestation::LocalAttestationKey;

/// The in-memory double, tampered with through its raw seam.
#[derive(Debug, Default)]
struct Memory {
    ledger: InMemoryLedger,
}

impl Harness for Memory {
    fn ledger(&self) -> &dyn OwnerLedger {
        &self.ledger
    }

    fn raw<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, Vec<u8>> {
        Box::pin(async move { Ok(self.ledger.raw(owner)) })
    }

    fn replace_raw<'a>(&'a self, owner: &'a OwnerId, bytes: Vec<u8>) -> LedgerFuture<'a, ()> {
        Box::pin(async move {
            self.ledger.replace_raw(owner, bytes);
            Ok(())
        })
    }
}

/// The filesystem adapter on a directory that disappears with the test.
#[derive(Debug)]
struct Disk {
    ledger: FilesystemLedger,
    _root: tempfile::TempDir,
}

impl Disk {
    async fn open() -> Self {
        Self::with_segment_bytes(fs::DEFAULT_SEGMENT_BYTES).await
    }

    async fn with_segment_bytes(segment_bytes: u64) -> Self {
        let root = tempfile::tempdir().expect("a temporary directory");
        let ledger = FilesystemLedger::with_segment_bytes(root.path(), segment_bytes)
            .await
            .expect("a fresh ledger opens");
        Self {
            ledger,
            _root: root,
        }
    }

    /// `owner`'s segment files, sorted by name; the lock file and the attachment directory
    /// beside them are not segments.
    fn segment_names(&self, owner: &OwnerId) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.ledger.owner_dir(owner))
            .expect("the owner has a directory")
            .map(|entry| {
                entry
                    .expect("a directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| std::path::Path::new(name).extension() == Some("log".as_ref()))
            .collect();
        names.sort();
        names
    }
}

impl Harness for Disk {
    fn ledger(&self) -> &dyn OwnerLedger {
        &self.ledger
    }

    fn raw<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let mut names = self.segment_names(owner);
            names.sort_by_key(|name| {
                name.trim_end_matches(".log")
                    .parse::<u64>()
                    .expect("a segment name")
            });
            let mut bytes = Vec::new();
            for name in names {
                bytes.extend(
                    std::fs::read(self.ledger.owner_dir(owner).join(name)).expect("a segment"),
                );
            }
            Ok(bytes)
        })
    }

    fn replace_raw<'a>(&'a self, owner: &'a OwnerId, bytes: Vec<u8>) -> LedgerFuture<'a, ()> {
        Box::pin(async move {
            let dir = self.ledger.owner_dir(owner);
            for name in self.segment_names(owner) {
                std::fs::remove_file(dir.join(name)).expect("a segment is removable");
            }
            std::fs::write(dir.join("1.log"), bytes).expect("a segment is writable");
            self.ledger.forget(owner).await;
            Ok(())
        })
    }
}

/// Declares one test per conformance case on each adapter, each on a fresh harness.
macro_rules! conformance_cases {
    ($($case:ident),+ $(,)?) => {
        mod in_memory {
            use super::*;
            $(
                #[tokio::test]
                async fn $case() {
                    conformance::$case(&Memory::default()).await;
                }
            )+
        }
        mod on_disk {
            use super::*;
            $(
                #[tokio::test]
                async fn $case() {
                    conformance::$case(&Disk::open().await).await;
                }
            )+
        }
    };
}

conformance_cases! {
    the_first_entry_opens_the_chain,
    entries_read_back_as_signed_and_verify_in_order,
    owners_have_separate_chains,
    concurrent_appends_never_fork_the_chain,
    an_edited_entry_is_detected,
    a_removed_entry_is_detected,
    a_dropped_head_is_detected,
    a_torn_tail_is_reported_and_cut_by_the_next_append,
    a_zero_filled_tail_is_cut_by_the_next_append,
    an_entry_from_a_newer_server_is_refused_not_cut,
    a_tail_cut_at_a_boundary_is_caught_by_an_anchor,
    an_unpublished_key_is_detected,
    a_damaged_ledger_refuses_appends,
    a_malformed_owner_is_refused,
}

#[tokio::test]
async fn the_whole_suite_passes_in_one_pass_on_each_adapter() {
    conformance::run_all(&Memory::default()).await;
    conformance::run_all(&Disk::open().await).await;
}

fn key() -> LocalAttestationKey {
    LocalAttestationKey::new("capsule.test", HybridSigningKey::generate())
}

fn event(tag: u64) -> LedgerEvent {
    LedgerEvent::Lifecycle {
        asset_id: format!("asset-{tag}"),
        action: "delete".to_owned(),
        manifest_hash: None,
        sync_seq: Some(tag),
    }
}

#[tokio::test]
async fn a_full_segment_rolls_over_to_one_named_for_its_first_entry() {
    let disk = Disk::with_segment_bytes(1).await;
    let signer = key();
    let owner = OwnerId::new("rolls-over");
    for tag in 1..=3 {
        disk.ledger
            .append(&owner, event(tag), Timestamp::UNIX_EPOCH, &signer)
            .await
            .expect("append");
    }
    assert_eq!(disk.segment_names(&owner), ["1.log", "2.log", "3.log"]);

    let scan = disk.ledger.scan(&owner).await.expect("scan");
    let report = verify_chain(
        &owner,
        &scan,
        &|id| (*id == signer.key_id()).then(|| signer.verifying_key()),
        None,
    );
    assert_eq!(report.fault, None, "the chain verifies across segments");
    assert_eq!(report.verified, 3);
}

#[tokio::test]
async fn a_reopened_ledger_continues_the_chain_it_finds_on_disk() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let signer = key();
    let owner = OwnerId::new("reopened");
    let first = FilesystemLedger::open(root.path()).await.expect("open");
    let written = first
        .append(&owner, event(1), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append");
    drop(first);

    let second = FilesystemLedger::open(root.path()).await.expect("reopen");
    let next = second
        .append(&owner, event(2), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append after a restart");
    assert_eq!(next.core.seq, 2);
    assert_eq!(next.core.prior_entry_hash, Some(written.chain_hash()));
    assert!(root.path().join("ledger/reopened/1.log").is_file());
    assert_eq!(
        second.owners().await.expect("owners"),
        [owner],
        "the owner listing reads the directory"
    );
}

/// Two adapters on one blob root, as the server and a `purge --apply` run from cron are (#767):
/// each holds its own in-process lock and cached tail, so only the cross-process lock and the
/// on-disk re-check keep them on one chain.
async fn two_writers(
    segment_bytes: u64,
) -> (tempfile::TempDir, FilesystemLedger, FilesystemLedger) {
    let root = tempfile::tempdir().expect("a temporary directory");
    let server = FilesystemLedger::with_segment_bytes(root.path(), segment_bytes)
        .await
        .expect("open");
    let purge = FilesystemLedger::with_segment_bytes(root.path(), segment_bytes)
        .await
        .expect("open a second time");
    (root, server, purge)
}

/// `owner`'s chain verifies end to end under `signer`, holding `expected` entries.
async fn assert_one_chain(
    ledger: &FilesystemLedger,
    owner: &OwnerId,
    signer: &LocalAttestationKey,
    expected: usize,
) {
    let scan = ledger.scan(owner).await.expect("scan");
    assert_eq!(scan.damage, None, "nothing torn or cut");
    let report = verify_chain(
        owner,
        &scan,
        &|id| (*id == signer.key_id()).then(|| signer.verifying_key()),
        None,
    );
    assert_eq!(report.fault, None, "one chain, not two forks");
    assert_eq!(report.verified, expected);
    let seqs: Vec<u64> = scan.entries.iter().map(|entry| entry.core.seq).collect();
    assert_eq!(seqs, (1..=expected as u64).collect::<Vec<_>>());
}

#[tokio::test]
async fn two_adapters_on_one_root_appending_alternately_keep_one_chain() {
    for segment_bytes in [fs::DEFAULT_SEGMENT_BYTES, 1] {
        let (_root, server, purge) = two_writers(segment_bytes).await;
        let signer = key();
        let owner = OwnerId::new("shared-root");
        for tag in 1..=6 {
            let writer = if tag % 2 == 1 { &server } else { &purge };
            let entry = writer
                .append(&owner, event(tag), Timestamp::UNIX_EPOCH, &signer)
                .await
                .expect("append");
            assert_eq!(
                entry.core.seq, tag,
                "each writer sees the other's append before taking a position"
            );
        }
        assert_one_chain(&server, &owner, &signer, 6).await;
        assert_one_chain(&purge, &owner, &signer, 6).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_adapters_on_one_root_appending_concurrently_keep_one_chain() {
    let (_root, server, purge) = two_writers(fs::DEFAULT_SEGMENT_BYTES).await;
    let server = Arc::new(server);
    let purge = Arc::new(purge);
    let signer = Arc::new(key());
    let owner = OwnerId::new("shared-root");
    let mut tasks = Vec::new();
    for tag in 1..=40_u64 {
        let writer = if tag % 2 == 1 {
            server.clone()
        } else {
            purge.clone()
        };
        let signer = signer.clone();
        let owner = owner.clone();
        tasks.push(tokio::spawn(async move {
            writer
                .append(&owner, event(tag), Timestamp::UNIX_EPOCH, signer.as_ref())
                .await
                .expect("append")
        }));
    }
    for task in tasks {
        task.await.expect("the task ran");
    }
    assert_one_chain(&server, &owner, &signer, 40).await;
}

/// Hold `owner`'s cross-process lock from outside the adapter, as another process would.
fn hold_lock_file(ledger: &FilesystemLedger, owner: &OwnerId) -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(ledger.owner_dir(owner).join(fs::LOCK_FILE))
        .expect("the owner's lock file exists once it has appended");
    file.lock().expect("the lock is free");
    file
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_append_and_a_scan_wait_for_a_lock_held_on_the_lock_file() {
    let (_root, server, purge) = two_writers(fs::DEFAULT_SEGMENT_BYTES).await;
    let server = Arc::new(server);
    let signer = Arc::new(key());
    let owner = OwnerId::new("held");
    server
        .append(&owner, event(1), Timestamp::UNIX_EPOCH, signer.as_ref())
        .await
        .expect("append");

    let held = hold_lock_file(&purge, &owner);
    let appending = tokio::spawn({
        let (server, signer, owner) = (server.clone(), signer.clone(), owner.clone());
        async move {
            server
                .append(&owner, event(2), Timestamp::UNIX_EPOCH, signer.as_ref())
                .await
        }
    });
    let scanning = tokio::spawn({
        let owner = owner.clone();
        async move { purge.scan(&owner).await }
    });
    let window = std::time::Duration::from_millis(200);
    tokio::time::sleep(window).await;
    assert!(
        !appending.is_finished(),
        "an append waits while another process holds the lock file"
    );
    assert!(
        !scanning.is_finished(),
        "a scan waits while another process holds the lock file"
    );

    drop(held);
    let entry = appending
        .await
        .expect("the task ran")
        .expect("the append proceeds once the lock is released");
    assert_eq!(entry.core.seq, 2);
    let scan = scanning
        .await
        .expect("the task ran")
        .expect("the scan proceeds once the lock is released");
    assert_eq!(scan.damage, None);
    assert_one_chain(&server, &owner, &signer, 2).await;
}

#[tokio::test]
async fn the_lock_file_is_never_read_as_a_segment_or_an_owner() {
    let disk = Disk::open().await;
    let signer = key();
    let owner = OwnerId::new("locked");
    disk.ledger
        .append(&owner, event(1), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append");
    disk.ledger
        .attach(&owner, Hash32::from_bytes([7; 32]), b"kept".to_vec())
        .await
        .expect("attach");
    let owner_dir = disk.ledger.owner_dir(&owner);
    assert!(
        owner_dir.join(fs::LOCK_FILE).is_file(),
        "the append took its lock on a file in the owner's directory"
    );
    assert!(
        owner_dir.join(fs::ATTACHMENT_DIR).is_dir(),
        "the lock file sits beside the attachment directory, not in it"
    );
    assert_one_chain(&disk.ledger, &owner, &signer, 1).await;
    assert_eq!(
        disk.ledger
            .append(&owner, event(2), Timestamp::UNIX_EPOCH, &signer)
            .await
            .expect("append after an attachment")
            .core
            .seq,
        2
    );
    assert_one_chain(&disk.ledger, &owner, &signer, 2).await;
    assert_eq!(
        disk.ledger.owners().await.expect("owners"),
        std::slice::from_ref(&owner)
    );

    // An owner whose first append failed after taking the lock holds the lock file alone: no
    // ledger, so neither a listed owner nor a damaged one.
    let empty = OwnerId::new("lock-only");
    let dir = disk.ledger.owner_dir(&empty);
    std::fs::create_dir(&dir).expect("create");
    std::fs::write(dir.join(fs::LOCK_FILE), b"").expect("write");
    assert_eq!(disk.ledger.owners().await.expect("owners"), [owner]);
    let scan = disk.ledger.scan(&empty).await.expect("scan");
    assert!(scan.entries.is_empty());
    assert_eq!(scan.damage, None);
    let first = disk
        .ledger
        .append(&empty, event(1), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append beside an existing lock file");
    assert_eq!(first.core.seq, 1);
}

/// Make `path` read-only (`true`) or writable again, returning whether this process is now
/// refused a write there; a privileged process is not, and a test then has nothing to prove.
#[cfg(unix)]
fn read_only(path: &std::path::Path, read_only: bool, probe: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mode = match (path.is_dir(), read_only) {
        (true, true) => 0o555,
        (true, false) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(probe)
        .is_err()
}

#[cfg(unix)]
#[tokio::test]
async fn a_reader_without_write_access_still_scans() {
    let disk = Disk::open().await;
    let signer = key();

    // A ledger whose lock file exists but is not writable to the reader: shared, read-only.
    let locked = OwnerId::new("read-only-lock");
    disk.ledger
        .append(&locked, event(1), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append");
    let lock_file = disk.ledger.owner_dir(&locked).join(fs::LOCK_FILE);
    if read_only(&lock_file, true, &lock_file) {
        assert_one_chain(&disk.ledger, &locked, &signer, 1).await;
    }
    read_only(&lock_file, false, &lock_file);

    // A ledger from before the lock file, in a directory the reader cannot create it in.
    let legacy = OwnerId::new("read-only-dir");
    disk.ledger
        .append(&legacy, event(1), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append");
    let dir = disk.ledger.owner_dir(&legacy);
    std::fs::remove_file(dir.join(fs::LOCK_FILE)).expect("remove");
    if read_only(&dir, true, &dir.join(fs::LOCK_FILE)) {
        assert_one_chain(&disk.ledger, &legacy, &signer, 1).await;
        assert!(
            matches!(
                disk.ledger
                    .append(&legacy, event(2), Timestamp::UNIX_EPOCH, &signer)
                    .await,
                Err(LedgerError::Backend { .. })
            ),
            "a writer that cannot take the lock refuses to append"
        );
    }
    read_only(&dir, false, &dir.join(fs::LOCK_FILE));
}

/// A scrub or rebuild may run as another user than the server; a lock file it created would be
/// one the server could not open for writing, refusing that owner's appends.
#[tokio::test]
async fn a_scan_never_creates_the_lock_file() {
    let disk = Disk::open().await;
    let signer = key();
    let owner = OwnerId::new("no-lock-yet");
    disk.ledger
        .append(&owner, event(1), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append");
    let lock_file = disk.ledger.owner_dir(&owner).join(fs::LOCK_FILE);
    // A ledger written before the lock file existed.
    std::fs::remove_file(&lock_file).expect("remove");

    assert_one_chain(&disk.ledger, &owner, &signer, 1).await;
    assert!(
        !lock_file.exists(),
        "a scan reads unlocked rather than create the lock file"
    );

    disk.ledger
        .append(&owner, event(2), Timestamp::UNIX_EPOCH, &signer)
        .await
        .expect("append");
    assert!(lock_file.is_file(), "the next append creates it");
    assert_one_chain(&disk.ledger, &owner, &signer, 2).await;
}

#[tokio::test]
async fn a_scan_of_an_owner_with_no_directory_creates_nothing() {
    let disk = Disk::open().await;
    let owner = OwnerId::new("absent");
    let scan = disk.ledger.scan(&owner).await.expect("scan");
    assert!(scan.entries.is_empty());
    assert!(
        !disk.ledger.owner_dir(&owner).exists(),
        "a read leaves no directory or lock file behind"
    );
}

#[tokio::test]
async fn a_torn_frame_in_an_earlier_segment_is_a_hole_not_a_tail() {
    let disk = Disk::with_segment_bytes(1).await;
    let signer = key();
    let owner = OwnerId::new("hole");
    for tag in 1..=2 {
        disk.ledger
            .append(&owner, event(tag), Timestamp::UNIX_EPOCH, &signer)
            .await
            .expect("append");
    }
    let first = disk.ledger.owner_dir(&owner).join("1.log");
    let mut bytes = std::fs::read(&first).expect("read");
    bytes.truncate(bytes.len() - 3);
    std::fs::write(&first, bytes).expect("write");
    disk.ledger.forget(&owner).await;

    let scan = disk.ledger.scan(&owner).await.expect("scan");
    assert_eq!(scan.damage, Some(ChainFault::Undecodable { entries: 0 }));
    assert!(matches!(
        disk.ledger
            .append(&owner, event(3), Timestamp::UNIX_EPOCH, &signer)
            .await,
        Err(LedgerError::Damaged { .. })
    ));
}

#[test]
fn an_oversized_or_non_canonical_frame_is_not_an_entry() {
    let signer = key();
    let valid = encode_frame(&sign_entry(
        &OwnerId::new("frame"),
        None,
        event(1),
        Timestamp::UNIX_EPOCH,
        &signer,
    ));

    let mut oversized = (MAX_FRAME_BYTES + 1).to_be_bytes().to_vec();
    oversized.extend_from_slice(&[0; 8]);
    assert_eq!(
        decode_frames(&oversized).damage,
        Some(ChainFault::TornTail { entries: 0 }),
        "bad bytes with no entry after them were never acknowledged"
    );
    assert_eq!(
        decode_frames(&[oversized.as_slice(), &valid].concat()).damage,
        Some(ChainFault::Undecodable { entries: 0 }),
        "bad bytes an entry follows are a hole in the chain"
    );

    // A whole frame of bytes that are CBOR but not an entry.
    let body = capsule_core::cbor::to_canonical_vec(&"not an entry").expect("encodes");
    let mut frame = u32::try_from(body.len())
        .expect("small")
        .to_be_bytes()
        .to_vec();
    frame.extend_from_slice(&body);
    assert_eq!(
        decode_frames(&frame).damage,
        Some(ChainFault::TornTail { entries: 0 })
    );
    assert_eq!(
        decode_frames(&[frame.as_slice(), &valid].concat()).damage,
        Some(ChainFault::Undecodable { entries: 0 })
    );

    // A crash that extended the file without writing the frame: zeroes after a whole entry.
    let zero_filled = [valid.as_slice(), &[0; 300]].concat();
    let decoded = decode_frames(&zero_filled);
    assert_eq!(decoded.damage, Some(ChainFault::TornTail { entries: 1 }));
    assert_eq!(decoded.whole_len, valid.len());

    assert_eq!(
        decode_frames(&[0, 0]).damage,
        Some(ChainFault::TornTail { entries: 0 }),
        "a length prefix cut short is a torn tail"
    );
    assert_eq!(
        decode_frames(&[]),
        Decoded {
            entries: Vec::new(),
            whole_len: 0,
            damage: None
        }
    );
}

/// The core a newer server would sign for an event kind this server has never heard of.
#[derive(serde::Serialize)]
struct CoreWithUnknownKind<'a> {
    version: &'a str,
    server_id: &'a str,
    server_key_id: Hash32,
    owner_id: &'a str,
    seq: u64,
    recorded_at: &'a str,
    event: UnknownKind,
}

#[derive(serde::Serialize)]
struct UnknownKind {
    kind: &'static str,
    peer: &'static str,
}

/// A frame holding a newer server's entry with an event kind this server cannot decode.
fn unknown_kind_frame(owner: &OwnerId, signer: &LocalAttestationKey) -> Vec<u8> {
    conformance::newer_frame(
        &CoreWithUnknownKind {
            version: LEDGER_VERSION,
            server_id: signer.server_id(),
            server_key_id: signer.key_id(),
            owner_id: owner.as_str(),
            seq: 1,
            recorded_at: "1970-01-01T00:00:00Z",
            event: UnknownKind {
                kind: "replicated",
                peer: "peer-1",
            },
        },
        signer,
    )
}

#[test]
fn an_intact_frame_this_server_cannot_read_is_unrecognised_not_torn() {
    let signer = key();
    let owner = OwnerId::new("frame");
    let valid = encode_frame(&sign_entry(
        &owner,
        None,
        event(1),
        Timestamp::UNIX_EPOCH,
        &signer,
    ));
    let newer = unknown_kind_frame(&owner, &signer);

    let decoded = decode_frames(&[valid.as_slice(), &newer].concat());
    assert_eq!(
        decoded.damage,
        Some(ChainFault::Unrecognised { entries: 1 }),
        "an unknown kind at the tail is a newer server's entry, not a torn write"
    );
    assert_eq!(decoded.entries.len(), 1);
    assert_eq!(decoded.whole_len, valid.len());

    assert_eq!(
        decode_frames(&[valid.as_slice(), &newer, &[0; 64]].concat()).damage,
        Some(ChainFault::Unrecognised { entries: 1 }),
        "a torn write after a newer server's entry does not make it cuttable"
    );
    assert_eq!(
        decode_frames(&[valid.as_slice(), &[0, 0, 0, 9, 1, 2], &newer].concat()).damage,
        Some(ChainFault::Undecodable { entries: 1 }),
        "garbage a newer server's entry follows is a hole, not a tail"
    );

    // The same frame cut short, with its structure destroyed, or with bytes after its one item,
    // was never acknowledged.
    let short = &newer[..newer.len() - 1];
    assert_eq!(
        decode_frames(&[valid.as_slice(), short].concat()).damage,
        Some(ChainFault::TornTail { entries: 1 })
    );
    let mut trailing = newer.clone();
    let len = u32::from_be_bytes([trailing[0], trailing[1], trailing[2], trailing[3]]) + 1;
    trailing[..4].copy_from_slice(&len.to_be_bytes());
    trailing.push(0);
    assert_eq!(
        decode_frames(&[valid.as_slice(), &trailing].concat()).damage,
        Some(ChainFault::TornTail { entries: 1 }),
        "a body with bytes after its one CBOR item is not an intact frame"
    );
    let mut unstructured = newer.clone();
    unstructured[4] = 0xff;
    assert_eq!(
        decode_frames(&[valid.as_slice(), &unstructured].concat()).damage,
        Some(ChainFault::TornTail { entries: 1 }),
        "a body whose map header is destroyed is not CBOR, so not an intact frame"
    );

    // A byte flipped inside a value, by contrast, leaves the frame intact: it is refused, never
    // cut, because nothing can tell it from an entry a newer server acknowledged.
    let mut flipped = newer.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x01;
    assert_eq!(
        decode_frames(&[valid.as_slice(), &flipped].concat()).damage,
        Some(ChainFault::Unrecognised { entries: 1 }),
        "a flipped signature byte keeps the frame canonical and entry-shaped"
    );
}

#[tokio::test]
async fn an_unrecognised_frame_in_an_earlier_segment_keeps_its_fault() {
    let disk = Disk::with_segment_bytes(1).await;
    let signer = key();
    let owner = OwnerId::new("newer-segment");
    for tag in 1..=2 {
        disk.ledger
            .append(&owner, event(tag), Timestamp::UNIX_EPOCH, &signer)
            .await
            .expect("append");
    }
    let first = disk.ledger.owner_dir(&owner).join("1.log");
    std::fs::write(&first, unknown_kind_frame(&owner, &signer)).expect("write");
    disk.ledger.forget(&owner).await;

    let scan = disk.ledger.scan(&owner).await.expect("scan");
    assert_eq!(scan.damage, Some(ChainFault::Unrecognised { entries: 0 }));
    assert!(matches!(
        disk.ledger
            .append(&owner, event(3), Timestamp::UNIX_EPOCH, &signer)
            .await,
        Err(LedgerError::Damaged {
            fault: ChainFault::Unrecognised { entries: 0 },
            ..
        })
    ));
}

#[test]
fn an_entry_round_trips_through_its_frame() {
    let signer = key();
    let owner = OwnerId::new("frame");
    let entry = sign_entry(&owner, None, event(1), Timestamp::UNIX_EPOCH, &signer);
    let decoded = decode_frames(&encode_frame(&entry));
    assert_eq!(decoded.entries, [entry]);
    assert_eq!(decoded.damage, None);
}

#[test]
fn an_owner_id_must_be_a_name_every_adapter_can_carry() {
    assert!(check_owner_id(&OwnerId::new("019286c1-3f7a-7c11-9d2e-6f5a1b2c3d4e")).is_ok());
    assert!(check_owner_id(&OwnerId::new("a".repeat(MAX_OWNER_ID_LEN))).is_ok());
    assert!(check_owner_id(&OwnerId::new("a".repeat(MAX_OWNER_ID_LEN + 1))).is_err());
    assert!(check_owner_id(&OwnerId::new("..")).is_err());
}
