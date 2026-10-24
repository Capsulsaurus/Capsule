//! Sony's PTP extension (vendor extension id `0x00000011`).
//!
//! Sony Alpha bodies that advertise `SDIOConnect` expect a three-phase connect handshake after
//! `OpenSession` before they serve the session normally: `SDIOConnect(1)`, `SDIOConnect(2)`,
//! `SDIOGetExtDeviceInfo(0xC8)`, `SDIOConnect(3)` — the sequence libgphoto2's Sony driver runs at
//! camera init. A body that does not advertise `SDIOConnect` gets no handshake. The handshake's
//! data phases are read and discarded: this crate is read-only and uses none of Sony's
//! remote-control properties.
//!
//! Sony's write extensions (deleting a file after import) are deliberately absent. Camera storage
//! is read-only to Capsule; a future delete-after-import must pass Verify Before Destroy first.

use crate::dataset::DeviceInfo;
use crate::error::Error;
use crate::ptp::OperationCode;
use crate::vendor::{Transact, VendorExtension};

/// Sony's PTP vendor extension id.
pub const VENDOR_ID: u32 = 0x0000_0011;

/// `SDIOConnect(phase, 0, 0)`.
pub const SDIO_CONNECT: OperationCode = OperationCode(0x9201);
/// `SDIOGetExtDeviceInfo(version)`.
pub const SDIO_GET_EXT_DEVICE_INFO: OperationCode = OperationCode(0x9202);

/// The extension-protocol version the handshake asks for (`0xC8`, i.e. 2.00).
const EXT_DEVICE_INFO_VERSION: u32 = 0xC8;

/// The Sony extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sony;

impl Sony {
    /// Whether `device` is a Sony body: its vendor extension id is Sony's, or it reports Sony as
    /// its manufacturer (some bodies advertise the Microsoft/MTP extension id over Wi-Fi).
    pub fn claims(device: &DeviceInfo) -> bool {
        device.vendor_extension_id == VENDOR_ID
            || device.manufacturer.to_ascii_lowercase().starts_with("sony")
    }
}

impl VendorExtension for Sony {
    fn name(&self) -> &'static str {
        "sony"
    }

    fn operation_name(&self, code: OperationCode) -> Option<&'static str> {
        match code {
            SDIO_CONNECT => Some("SDIOConnect"),
            SDIO_GET_EXT_DEVICE_INFO => Some("SDIOGetExtDeviceInfo"),
            _ => None,
        }
    }

    fn after_open_session(
        &self,
        session: &mut dyn Transact,
        device: &DeviceInfo,
    ) -> Result<(), Error> {
        if !device.supports(SDIO_CONNECT) {
            tracing::debug!(model = %device.model, "ptpip sony: no SDIOConnect; no handshake");
            return Ok(());
        }
        tracing::info!(model = %device.model, "ptpip sony: SDIO connect handshake");
        let mut discard = std::io::sink();
        session.transact(SDIO_CONNECT, &[1, 0, 0], Some(&mut discard))?;
        session.transact(SDIO_CONNECT, &[2, 0, 0], Some(&mut discard))?;
        if device.supports(SDIO_GET_EXT_DEVICE_INFO) {
            session.transact(
                SDIO_GET_EXT_DEVICE_INFO,
                &[EXT_DEVICE_INFO_VERSION],
                Some(&mut discard),
            )?;
        }
        session.transact(SDIO_CONNECT, &[3, 0, 0], Some(&mut discard))?;
        tracing::debug!("ptpip sony: handshake complete");
        Ok(())
    }
}
