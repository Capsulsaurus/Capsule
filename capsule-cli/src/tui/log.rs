//! Where log events go while the TUI holds the terminal.
//!
//! The `capsule` binary writes every log event to stderr. Under the TUI, stderr is the same
//! terminal the alternate screen is drawn on, so an event written there would scribble over the
//! frame. [`LogWriter`] is the `tracing-subscriber` writer the binary installs: it writes to
//! stderr until [`redirect_to`] points it at a file, and back to stderr once the returned
//! [`Redirect`] is dropped. Nothing is lost while the TUI runs; it lands in the file.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tracing_subscriber::fmt::MakeWriter;

/// The file log events go to while a [`Redirect`] is held; `None` means stderr.
static TARGET: Mutex<Option<File>> = Mutex::new(None);

fn target() -> MutexGuard<'static, Option<File>> {
    // A panic while writing a log line leaves nothing half-updated worth refusing over.
    TARGET.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The writer the binary's log layer uses: stderr, or the file a [`Redirect`] names.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogWriter;

/// One log event's writer, holding the routing lock for the event so concurrent events never
/// interleave within a line.
pub struct LogSink {
    target: MutexGuard<'static, Option<File>>,
}

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.target.as_mut() {
            Some(file) => file.write(buf),
            None => io::stderr().write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.target.as_mut() {
            Some(file) => file.flush(),
            None => io::stderr().flush(),
        }
    }
}

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = LogSink;

    fn make_writer(&'a self) -> Self::Writer {
        LogSink { target: target() }
    }
}

/// While held, log events go to the file [`redirect_to`] opened; dropping it sends them back to
/// stderr.
#[must_use = "dropping the redirect sends logs back to stderr at once"]
pub struct Redirect(());

impl Drop for Redirect {
    fn drop(&mut self) {
        *target() = None;
    }
}

/// Send log events to `path` (truncated: it holds one TUI session) until the returned guard
/// drops.
///
/// # Errors
///
/// The file could not be created; logs then stay on stderr.
pub fn redirect_to(path: &Path) -> io::Result<Redirect> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    *target() = Some(file);
    Ok(Redirect(()))
}

#[cfg(test)]
mod tests;
