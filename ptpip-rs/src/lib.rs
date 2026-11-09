//! A portable PTP/IP (ISO 15740, CIPA DC-005) initiator.
//!
//! Cameras expose their storage over the Picture Transfer Protocol carried on TCP. This crate
//! speaks the initiator side of it, read-only, for an importer:
//!
//! - [`codec`] — the little-endian PTP primitives (integers, UCS-2 strings, arrays).
//! - [`packet`] — PTP/IP framing: the length-prefixed packets on the command and event channels.
//! - [`dataset`] — the PTP datasets an importer reads (`DeviceInfo`, `ObjectInfo`).
//! - [`session`] — a blocking [`Session`] over any `Read + Write` channel pair: the PTP/IP
//!   handshake, `OpenSession`, and the enumeration and transfer operations.
//! - [`vendor`] — the vendor-extension seam, chosen from the device's `DeviceInfo`; Sony first.
//! - [`tcp`] — connecting a [`Session`] to a camera's TCP endpoint (port 15740).
//!
//! The protocol carries no checksum. TCP gives transport-grade integrity only, so the caller
//! hashes what it receives; the session's own guarantee is narrower: a data phase that delivers
//! more or fewer bytes than its `StartData` declared is an error, never a short object.
//!
//! The crate never writes to camera storage: no operation that deletes, moves or sends objects is
//! exposed.

pub mod codec;
pub mod dataset;
pub mod error;
pub mod packet;
pub mod ptp;
pub mod session;
pub mod tcp;
pub mod vendor;

pub use dataset::{DeviceInfo, ObjectInfo, PtpDateTime};
pub use error::Error;
pub use ptp::{ObjectFormat, ObjectHandle, OperationCode, ResponseCode, StorageId};
pub use session::{
    Event, EventsServed, InitiatorIdentity, ResponderIdentity, Session, serve_events,
};
pub use vendor::{VendorExtension, extension_for};

#[cfg(test)]
mod testkit;
