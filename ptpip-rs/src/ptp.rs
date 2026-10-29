//! PTP identifiers: operation, response and object-format codes, storage ids and object handles.
//!
//! Codes are open newtypes rather than closed enums: a vendor extension adds operations and
//! response codes in the `0x9xxx` / `0xAxxx` ranges, and a code this crate does not name must
//! still round-trip.

use std::fmt;

/// A PTP operation code (ISO 15740 §10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationCode(pub u16);

impl OperationCode {
    /// `GetDeviceInfo` — the responder's capabilities; valid outside a session.
    pub const GET_DEVICE_INFO: Self = Self(0x1001);
    /// `OpenSession(session_id)`.
    pub const OPEN_SESSION: Self = Self(0x1002);
    /// `CloseSession`.
    pub const CLOSE_SESSION: Self = Self(0x1003);
    /// `GetStorageIDs` — the storages present.
    pub const GET_STORAGE_IDS: Self = Self(0x1004);
    /// `GetObjectHandles(storage, format, association)`.
    pub const GET_OBJECT_HANDLES: Self = Self(0x1007);
    /// `GetObjectInfo(handle)`.
    pub const GET_OBJECT_INFO: Self = Self(0x1008);
    /// `GetObject(handle)` — the whole object.
    pub const GET_OBJECT: Self = Self(0x1009);
    /// `GetPartialObject(handle, offset, max_bytes)` — a byte range, offset limited to 32 bits.
    pub const GET_PARTIAL_OBJECT: Self = Self(0x101B);

    /// The standard name, where this crate knows one.
    pub fn standard_name(self) -> Option<&'static str> {
        Some(match self {
            Self::GET_DEVICE_INFO => "GetDeviceInfo",
            Self::OPEN_SESSION => "OpenSession",
            Self::CLOSE_SESSION => "CloseSession",
            Self::GET_STORAGE_IDS => "GetStorageIDs",
            Self::GET_OBJECT_HANDLES => "GetObjectHandles",
            Self::GET_OBJECT_INFO => "GetObjectInfo",
            Self::GET_OBJECT => "GetObject",
            Self::GET_PARTIAL_OBJECT => "GetPartialObject",
            _ => return None,
        })
    }
}

impl fmt::Display for OperationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.standard_name() {
            Some(name) => write!(f, "{name} ({:#06x})", self.0),
            None => write!(f, "{:#06x}", self.0),
        }
    }
}

/// A PTP response code (ISO 15740 §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResponseCode(pub u16);

impl ResponseCode {
    /// `OK`.
    pub const OK: Self = Self(0x2001);
    /// `General_Error`.
    pub const GENERAL_ERROR: Self = Self(0x2002);
    /// `Session_Not_Open`.
    pub const SESSION_NOT_OPEN: Self = Self(0x2003);
    /// `Operation_Not_Supported`.
    pub const OPERATION_NOT_SUPPORTED: Self = Self(0x2005);
    /// `Invalid_ObjectHandle`.
    pub const INVALID_OBJECT_HANDLE: Self = Self(0x2009);
    /// `Access_Denied`.
    pub const ACCESS_DENIED: Self = Self(0x200F);
    /// `Device_Busy`.
    pub const DEVICE_BUSY: Self = Self(0x2019);
    /// `Session_Already_Open` — `OpenSession` on a responder that already has this session.
    pub const SESSION_ALREADY_OPEN: Self = Self(0x201E);
}

impl fmt::Display for ResponseCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::OK => "OK",
            Self::GENERAL_ERROR => "General_Error",
            Self::SESSION_NOT_OPEN => "Session_Not_Open",
            Self::OPERATION_NOT_SUPPORTED => "Operation_Not_Supported",
            Self::INVALID_OBJECT_HANDLE => "Invalid_ObjectHandle",
            Self::ACCESS_DENIED => "Access_Denied",
            Self::DEVICE_BUSY => "Device_Busy",
            Self::SESSION_ALREADY_OPEN => "Session_Already_Open",
            _ => return write!(f, "{:#06x}", self.0),
        };
        write!(f, "{name} ({:#06x})", self.0)
    }
}

/// A PTP object-format code (ISO 15740 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectFormat(pub u16);

impl ObjectFormat {
    /// `Association` — a folder. It has no bytes of its own to import.
    pub const ASSOCIATION: Self = Self(0x3001);

    /// Whether the object is a folder rather than a file.
    pub fn is_association(self) -> bool {
        self == Self::ASSOCIATION
    }
}

/// A storage id: the physical store in the high 16 bits, the logical one in the low 16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorageId(pub u32);

impl StorageId {
    /// `0xFFFFFFFF`: every storage, as a `GetObjectHandles` argument.
    pub const ALL: Self = Self(0xFFFF_FFFF);
}

impl fmt::Display for StorageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

/// An object handle. Unique within a session; most cameras also keep it stable across sessions,
/// but the standard does not promise that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectHandle(pub u32);

impl ObjectHandle {
    /// `0x00000000` as an association argument: no parent filter. As a `ParentObject`, the
    /// storage root.
    pub const ROOT: Self = Self(0);
}

impl fmt::Display for ObjectHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}
