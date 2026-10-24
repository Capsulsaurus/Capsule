//! Connecting a [`Session`] to a camera's PTP/IP TCP endpoint.
//!
//! [`connect`] runs the whole bring-up: the command channel, the event channel (a second TCP
//! connection to the same endpoint, opened only after the command channel is accepted),
//! `GetDeviceInfo`, `OpenSession`, and the vendor extension's post-open step. Discovery is the
//! caller's: an address from mDNS (`_ptp._tcp`) or typed by the user.

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::dataset::DeviceInfo;
use crate::error::Error;
use crate::session::{InitiatorIdentity, Session};
use crate::vendor::{VendorExtension, extension_for};

/// The IANA-registered PTP/IP port.
pub const DEFAULT_PORT: u16 = 15740;

/// How long a connect, read or write may block before the session fails with a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// TCP connect.
    pub connect: Duration,
    /// Any single read or write on either channel.
    pub io: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            io: Duration::from_secs(30),
        }
    }
}

/// A connected camera: the open session, what it said about itself, and its vendor extension.
#[derive(Debug)]
pub struct Camera {
    /// The open PTP session.
    pub session: Session<TcpStream, TcpStream>,
    /// The camera's `DeviceInfo`.
    pub device: DeviceInfo,
    /// The vendor extension chosen from `device`.
    pub extension: Box<dyn VendorExtension>,
}

fn open(addr: SocketAddr, timeouts: Timeouts) -> Result<TcpStream, Error> {
    let stream = TcpStream::connect_timeout(&addr, timeouts.connect)?;
    stream.set_read_timeout(Some(timeouts.io))?;
    stream.set_write_timeout(Some(timeouts.io))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Connect to the camera at `addr` and open PTP session `session_id`.
pub fn connect(
    addr: SocketAddr,
    initiator: &InitiatorIdentity,
    session_id: u32,
    timeouts: Timeouts,
) -> Result<Camera, Error> {
    tracing::info!(%addr, "ptpip: connecting");
    let pending = Session::begin(open(addr, timeouts)?, initiator)?;
    let mut session = pending.attach_event(open(addr, timeouts)?)?;
    let device = session.device_info()?;
    tracing::info!(
        manufacturer = %device.manufacturer,
        model = %device.model,
        firmware = %device.device_version,
        "ptpip: device info"
    );
    session.open_session(session_id)?;
    let extension = extension_for(&device);
    extension.after_open_session(&mut session, &device)?;
    Ok(Camera {
        session,
        device,
        extension,
    })
}

#[cfg(test)]
mod tests;
