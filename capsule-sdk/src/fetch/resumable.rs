//! File-backed resumable download (SSoT: [Download & Sync] §Resumption and Verification).
//!
//! [`RangedFetcher::fetch_to_file`] lands a blob at a destination path through a partial file
//! beside it ([`partial_path`]):
//!
//! - **The partial's length is the resume position.** Each window is written and then
//!   `fsync`ed before the next is requested, so the length a later attempt reads never counts a
//!   byte that is not on disk. There is no separate counter that could run ahead of the data.
//! - **Resume across a restart.** A new attempt — in this process or the next — hashes the
//!   bytes the partial already holds and asks the source only for the rest.
//! - **Atomic landing.** The destination name appears only by a `rename` of the partial after
//!   the content address verifies over every byte, so no reader ever sees a partial or
//!   unverified file under the final name.
//! - **A corrupt partial is discarded.** A content-address mismatch deletes the partial (it
//!   cannot be trusted as a prefix of anything); so does a partial longer than the blob. The
//!   caller's next attempt starts from zero.
//!
//! Any other failure — transient, stalled, refused, a local I/O error — keeps the partial, so
//! the next attempt resumes from it. The partial is keyed by the destination path, so one
//! destination names one content address: a partial left by a different blob fails
//! verification and is discarded, never renamed into place.
//!
//! [Download & Sync]: https://docs/design/transfer/download/

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracing::instrument;

use super::{BlobSource, FetchError, RangedFetcher, Resume};
use crate::net::{ConnectionClass, MonotonicClock, Sleeper, TransferSignal};

/// The suffix [`partial_path`] appends to a destination's file name.
pub const PARTIAL_SUFFIX: &str = ".partial";

/// The read buffer used to re-hash a partial on resume: bounded, whatever the partial's size.
const REHASH_BUFFER: usize = 64 * 1024;

/// The partial file a download to `dest` is written to before it is renamed into place:
/// `dest` with [`PARTIAL_SUFFIX`] appended, in the same directory so the rename stays on one
/// filesystem and is atomic.
#[must_use]
pub fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(PARTIAL_SUFFIX);
    PathBuf::from(name)
}

impl<S: Sleeper, C: MonotonicClock> RangedFetcher<S, C> {
    /// Fetch a whole blob into the file `dest`, resumable across a process restart, and make it
    /// visible under `dest` only once its content address verifies.
    ///
    /// Bytes land in [`partial_path`]`(dest)`, one bounded window at a time (as
    /// [`fetch_into`](Self::fetch_into)), each `fsync`ed before the next is requested. An
    /// attempt that finds a partial already there re-hashes it in bounded memory and resumes
    /// from its length. On success the partial is synced and atomically renamed over `dest`;
    /// the directory holding `dest` is then synced, and a failure of that last sync is logged,
    /// not returned, because the verified blob has already landed.
    ///
    /// On [`FetchError::IntegrityFailed`] the partial is deleted and `dest` is untouched. Every
    /// other error leaves the partial for the next attempt to resume. `dest`'s directory must
    /// exist.
    #[instrument(skip(self, source, dest), fields(dest = %dest.display(), class = ?self.class))]
    pub async fn fetch_to_file<B: BlobSource>(
        &self,
        source: &B,
        hash: &str,
        expected_len: u64,
        dest: &Path,
    ) -> Result<Vec<TransferSignal>, FetchError> {
        let partial = partial_path(dest);
        let (mut file, resume) = open_partial(&partial, expected_len).map_err(FetchError::Sink)?;
        if resume.held > 0 {
            tracing::info!(
                hash,
                held = resume.held,
                expected_len,
                "resuming download from its partial file"
            );
        } else {
            tracing::debug!(hash, expected_len, "starting download into a partial file");
        }
        let outcome = self
            .run_from(
                source,
                hash,
                expected_len,
                self.sink_window(),
                resume,
                &mut file,
                |file: &mut File| file.sync_data(),
            )
            .await;
        match outcome {
            Ok(signals) => {
                file.sync_all().map_err(FetchError::Sink)?;
                drop(file);
                land(&partial, dest).map_err(FetchError::Sink)?;
                tracing::info!(
                    hash,
                    expected_len,
                    "download verified and renamed into place"
                );
                Ok(signals)
            }
            Err(error @ FetchError::IntegrityFailed { .. }) => {
                drop(file);
                discard(&partial);
                Err(error)
            }
            Err(error) => {
                tracing::debug!(hash, %error, "download interrupted; partial kept for resume");
                Err(error)
            }
        }
    }
}

/// Fetch a whole blob into the file `dest` — the unmetered convenience over
/// [`RangedFetcher::fetch_to_file`], the file-backed counterpart of
/// [`fetch_blob_into`](super::fetch_blob_into).
pub async fn fetch_blob_to_file<S: BlobSource>(
    source: &S,
    hash: &str,
    expected_len: u64,
    dest: &Path,
) -> Result<(), FetchError> {
    RangedFetcher::new(ConnectionClass::Unmetered)
        .fetch_to_file(source, hash, expected_len, dest)
        .await
        .map(drop)
}

/// Open (or create) the partial, positioned at its end, with the hash of what it holds.
///
/// A partial longer than the blob cannot be a prefix of it, so it is truncated and the fetch
/// starts over.
fn open_partial(partial: &Path, expected_len: u64) -> io::Result<(File, Resume)> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(partial)?;
    let mut held = file.metadata()?.len();
    if held > expected_len {
        tracing::warn!(
            partial = %partial.display(),
            held,
            expected_len,
            "partial is longer than the blob; discarding it"
        );
        file.set_len(0)?;
        file.sync_data()?;
        held = 0;
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; REHASH_BUFFER];
    let mut hashed: u64 = 0;
    file.seek(SeekFrom::Start(0))?;
    while hashed < held {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        hashed += read as u64;
    }
    if hashed != held {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("partial reported {held} bytes but {hashed} could be read"),
        ));
    }
    file.seek(SeekFrom::Start(held))?;
    Ok((file, Resume { held, hasher }))
}

/// Rename the verified partial over `dest`, then make the rename itself durable.
///
/// Only a failed rename is an error: the partial is then still in place for the next attempt.
/// Once the rename returns, the verified blob is at `dest` and the partial is gone, so a failure
/// to `fsync` the parent directory is logged rather than raised. A crash before the directory
/// reaches disk can at worst revert the rename, which brings back the complete partial; the next
/// attempt re-verifies it and lands it without fetching a byte.
fn land(partial: &Path, dest: &Path) -> io::Result<()> {
    fs::rename(partial, dest)?;
    if let Err(error) = sync_parent(dest) {
        tracing::warn!(
            dest = %dest.display(),
            %error,
            "download landed but its directory could not be synced; the rename may not survive a crash"
        );
    }
    Ok(())
}

/// `fsync` the directory holding `path`, so a rename into it survives a crash. Windows has no
/// directory handle to sync; its rename is already durable once it returns.
#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Delete a partial that failed verification. A failure to delete is logged, not raised: the
/// verification error is the one the caller acts on, and the next attempt re-verifies anyway.
fn discard(partial: &Path) {
    match fs::remove_file(partial) {
        Ok(()) => tracing::warn!(
            partial = %partial.display(),
            "partial failed content-address verification; discarded"
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::error!(
            partial = %partial.display(),
            %error,
            "partial failed verification and could not be deleted"
        ),
    }
}
