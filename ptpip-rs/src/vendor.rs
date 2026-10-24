//! The vendor-extension seam.
//!
//! PTP leaves the `0x9xxx` operation range to vendors, and a camera may need a vendor handshake
//! before it serves its storage the standard way. A [`VendorExtension`] is chosen from the
//! responder's `DeviceInfo` by [`extension_for`] and runs once after `OpenSession`. Everything
//! vendor-specific stays behind this trait, so an importer speaks standard PTP and never names a
//! vendor opcode.
//!
//! Sony is the first extension ([`sony::Sony`]); a camera no extension claims gets [`Generic`],
//! which does nothing beyond the standard.

pub mod sony;

use std::fmt;
use std::io::{Read, Write};

use crate::dataset::DeviceInfo;
use crate::error::Error;
use crate::ptp::OperationCode;
use crate::session::{Completed, Session};

/// Issues one PTP transaction. The object-safe face of a [`Session`] a [`VendorExtension`] drives.
pub trait Transact {
    /// Run `operation` with `params`; a responder data phase flows into `sink`.
    fn transact(
        &mut self,
        operation: OperationCode,
        params: &[u32],
        sink: Option<&mut dyn Write>,
    ) -> Result<Completed, Error>;
}

impl<C: Read + Write, E: Read + Write> Transact for Session<C, E> {
    fn transact(
        &mut self,
        operation: OperationCode,
        params: &[u32],
        sink: Option<&mut dyn Write>,
    ) -> Result<Completed, Error> {
        self.transaction(operation, params, sink)
    }
}

/// A vendor's additions to standard PTP.
pub trait VendorExtension: fmt::Debug + Send + Sync {
    /// The vendor's name, for logs.
    fn name(&self) -> &'static str;

    /// The name of a vendor operation code, for logs.
    fn operation_name(&self, _code: OperationCode) -> Option<&'static str> {
        None
    }

    /// Run once after `OpenSession`, before any enumeration. The default does nothing.
    fn after_open_session(
        &self,
        _session: &mut dyn Transact,
        _device: &DeviceInfo,
    ) -> Result<(), Error> {
        Ok(())
    }
}

/// The extension for a camera no vendor extension claims: standard PTP only.
#[derive(Debug, Clone, Copy, Default)]
pub struct Generic;

impl VendorExtension for Generic {
    fn name(&self) -> &'static str {
        "generic"
    }
}

/// The extension for `device`: the first vendor extension that claims it, else [`Generic`].
pub fn extension_for(device: &DeviceInfo) -> Box<dyn VendorExtension> {
    if sony::Sony::claims(device) {
        tracing::debug!(model = %device.model, "ptpip: Sony vendor extension");
        return Box::new(sony::Sony);
    }
    tracing::debug!(
        vendor_extension_id = format_args!("{:#010x}", device.vendor_extension_id),
        model = %device.model,
        "ptpip: no vendor extension claims the device; standard PTP only"
    );
    Box::new(Generic)
}

#[cfg(test)]
mod tests;
