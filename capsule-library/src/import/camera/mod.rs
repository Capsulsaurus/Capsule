//! The **camera source adapter**: import straight from a camera over PTP/IP.
//!
//! A camera is a source adapter on the same seam as the filesystem scanner and the
//! [third-party importers](crate::import::importers): it feeds Scan & Extract, and planning,
//! execution and upload are unchanged. It adds no pipeline stage and no server surface. The work
//! splits in two:
//!
//! 1. [`CameraAdapter::receive`] copies every object off the camera into a local staging root,
//!    hashing each one as its bytes arrive.
//! 2. [`CameraAdapter::extract`](SourceAdapter::extract) hands the staged objects to the pipeline
//!    exactly as a card reader's files would be: the scanner's grouping (RAW+JPEG stacks, Live
//!    Photos) and the planner's content-hash dedup apply unchanged.
//!
//! The contract ([Import — Camera Import]):
//!
//! - **Deterministic enumeration** — storages by id, then objects by handle, then capture date.
//! - **Dedup by content, not source.** The adapter keeps no "seen" state; re-importing a card the
//!   library already holds is the planner's content-hash `SkipDuplicate`.
//! - **Integrity.** PTP/IP carries no checksum, so each object is SHA-256-hashed on receipt and
//!   its staged bytes are read back and re-hashed before it is accepted. A byte count that differs
//!   from the camera's declared size, or a read-back that differs from the receipt hash, fails the
//!   object at import, before anything is signed; the staged file the planner then hashes is the
//!   one whose digest matched.
//! - **Resumable per object.** A transfer cut off midway leaves a partial file; the next run
//!   continues it with `GetPartialObject` where the camera supports that, and restarts it where
//!   it does not. Finished objects are never fetched again.
//! - **Read-only.** Nothing here mutates camera storage, and the [`CameraDevice`] port has no
//!   operation that could.
//!
//! [`CameraDevice`] is the port; [`PtpCamera`] is its adapter over a `ptpip` session. Discovery
//! (mDNS `_ptp._tcp` or a typed address) and pairing state are the caller's.
//!
//! [Import — Camera Import]: https://docs/design/import/camera-import/

pub(crate) mod ptp_device;
pub(crate) mod staging;

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use capsule_core::crypto::hash::{Hash32, Sha256Hasher};
use jiff::civil::DateTime;
pub use ptp_device::PtpCamera;
use thiserror::Error;

use crate::import::importers::{
    AdapterError, ExtractedImport, ExtractedMetadata, ImportProvider, SourceAdapter, SourceEntry,
};
use crate::utils::hash_file;

/// One object on the camera, as the [`CameraDevice`] port describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraObject {
    /// The storage (card slot) holding it.
    pub storage: u32,
    /// Its object handle.
    pub handle: u32,
    /// The handle of the folder holding it; `0` for the storage root.
    pub parent: u32,
    /// The file name the camera stores it under.
    pub filename: String,
    /// Its size in bytes, where the camera states it (PTP cannot state 4 GiB or more).
    pub size: Option<u64>,
    /// The capture date from the camera's object record, as wall-clock time.
    pub captured_at: Option<DateTime>,
    /// Whether it is a folder rather than a file.
    pub is_folder: bool,
}

/// What can go wrong talking to the camera or staging what it sends.
#[derive(Debug, Error)]
pub enum CameraError {
    /// The connection to the camera is gone or no longer trustworthy. Ends the run.
    #[error("camera connection lost: {0}")]
    Disconnected(String),
    /// The camera refused one operation. Fails that object only.
    #[error("camera refused {operation}: {code}")]
    Refused {
        /// The operation, as the camera protocol names it.
        operation: String,
        /// The camera's response code.
        code: String,
    },
    /// The object needs a partial read the camera cannot serve.
    #[error("camera cannot resume object {handle:08x} at byte {offset}")]
    ResumeUnsupported {
        /// The object.
        handle: u32,
        /// The requested offset.
        offset: u64,
    },
    /// A local write into the staging root failed. Ends the run.
    #[error("staging {path}: {source}")]
    Staging {
        /// The path being written.
        path: PathBuf,
        /// The I/O failure.
        source: io::Error,
    },
    /// The sink the camera's bytes were streamed into failed. Ends the run.
    #[error("staging write: {0}")]
    Sink(io::Error),
    /// The camera delivered a byte count other than the size it declared. Fails that object.
    #[error("object {handle:08x}: received {received} bytes, camera declared {declared}")]
    SizeMismatch {
        /// The object.
        handle: u32,
        /// The size the camera's object record declared.
        declared: u64,
        /// The bytes that arrived.
        received: u64,
    },
    /// The staged bytes did not re-hash to the digest computed as they arrived. Fails that
    /// object.
    #[error("object {handle:08x}: staged bytes hash {staged}, received bytes hashed {received}")]
    IntegrityMismatch {
        /// The object.
        handle: u32,
        /// The receipt digest.
        received: Hash32,
        /// The read-back digest.
        staged: Hash32,
    },
}

impl CameraError {
    /// Whether the run must stop: the connection or the local disk failed. Every other error
    /// fails one object and the run moves on.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Disconnected(_) | Self::Staging { .. } | Self::Sink(_)
        )
    }
}

/// The camera the adapter reads from. Read-only by construction: enumeration and reads only.
pub trait CameraDevice {
    /// Whether [`read_object`](Self::read_object) accepts a non-zero offset.
    fn supports_partial_reads(&self) -> bool;

    /// The storages present.
    fn storage_ids(&mut self) -> Result<Vec<u32>, CameraError>;

    /// Every object handle on `storage`, folders included, at any depth.
    fn object_handles(&mut self, storage: u32) -> Result<Vec<u32>, CameraError>;

    /// One object's record.
    fn object(&mut self, handle: u32) -> Result<CameraObject, CameraError>;

    /// Stream `object`'s bytes from `offset` to its end into `sink`; returns the bytes written.
    /// A non-zero `offset` is only asked of a device that
    /// [supports partial reads](Self::supports_partial_reads), for an object of known size.
    fn read_object(
        &mut self,
        object: &CameraObject,
        offset: u64,
        sink: &mut dyn Write,
    ) -> Result<u64, CameraError>;
}

/// One object received and staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedObject {
    /// The camera's record of it.
    pub object: CameraObject,
    /// Where it is staged.
    pub path: PathBuf,
    /// Its size in bytes.
    pub size: u64,
    /// The SHA-256 of its bytes, computed as they arrived and confirmed by reading them back.
    pub sha256: Hash32,
    /// The byte offset this run continued from; `0` for a whole transfer.
    pub resumed_from: u64,
    /// Whether it was already staged by an earlier run and not fetched again.
    pub already_staged: bool,
}

/// One object that could not be received.
#[derive(Debug)]
pub struct ObjectFailure {
    /// The storage it is on.
    pub storage: u32,
    /// Its handle.
    pub handle: u32,
    /// Its file name, where its record was read.
    pub filename: Option<String>,
    /// Why it failed.
    pub error: CameraError,
}

/// A storage, or the storage list itself, that the camera would not enumerate.
#[derive(Debug)]
pub struct StorageFailure {
    /// The storage (card slot) whose object list was refused; `None` when the camera refused to
    /// list its storages at all.
    pub storage: Option<u32>,
    /// Why it failed.
    pub error: CameraError,
}

/// The outcome of [`CameraAdapter::receive`].
#[derive(Debug)]
pub struct CameraReceipt {
    /// The root to hand to [`SourceAdapter::extract`].
    pub objects_root: PathBuf,
    /// Every object received, in enumeration order.
    pub received: Vec<ReceivedObject>,
    /// Every object that failed, in enumeration order. Never dropped silently.
    pub failures: Vec<ObjectFailure>,
    /// Every storage whose objects could not be listed, such as an empty or unavailable card
    /// slot, in storage order. Its objects are neither received nor listed in `failures`.
    pub storage_failures: Vec<StorageFailure>,
}

/// The camera import source adapter, staging into `staging_root`.
#[derive(Debug, Clone)]
pub struct CameraAdapter {
    staging_root: PathBuf,
}

impl CameraAdapter {
    /// An adapter staging under `staging_root`. Reuse the same root across runs so an
    /// interrupted import resumes instead of starting over.
    pub fn new(staging_root: impl Into<PathBuf>) -> Self {
        Self {
            staging_root: staging_root.into(),
        }
    }

    /// The root [`SourceAdapter::extract`] scans.
    pub fn objects_root(&self) -> PathBuf {
        staging::objects_dir(&self.staging_root)
    }

    /// Copy every object off `device` into the staging root.
    ///
    /// Folders are skipped; every file is received whatever its format. A fatal error (the
    /// connection or the local disk) returns `Err` and leaves what was staged in place for the
    /// next run to resume. Any other error is recorded and the run moves on: a storage the camera
    /// will not list in [`CameraReceipt::storage_failures`], an object in
    /// [`CameraReceipt::failures`].
    pub fn receive(&self, device: &mut dyn CameraDevice) -> Result<CameraReceipt, CameraError> {
        let started = Instant::now();
        let root = &self.staging_root;
        tracing::info!(staging_root = %root.display(), "camera import: receive starts");
        let mut failures = Vec::new();
        let mut storage_failures = Vec::new();
        let objects = enumerate(device, &mut failures, &mut storage_failures)?;
        tracing::info!(
            objects = objects.len(),
            unreadable = failures.len(),
            unlisted_storages = storage_failures.len(),
            "camera import: enumerated"
        );

        let generations = match group_generations(root, &objects) {
            Ok(generations) => generations,
            Err(error) => {
                tracing::error!(
                    %error,
                    "camera import: receive halted before any transfer; the staging tree is unreadable"
                );
                return Err(error);
            }
        };

        let mut received = Vec::new();
        for object in objects {
            let start = generations
                .get(&StemGroup::of(&object))
                .copied()
                .unwrap_or(0);
            match receive_one(root, device, &object, start) {
                Ok(done) => received.push(done),
                Err(error) if !error.is_fatal() => {
                    tracing::warn!(
                        storage = format_args!("{:08x}", object.storage),
                        handle = format_args!("{:08x}", object.handle),
                        filename = %object.filename,
                        %error,
                        "camera import: object failed"
                    );
                    failures.push(ObjectFailure {
                        storage: object.storage,
                        handle: object.handle,
                        filename: Some(object.filename.clone()),
                        error,
                    });
                }
                Err(error) => {
                    tracing::error!(
                        %error,
                        received = received.len(),
                        "camera import: receive halted; staged objects are kept for the next run"
                    );
                    return Err(error);
                }
            }
        }

        let bytes: u64 = received.iter().map(|r| r.size).sum();
        tracing::info!(
            received = received.len(),
            already_staged = received.iter().filter(|r| r.already_staged).count(),
            failed = failures.len(),
            unlisted_storages = storage_failures.len(),
            bytes,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "camera import: receive complete"
        );
        Ok(CameraReceipt {
            objects_root: self.objects_root(),
            received,
            failures,
            storage_failures,
        })
    }
}

impl SourceAdapter for CameraAdapter {
    fn provider(&self) -> ImportProvider {
        ImportProvider::Camera
    }

    /// Scan staged object roots (normally [`CameraReceipt::objects_root`]) into entries, sorted by
    /// primary path. A camera carries no out-of-band metadata: every entry's
    /// [`ExtractedMetadata`] is empty and the files' own EXIF is authoritative.
    fn extract(&self, parts: &[PathBuf]) -> Result<ExtractedImport, AdapterError> {
        if parts.is_empty() {
            return Err(AdapterError::NoParts);
        }
        for part in parts {
            if !part.is_dir() {
                return Err(AdapterError::Io {
                    root: part.display().to_string(),
                    reason: "not a directory".into(),
                });
            }
        }
        let scan = crate::import::scanner::scan(parts).map_err(|e| AdapterError::Io {
            root: parts
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            reason: e.to_string(),
        })?;
        let mut entries: Vec<SourceEntry> = scan
            .candidates
            .into_iter()
            .map(|candidate| SourceEntry {
                candidate,
                metadata: ExtractedMetadata::default(),
            })
            .collect();
        entries.sort_by(|a, b| a.primary_path().cmp(b.primary_path()));
        Ok(ExtractedImport { entries })
    }
}

/// Every file object on the camera, in the contract's order. An object whose record cannot be
/// read is a failure, not an omission, and so is a storage the camera will not list: the other
/// storages are still enumerated.
fn enumerate(
    device: &mut dyn CameraDevice,
    failures: &mut Vec<ObjectFailure>,
    storage_failures: &mut Vec<StorageFailure>,
) -> Result<Vec<CameraObject>, CameraError> {
    let mut storages = match device.storage_ids() {
        Ok(storages) => storages,
        Err(error) if !error.is_fatal() => {
            tracing::warn!(%error, "camera import: storage list refused");
            storage_failures.push(StorageFailure {
                storage: None,
                error,
            });
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    storages.sort_unstable();
    storages.dedup();
    let mut objects = Vec::new();
    for storage in storages {
        let mut handles = match device.object_handles(storage) {
            Ok(handles) => handles,
            Err(error) if !error.is_fatal() => {
                tracing::warn!(
                    storage = format_args!("{storage:08x}"),
                    %error,
                    "camera import: storage not listed; continuing with the others"
                );
                storage_failures.push(StorageFailure {
                    storage: Some(storage),
                    error,
                });
                continue;
            }
            Err(error) => return Err(error),
        };
        handles.sort_unstable();
        handles.dedup();
        tracing::debug!(
            storage = format_args!("{storage:08x}"),
            handles = handles.len(),
            "camera import: storage listed"
        );
        for handle in handles {
            match device.object(handle) {
                Ok(object) if object.is_folder => {}
                Ok(object) => objects.push(object),
                Err(error) if !error.is_fatal() => failures.push(ObjectFailure {
                    storage,
                    handle,
                    filename: None,
                    error,
                }),
                Err(error) => return Err(error),
            }
        }
    }
    objects.sort_by(|a, b| {
        (a.storage, a.handle, a.captured_at).cmp(&(b.storage, b.handle, b.captured_at))
    });
    Ok(objects)
}

/// A file sink that hashes and counts what passes through it.
struct HashingSink<W> {
    inner: W,
    hasher: Sha256Hasher,
    written: u64,
}

impl<W: Write> Write for HashingSink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn staging_error(path: &Path) -> impl FnOnce(io::Error) -> CameraError + '_ {
    move |source| CameraError::Staging {
        path: path.to_path_buf(),
        source,
    }
}

/// Receive one object into its staged path, in folder generation `start` (its stem group's) or,
/// where that is taken, the next free one.
fn receive_one(
    root: &Path,
    device: &mut dyn CameraDevice,
    object: &CameraObject,
    start: u32,
) -> Result<ReceivedObject, CameraError> {
    let generation = match find_slot(root, object, start)? {
        Slot::Staged(done) => return Ok(done),
        Slot::Free { generation } => generation,
    };
    let final_path = staging::object_path(root, object, generation);

    let partial = staging::partial_path(root, object);
    let checkpoint = staging::checkpoint_path(&partial);
    let parent = partial.parent().unwrap_or(root);
    fs::create_dir_all(parent).map_err(staging_error(parent))?;

    // Continue a partial file only where the camera can serve the rest, the size is known, and
    // the prefix an earlier run made durable still hashes to its checkpoint.
    let durable = match object.size {
        Some(size) if device.supports_partial_reads() => {
            durable_prefix(&partial, &checkpoint, size)
        }
        _ => None,
    };
    let (file, hasher, offset) = if let Some((offset, hasher)) = durable {
        let file = OpenOptions::new()
            .append(true)
            .open(&partial)
            .map_err(staging_error(&partial))?;
        // Bytes past the checkpoint were never confirmed durable; receive them again.
        file.set_len(offset).map_err(staging_error(&partial))?;
        tracing::debug!(
            handle = format_args!("{:08x}", object.handle),
            offset,
            "camera import: resuming partial object"
        );
        (file, hasher, offset)
    } else {
        // A stale checkpoint could only ever match the very bytes it recorded; drop it anyway.
        let _ = fs::remove_file(&checkpoint);
        let file = File::create(&partial).map_err(staging_error(&partial))?;
        (file, Sha256Hasher::new(), 0)
    };

    let mut sink = HashingSink {
        inner: BufWriter::new(file),
        hasher,
        written: 0,
    };
    let delivered = match device.read_object(object, offset, &mut sink) {
        Ok(delivered) => delivered,
        Err(CameraError::Sink(source)) => {
            return Err(CameraError::Staging {
                path: partial,
                source,
            });
        }
        Err(error @ CameraError::Disconnected(_)) => {
            // Keep the partial file only when the connection dropped, which is what resuming is
            // for, and only once its bytes are synced and its checkpoint records them.
            if let Err(source) = checkpoint_partial(sink, offset, &partial, &checkpoint) {
                tracing::warn!(
                    handle = format_args!("{:08x}", object.handle),
                    path = %partial.display(),
                    %source,
                    "camera import: partial object not made durable; it will restart"
                );
                discard(&partial, &checkpoint);
            }
            return Err(error);
        }
        Err(error) => {
            // A refusal says nothing about the bytes already written, so start over.
            drop(sink);
            discard(&partial, &checkpoint);
            return Err(error);
        }
    };
    let HashingSink {
        inner,
        hasher,
        written,
    } = sink;
    let file = inner.into_inner().map_err(|e| CameraError::Staging {
        path: partial.clone(),
        source: e.into_error(),
    })?;
    file.sync_all().map_err(staging_error(&partial))?;
    drop(file);

    let size = offset + written;
    if written != delivered {
        tracing::warn!(
            delivered,
            written,
            "camera import: device count and sink count differ; trusting the sink"
        );
    }
    if let Some(declared) = object.size
        && declared != size
    {
        discard(&partial, &checkpoint);
        return Err(CameraError::SizeMismatch {
            handle: object.handle,
            declared,
            received: size,
        });
    }

    let received_hash = hasher.finalize();
    let staged_hash = hash_file(&partial).map_err(staging_error(&partial))?;
    if staged_hash != received_hash {
        discard(&partial, &checkpoint);
        return Err(CameraError::IntegrityMismatch {
            handle: object.handle,
            received: received_hash,
            staged: staged_hash,
        });
    }

    // Record which object this is before the file appears, so a staged file always has one.
    let identity = staging::identity_path(root, object, generation);
    let identity_parent = identity.parent().unwrap_or(root);
    fs::create_dir_all(identity_parent).map_err(staging_error(identity_parent))?;
    write_synced(&identity, staging::identity_key(object).as_bytes())
        .map_err(staging_error(&identity))?;
    let dir = final_path.parent().unwrap_or(root);
    fs::create_dir_all(dir).map_err(staging_error(dir))?;
    fs::rename(&partial, &final_path).map_err(staging_error(&final_path))?;
    let _ = fs::remove_file(&checkpoint);
    tracing::debug!(
        handle = format_args!("{:08x}", object.handle),
        path = %final_path.display(),
        generation,
        size,
        resumed_from = offset,
        sha256 = %received_hash,
        "camera import: object received"
    );
    Ok(ReceivedObject {
        object: object.clone(),
        path: final_path,
        size,
        sha256: received_hash,
        resumed_from: offset,
        already_staged: false,
    })
}

/// The most folder generations one staged path is probed through before the run gives up.
const MAX_GENERATIONS: u32 = 1024;

/// Where an object goes in the staging tree.
enum Slot {
    /// An earlier run already verified and staged this very object.
    Staged(ReceivedObject),
    /// The folder generation to stage it in: free, or holding a damaged copy of this object.
    Free { generation: u32 },
}

/// The scanner's stacking key for a staged object: its storage, its camera folder, and the
/// lowercase stem of its staged file name, as `group_by_stem` computes it. Members of one group
/// stack only when they share a directory, so each group is staged in one folder generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StemGroup {
    storage: u32,
    parent: u32,
    stem: String,
}

impl StemGroup {
    fn of(object: &CameraObject) -> Self {
        Self {
            storage: object.storage,
            parent: object.parent,
            stem: stem_of(&staged_name(object)),
        }
    }
}

/// The file name `object` is staged under, in whichever generation.
fn staged_name(object: &CameraObject) -> OsString {
    staging::object_path(Path::new(""), object, 0)
        .file_name()
        .map(OsString::from)
        .unwrap_or_default()
}

/// The lowercase stem of a file name, as the scanner's stem grouping compares it.
fn stem_of(name: &std::ffi::OsStr) -> String {
    Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}

/// The staged file names in one folder directory, by lowercase stem.
type FolderListing = HashMap<String, Vec<OsString>>;

/// List `dir` by stem; a directory not yet created is empty.
fn list_folder(dir: &Path) -> Result<FolderListing, CameraError> {
    let mut listing = FolderListing::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(listing),
        Err(error) => return Err(staging_error(dir)(error)),
    };
    for entry in entries {
        let name = entry.map_err(staging_error(dir))?.file_name();
        listing.entry(stem_of(&name)).or_default().push(name);
    }
    Ok(listing)
}

/// The folder generation each stem group among `objects` is staged in.
///
/// A group goes to the first generation whose directory holds no file of its stem other than
/// its own members' verified copies, so a stack's members always land side by side and never
/// beside an unrelated earlier object of the same stem (a reformatted card that restarted its
/// counter, shot RAW-only before and RAW+JPEG now). Each folder directory is listed once.
fn group_generations(
    root: &Path,
    objects: &[CameraObject],
) -> Result<HashMap<StemGroup, u32>, CameraError> {
    let mut groups: HashMap<StemGroup, Vec<&CameraObject>> = HashMap::new();
    for object in objects {
        groups
            .entry(StemGroup::of(object))
            .or_default()
            .push(object);
    }
    let mut listings: HashMap<PathBuf, FolderListing> = HashMap::new();
    let mut generations = HashMap::with_capacity(groups.len());
    for (group, members) in groups {
        let generation = group_generation(root, &group, &members, &mut listings)?;
        if generation > 0 {
            tracing::debug!(
                storage = format_args!("{:08x}", group.storage),
                parent = format_args!("{:08x}", group.parent),
                stem = %group.stem,
                members = members.len(),
                generation,
                "camera import: stem group staged in a later folder generation"
            );
        }
        generations.insert(group, generation);
    }
    tracing::debug!(
        groups = generations.len(),
        moved = generations.values().filter(|g| **g > 0).count(),
        folders_listed = listings.len(),
        "camera import: folder generations chosen"
    );
    Ok(generations)
}

/// The first folder generation where every staged file of `group`'s stem is one of `members`
/// holding its own identity record.
fn group_generation(
    root: &Path,
    group: &StemGroup,
    members: &[&CameraObject],
    listings: &mut HashMap<PathBuf, FolderListing>,
) -> Result<u32, CameraError> {
    let first = members[0];
    for generation in 0..MAX_GENERATIONS {
        let path = staging::object_path(root, first, generation);
        let dir = path.parent().unwrap_or(root).to_path_buf();
        if !listings.contains_key(&dir) {
            let listing = list_folder(&dir)?;
            listings.insert(dir.clone(), listing);
        }
        let staged = listings[&dir]
            .get(&group.stem)
            .map_or(&[][..], Vec::as_slice);
        let ours = staged.iter().all(|name| {
            members.iter().any(|member| {
                staged_name(member) == *name && holds_identity(root, member, generation)
            })
        });
        if ours {
            return Ok(generation);
        }
        tracing::debug!(
            dir = %dir.display(),
            stem = %group.stem,
            "camera import: folder holds another object of this stem; trying the next generation"
        );
    }
    Err(CameraError::Staging {
        path: staging::object_path(root, first, 0),
        source: io::Error::other(format!(
            "{MAX_GENERATIONS} staged folders already hold this file stem"
        )),
    })
}

/// Whether the identity recorded for `object`'s path in `generation` is `object`'s own.
fn holds_identity(root: &Path, object: &CameraObject, generation: u32) -> bool {
    fs::read_to_string(staging::identity_path(root, object, generation))
        .is_ok_and(|recorded| recorded.trim() == staging::identity_key(object))
}

/// Find `object`'s place in the staging tree, from folder generation `start` (its stem group's)
/// onwards.
///
/// A staged file is this object only when the identity recorded beside it is this object's
/// identity key; a file without one, or with another object's, is left alone and the next folder
/// generation is tried. That happens past `start` only where two objects of one run share a
/// staged name. A staged copy of this object whose size differs from the camera's declared
/// size is damaged and is received again in place. An accepted copy's bytes were verified before
/// the rename that put them there.
fn find_slot(root: &Path, object: &CameraObject, start: u32) -> Result<Slot, CameraError> {
    let key = staging::identity_key(object);
    for generation in start..MAX_GENERATIONS {
        let path = staging::object_path(root, object, generation);
        let Ok(metadata) = fs::metadata(&path) else {
            return Ok(Slot::Free { generation });
        };
        let recorded = fs::read_to_string(staging::identity_path(root, object, generation)).ok();
        if recorded.as_deref().map(str::trim) != Some(key.as_str()) {
            tracing::debug!(
                handle = format_args!("{:08x}", object.handle),
                path = %path.display(),
                "camera import: staged path holds another object; trying the next generation"
            );
            continue;
        }
        if object.size.is_some_and(|size| size != metadata.len()) {
            tracing::warn!(
                path = %path.display(),
                staged = metadata.len(),
                declared = ?object.size,
                "camera import: staged file differs from the camera's object; fetching again"
            );
            return Ok(Slot::Free { generation });
        }
        let sha256 = hash_file(&path).map_err(staging_error(&path))?;
        tracing::debug!(
            handle = format_args!("{:08x}", object.handle),
            path = %path.display(),
            "camera import: already staged"
        );
        return Ok(Slot::Staged(ReceivedObject {
            object: object.clone(),
            path,
            size: metadata.len(),
            sha256,
            resumed_from: 0,
            already_staged: true,
        }));
    }
    Err(CameraError::Staging {
        path: staging::object_path(root, object, 0),
        source: io::Error::other(format!(
            "{MAX_GENERATIONS} staged objects already hold this path"
        )),
    })
}

/// The length of `partial`'s prefix that an earlier run made durable, and a hasher over it, if
/// its checkpoint is readable and the prefix on disk still hashes to the recorded digest. A
/// prefix a crash corrupted, or one never checkpointed, is not resumed.
fn durable_prefix(partial: &Path, checkpoint: &Path, size: u64) -> Option<(u64, Sha256Hasher)> {
    let recorded = fs::read_to_string(checkpoint).ok()?;
    let (len, digest) = recorded.trim().split_once(' ')?;
    let len: u64 = len.parse().ok()?;
    let digest = Hash32::from_hex(digest).ok()?;
    if len == 0 || len >= size {
        return None;
    }
    let mut hasher = Sha256Hasher::new();
    let read = File::open(partial)
        .and_then(|file| hash_into(&mut file.take(len), &mut hasher))
        .ok()?;
    if read != len || hasher.clone().finalize() != digest {
        tracing::warn!(
            path = %partial.display(),
            checkpoint = len,
            on_disk = read,
            "camera import: partial object no longer matches its checkpoint; restarting it"
        );
        return None;
    }
    Some((len, hasher))
}

/// Sync what `sink` holds to disk, then record its length and digest in `checkpoint`.
fn checkpoint_partial(
    sink: HashingSink<BufWriter<File>>,
    offset: u64,
    partial: &Path,
    checkpoint: &Path,
) -> io::Result<()> {
    let HashingSink {
        inner,
        hasher,
        written,
    } = sink;
    let file = inner.into_inner().map_err(io::IntoInnerError::into_error)?;
    file.sync_all()?;
    drop(file);
    let len = offset + written;
    write_synced(
        checkpoint,
        format!("{len} {}\n", hasher.finalize().to_hex()).as_bytes(),
    )?;
    tracing::debug!(
        path = %partial.display(),
        len,
        "camera import: partial object checkpointed"
    );
    Ok(())
}

/// Write `bytes` to a fresh file at `path` and sync it.
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Remove a partial file and its checkpoint, so the object starts over next time.
fn discard(partial: &Path, checkpoint: &Path) {
    let _ = fs::remove_file(partial);
    let _ = fs::remove_file(checkpoint);
}

/// Hash `reader` to its end into `hasher`; returns the bytes read.
fn hash_into(reader: &mut dyn Read, hasher: &mut Sha256Hasher) -> io::Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
}

#[cfg(test)]
mod tests;
