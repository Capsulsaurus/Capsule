//! The [`CameraDevice`] port over a `ptpip` session.

use std::io::{Read, Write};

use ptpip::session::Session;
use ptpip::{DeviceInfo, ObjectHandle, OperationCode, StorageId};

use super::{CameraDevice, CameraError, CameraObject};

/// The largest range one `GetPartialObject` asks for when resuming.
const PARTIAL_CHUNK: u32 = 8 * 1024 * 1024;

/// A camera reached over PTP/IP: an open session and the `DeviceInfo` it reported.
#[derive(Debug)]
pub struct PtpCamera<C, E> {
    session: Session<C, E>,
    device: DeviceInfo,
}

impl<C: Read + Write, E: Read + Write> PtpCamera<C, E> {
    /// Wrap a session whose PTP session is already open.
    pub fn new(session: Session<C, E>, device: DeviceInfo) -> Self {
        Self { session, device }
    }

    /// The camera's `DeviceInfo`.
    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    /// The underlying session, e.g. to close it.
    pub fn into_session(self) -> Session<C, E> {
        self.session
    }
}

impl From<ptpip::tcp::Camera> for PtpCamera<std::net::TcpStream, std::net::TcpStream> {
    fn from(camera: ptpip::tcp::Camera) -> Self {
        Self::new(camera.session, camera.device)
    }
}

impl From<ptpip::Error> for CameraError {
    fn from(error: ptpip::Error) -> Self {
        match error {
            ptpip::Error::Response { operation, code } => CameraError::Refused {
                operation: operation.to_string(),
                code: code.to_string(),
            },
            ptpip::Error::Sink(source) => CameraError::Sink(source),
            other => CameraError::Disconnected(other.to_string()),
        }
    }
}

impl<C: Read + Write, E: Read + Write> CameraDevice for PtpCamera<C, E> {
    fn supports_partial_reads(&self) -> bool {
        self.device.supports(OperationCode::GET_PARTIAL_OBJECT)
    }

    fn storage_ids(&mut self) -> Result<Vec<u32>, CameraError> {
        Ok(self
            .session
            .storage_ids()?
            .into_iter()
            .map(|id| id.0)
            .collect())
    }

    fn object_handles(&mut self, storage: u32) -> Result<Vec<u32>, CameraError> {
        Ok(self
            .session
            .object_handles(StorageId(storage))?
            .into_iter()
            .map(|handle| handle.0)
            .collect())
    }

    fn object(&mut self, handle: u32) -> Result<CameraObject, CameraError> {
        let info = self.session.object_info(ObjectHandle(handle))?;
        Ok(CameraObject {
            storage: info.storage_id.0,
            handle,
            parent: info.parent.0,
            size: info.known_size(),
            captured_at: info.captured_at().map(|at| at.civil),
            is_folder: info.format.is_association(),
            filename: info.filename,
        })
    }

    fn read_object(
        &mut self,
        object: &CameraObject,
        offset: u64,
        sink: &mut dyn Write,
    ) -> Result<u64, CameraError> {
        let handle = ObjectHandle(object.handle);
        if offset == 0 {
            return Ok(self.session.get_object(handle, sink)?);
        }
        let unsupported = CameraError::ResumeUnsupported {
            handle: object.handle,
            offset,
        };
        let Some(size) = object.size else {
            return Err(unsupported);
        };
        if !self.supports_partial_reads() || u32::try_from(offset).is_err() {
            return Err(unsupported);
        }
        let mut position = offset;
        while position < size {
            // A known size is below 4 GiB, so every offset inside it fits the 32-bit parameter.
            let Ok(at) = u32::try_from(position) else {
                return Err(CameraError::ResumeUnsupported {
                    handle: object.handle,
                    offset: position,
                });
            };
            let want = u32::try_from(size - position)
                .map_or(PARTIAL_CHUNK, |left| left.min(PARTIAL_CHUNK));
            let got = self.session.get_partial_object(handle, at, want, sink)?;
            tracing::trace!(handle = %handle, at, got, "camera import: partial chunk");
            if got == 0 {
                // The camera has nothing past `position`; the caller's size check reports it.
                break;
            }
            position += got;
        }
        Ok(position - offset)
    }
}

#[cfg(test)]
mod tests;
