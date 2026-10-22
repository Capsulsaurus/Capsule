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
//! not wait on each other. One process owns a blob root; two processes appending to one ledger
//! is a deployment the design does not allow, exactly as two processes committing to one blob
//! store is.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiff::Timestamp;
use tokio::io::AsyncWriteExt;

use super::{
    ChainFault, LedgerEntry, LedgerError, LedgerEvent, LedgerFuture, LedgerHead, LedgerScan,
    OwnerLedger, check_owner_id, decode_frames, encode_frame, link_check, sign_entry,
};
use crate::attestation::ReceiptSigner;
use crate::store::OwnerId;

/// The directory under the blob root the ledgers live in.
pub const LEDGER_DIR: &str = "ledger";

/// A segment's file-name suffix.
const SEGMENT_SUFFIX: &str = ".log";

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
                Some(_) => {
                    return Err(LedgerError::Damaged {
                        owner: owner.clone(),
                        fault: ChainFault::Undecodable {
                            entries: entries.len(),
                        },
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
            let tail = match cached.take() {
                Some(tail) => tail,
                None => self.load_tail(owner).await?,
            };

            let entry = sign_entry(owner, tail.head, event, recorded_at, signer);
            let frame = encode_frame(&entry);

            let (path, len, created) = match &tail.segment {
                Some((path, len)) if *len < self.segment_bytes => (path.clone(), *len, false),
                _ => {
                    let dir = self.ensure_owner_dir(owner).await?;
                    (
                        dir.join(format!("{}{SEGMENT_SUFFIX}", entry.core.seq)),
                        0,
                        true,
                    )
                }
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
            // Under the owner's lock, so a scan never reads an append half-written.
            let lock = self.lock_for(owner);
            let _held = lock.lock().await;
            let mut scan = LedgerScan::default();
            let segments = segments(&self.owner_dir(owner)).await?;
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
                        _ => ChainFault::Undecodable { entries },
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
}
