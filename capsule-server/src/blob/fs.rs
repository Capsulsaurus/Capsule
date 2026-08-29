//! The filesystem blob store — the one backend design/filesystem/server.md requires.
//!
//! # Atomicity, and where every fsync goes
//!
//! Finalization is a `rename` and nothing else, which is atomic only because the whole tree is
//! on one filesystem (design/filesystem/server.md; the requirement is recorded in
//! `self-hosting.md`). The staging file *is* the temp file, so there is no copy step and no
//! `blobs/{hash}.tmp` beside the target.
//!
//! A rename is atomic but not yet **durable**: POSIX does not commit the new directory entry
//! until the containing directory is fsynced, and does not commit a newly created directory
//! until its parent is. [`FilesystemBlobStore::commit`] therefore runs, in order:
//!
//! 1. the staged bytes are already durable — every [`BlobStore::append`] fsyncs the file before
//!    it returns, which is what makes [`BlobStore::staged_len`] the truth a session's cached
//!    counter is reconciled up to;
//! 2. create the two shard directories, fsyncing the parent of each one this call actually
//!    created;
//! 3. `rename` the staged file onto its content address;
//! 4. fsync the shard directory the rename landed in.
//!
//! Only then may the caller commit its Postgres row. Skipping step 4 would trade the benign
//! failure for the dangerous one: a lost blob under a committed row is a **dangling reference**,
//! which design/filesystem/server.md makes a loud integrity error that is never auto-deleted,
//! whereas a blob whose row never committed is an orphan the refcount GC reclaims. The cost is
//! up to three extra directory fsyncs the first time a shard is touched and one per commit
//! after that. It is not a knob.
//!
//! Directory fsync is a Unix primitive — a directory is opened and `fsync`ed like a file — and
//! Capsule's server targets Unix. On any other platform the calls below compile to nothing and
//! the durability argument above does not hold; that platform is not a supported deployment.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::address::{
    BLOB_SUFFIX, ContentAddress, QUARANTINE_REASON_SUFFIX, blob_path, blobs_dir, incoming_dir,
    is_shard_segment, quarantine_dir, shard_dir,
};
use super::{
    BlobError, BlobFuture, BlobPage, BlobStat, BlobStore, Placement, QuarantineReason,
    QuarantinedBlob, check_upload_id, window,
};
use crate::store::UploadId;

/// Wrap an IO failure as the port's backend error.
fn backend(operation: &'static str, error: &std::io::Error) -> BlobError {
    BlobError::Backend {
        operation,
        detail: error.to_string(),
    }
}

/// Make a directory entry durable.
///
/// See the module docs: without this a `rename` or a `mkdir` can be lost by a crash even though
/// the call returned.
#[cfg(unix)]
async fn sync_dir(path: &Path) -> Result<(), BlobError> {
    let directory = tokio::fs::File::open(path)
        .await
        .map_err(|error| backend("open a directory to sync it", &error))?;
    directory
        .sync_all()
        .await
        .map_err(|error| backend("sync a directory", &error))
}

/// No-op: fsyncing a directory is a Unix primitive, and no other platform is a supported
/// deployment. See the module docs.
#[cfg(not(unix))]
async fn sync_dir(_path: &Path) -> Result<(), BlobError> {
    Ok(())
}

/// Create `path`, reporting whether this call is the one that made it.
///
/// The distinction is what keeps the parent fsync off the common path: only the creator owes it.
async fn create_dir(path: &Path) -> Result<bool, BlobError> {
    match tokio::fs::create_dir(path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(backend("create a shard directory", &error)),
    }
}

/// One directory entry, as the enumeration walk sees it.
struct Entry {
    name: String,
    is_dir: bool,
}

/// Every entry of `path`, sorted by name.
///
/// Sorted here rather than relied upon from the filesystem: `readdir` order is arbitrary, and
/// the port's ordering guarantee is what makes the enumeration cursor resumable.
async fn read_dir_sorted(path: &Path) -> Result<Vec<Entry>, BlobError> {
    let mut reader = match tokio::fs::read_dir(path).await {
        Ok(reader) => reader,
        // A shard tree is created on demand, so an absent directory is an empty one — this is
        // the "partially-populated tree enumerates without error" case.
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(backend("read a directory", &error)),
    };

    let mut entries = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|error| backend("read a directory entry", &error))?
    {
        let is_dir = entry
            .file_type()
            .await
            .map_err(|error| backend("read a directory entry's type", &error))?
            .is_dir();
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

/// The one blob store a Capsule deployment runs on.
#[derive(Debug)]
pub struct FilesystemBlobStore {
    root: PathBuf,
}

impl FilesystemBlobStore {
    /// Open the store rooted at `root`, creating its three directories if they are absent.
    ///
    /// `.server/` is deliberately not created or read here: the schema version and the operator's
    /// configuration are plaintext server metadata, not blobs, and nothing in this port reads
    /// them.
    ///
    /// # Errors
    ///
    /// [`BlobError::Backend`] if the tree cannot be created.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, BlobError> {
        let root = root.into();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|error| backend("create the blob root", &error))?;

        for directory in [blobs_dir(&root), incoming_dir(&root), quarantine_dir(&root)] {
            tokio::fs::create_dir_all(&directory)
                .await
                .map_err(|error| backend("create a blob store directory", &error))?;
        }
        sync_dir(&root).await?;

        tracing::info!(root = %root.display(), "opened the blob store");
        Ok(Self { root })
    }

    /// The tree this store owns.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The append-only staging file for `upload`.
    ///
    /// Flat, and bounded by concurrent sessions rather than by the store's history — see the
    /// port's module docs on what does and does not shard.
    fn staged_path(&self, upload: &UploadId) -> PathBuf {
        incoming_dir(&self.root).join(format!("{upload}{BLOB_SUFFIX}"))
    }

    /// Create `address`'s shard directories, fsyncing the parent of each one created.
    ///
    /// Returns the directory the blob belongs in. A concurrent creator may win the race for one
    /// level, in which case it owes the parent fsync and this call does not; either way the
    /// creator syncs before it renames.
    async fn ensure_shard(&self, address: &ContentAddress) -> Result<PathBuf, BlobError> {
        let blobs = blobs_dir(&self.root);
        let [first_name, second_name] = address.shard();

        let first = blobs.join(first_name);
        if create_dir(&first).await? {
            sync_dir(&blobs).await?;
        }

        let second = first.join(second_name);
        if create_dir(&second).await? {
            sync_dir(&first).await?;
        }

        Ok(second)
    }

    /// Drop `address`'s shard directories once they hold nothing.
    ///
    /// A purge that left the tree behind would keep charging every later enumeration for
    /// directories with nothing in them — the exact cost the shard exists to avoid. `remove_dir`
    /// refuses a non-empty directory, so this cannot race a concurrent write into the shard.
    async fn prune_shard(&self, address: &ContentAddress) {
        let blobs = blobs_dir(&self.root);
        let [first_name, _] = address.shard();
        let first = blobs.join(first_name);
        let second = shard_dir(&self.root, address);

        if tokio::fs::remove_dir(&second).await.is_ok() {
            let _ = sync_dir(&first).await;
            if tokio::fs::remove_dir(&first).await.is_ok() {
                let _ = sync_dir(&blobs).await;
            }
        }
    }

    /// The staged file's length, or `None` when nothing is staged.
    async fn staged_length(&self, upload: &UploadId) -> Result<Option<u64>, BlobError> {
        match tokio::fs::metadata(self.staged_path(upload)).await {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(backend("stat a staged upload", &error)),
        }
    }

    /// Read one page of the shard tree. See [`BlobStore::enumerate`] for the contract.
    async fn walk(
        &self,
        after: Option<&ContentAddress>,
        limit: usize,
    ) -> Result<BlobPage, BlobError> {
        let blobs = blobs_dir(&self.root);
        let resume = after.map(ContentAddress::shard);
        let mut page = BlobPage::default();

        for first in read_dir_sorted(&blobs).await? {
            if !first.is_dir || !is_shard_segment(&first.name) {
                page.debris.push(first.name);
                continue;
            }
            if let Some([resume_first, _]) = resume
                && first.name.as_str() < resume_first
            {
                continue;
            }

            let first_path = blobs.join(&first.name);
            for second in read_dir_sorted(&first_path).await? {
                if !second.is_dir || !is_shard_segment(&second.name) {
                    page.debris.push(format!("{}/{}", first.name, second.name));
                    continue;
                }
                if let Some([resume_first, resume_second]) = resume
                    && first.name == resume_first
                    && second.name.as_str() < resume_second
                {
                    continue;
                }

                let second_path = first_path.join(&second.name);
                for file in read_dir_sorted(&second_path).await? {
                    let relative = format!("{}/{}/{}", first.name, second.name, file.name);
                    let Some(address) = ContentAddress::from_file_name(&file.name) else {
                        page.debris.push(relative);
                        continue;
                    };
                    if file.is_dir
                        || !address.is_filed_under([first.name.as_str(), second.name.as_str()])
                    {
                        // A name that is a valid address filed under a shard it does not derive
                        // would answer one content address with another's bytes. It is debris,
                        // loudly, and never an entry.
                        tracing::warn!(%address, path = %relative, "a blob is filed under the wrong shard");
                        page.debris.push(relative);
                        continue;
                    }
                    if after.is_some_and(|after| &address <= after) {
                        continue;
                    }

                    if page.entries.len() == limit {
                        page.next = page.entries.last().map(|entry| entry.address.clone());
                        return Ok(page);
                    }

                    let size = tokio::fs::metadata(second_path.join(&file.name))
                        .await
                        .map_err(|error| backend("stat a blob during enumeration", &error))?
                        .len();
                    page.entries.push(BlobStat { address, size });
                }
            }
        }

        Ok(page)
    }
}

impl BlobStore for FilesystemBlobStore {
    fn begin<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let path = self.staged_path(upload);
            let file = tokio::fs::File::create(&path)
                .await
                .map_err(|error| backend("create a staging file", &error))?;
            file.sync_all()
                .await
                .map_err(|error| backend("sync a staging file", &error))?;
            sync_dir(&incoming_dir(&self.root)).await?;
            tracing::debug!(%upload, path = %path.display(), "staged an upload");
            Ok(())
        })
    }

    fn append<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, u64> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let Some(actual) = self.staged_length(upload).await? else {
                return Err(BlobError::NotStaged {
                    upload: upload.clone(),
                });
            };
            if offset != actual {
                return Err(BlobError::OffsetMismatch {
                    upload: upload.clone(),
                    offset,
                    actual,
                });
            }

            let mut file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(self.staged_path(upload))
                .await
                .map_err(|error| backend("open a staging file to append", &error))?;
            file.write_all(bytes)
                .await
                .map_err(|error| backend("append to a staging file", &error))?;
            // Durable before the acknowledgement: the on-disk length is the truth the session's
            // received-byte counter caches, and it may lag the file but must never lead it.
            file.sync_all()
                .await
                .map_err(|error| backend("sync an appended staging file", &error))?;

            let length = actual + bytes.len() as u64;
            tracing::trace!(%upload, offset, appended = bytes.len(), length, "appended to a stage");
            Ok(length)
        })
    }

    fn staged_len<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, Option<u64>> {
        Box::pin(async move {
            check_upload_id(upload)?;
            self.staged_length(upload).await
        })
    }

    fn abandon<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, bool> {
        Box::pin(async move {
            check_upload_id(upload)?;
            match tokio::fs::remove_file(self.staged_path(upload)).await {
                Ok(()) => {
                    sync_dir(&incoming_dir(&self.root)).await?;
                    tracing::debug!(%upload, "abandoned a stage");
                    Ok(true)
                }
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
                Err(error) => Err(backend("remove a staging file", &error)),
            }
        })
    }

    fn staged(&self) -> BlobFuture<'_, Vec<UploadId>> {
        Box::pin(async move {
            let mut staged = Vec::new();
            for entry in read_dir_sorted(&incoming_dir(&self.root)).await? {
                if entry.is_dir {
                    continue;
                }
                if let Some(name) = entry.name.strip_suffix(BLOB_SUFFIX) {
                    let upload = UploadId::new(name);
                    // A name this port would never have written is not a session; the startup
                    // scrub sees it through the debris the finalized store reports, not here.
                    if check_upload_id(&upload).is_ok() {
                        staged.push(upload);
                    }
                }
            }
            Ok(staged)
        })
    }

    fn commit<'a>(
        &'a self,
        upload: &'a UploadId,
        address: &'a ContentAddress,
    ) -> BlobFuture<'a, Placement> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let staged = self.staged_path(upload);
            let Some(size) = self.staged_length(upload).await? else {
                return Err(BlobError::NotStaged {
                    upload: upload.clone(),
                });
            };

            let shard = self.ensure_shard(address).await?;
            let target = shard.join(address.file_name());

            if tokio::fs::try_exists(&target)
                .await
                .map_err(|error| backend("check whether a blob is already stored", &error))?
            {
                tokio::fs::remove_file(&staged)
                    .await
                    .map_err(|error| backend("discard a duplicate staging file", &error))?;
                sync_dir(&incoming_dir(&self.root)).await?;
                tracing::info!(%upload, %address, "committed onto an address already present");
                return Ok(Placement::AlreadyPresent);
            }

            // A concurrent commit of the *same* address can still land between the check and the
            // rename. It is harmless by construction: both files are the same content address, so
            // whichever entry survives holds the same bytes.
            tokio::fs::rename(&staged, &target).await.map_err(|error| {
                backend("rename a staged upload onto its content address", &error)
            })?;
            sync_dir(&shard).await?;
            sync_dir(&incoming_dir(&self.root)).await?;

            tracing::info!(%upload, %address, size, path = %target.display(), "committed a blob");
            Ok(Placement::Stored)
        })
    }

    fn put<'a>(
        &'a self,
        address: &'a ContentAddress,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, Placement> {
        Box::pin(async move {
            let shard = self.ensure_shard(address).await?;
            let target = shard.join(address.file_name());
            if tokio::fs::try_exists(&target)
                .await
                .map_err(|error| backend("check whether a blob is already stored", &error))?
            {
                return Ok(Placement::AlreadyPresent);
            }

            // The temp lives *inside the target shard*: same directory is a stronger guarantee
            // than same filesystem and needs no configuration to stay true, and a temp a crash
            // leaves behind is debris the enumeration walk already inventories.
            let temp = shard.join(format!(
                ".{address}.{}.tmp",
                uuid::Uuid::now_v7().as_simple()
            ));
            let mut file = tokio::fs::File::create(&temp)
                .await
                .map_err(|error| backend("create a temporary blob", &error))?;
            file.write_all(bytes)
                .await
                .map_err(|error| backend("write a blob", &error))?;
            file.sync_all()
                .await
                .map_err(|error| backend("sync a blob", &error))?;
            drop(file);

            tokio::fs::rename(&temp, &target)
                .await
                .map_err(|error| backend("rename a blob onto its content address", &error))?;
            sync_dir(&shard).await?;

            tracing::info!(%address, size = bytes.len(), path = %target.display(), "stored a blob");
            Ok(Placement::Stored)
        })
    }

    fn stat<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, Option<BlobStat>> {
        Box::pin(async move {
            match tokio::fs::metadata(blob_path(&self.root, address)).await {
                Ok(metadata) => Ok(Some(BlobStat {
                    address: address.clone(),
                    size: metadata.len(),
                })),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(backend("stat a blob", &error)),
            }
        })
    }

    fn read_at<'a>(
        &'a self,
        address: &'a ContentAddress,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let path = blob_path(&self.root, address);
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(backend("open a blob", &error)),
            };
            let size = file
                .metadata()
                .await
                .map_err(|error| backend("stat an open blob", &error))?
                .len();

            let (start, taken) = window(size, offset, len);
            if taken == 0 {
                return Ok(Some(Vec::new()));
            }

            file.seek(std::io::SeekFrom::Start(start as u64))
                .await
                .map_err(|error| backend("seek within a blob", &error))?;
            let mut buffer = vec![0_u8; taken];
            file.read_exact(&mut buffer)
                .await
                .map_err(|error| backend("read a blob", &error))?;
            tracing::trace!(%address, offset, len = taken, "read a window of a blob");
            Ok(Some(buffer))
        })
    }

    fn enumerate<'a>(
        &'a self,
        after: Option<&'a ContentAddress>,
        limit: usize,
    ) -> BlobFuture<'a, BlobPage> {
        Box::pin(async move {
            let page = self.walk(after, limit).await?;
            tracing::debug!(
                entries = page.entries.len(),
                debris = page.debris.len(),
                more = page.next.is_some(),
                "enumerated a page of the blob store"
            );
            Ok(page)
        })
    }

    fn remove<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, bool> {
        Box::pin(async move {
            match tokio::fs::remove_file(blob_path(&self.root, address)).await {
                Ok(()) => {
                    sync_dir(&shard_dir(&self.root, address)).await?;
                    self.prune_shard(address).await;
                    tracing::info!(%address, "removed a blob");
                    Ok(true)
                }
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
                Err(error) => Err(backend("remove a blob", &error)),
            }
        })
    }

    fn quarantine<'a>(
        &'a self,
        address: &'a ContentAddress,
        reason: QuarantineReason,
    ) -> BlobFuture<'a, bool> {
        Box::pin(async move {
            let held = quarantine_dir(&self.root);
            let destination = held.join(address.file_name());
            match tokio::fs::rename(blob_path(&self.root, address), &destination).await {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(backend("move a blob into quarantine", &error)),
            }

            // The encoding is the adapter's, not the record's: `.reason.json` is what
            // design/filesystem/server.md names, and it is written here rather than derived on
            // the port's type.
            let record = serde_json::json!({
                "code": reason.code,
                "detail": reason.detail,
                "quarantined_at": reason.at.to_string(),
            });
            let path = held.join(format!("{address}{QUARANTINE_REASON_SUFFIX}"));
            let mut file = tokio::fs::File::create(&path)
                .await
                .map_err(|error| backend("create a quarantine record", &error))?;
            file.write_all(record.to_string().as_bytes())
                .await
                .map_err(|error| backend("write a quarantine record", &error))?;
            file.sync_all()
                .await
                .map_err(|error| backend("sync a quarantine record", &error))?;
            drop(file);
            sync_dir(&held).await?;
            self.prune_shard(address).await;

            tracing::warn!(%address, code = %reason.code, detail = %reason.detail, "quarantined a blob");
            Ok(true)
        })
    }

    fn quarantined(&self) -> BlobFuture<'_, Vec<QuarantinedBlob>> {
        Box::pin(async move {
            let held = quarantine_dir(&self.root);
            let mut blobs = Vec::new();

            for entry in read_dir_sorted(&held).await? {
                let Some(address) = ContentAddress::from_file_name(&entry.name) else {
                    continue;
                };
                let path = held.join(format!("{address}{QUARANTINE_REASON_SUFFIX}"));
                // Loud rather than lossy. The bytes are preserved either way — that is the
                // guarantee quarantine makes — but a rejection record an operator cannot read is
                // a fault they need told about, not one to paper over with a synthesized reason.
                let raw = tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|error| backend("read a quarantine record", &error))?;
                let record: serde_json::Value =
                    serde_json::from_str(&raw).map_err(|error| BlobError::Backend {
                        operation: "parse a quarantine record",
                        detail: error.to_string(),
                    })?;

                let field = |name: &str| {
                    record
                        .get(name)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let at = field("quarantined_at")
                    .parse::<Timestamp>()
                    .unwrap_or(Timestamp::UNIX_EPOCH);

                blobs.push(QuarantinedBlob {
                    address,
                    reason: QuarantineReason {
                        code: field("code"),
                        detail: field("detail"),
                        at,
                    },
                });
            }

            Ok(blobs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::conformance::{self, Harness};
    use super::*;

    /// A store on a directory that disappears with the test.
    ///
    /// No temporary-directory crate: one `remove_dir_all` in `Drop` is the whole requirement, and
    /// a dependency for it would need a row in a design doc this slice does not own.
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
}
