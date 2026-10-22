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

    /// `owner`'s segment files, sorted by name.
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
