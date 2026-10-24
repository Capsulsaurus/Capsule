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

/// The outcome of [`CameraAdapter::receive`].
#[derive(Debug)]
pub struct CameraReceipt {
    /// The root to hand to [`SourceAdapter::extract`].
    pub objects_root: PathBuf,
    /// Every object received, in enumeration order.
    pub received: Vec<ReceivedObject>,
    /// Every object that failed, in enumeration order. Never dropped silently.
    pub failures: Vec<ObjectFailure>,
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
    /// next run to resume; any other error is recorded in [`CameraReceipt::failures`] and the run
    /// moves on.
    pub fn receive(&self, device: &mut dyn CameraDevice) -> Result<CameraReceipt, CameraError> {
        let started = Instant::now();
        let root = &self.staging_root;
        tracing::info!(staging_root = %root.display(), "camera import: receive starts");
        let mut failures = Vec::new();
        let objects = enumerate(device, &mut failures)?;
        tracing::info!(
            objects = objects.len(),
            unreadable = failures.len(),
            "camera import: enumerated"
        );

        let mut received = Vec::new();
        for object in objects {
            match receive_one(root, device, &object) {
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
            bytes,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "camera import: receive complete"
        );
        Ok(CameraReceipt {
            objects_root: self.objects_root(),
            received,
            failures,
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
/// read is a failure, not an omission.
fn enumerate(
    device: &mut dyn CameraDevice,
    failures: &mut Vec<ObjectFailure>,
) -> Result<Vec<CameraObject>, CameraError> {
    let mut storages = device.storage_ids()?;
    storages.sort_unstable();
    storages.dedup();
    let mut objects = Vec::new();
    for storage in storages {
        let mut handles = device.object_handles(storage)?;
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

/// Receive one object into its staged path.
fn receive_one(
    root: &Path,
    device: &mut dyn CameraDevice,
    object: &CameraObject,
) -> Result<ReceivedObject, CameraError> {
    let final_path = staging::object_path(root, object);
    if let Some(done) = already_staged(&final_path, object)? {
        return Ok(done);
    }

    let partial = staging::partial_path(root, object);
    let parent = partial.parent().unwrap_or(root);
    fs::create_dir_all(parent).map_err(staging_error(parent))?;

    // Continue a partial file only where the camera can serve the rest and the size is known.
    let mut hasher = Sha256Hasher::new();
    let mut offset = 0u64;
    let existing = fs::metadata(&partial).map_or(0, |m| m.len());
    let resumable = device.supports_partial_reads()
        && object
            .size
            .is_some_and(|size| existing > 0 && existing < size);
    let file = if resumable {
        let mut reader = File::open(&partial).map_err(staging_error(&partial))?;
        hash_into(&mut reader, &mut hasher).map_err(staging_error(&partial))?;
        offset = existing;
        tracing::debug!(
            handle = format_args!("{:08x}", object.handle),
            offset,
            "camera import: resuming partial object"
        );
        OpenOptions::new()
            .append(true)
            .open(&partial)
            .map_err(staging_error(&partial))?
    } else {
        File::create(&partial).map_err(staging_error(&partial))?
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
        Err(error) => {
            // Keep the partial file only when the connection dropped: that is what resuming is
            // for. A refusal says nothing about the bytes already written, so start over.
            if !matches!(error, CameraError::Disconnected(_)) {
                drop(sink);
                let _ = fs::remove_file(&partial);
            }
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
        let _ = fs::remove_file(&partial);
        return Err(CameraError::SizeMismatch {
            handle: object.handle,
            declared,
            received: size,
        });
    }

    let received_hash = hasher.finalize();
    let staged_hash = hash_file(&partial).map_err(staging_error(&partial))?;
    if staged_hash != received_hash {
        let _ = fs::remove_file(&partial);
        return Err(CameraError::IntegrityMismatch {
            handle: object.handle,
            received: received_hash,
            staged: staged_hash,
        });
    }

    let dir = final_path.parent().unwrap_or(root);
    fs::create_dir_all(dir).map_err(staging_error(dir))?;
    fs::rename(&partial, &final_path).map_err(staging_error(&final_path))?;
    tracing::debug!(
        handle = format_args!("{:08x}", object.handle),
        path = %final_path.display(),
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

/// An object an earlier run already verified and staged, if its staged file still matches the
/// camera's declared size. Its bytes were verified before the rename that put them there.
fn already_staged(
    final_path: &Path,
    object: &CameraObject,
) -> Result<Option<ReceivedObject>, CameraError> {
    let Ok(metadata) = fs::metadata(final_path) else {
        return Ok(None);
    };
    if object.size.is_some_and(|size| size != metadata.len()) {
        tracing::warn!(
            path = %final_path.display(),
            staged = metadata.len(),
            declared = ?object.size,
            "camera import: staged file differs from the camera's object; fetching again"
        );
        return Ok(None);
    }
    let sha256 = hash_file(final_path).map_err(staging_error(final_path))?;
    tracing::debug!(
        handle = format_args!("{:08x}", object.handle),
        path = %final_path.display(),
        "camera import: already staged"
    );
    Ok(Some(ReceivedObject {
        object: object.clone(),
        path: final_path.to_path_buf(),
        size: metadata.len(),
        sha256,
        resumed_from: 0,
        already_staged: true,
    }))
}

fn hash_into(reader: &mut dyn Read, hasher: &mut Sha256Hasher) -> io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        hasher.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests;
