//! The filesystem owner ledger: `{blob_root}/ledger/{owner_id}/{first_seq}.log`.
//!
//! # Durability, and where every fsync goes
//!
//! An append is acknowledged only once it would survive a crash, because the caller commits its
//! index row next and a row without an entry is the state the ledger exists to rule out. So an
//! append:
//!
//! 1. creates the owner's directory if it is absent, and fsyncs `ledger/` when it did;
//! 2. creates a new segment when there is none or the current one is full, and remembers to
//!    fsync the owner's directory once the segment holds its first frame;
//! 3. writes the frame and fsyncs the segment;
//! 4. fsyncs the owner's directory if step 2 created the segment.
//!
//! A write that fails part-way is cut back to the segment's previous length before the error
//! is returned, exactly as [`crate::blob::BlobStore::append`] cuts a stage back: the bytes were
//! never acknowledged, and leaving them would make the next append's frame unreadable.
//!
//! # Attachments
//!
//! `ledger/{owner_id}/attachments/{key}` holds what [`OwnerLedger::attach`] keeps, one file per
//! key. A write goes to `{key}.{uuid}.partial`, is fsynced, renamed into place and the directory
//! fsynced, so a reader sees the old bytes or the new ones and never a torn file, and a crash
//! leaves at most a `.partial` file nothing reads.
//!
//! # Segments
//!
//! A segment is named for the `seq` of its first entry and holds frames until it reaches the
//! segment size, after which the next append opens a new one. Segments bound the size of any one
//! file a backup or an operator handles; they are an index into the chain, not part of it, so the
//! chain verifies across them exactly as it does within one.
//!
//! # Concurrency
//!
//! Appends to one owner are serialised by a per-owner lock, held from reading the head to the
//! fsync, so two finalizations for one owner get two positions. Appends to different owners do
//! not wait on each other.
//!
//! That lock is two locks, because two **processes** append to one ledger: the server, and a
//! `capsule-server purge --apply` run from cron while it serves (#767). Inside a process, a
//! per-owner async mutex. Across processes, an exclusive OS file lock (`flock`) on the owner's
//! lock file, [`LOCK_FILE`], taken after the mutex and held to the fsync; a scan takes it shared.
//! The lock is on a file opened for writing, not on the directory: Linux emulates `flock` on NFS
//! with `fcntl` byte-range locks, and those refuse an exclusive lock on a read-only descriptor,
//! which is all a directory can be opened as. The lock file holds no bytes and, like the
//! attachment directory beside it, is not a segment, so listing, rebuild and the scrub never see
//! it. Under the lock the cached tail is **re-checked against the disk** — the last segment's
//! name and length — and re-read when another process has appended since, so no append reuses a
//! position another process took. A failed append's cut-back goes to the length observed under
//! the same lock, so it can only remove its own bytes. The lock is advisory, and every writer is this adapter.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;
use tokio::io::AsyncWriteExt;

use super::{
    ChainFault, LedgerEntry, LedgerError, LedgerEvent, LedgerFuture, LedgerHead, LedgerScan,
    OwnerLedger, check_attachment_size, check_owner_id, decode_frames, encode_frame, link_check,
    sign_entry,
};
use crate::attestation::ReceiptSigner;
use crate::store::OwnerId;

/// The directory under the blob root the ledgers live in.
pub const LEDGER_DIR: &str = "ledger";

/// A segment's file-name suffix.
const SEGMENT_SUFFIX: &str = ".log";

/// The empty file in each owner's directory that the cross-process lock is taken on. See the
/// module docs. Its name has no segment suffix (`.log`), so it is never read as a segment.
pub const LOCK_FILE: &str = ".lock";

/// The directory inside an owner's ledger directory its attachments live in, one file each,
/// named for its key in hex.
pub const ATTACHMENT_DIR: &str = "attachments";

/// The suffix a partly written attachment carries until it is renamed into place.
const PARTIAL_SUFFIX: &str = ".partial";

/// The size past which an append opens a new segment: 64 MiB, some tens of thousands of entries.
pub const DEFAULT_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;

/// Wrap an IO failure as the port's error, keeping a full disk distinct.
fn backend(operation: &'static str, error: &std::io::Error) -> LedgerError {
    if error.kind() == ErrorKind::StorageFull {
        return LedgerError::StorageFull { operation };
    }
    LedgerError::Backend {
        operation,
        detail: error.to_string(),
    }
}

/// Make a directory entry durable. See the module docs.
#[cfg(unix)]
async fn sync_dir(path: &Path) -> Result<(), LedgerError> {
    let directory = tokio::fs::File::open(path)
        .await
        .map_err(|error| backend("open a directory to sync it", &error))?;
    directory
        .sync_all()
        .await
        .map_err(|error| backend("sync a directory", &error))
}

/// No-op: fsyncing a directory is a Unix primitive, and no other platform is a supported
/// deployment.
#[cfg(not(unix))]
async fn sync_dir(_path: &Path) -> Result<(), LedgerError> {
    Ok(())
}

/// Where an owner's ledger currently ends.
#[derive(Debug, Clone)]
struct Tail {
    /// The last entry, or `None` for an empty ledger.
    head: Option<LedgerHead>,
    /// The segment the next append lands in, and its length, or `None` when there is none.
    segment: Option<(PathBuf, u64)>,
}

/// The filesystem ledger.
#[derive(Debug)]
pub struct FilesystemLedger {
    /// `{blob_root}/ledger`.
    dir: PathBuf,
    /// When a segment is full.
    segment_bytes: u64,
    /// One lock per owner, holding the tail once an append has read it.
    owners: std::sync::Mutex<BTreeMap<OwnerId, Arc<tokio::sync::Mutex<Option<Tail>>>>>,
}

impl FilesystemLedger {
    /// Open the ledgers under `blob_root`, creating `ledger/` if it is absent.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Backend`] if the directory cannot be created.
    pub async fn open(blob_root: impl AsRef<Path>) -> Result<Self, LedgerError> {
        Self::with_segment_bytes(blob_root, DEFAULT_SEGMENT_BYTES).await
    }

    /// The same, with segments that fill at `segment_bytes`.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub async fn with_segment_bytes(
        blob_root: impl AsRef<Path>,
        segment_bytes: u64,
    ) -> Result<Self, LedgerError> {
        let root = blob_root.as_ref();
        let dir = root.join(LEDGER_DIR);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| backend("create the ledger directory", &error))?;
        sync_dir(root).await?;
        tracing::info!(dir = %dir.display(), segment_bytes, "opened the owner ledgers");
        Ok(Self {
            dir,
            segment_bytes: segment_bytes.max(1),
            owners: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    /// The directory the ledgers live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `owner`'s directory.
    pub fn owner_dir(&self, owner: &OwnerId) -> PathBuf {
        self.dir.join(owner.as_str())
    }

    /// The lock for `owner`.
    fn lock_for(&self, owner: &OwnerId) -> Arc<tokio::sync::Mutex<Option<Tail>>> {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(owner.clone())
            .or_default()
            .clone()
    }

    /// Forget the tail cached for `owner`, as a restart would.
    ///
    /// For a test that rewrites the files behind the adapter's back.
    #[cfg(test)]
    pub(crate) async fn forget(&self, owner: &OwnerId) {
        *self.lock_for(owner).lock().await = None;
    }

    /// Read `owner`'s ledger from disk, cutting a torn tail off its last segment.
    async fn load_tail(&self, owner: &OwnerId) -> Result<Tail, LedgerError> {
        let segments = segments(&self.owner_dir(owner)).await?;
        let mut entries = Vec::new();
        let mut last = None;
        let count = segments.len();
        for (index, (_, path)) in segments.into_iter().enumerate() {
            let bytes = read(&path).await?;
            let decoded = decode_frames(&bytes);
            entries.extend(decoded.entries);
            match decoded.damage {
                None => {}
                // Only the final segment's tail can be an interrupted append; anywhere else a
                // short frame is a hole in the middle of the chain.
                Some(ChainFault::TornTail { .. }) if index + 1 == count => {
                    tracing::warn!(
                        %owner,
                        segment = %path.display(),
                        cut = bytes.len() - decoded.whole_len,
                        "cut a torn tail off an owner ledger before appending"
                    );
                    cut(&path, decoded.whole_len as u64).await?;
                }
                Some(damage) => {
                    return Err(LedgerError::Damaged {
                        owner: owner.clone(),
                        fault: across_segments(&damage, entries.len()),
                    });
                }
            }
            last = Some((path, decoded.whole_len as u64));
        }
        let head = link_check(owner, &entries).map_err(|fault| LedgerError::Damaged {
            owner: owner.clone(),
            fault,
        })?;
        tracing::debug!(%owner, head = ?head.map(|head| head.seq), "read an owner ledger's tail");
        Ok(Tail {
            head,
            segment: last,
        })
    }

    /// Whether the cached `tail` still ends where `owner`'s ledger ends on disk.
    ///
    /// Asked under the cross-process lock. Another process's append either grew the last
    /// segment or opened a new one, and its cut-back only ever restores a length it observed
    /// under the same lock, so the last segment's name and length are enough to tell.
    async fn still_current(&self, owner: &OwnerId, tail: &Tail) -> Result<bool, LedgerError> {
        let on_disk = segments(&self.owner_dir(owner)).await?;
        Ok(match (&tail.segment, on_disk.last()) {
            (None, None) => true,
            (Some((cached, len)), Some((_, last))) if cached == last => {
                let held = tokio::fs::metadata(last)
                    .await
                    .map_err(|error| backend("read a ledger segment's length", &error))?
                    .len();
                held == *len
            }
            _ => false,
        })
    }

    /// Create `owner`'s directory if it is absent, fsyncing `ledger/` when this call made it.
    async fn ensure_owner_dir(&self, owner: &OwnerId) -> Result<PathBuf, LedgerError> {
        let path = self.owner_dir(owner);
        match tokio::fs::create_dir(&path).await {
            Ok(()) => sync_dir(&self.dir).await?,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(backend("create an owner's ledger directory", &error)),
        }
        Ok(path)
    }
}

/// `{first_seq}.log` segments in `dir`, in chain order. An absent directory has none.
async fn segments(dir: &Path) -> Result<Vec<(u64, PathBuf)>, LedgerError> {
    let mut reader = match tokio::fs::read_dir(dir).await {
        Ok(reader) => reader,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(backend("list an owner's ledger", &error)),
    };
    let mut found = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|error| backend("list an owner's ledger", &error))?
    {
        let name = entry.file_name();
        if name == LOCK_FILE || name == ATTACHMENT_DIR {
            continue;
        }
        let Some(first) = name
            .to_str()
            .and_then(|name| name.strip_suffix(SEGMENT_SUFFIX))
            .and_then(|stem| stem.parse::<u64>().ok())
        else {
            tracing::warn!(entry = %entry.path().display(), "an owner ledger holds a file that is not a segment");
            continue;
        };
        found.push((first, entry.path()));
    }
    found.sort_by_key(|(first, _)| *first);
    Ok(found)
}

/// `damage` found in one segment, restated for the whole ledger with `entries` before it.
///
/// Anything that is not a cuttable tail is a hole, except an intact entry this server cannot
/// read, which keeps its own fault so an operator sees it was most likely written by a newer
/// server rather than damaged.
fn across_segments(damage: &ChainFault, entries: usize) -> ChainFault {
    match damage {
        ChainFault::Unrecognised { .. } => ChainFault::Unrecognised { entries },
        _ => ChainFault::Undecodable { entries },
    }
}

/// Open the [`LOCK_FILE`] in the owner directory at `dir` for the lock wanted, or `None` when
/// there is nothing to lock.
///
/// An exclusive lock opens it read-write, creating it, because an exclusive lock emulated with
/// `fcntl` (Linux on NFS) is refused on a read-only descriptor. A shared lock needs only a
/// readable one, so it opens an existing file read-only, and a reader without write access to the
/// blob root (a scrub on a read-only mount) still takes it. `None` when the directory is absent,
/// or for a shared lock when the file is absent and this process cannot create it: a ledger
/// written before the lock file existed, read by such a reader, is read unlocked, and an append it
/// races shows as a torn tail in the scan rather than as an error.
fn open_lock_file(dir: &Path, exclusive: bool) -> std::io::Result<Option<std::fs::File>> {
    let path = dir.join(LOCK_FILE);
    if !exclusive {
        match std::fs::File::open(&path) {
            Ok(file) => return Ok(Some(file)),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error)
            if !exclusive
                && matches!(
                    error.kind(),
                    ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
                ) =>
        {
            tracing::warn!(
                lock_file = %path.display(),
                %error,
                "cannot create an owner ledger's lock file; reading it unlocked"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Take the cross-process lock on the owner directory at `dir`, exclusive or shared, through its
/// [`LOCK_FILE`]; the lock is released when the returned handle is dropped. `None` as
/// [`open_lock_file`] says. See the module docs.
///
/// Blocking, so it waits on the blocking pool rather than on a runtime worker.
async fn lock_owner(dir: PathBuf, exclusive: bool) -> Result<Option<std::fs::File>, LedgerError> {
    let locked = tokio::task::spawn_blocking(move || {
        let Some(file) = open_lock_file(&dir, exclusive)? else {
            return Ok(None);
        };
        if exclusive {
            file.lock()?;
        } else {
            file.lock_shared()?;
        }
        Ok::<_, std::io::Error>(Some(file))
    })
    .await
    .map_err(|error| LedgerError::Backend {
        operation: "lock an owner's ledger",
        detail: error.to_string(),
    })?;
    locked.map_err(|error| backend("lock an owner's ledger", &error))
}

/// A segment's bytes.
async fn read(path: &Path) -> Result<Vec<u8>, LedgerError> {
    tokio::fs::read(path)
        .await
        .map_err(|error| backend("read a ledger segment", &error))
}

/// Cut `path` back to `len` bytes, durably.
async fn cut(path: &Path, len: u64) -> Result<(), LedgerError> {
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .await
        .map_err(|error| backend("open a ledger segment to cut it", &error))?;
    file.set_len(len)
        .await
        .map_err(|error| backend("cut a ledger segment", &error))?;
    file.sync_all()
        .await
        .map_err(|error| backend("sync a cut ledger segment", &error))
}

/// Write `frame` at the end of `path` and make it durable, cutting back to `len` on failure.
async fn write_frame(path: &Path, len: u64, frame: &[u8]) -> Result<(), LedgerError> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|error| backend("open a ledger segment", &error))?;
    let written = async {
        file.write_all(frame).await?;
        file.sync_all().await
    }
    .await;
    if let Err(error) = written {
        if let Err(cut_error) = cut(path, len).await {
            tracing::error!(
                segment = %path.display(),
                %cut_error,
                "a failed ledger append could not be cut back; the next append will cut its torn tail"
            );
        }
        return Err(backend("append to a ledger segment", &error));
    }
    Ok(())
}

impl OwnerLedger for FilesystemLedger {
    fn append<'a>(
        &'a self,
        owner: &'a OwnerId,
        event: LedgerEvent,
        recorded_at: Timestamp,
        signer: &'a dyn ReceiptSigner,
    ) -> LedgerFuture<'a, LedgerEntry> {
        Box::pin(async move {
            check_owner_id(owner)?;
            let lock = self.lock_for(owner);
            let mut cached = lock.lock().await;
            // The directory first, because the cross-process lock file lives in it.
            let dir = self.ensure_owner_dir(owner).await?;
            let _across_processes =
                lock_owner(dir.clone(), true)
                    .await?
                    .ok_or_else(|| LedgerError::Backend {
                        operation: "lock an owner's ledger",
                        detail: format!("{} vanished while appending", dir.display()),
                    })?;
            let tail = match cached.take() {
                Some(tail) if self.still_current(owner, &tail).await? => tail,
                Some(_) => {
                    tracing::debug!(%owner, "another process appended to this ledger; re-reading its tail");
                    self.load_tail(owner).await?
                }
                None => self.load_tail(owner).await?,
            };

            let entry = sign_entry(owner, tail.head, event, recorded_at, signer);
            let frame = encode_frame(&entry);

            let (path, len, created) = match &tail.segment {
                Some((path, len)) if *len < self.segment_bytes => (path.clone(), *len, false),
                _ => (
                    dir.join(format!("{}{SEGMENT_SUFFIX}", entry.core.seq)),
                    0,
                    true,
                ),
            };

            if let Err(error) = write_frame(&path, len, &frame).await {
                // The tail is unchanged on disk (or torn, which the next load cuts), so forget
                // it rather than trust it.
                tracing::error!(%owner, %error, "an owner ledger append failed");
                return Err(error);
            }
            if created {
                sync_dir(&self.owner_dir(owner)).await?;
            }

            *cached = Some(Tail {
                head: Some(LedgerHead {
                    seq: entry.core.seq,
                    hash: entry.chain_hash(),
                }),
                segment: Some((path, len + frame.len() as u64)),
            });
            tracing::debug!(
                %owner,
                seq = entry.core.seq,
                kind = entry.core.event.kind(),
                new_segment = created,
                "appended to an owner ledger"
            );
            Ok(entry)
        })
    }

    fn scan<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, LedgerScan> {
        Box::pin(async move {
            check_owner_id(owner)?;
            // Under the owner's lock, so a scan never reads an append half-written — this
            // process's or another's.
            let lock = self.lock_for(owner);
            let _held = lock.lock().await;
            let mut scan = LedgerScan::default();
            let dir = self.owner_dir(owner);
            // `None` when there is no directory (no ledger, nothing to wait for), or no lock file
            // a read-only reader could take.
            let _across_processes = lock_owner(dir.clone(), false).await?;
            let segments = segments(&dir).await?;
            let count = segments.len();
            for (index, (_, path)) in segments.into_iter().enumerate() {
                let decoded = decode_frames(&read(&path).await?);
                scan.entries.extend(decoded.entries);
                if let Some(damage) = decoded.damage {
                    // Counted across the whole ledger, not the segment it was found in; and a
                    // short frame anywhere but the last segment is a hole, not a tail.
                    let entries = scan.entries.len();
                    scan.damage = Some(match damage {
                        ChainFault::TornTail { .. } if index + 1 == count => {
                            ChainFault::TornTail { entries }
                        }
                        other => across_segments(&other, entries),
                    });
                    break;
                }
            }
            Ok(scan)
        })
    }

    fn owners(&self) -> LedgerFuture<'_, Vec<OwnerId>> {
        Box::pin(async move {
            let mut reader = tokio::fs::read_dir(&self.dir)
                .await
                .map_err(|error| backend("list the owner ledgers", &error))?;
            let mut owners = Vec::new();
            while let Some(entry) = reader
                .next_entry()
                .await
                .map_err(|error| backend("list the owner ledgers", &error))?
            {
                let Some(name) = entry.file_name().to_str().map(OwnerId::new) else {
                    continue;
                };
                if check_owner_id(&name).is_err() {
                    continue;
                }
                if !segments(&entry.path()).await?.is_empty() {
                    owners.push(name);
                }
            }
            owners.sort();
            Ok(owners)
        })
    }

    fn attach<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: Hash32,
        bytes: Vec<u8>,
    ) -> LedgerFuture<'a, ()> {
        Box::pin(async move {
            check_owner_id(owner)?;
            check_attachment_size(bytes.len() as u64)?;
            let owner_dir = self.ensure_owner_dir(owner).await?;
            let dir = owner_dir.join(ATTACHMENT_DIR);
            match tokio::fs::create_dir(&dir).await {
                Ok(()) => sync_dir(&owner_dir).await?,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(backend("create an owner's attachment directory", &error));
                }
            }
            let name = key.to_hex();
            let target = dir.join(&name);
            let partial = dir.join(format!("{name}.{}{PARTIAL_SUFFIX}", uuid::Uuid::now_v7()));
            let written = async {
                let mut file = tokio::fs::File::create(&partial).await?;
                file.write_all(&bytes).await?;
                file.sync_all().await?;
                tokio::fs::rename(&partial, &target).await
            }
            .await;
            if let Err(error) = written {
                if let Err(remove_error) = tokio::fs::remove_file(&partial).await
                    && remove_error.kind() != ErrorKind::NotFound
                {
                    tracing::warn!(
                        file = %partial.display(),
                        %remove_error,
                        "a failed attachment write left a partial file behind; nothing reads it"
                    );
                }
                tracing::error!(%owner, %key, %error, "an owner ledger attachment could not be written");
                return Err(backend("write an attachment", &error));
            }
            sync_dir(&dir).await?;
            tracing::debug!(%owner, %key, bytes = bytes.len(), "attached a document to an owner ledger");
            Ok(())
        })
    }

    fn attachment<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: &'a Hash32,
    ) -> LedgerFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            check_owner_id(owner)?;
            let path = self
                .owner_dir(owner)
                .join(ATTACHMENT_DIR)
                .join(key.to_hex());
            let size = match tokio::fs::metadata(&path).await {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(backend("read an attachment", &error)),
            };
            check_attachment_size(size)?;
            match tokio::fs::read(&path).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(backend("read an attachment", &error)),
            }
        })
    }
}
