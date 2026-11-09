//! `capsule camera import`: the reference flow for importing straight from a camera over PTP/IP.
//!
//! The run is the camera source adapter ([`CameraAdapter`]) feeding the one import pipeline
//! ([Import — Camera Import]):
//!
//! 1. **Connect** to the address the person gave, or the one camera discovery found
//!    ([`crate::camera_discovery`]), through `ptpip::tcp::connect`, presenting the client's
//!    persisted initiator identity ([`crate::camera_pairing`]), and start the event-channel reader
//!    ([`ptpip::serve_events`]) on its own thread so the camera's keep-alive probes are answered
//!    while a long transfer holds the command channel.
//! 2. **Receive** every object into a staging root under the library,
//!    `.library/camera-staging/<camera>/`, one per camera so an interrupted run resumes only its
//!    own partial objects. Every object the camera refused and every storage it would not list
//!    is reported, never dropped.
//! 3. **Import** the staged tree through the same plan and execute path `capsule import` runs
//!    (`import_run::import_scanned`): RAW+JPEG stacking, content-hash dedup and copy
//!    mode are unchanged, so re-importing a card the library holds reports duplicates.
//!
//! The staged objects are kept after the run: they are what lets the next run skip every object
//! it already holds without fetching it again, and the camera's own storage is never written.
//!
//! [Import — Camera Import]: https://docs/design/import/camera-import/

use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use capsule_library::import::{
    CameraAdapter, CameraDevice, CameraReceipt, PtpCamera, SourceAdapter, SourceMetadataIndex,
    safe_file_name,
};
use capsule_library::lifecycle::Workspace;
use eyre::{Result, WrapErr, eyre};
use ptpip::tcp::{DEFAULT_PORT, Timeouts};
use ptpip::{DeviceInfo, EventsServed};
use serde::Serialize;

use crate::camera_discovery::DiscoveredCamera;
use crate::camera_pairing::Pairing;
use crate::import_report::{self, ImportReport};

/// Where, under a library root, the camera adapter stages what it receives.
pub const STAGING_DIR: &str = ".library/camera-staging";

/// The PTP session id the CLI opens. A camera that still holds this id from an interrupted run
/// answers `Session_Already_Open`, which the session treats as open.
const SESSION_ID: u32 = 1;

/// The longest per-camera staging directory name.
const MAX_CAMERA_KEY: usize = 96;

/// The camera a run reached, as its `DeviceInfo` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CameraSummary {
    /// The address connected to.
    pub address: String,
    /// The manufacturer.
    pub manufacturer: String,
    /// The model.
    pub model: String,
    /// The serial number.
    pub serial_number: String,
}

/// One object the camera would not hand over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceiveFailure {
    /// The storage (card slot) it is on.
    pub storage: u32,
    /// Its object handle.
    pub handle: u32,
    /// Its file name, where its record could be read.
    pub filename: Option<String>,
    /// Why it failed.
    pub error: String,
}

/// A storage, or the storage list itself (`storage: null`), the camera would not enumerate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageFailureReport {
    /// The storage; `None` when the camera would not list its storages at all.
    pub storage: Option<u32>,
    /// Why it failed.
    pub error: String,
}

/// What the receive phase did: the adapter's [`CameraReceipt`], counted and made printable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReceiveReport {
    /// The staging root the objects were received into.
    pub staging_root: String,
    /// File objects received or found already staged.
    pub received: usize,
    /// Of those, the ones an earlier run had already staged and were not fetched again.
    pub already_staged: usize,
    /// Of those, the ones continued from a partial transfer an earlier run left.
    pub resumed: usize,
    /// Every object that failed, in enumeration order.
    pub failures: Vec<ReceiveFailure>,
    /// Every storage the camera would not list, in storage order.
    pub storage_failures: Vec<StorageFailureReport>,
}

impl ReceiveReport {
    /// Count `receipt`, received into `staging_root`.
    #[must_use]
    pub fn new(staging_root: &Path, receipt: &CameraReceipt) -> Self {
        Self {
            staging_root: staging_root.display().to_string(),
            received: receipt.received.len(),
            already_staged: receipt.received.iter().filter(|r| r.already_staged).count(),
            resumed: receipt
                .received
                .iter()
                .filter(|r| r.resumed_from > 0)
                .count(),
            failures: receipt
                .failures
                .iter()
                .map(|failure| ReceiveFailure {
                    storage: failure.storage,
                    handle: failure.handle,
                    filename: failure.filename.clone(),
                    error: failure.error.to_string(),
                })
                .collect(),
            storage_failures: receipt
                .storage_failures
                .iter()
                .map(|failure| StorageFailureReport {
                    storage: failure.storage,
                    error: failure.error.to_string(),
                })
                .collect(),
        }
    }
}

/// What the event-channel reader handled during the run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EventsReport {
    /// Events the camera sent.
    pub events: u64,
    /// Keep-alive probes answered.
    pub probes: u64,
    /// Why the reader stopped early, if the event channel failed. The command channel, and so
    /// the import, is unaffected by it.
    pub error: Option<String>,
}

/// The outcome of a `capsule camera import` run: the one JSON document `--json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CameraImportReport {
    /// The camera.
    pub camera: CameraSummary,
    /// The discovery answer the camera was chosen from when no address was given; `None` when
    /// the person gave one.
    pub discovered: Option<DiscoveredCamera>,
    /// The initiator identity presented to it.
    pub initiator: Pairing,
    /// The receive phase.
    pub receive: ReceiveReport,
    /// The import of what was received, exactly as `capsule import --json` reports one.
    pub import: ImportReport,
    /// The event channel.
    pub events: EventsReport,
}

/// Resolve `address` (`HOST` or `HOST:PORT`, an IPv6 literal in brackets with a port) to the
/// first socket address it names, on the PTP/IP port 15740 when none is given.
///
/// # Errors
///
/// The address is blank, or resolves to nothing.
pub fn parse_address(address: &str) -> Result<SocketAddr> {
    let address = address.trim();
    if address.is_empty() {
        return Err(eyre!("The camera address is empty."));
    }
    if let Ok(addr) = address.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = address
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
    {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT));
    }
    // A host name: with a port when the last `:` ends in one, else on the default port.
    let with_port = match address.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() => address.to_owned(),
        _ => format!("{address}:{DEFAULT_PORT}"),
    };
    with_port
        .to_socket_addrs()
        .wrap_err_with(|| format!("Could not resolve the camera address {address}."))?
        .next()
        .ok_or_else(|| eyre!("The camera address {address} resolves to nothing."))
}

/// The per-camera staging directory name: manufacturer, model and serial number, reduced to
/// characters valid in a file name on every supported platform, and never a name Windows
/// reserves for a device.
#[must_use]
pub fn camera_key(device: &DeviceInfo) -> String {
    let raw = [
        device.manufacturer.as_str(),
        device.model.as_str(),
        device.serial_number.as_str(),
    ]
    .iter()
    .map(|part| part.trim())
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("-");
    let mut key = String::with_capacity(raw.len());
    for c in raw.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '-' | '.') {
            c
        } else {
            '_'
        };
        // One `_` for a run of replaced characters.
        if !(c == '_' && key.ends_with('_')) {
            key.push(c);
        }
    }
    let key: String = key
        .trim_matches(|c| matches!(c, '_' | '.' | '-'))
        .chars()
        .take(MAX_CAMERA_KEY)
        .collect();
    if key.is_empty() {
        return "camera".to_owned();
    }
    // The key holds only ASCII letters, digits, `-`, `.` and `_` and no trailing dot, so what is
    // left to repair is a Windows device name (`CON`, `NUL.x`, `COM1`), which gains a leading `_`.
    safe_file_name(&key).map_or_else(|| "camera".to_owned(), std::borrow::Cow::into_owned)
}

/// The staging root for the camera `device` describes, under the library at `library_root`.
#[must_use]
pub fn staging_root(library_root: &Path, device: &DeviceInfo) -> PathBuf {
    library_root.join(STAGING_DIR).join(camera_key(device))
}

/// Receive everything on `device` into `staging_root`, then import what is staged into
/// `workspace`.
///
/// # Errors
///
/// The connection or the local disk failed while receiving (what was staged is kept, and the
/// next run resumes it), or the staged tree could not be read, planned or executed. One object
/// the camera refused is not one of them: it is listed in the report's receive failures.
pub fn receive_and_import(
    workspace: &mut Workspace,
    device: &mut dyn CameraDevice,
    staging_root: &Path,
) -> Result<(ReceiveReport, ImportReport)> {
    let started = Instant::now();
    tracing::info!(staging_root = %staging_root.display(), "camera import: receiving");
    let adapter = CameraAdapter::new(staging_root);
    let receipt = adapter.receive(device).map_err(|error| {
        tracing::warn!(%error, "camera import: receive stopped");
        eyre!(
            "The camera import stopped: {error}. What was already received is kept under {} \
             and the next run resumes it.",
            staging_root.display()
        )
    })?;
    let receive = ReceiveReport::new(staging_root, &receipt);
    tracing::info!(
        received = receive.received,
        already_staged = receive.already_staged,
        resumed = receive.resumed,
        failures = receive.failures.len(),
        storage_failures = receive.storage_failures.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "camera import: received"
    );

    // Nothing ever staged: there is no tree to scan, and nothing to import.
    if !receipt.objects_root.is_dir() {
        tracing::info!("camera import: nothing staged");
        return Ok((receive, ImportReport::default()));
    }
    let extracted = adapter
        .extract(std::slice::from_ref(&receipt.objects_root))
        .map_err(|e| eyre!("Reading the staged camera objects failed: {e}"))?;
    let index = SourceMetadataIndex::from_extracted(&extracted);
    let import = crate::import_run::import_scanned(workspace, &extracted.to_scan_result(), &index)?;
    tracing::info!(
        imported = import.imported,
        duplicates = import.duplicates,
        errors = import.errors,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "camera import: finished"
    );
    Ok((receive, import))
}

/// The event-channel reader running beside a session, and what stops it.
struct EventReader {
    stop: Arc<AtomicBool>,
    channel: TcpStream,
    thread: JoinHandle<Result<EventsServed, ptpip::Error>>,
}

impl EventReader {
    /// Start servicing `event` on its own thread, over a second handle to the same socket.
    fn start(event: &TcpStream) -> std::io::Result<Self> {
        let reader = event.try_clone()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("ptpip-events".into())
            .spawn(move || {
                ptpip::serve_events(reader, &flag, |event| {
                    tracing::debug!(
                        code = format_args!("{:#06x}", event.code),
                        params = ?event.params,
                        "camera import: camera event"
                    );
                })
            })?;
        Ok(Self {
            stop,
            channel: event.try_clone()?,
            thread,
        })
    }

    /// Stop the reader: set its flag and shut the channel, which ends a read in progress.
    fn finish(self) -> EventsReport {
        self.stop.store(true, Ordering::Relaxed);
        if let Err(error) = self.channel.shutdown(Shutdown::Both) {
            tracing::debug!(%error, "camera import: event channel already closed");
        }
        match self.thread.join() {
            Ok(Ok(served)) => EventsReport {
                events: served.events,
                probes: served.probes,
                error: None,
            },
            Ok(Err(error)) => {
                tracing::warn!(%error, "camera import: event channel reader failed");
                EventsReport {
                    error: Some(error.to_string()),
                    ..EventsReport::default()
                }
            }
            Err(_) => EventsReport {
                error: Some("the event channel reader panicked".to_owned()),
                ..EventsReport::default()
            },
        }
    }
}

/// Connect to the camera at `address` as `pairing`, receive everything on it into the library
/// `workspace` holds, import it, and close the session.
///
/// `discovered` is the discovery answer `address` came from, when the person gave none; it is
/// carried into the report as is. `on_connected` is called once the camera answered, so a
/// person sees which camera the run reached before the transfer starts.
///
/// # Errors
///
/// The camera could not be reached or refused the session, or [`receive_and_import`] failed.
pub fn run(
    workspace: &mut Workspace,
    address: SocketAddr,
    discovered: Option<DiscoveredCamera>,
    pairing: &Pairing,
    on_connected: impl FnOnce(&CameraSummary),
) -> Result<CameraImportReport> {
    let mut camera = ptpip::tcp::connect(
        address,
        &pairing.initiator(),
        SESSION_ID,
        Timeouts::default(),
    )
    .map_err(|e| eyre!("Could not open a PTP/IP session with the camera at {address}: {e}"))?;
    let summary = CameraSummary {
        address: address.to_string(),
        manufacturer: camera.device.manufacturer.clone(),
        model: camera.device.model.clone(),
        serial_number: camera.device.serial_number.clone(),
    };
    on_connected(&summary);
    let staging = staging_root(workspace.root(), &camera.device);
    let reader = EventReader::start(camera.session.event_channel())
        .wrap_err("Could not start the camera's event channel reader")?;
    let mut device = PtpCamera::from(camera);

    let outcome = receive_and_import(workspace, &mut device, &staging);

    // Close the PTP session whatever the outcome, so the camera is not left holding it; a close
    // the camera refuses or a connection already gone changes nothing that was imported.
    if let Err(error) = device.into_session().close_session() {
        tracing::debug!(%error, "camera import: closing the session failed");
    }
    let events = reader.finish();
    let (receive, import) = outcome?;
    Ok(CameraImportReport {
        camera: summary,
        discovered,
        initiator: pairing.clone(),
        receive,
        import,
        events,
    })
}

/// The lines a person reads after a run: what the camera would not hand over, then the import's
/// closing line. Every receive failure is named, so an object left on the camera is never
/// silent.
#[must_use]
pub fn summary_lines(report: &CameraImportReport) -> Vec<String> {
    let receive = &report.receive;
    let mut lines = vec![format!(
        "Received {} object(s) from the camera ({} already staged, {} resumed) into {}.",
        receive.received, receive.already_staged, receive.resumed, receive.staging_root
    )];
    for failure in &receive.storage_failures {
        lines.push(match failure.storage {
            Some(storage) => format!(
                "  ✗ storage {storage:#010x} could not be listed: {}",
                failure.error
            ),
            None => format!(
                "  ✗ the camera's storages could not be listed: {}",
                failure.error
            ),
        });
    }
    for failure in &receive.failures {
        lines.push(format!(
            "  ✗ {} (storage {:#010x}, handle {:#010x}) was not received: {}",
            failure.filename.as_deref().unwrap_or("an object"),
            failure.storage,
            failure.handle,
            failure.error
        ));
    }
    if !receive.failures.is_empty() || !receive.storage_failures.is_empty() {
        lines.push(format!(
            "{} object(s) and {} storage(s) stayed on the camera; run the import again to retry \
             them.",
            receive.failures.len(),
            receive.storage_failures.len()
        ));
    }
    lines.push(import_report::summary_line(&report.import));
    lines
}

#[cfg(test)]
mod tests;
