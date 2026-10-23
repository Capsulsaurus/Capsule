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

/// Wrap an IO failure as the port's error.
///
/// A full filesystem is [`BlobError::StorageFull`] wherever it is met, never a
/// [`BlobError::Backend`] detail: it is the one storage failure a client is told to wait out
/// (`507`) rather than reported as an outage (`503`), so the distinction has to survive every
/// operation that can run out of room — a stage's creation as much as an append.
fn backend(operation: &'static str, error: &std::io::Error) -> BlobError {
    if error.kind() == ErrorKind::StorageFull {
        return BlobError::StorageFull { operation };
    }
    BlobError::Backend {
        operation,
        detail: error.to_string(),
    }
}

/// Write `bytes` at the end of `file` and make them durable.
///
/// One step for [`FilesystemBlobStore::append`]'s purposes: a failure anywhere in it leaves an
/// unknown number of bytes behind, and the caller cuts the file back to where it started either
/// way.
async fn write_durably(file: &mut tokio::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    file.write_all(bytes).await?;
    // Durable before the acknowledgement: the on-disk length is the truth the session's
    // received-byte counter caches, and it may lag the file but must never lead it.
    file.sync_all().await
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
    /// A short write armed by a unit test: the next append writes this many bytes of its chunk
    /// and then fails as a full disk would. The only way to drive the truncation path against a
    /// real file without filling a real filesystem.
    #[cfg(test)]
    short_write: std::sync::Mutex<Option<usize>>,
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
        Ok(Self {
            root,
            #[cfg(test)]
            short_write: std::sync::Mutex::new(None),
        })
    }

    /// Write `bytes` to an open stage, honouring a short write a unit test armed.
    async fn write_chunk(&self, file: &mut tokio::fs::File, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(test)]
        {
            let armed = self
                .short_write
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(written) = armed {
                file.write_all(&bytes[..written.min(bytes.len())]).await?;
                file.sync_all().await?;
                return Err(std::io::Error::from(ErrorKind::StorageFull));
            }
        }
        write_durably(file, bytes).await
    }

    /// Cut the stage behind `file` back to `len` bytes and make the cut durable.
    async fn cut(file: &tokio::fs::File, len: u64) -> std::io::Result<()> {
        file.set_len(len).await?;
        file.sync_all().await
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
            if let Err(error) = self.write_chunk(&mut file, bytes).await {
                // Some prefix of the chunk may already be on disk. Cut it away before answering,
                // so the stage ends at `offset` again and the client's retry there is an
                // ordinary append rather than a mismatch that repeats until the session expires.
                match Self::cut(&file, actual).await {
                    Ok(()) => tracing::warn!(
                        %upload,
                        offset,
                        %error,
                        "an append failed and the stage was cut back to its offset"
                    ),
                    // Nothing more this call can do. The next append through the chunk path
                    // meets the longer stage and cuts it back itself (`truncate_staged`).
                    Err(cut) => tracing::error!(
                        %upload,
                        offset,
                        %error,
                        %cut,
                        "an append failed and the stage could not be cut back to its offset"
                    ),
                }
                return Err(backend("append to a staging file", &error));
            }

            let length = actual + bytes.len() as u64;
            tracing::trace!(%upload, offset, appended = bytes.len(), length, "appended to a stage");
            Ok(length)
        })
    }

    fn truncate_staged<'a>(
        &'a self,
        upload: &'a UploadId,
        len: u64,
    ) -> BlobFuture<'a, Option<u64>> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let file = match tokio::fs::OpenOptions::new()
                .write(true)
                .open(self.staged_path(upload))
                .await
            {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(backend("open a staging file to truncate", &error)),
            };
            let before = file
                .metadata()
                .await
                .map_err(|error| backend("stat a staging file to truncate", &error))?
                .len();
            if before <= len {
                return Ok(Some(before));
            }
            Self::cut(&file, len)
                .await
                .map_err(|error| backend("truncate a staging file", &error))?;
            tracing::info!(%upload, before, after = len, "cut a stage back to its session offset");
            Ok(Some(len))
        })
    }

    fn staged_len<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, Option<u64>> {
        Box::pin(async move {
            check_upload_id(upload)?;
            self.staged_length(upload).await
        })
    }

    fn read_staged_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let path = self.staged_path(upload);
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(backend("open a staged upload", &error)),
            };
            let size = file
                .metadata()
                .await
                .map_err(|error| backend("stat an open staged upload", &error))?
                .len();

            let (start, taken) = window(size, offset, len);
            if taken == 0 {
                return Ok(Some(Vec::new()));
            }

            file.seek(std::io::SeekFrom::Start(start as u64))
                .await
                .map_err(|error| backend("seek within a staged upload", &error))?;
            let mut buffer = vec![0_u8; taken];
            file.read_exact(&mut buffer)
                .await
                .map_err(|error| backend("read a staged upload", &error))?;
            tracing::trace!(%upload, offset, len = taken, "read a window of a stage");
            Ok(Some(buffer))
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
mod tests;
