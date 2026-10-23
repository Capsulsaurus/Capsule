use super::super::conformance::{self, Harness};
use super::*;

/// A store on a directory that disappears with the test.
///
/// No temporary-directory crate: one `remove_dir_all` in `Drop` is the whole requirement, and
/// a dependency for it would need a row in a design doc this change does not own.
#[derive(Debug)]
struct TempStore {
    store: FilesystemBlobStore,
    root: PathBuf,
}

impl TempStore {
    async fn open() -> Self {
        let root = std::env::temp_dir().join(format!(
            "capsule-blob-conformance-{}",
            uuid::Uuid::now_v7().as_simple()
        ));
        let store = FilesystemBlobStore::open(&root)
            .await
            .expect("a fresh blob store opens");
        Self { store, root }
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Harness for TempStore {
    fn store(&self) -> &dyn BlobStore {
        &self.store
    }

    fn plant_debris(&self, relative: &str) -> BlobFuture<'_, ()> {
        let path = blobs_dir(&self.root).join(relative);
        Box::pin(async move {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| backend("create a debris directory", &error))?;
            }
            tokio::fs::write(&path, b"not a blob")
                .await
                .map_err(|error| backend("plant debris", &error))
        })
    }
}

/// Declares one test per conformance case, each on its own tree.
macro_rules! conformance_cases {
    ($($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                conformance::$case(&TempStore::open().await).await;
            }
        )+
    };
}

conformance_cases! {
    staging_appends_only_at_the_end_and_tracks_its_length,
    an_append_at_the_wrong_offset_is_refused_and_names_the_resume_point,
    truncating_a_stage_cuts_it_back_and_never_grows_it,
    appending_to_an_upload_that_was_never_begun_is_not_staged,
    abandoning_removes_the_staged_bytes_and_says_whether_there_were_any,
    the_staged_listing_is_ordered_and_holds_only_open_uploads,
    an_upload_id_that_cannot_name_a_file_is_refused_by_every_operation,
    committing_places_the_staged_bytes_at_their_content_address,
    committing_onto_an_occupied_address_keeps_the_bytes_already_there,
    committing_an_upload_that_was_never_staged_is_not_staged,
    staged_bytes_are_not_a_blob_until_they_are_committed,
    put_stores_bytes_at_its_address_and_never_overwrites,
    an_absent_address_stats_and_reads_as_none,
    a_ranged_read_returns_exactly_its_window_and_clamps_at_the_end,
    a_staged_upload_reads_back_in_windows,
    enumeration_yields_every_blob_in_content_address_order,
    enumeration_resumes_from_its_cursor_without_gaps_or_repeats,
    an_empty_store_enumerates_to_nothing_rather_than_failing,
    a_partially_populated_shard_tree_enumerates_completely,
    enumeration_reports_what_is_not_a_blob_as_debris,
    removing_a_blob_drops_it_from_lookup_and_from_enumeration,
    quarantining_pulls_a_blob_out_of_the_store_and_records_why,
    quarantining_an_absent_address_holds_nothing,
}

/// The whole suite, in one pass on one tree.
#[tokio::test]
async fn the_whole_suite_passes_in_one_pass() {
    conformance::run_all(&TempStore::open().await).await;
}

fn address(hex: &str) -> ContentAddress {
    let mut hex = hex.to_owned();
    hex.push_str(&"0".repeat(64 - hex.len()));
    ContentAddress::parse(&hex).expect("a fixture address")
}

/// A disk that fills mid-append leaves the stage where it was, and the retry lands.
///
/// The wedge #694 reports: the short write used to stay on disk, so the stage ended past the
/// session's offset and every later chunk met an offset mismatch until the session expired.
#[tokio::test]
async fn a_short_write_on_a_full_disk_is_cut_back_and_the_retry_lands() {
    let temp = TempStore::open().await;
    let upload = UploadId::new("short-write");
    temp.store.begin(&upload).await.expect("begin");
    temp.store
        .append(&upload, 0, b"0123")
        .await
        .expect("the first append lands");

    *temp
        .store
        .short_write
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(3);
    match temp.store.append(&upload, 4, b"456789").await {
        Err(BlobError::StorageFull { .. }) => {}
        other => panic!("a full disk is StorageFull, not {other:?}"),
    }
    assert_eq!(
        temp.store.staged_len(&upload).await.expect("staged_len"),
        Some(4),
        "the three bytes that did reach the disk were cut away again"
    );

    assert_eq!(
        temp.store
            .append(&upload, 4, b"456789")
            .await
            .expect("the retry at the same offset lands"),
        10
    );
    assert_eq!(
        temp.store
            .read_staged_at(&upload, 0, 64)
            .await
            .expect("read_staged_at"),
        Some(b"0123456789".to_vec())
    );
}

/// An `ENOSPC` from any operation is the port's `StorageFull`, never a backend detail.
#[test]
fn a_full_filesystem_is_storage_full_wherever_it_is_met() {
    assert!(matches!(
        backend(
            "create a staging file",
            &std::io::Error::from(ErrorKind::StorageFull)
        ),
        BlobError::StorageFull {
            operation: "create a staging file"
        }
    ));
    assert!(matches!(
        backend(
            "create a staging file",
            &std::io::Error::from(ErrorKind::PermissionDenied)
        ),
        BlobError::Backend { .. }
    ));
}

/// Opening a root builds the three directories the layout names, and nothing else.
#[tokio::test]
async fn opening_a_root_creates_the_layout_and_leaves_server_metadata_alone() {
    let temp = TempStore::open().await;

    for directory in ["blobs", "incoming", "quarantine"] {
        assert!(
            temp.root.join(directory).is_dir(),
            "the layout's `{directory}/` must exist"
        );
    }
    assert!(
        !temp.root.join(".server").exists(),
        "`.server/` is the operator's plaintext metadata; this port neither creates nor reads it"
    );
}

/// The layout round-trip design/filesystem/server.md asks for, asserted against the real path.
#[tokio::test]
async fn a_committed_blob_lives_at_its_sharded_address_and_never_at_a_flat_one() {
    let temp = TempStore::open().await;
    let upload = UploadId::new("layout-round-trip");
    let address = address("abcdef12");
    let bytes = b"opaque ciphertext".to_vec();

    temp.store.begin(&upload).await.expect("begin");
    temp.store.append(&upload, 0, &bytes).await.expect("append");
    temp.store.commit(&upload, &address).await.expect("commit");

    let sharded = temp
        .root
        .join("blobs")
        .join("ab")
        .join("cd")
        .join(format!("{address}.bin"));
    assert!(
        sharded.is_file(),
        "the blob must live at its two-level shard"
    );
    assert!(
        !temp
            .root
            .join("blobs")
            .join(format!("{address}.bin"))
            .exists(),
        "and never at the flat address the shard overturned"
    );
    assert_eq!(
        std::fs::read(&sharded).expect("read the blob back off disk"),
        bytes,
        "the bytes on disk are the bytes that were staged"
    );
    assert!(
        !temp
            .root
            .join("incoming")
            .join("layout-round-trip.bin")
            .exists(),
        "the staging file is the temp file, and the rename consumed it"
    );
}

/// A whole-blob write leaves no temp file, inside the shard or anywhere else.
#[tokio::test]
async fn a_direct_put_leaves_no_temporary_file_behind() {
    let temp = TempStore::open().await;
    let address = address("beef0011");
    temp.store
        .put(&address, b"a manifest envelope")
        .await
        .expect("put");

    let shard = temp.root.join("blobs").join("be").join("ef");
    let names: Vec<String> = std::fs::read_dir(&shard)
        .expect("the shard exists")
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    assert_eq!(
        names,
        vec![address.file_name()],
        "the shard holds the blob and nothing else"
    );
    let (_, debris) = {
        let page = temp.store.enumerate(None, 64).await.expect("enumerate");
        (page.entries, page.debris)
    };
    assert!(debris.is_empty(), "and the walk sees no debris: {debris:?}");
}

/// A crashed `put` leaves its temp inside the shard, where the walk inventories it.
#[tokio::test]
async fn a_temporary_file_left_by_a_crash_is_debris_inside_its_shard() {
    let temp = TempStore::open().await;
    let address = address("beef0011");
    temp.store
        .put(&address, b"a manifest envelope")
        .await
        .expect("put");

    let orphan = format!(".{address}.0191ab.tmp");
    std::fs::write(
        temp.root.join("blobs").join("be").join("ef").join(&orphan),
        b"half a blob",
    )
    .expect("plant a crashed temp file");

    let page = temp.store.enumerate(None, 64).await.expect("enumerate");
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.address.clone())
            .collect::<Vec<_>>(),
        vec![address],
        "a temp file is not a blob"
    );
    assert_eq!(page.debris, vec![format!("be/ef/{orphan}")]);
}

/// A blob filed under a shard it does not derive is debris, never served as an entry.
#[tokio::test]
async fn a_blob_filed_under_the_wrong_shard_is_debris() {
    let temp = TempStore::open().await;
    let address = address("cc99");
    let wrong = temp.root.join("blobs").join("ab").join("cd");
    std::fs::create_dir_all(&wrong).expect("make a shard by hand");
    std::fs::write(wrong.join(address.file_name()), b"misfiled").expect("misfile a blob");

    let page = temp.store.enumerate(None, 64).await.expect("enumerate");
    assert!(
        page.entries.is_empty(),
        "answering one content address with another's bytes is the failure the shard check exists for"
    );
    assert_eq!(page.debris, vec![format!("ab/cd/{}", address.file_name())]);
    assert_eq!(
        temp.store.stat(&address).await.expect("stat"),
        None,
        "and it is not reachable by lookup either"
    );
}

/// A directory name the layout could not have written is debris, and does not stop the walk.
#[tokio::test]
async fn a_shard_directory_that_is_not_hex_is_debris() {
    let temp = TempStore::open().await;
    let address = address("112233");
    temp.store.put(&address, b"a real blob").await.expect("put");
    std::fs::create_dir_all(temp.root.join("blobs").join("zz")).expect("plant a bad shard");

    let page = temp.store.enumerate(None, 64).await.expect("enumerate");
    assert_eq!(
        page.entries.len(),
        1,
        "the walk keeps going past what it cannot read"
    );
    assert_eq!(page.debris, vec!["zz".to_owned()]);
}

/// Emptying a shard takes its directories with it.
#[tokio::test]
async fn removing_the_last_blob_in_a_shard_prunes_its_directories() {
    let temp = TempStore::open().await;
    let first = address("aabb01");
    let second = address("aabb02");
    temp.store.put(&first, b"one").await.expect("put");
    temp.store.put(&second, b"two").await.expect("put");

    let shard = temp.root.join("blobs").join("aa").join("bb");
    assert!(temp.store.remove(&first).await.expect("remove"));
    assert!(shard.is_dir(), "a shard with a blob left in it stays");

    assert!(temp.store.remove(&second).await.expect("remove"));
    assert!(
        !shard.exists(),
        "an empty shard is not left to slow every later walk"
    );
    assert!(!temp.root.join("blobs").join("aa").exists());
    assert!(
        temp.root.join("blobs").is_dir(),
        "the store itself is not pruned away with its last blob"
    );
}

/// Quarantine writes the sibling rejection record the design names.
#[tokio::test]
async fn a_quarantined_blob_keeps_a_sibling_reason_record() {
    let temp = TempStore::open().await;
    let address = address("dead0001");
    temp.store
        .put(&address, b"a malformed envelope")
        .await
        .expect("put");

    let reason = QuarantineReason {
        code: "error.upload.envelope_malformed".to_owned(),
        detail: "amk_version is absent".to_owned(),
        at: Timestamp::UNIX_EPOCH,
    };
    assert!(
        temp.store
            .quarantine(&address, reason.clone())
            .await
            .expect("quarantine")
    );

    let held = temp.root.join("quarantine");
    assert_eq!(
        std::fs::read(held.join(format!("{address}.bin"))).expect("the held bytes"),
        b"a malformed envelope".to_vec(),
        "the bytes are preserved, not dropped"
    );
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(held.join(format!("{address}.reason.json")))
            .expect("the sibling record"),
    )
    .expect("the record is JSON");
    assert_eq!(record["code"], "error.upload.envelope_malformed");
    assert_eq!(record["detail"], "amk_version is absent");
    assert_eq!(record["quarantined_at"], "1970-01-01T00:00:00Z");

    assert_eq!(
        temp.store.quarantined().await.expect("quarantined"),
        vec![QuarantinedBlob { address, reason }],
        "and the record round-trips back through the port"
    );
}

/// A rejection record an operator cannot read is a fault, reported rather than papered over.
#[tokio::test]
async fn an_unreadable_quarantine_record_is_reported_not_invented() {
    let temp = TempStore::open().await;
    let address = address("dead0002");
    temp.store.put(&address, b"held").await.expect("put");
    temp.store
        .quarantine(
            &address,
            QuarantineReason {
                code: "error.upload.envelope_malformed".to_owned(),
                detail: "detail".to_owned(),
                at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("quarantine");

    std::fs::write(
        temp.root
            .join("quarantine")
            .join(format!("{address}.reason.json")),
        b"{ not json",
    )
    .expect("corrupt the record");

    assert!(
        temp.store.quarantined().await.is_err(),
        "the listing says so rather than synthesizing a reason nobody wrote"
    );
    assert!(
        temp.root
            .join("quarantine")
            .join(format!("{address}.bin"))
            .is_file(),
        "the bytes are still preserved either way"
    );
}

/// The startup scrub sees every open stage and nothing a foreign name planted.
#[tokio::test]
async fn the_staged_listing_ignores_names_this_port_would_never_have_written() {
    let temp = TempStore::open().await;
    let upload = UploadId::new("scrub-me");
    temp.store.begin(&upload).await.expect("begin");
    std::fs::write(temp.root.join("incoming").join("not an upload.bin"), b"")
        .expect("plant a foreign name");
    std::fs::write(temp.root.join("incoming").join("no-suffix"), b"")
        .expect("plant a suffixless name");

    assert_eq!(temp.store.staged().await.expect("staged"), vec![upload]);
}
