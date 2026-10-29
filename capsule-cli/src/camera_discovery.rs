//! `capsule camera discover`, and the camera `capsule camera import` takes without `--address`:
//! cameras found on the local network through DNS-SD `_ptp._tcp`
//! ([Import — Camera Import — Discovery]).
//!
//! Browsing is a seam ([`Browser`]) with one live adapter, [`MdnsBrowser`] over `mdns-sd`, so the
//! window, a withdrawal and the choice of camera are proved against a scripted responder rather
//! than the real network. [`browse`] drains the seam for a bounded window and folds what answered
//! into the cameras offered; [`choose`] picks the one an import connects to, refusing to guess
//! between several.
//!
//! [Import — Camera Import — Discovery]: https://docs/design/import/camera-import/#discovery

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use eyre::{Result, eyre};
use serde::Serialize;

/// The DNS-SD service type a PTP/IP camera advertises (ISO 15740 over TCP).
pub const SERVICE_TYPE: &str = "_ptp._tcp.local.";

/// The window `capsule camera discover` browses for unless told otherwise, and the one
/// `capsule camera import` browses for without `--address`, in seconds.
pub const DEFAULT_WINDOW_SECS: u64 = 3;

/// The longest window `--timeout` accepts, in seconds: discovery never waits open-ended.
pub const MAX_WINDOW_SECS: u64 = 60;

/// One service instance the network answered with: what a [`Browser`] reports once it is
/// resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAnswer {
    /// The full instance name, e.g. `EOS R5._ptp._tcp.local.`.
    pub fullname: String,
    /// The host the instance runs on, e.g. `eos-r5.local.`.
    pub host: String,
    /// The TCP port it accepts PTP/IP on.
    pub port: u16,
    /// Every address the host answered with, in no particular order.
    pub addresses: Vec<IpAddr>,
}

/// What a [`Browser`] reports while browsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseEvent {
    /// An instance resolved, or resolved again with new records.
    Resolved(ServiceAnswer),
    /// An instance was withdrawn (its goodbye packet, or its records expired).
    Removed {
        /// The full instance name.
        fullname: String,
    },
}

/// The browsing seam: a source of [`BrowseEvent`]s for [`SERVICE_TYPE`].
pub trait Browser {
    /// The next event, waiting at most `timeout` for one; `Ok(None)` when none arrived in that
    /// time or the browse has ended.
    ///
    /// # Errors
    ///
    /// The browse failed and no further event will arrive.
    fn next_event(&mut self, timeout: Duration) -> Result<Option<BrowseEvent>>;
}

/// A camera discovery offered: one `_ptp._tcp` instance, at the one address a client connects
/// to. The camera entry `capsule camera discover --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredCamera {
    /// The instance name, without the service type and domain, e.g. `EOS R5`.
    pub name: String,
    /// The host it advertised, e.g. `eos-r5.local.`.
    pub host: String,
    /// The address to connect to: IPv4 where the host has one, else a routable IPv6 one.
    pub address: IpAddr,
    /// The TCP port it accepts PTP/IP on.
    pub port: u16,
}

impl DiscoveredCamera {
    /// The socket address an import connects to.
    #[must_use]
    pub const fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.address, self.port)
    }
}

/// What one discovery window found: the document `capsule camera discover --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryReport {
    /// The DNS-SD service type browsed.
    pub service_type: &'static str,
    /// How long the window was, in seconds.
    pub window_seconds: u64,
    /// Every camera offered, by name then address.
    pub cameras: Vec<DiscoveredCamera>,
}

/// The instance name `fullname` carries before its service type, e.g. `EOS R5` from
/// `EOS R5._ptp._tcp.local.`; the whole name where it does not end in [`SERVICE_TYPE`].
#[must_use]
pub fn instance_name(fullname: &str) -> &str {
    fullname
        .strip_suffix(SERVICE_TYPE)
        .and_then(|name| name.strip_suffix('.'))
        .unwrap_or(fullname)
}

/// The address a client connects to among `addresses`: the lowest IPv4 one, else the lowest
/// IPv6 one that is not link-local. A camera is offered at a plain address, the one
/// `--json` prints and `--address` takes back, so the interface scope `mdns-sd` reports with
/// each address is not kept, and a link-local address without its scope names no interface to
/// connect through. `None` when there is no such address.
#[must_use]
pub fn connectable_address(addresses: &[IpAddr]) -> Option<IpAddr> {
    let lowest_v4 = addresses.iter().filter(|ip| ip.is_ipv4()).min();
    let lowest_v6 = addresses
        .iter()
        .filter(|ip| match ip {
            IpAddr::V6(v6) => !v6.is_unspecified() && !v6.is_unicast_link_local(),
            IpAddr::V4(_) => false,
        })
        .min();
    lowest_v4.or(lowest_v6).copied()
}

/// The camera `answer` describes, or `None` when it has no address a client can connect to.
#[must_use]
pub fn camera_from(answer: &ServiceAnswer) -> Option<DiscoveredCamera> {
    let address = connectable_address(&answer.addresses)?;
    Some(DiscoveredCamera {
        name: instance_name(&answer.fullname).to_owned(),
        host: answer.host.clone(),
        address,
        port: answer.port,
    })
}

/// Drain `browser` until `window` has passed by `now`, and return every camera still offered at
/// its end, by name then address.
///
/// An instance resolved more than once is offered once, with its latest records; one withdrawn
/// before the window ends is not offered; one with no connectable address is skipped and logged.
///
/// # Errors
///
/// The browse failed before the window ended.
pub fn browse(
    browser: &mut dyn Browser,
    window: Duration,
    mut now: impl FnMut() -> Instant,
) -> Result<Vec<DiscoveredCamera>> {
    let started = now();
    let deadline = started + window;
    tracing::debug!(
        service_type = SERVICE_TYPE,
        window_ms = window.as_millis() as u64,
        "camera discovery: browsing"
    );
    let mut offered: BTreeMap<String, DiscoveredCamera> = BTreeMap::new();
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            break;
        }
        match browser.next_event(remaining)? {
            Some(BrowseEvent::Resolved(answer)) => {
                if let Some(camera) = camera_from(&answer) {
                    tracing::debug!(
                        fullname = %answer.fullname,
                        address = %camera.address,
                        port = camera.port,
                        "camera discovery: resolved"
                    );
                    offered.insert(answer.fullname, camera);
                } else {
                    tracing::debug!(
                        fullname = %answer.fullname,
                        addresses = ?answer.addresses,
                        "camera discovery: resolved with no connectable address; skipped"
                    );
                    offered.remove(&answer.fullname);
                }
            }
            Some(BrowseEvent::Removed { fullname }) => {
                tracing::debug!(%fullname, "camera discovery: withdrawn");
                offered.remove(&fullname);
            }
            None => {}
        }
    }
    let mut cameras: Vec<DiscoveredCamera> = offered.into_values().collect();
    cameras.sort_by(|a, b| (&a.name, a.address).cmp(&(&b.name, b.address)));
    tracing::info!(
        cameras = cameras.len(),
        elapsed_ms = now().duration_since(started).as_millis() as u64,
        "camera discovery: window closed"
    );
    Ok(cameras)
}

/// The camera an import without `--address` connects to: the only one `cameras` holds.
///
/// # Errors
///
/// `cameras` is empty, or holds more than one camera; the message names each one, so the person
/// can pass one by `--address`.
pub fn choose(cameras: Vec<DiscoveredCamera>, window: Duration) -> Result<DiscoveredCamera> {
    let count = cameras.len();
    let mut cameras = cameras.into_iter();
    match (cameras.next(), cameras.next()) {
        (Some(camera), None) => Ok(camera),
        (None, _) => Err(eyre!(
            "No camera answered on the network within {} s. Turn on the camera's Wi-Fi or \
             \"Remote/PC connection\" mode on this network, or pass --address HOST[:PORT].",
            window.as_secs()
        )),
        (Some(first), Some(second)) => {
            let listed = [first, second]
                .into_iter()
                .chain(cameras)
                .map(|camera| format!("  {} at {}", camera.name, camera.socket_addr()))
                .collect::<Vec<_>>()
                .join("\n");
            Err(eyre!(
                "{count} cameras answered on the network; pass --address with the one to \
                 import from:\n{listed}"
            ))
        }
    }
}

/// The live [`Browser`]: an `mdns-sd` daemon browsing [`SERVICE_TYPE`] on every interface,
/// stopped and shut down when dropped.
pub struct MdnsBrowser {
    daemon: mdns_sd::ServiceDaemon,
    events: mdns_sd::Receiver<mdns_sd::ServiceEvent>,
}

impl MdnsBrowser {
    /// Start browsing.
    ///
    /// # Errors
    ///
    /// The daemon could not open its sockets, or would not start the browse.
    pub fn start() -> Result<Self> {
        let daemon = mdns_sd::ServiceDaemon::new()
            .map_err(|e| eyre!("Could not start looking for cameras on the network: {e}"))?;
        let events = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| eyre!("Could not start looking for cameras on the network: {e}"))?;
        Ok(Self { daemon, events })
    }
}

impl Browser for MdnsBrowser {
    fn next_event(&mut self, timeout: Duration) -> Result<Option<BrowseEvent>> {
        match self.events.recv_timeout(timeout) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(resolved)) => {
                Ok(Some(BrowseEvent::Resolved(ServiceAnswer {
                    fullname: resolved.fullname.clone(),
                    host: resolved.host.clone(),
                    port: resolved.port,
                    // The interface scope each address carries is dropped: a camera is offered
                    // at a plain address (see `connectable_address`).
                    addresses: resolved
                        .addresses
                        .iter()
                        .map(mdns_sd::ScopedIp::to_ip_addr)
                        .collect(),
                })))
            }
            Ok(mdns_sd::ServiceEvent::ServiceRemoved(_, fullname)) => {
                Ok(Some(BrowseEvent::Removed { fullname }))
            }
            // Search started or stopped, an instance found but not yet resolved: nothing to
            // offer yet.
            Ok(_) | Err(mdns_sd::RecvTimeoutError::Timeout) => Ok(None),
            Err(mdns_sd::RecvTimeoutError::Disconnected) => Err(eyre!(
                "Looking for cameras on the network stopped: the mDNS daemon ended."
            )),
        }
    }
}

impl Drop for MdnsBrowser {
    fn drop(&mut self) {
        if let Err(error) = self.daemon.stop_browse(SERVICE_TYPE) {
            tracing::debug!(%error, "camera discovery: stopping the browse failed");
        }
        if let Err(error) = self.daemon.shutdown() {
            tracing::debug!(%error, "camera discovery: shutting the mDNS daemon down failed");
        }
    }
}

/// Browse the network for `window` through the live [`MdnsBrowser`].
///
/// # Errors
///
/// The browse could not start, or failed before the window ended.
pub fn discover(window: Duration) -> Result<DiscoveryReport> {
    let mut browser = MdnsBrowser::start()?;
    let cameras = browse(&mut browser, window, Instant::now)?;
    Ok(DiscoveryReport {
        service_type: SERVICE_TYPE,
        window_seconds: window.as_secs(),
        cameras,
    })
}

/// The lines a person reads after `capsule camera discover`.
#[must_use]
pub fn summary_lines(report: &DiscoveryReport) -> Vec<String> {
    if report.cameras.is_empty() {
        return vec![format!(
            "No camera answered on the network within {} s.",
            report.window_seconds
        )];
    }
    let mut lines = vec![format!(
        "{} camera(s) answered within {} s:",
        report.cameras.len(),
        report.window_seconds
    )];
    lines.extend(report.cameras.iter().map(|camera| {
        format!(
            "  {} ({}) at {}",
            camera.name,
            camera.host,
            camera.socket_addr()
        )
    }));
    lines
}

#[cfg(test)]
mod tests;
