//! The PTP datasets an importer reads (ISO 15740 §5.5).
//!
//! Each dataset decodes from a data phase's bytes and encodes back, so a test responder can serve
//! exactly what a camera would. Trailing bytes after the last standard field are ignored: vendors
//! append fields, and the standard ones keep their positions.

use std::fmt;

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::Offset;

use crate::codec::{DecodeError, Reader, Writer};
use crate::ptp::{ObjectFormat, ObjectHandle, OperationCode, StorageId};

/// `DeviceInfo` — what the responder is and which operations it implements.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    /// The PTP version, times 100.
    pub standard_version: u16,
    /// The vendor extension id; [`crate::vendor`] chooses an extension from it.
    pub vendor_extension_id: u32,
    /// The vendor extension's version.
    pub vendor_extension_version: u16,
    /// The vendor extension's description.
    pub vendor_extension_desc: String,
    /// The functional mode.
    pub functional_mode: u16,
    /// Operation codes the responder implements.
    pub operations_supported: Vec<u16>,
    /// Event codes it may send.
    pub events_supported: Vec<u16>,
    /// Device property codes it exposes.
    pub device_properties_supported: Vec<u16>,
    /// Formats it can capture.
    pub capture_formats: Vec<u16>,
    /// Formats it can store.
    pub image_formats: Vec<u16>,
    /// Manufacturer.
    pub manufacturer: String,
    /// Model.
    pub model: String,
    /// Firmware version.
    pub device_version: String,
    /// Serial number.
    pub serial_number: String,
}

impl DeviceInfo {
    /// Whether the responder lists `operation` as supported.
    pub fn supports(&self, operation: OperationCode) -> bool {
        self.operations_supported.contains(&operation.0)
    }

    /// Decode a `GetDeviceInfo` data phase.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        Ok(Self {
            standard_version: r.u16()?,
            vendor_extension_id: r.u32()?,
            vendor_extension_version: r.u16()?,
            vendor_extension_desc: r.string()?,
            functional_mode: r.u16()?,
            operations_supported: r.u16_array()?,
            events_supported: r.u16_array()?,
            device_properties_supported: r.u16_array()?,
            capture_formats: r.u16_array()?,
            image_formats: r.u16_array()?,
            manufacturer: r.string()?,
            model: r.string()?,
            device_version: r.string()?,
            serial_number: r.string()?,
        })
    }

    /// Encode as a `GetDeviceInfo` data phase.
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        let mut w = Writer::new();
        w.u16(self.standard_version)
            .u32(self.vendor_extension_id)
            .u16(self.vendor_extension_version)
            .string(&self.vendor_extension_desc)?
            .u16(self.functional_mode)
            .u16_array(&self.operations_supported)
            .u16_array(&self.events_supported)
            .u16_array(&self.device_properties_supported)
            .u16_array(&self.capture_formats)
            .u16_array(&self.image_formats)
            .string(&self.manufacturer)?
            .string(&self.model)?
            .string(&self.device_version)?
            .string(&self.serial_number)?;
        Ok(w.into_bytes())
    }
}

/// `ObjectInfo` — one object's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    /// The storage holding the object.
    pub storage_id: StorageId,
    /// Its format.
    pub format: ObjectFormat,
    /// Protection status (`0` none, `1` read-only).
    pub protection_status: u16,
    /// Its size in bytes. `0xFFFFFFFF` means "4 GiB or more"; the true size is then unknown.
    pub compressed_size: u32,
    /// Thumbnail format.
    pub thumb_format: u16,
    /// Thumbnail size.
    pub thumb_compressed_size: u32,
    /// Thumbnail width.
    pub thumb_pix_width: u32,
    /// Thumbnail height.
    pub thumb_pix_height: u32,
    /// Image width.
    pub image_pix_width: u32,
    /// Image height.
    pub image_pix_height: u32,
    /// Image bit depth.
    pub image_bit_depth: u32,
    /// The folder holding it; [`ObjectHandle::ROOT`] for the storage root.
    pub parent: ObjectHandle,
    /// Association type, for folders.
    pub association_type: u16,
    /// Association description, for folders.
    pub association_desc: u32,
    /// Sequence number within a burst.
    pub sequence_number: u32,
    /// The file name as the camera stores it.
    pub filename: String,
    /// Capture date, as sent. Decode it with [`PtpDateTime::parse`].
    pub capture_date: String,
    /// Modification date, as sent.
    pub modification_date: String,
    /// Keywords.
    pub keywords: String,
}

impl ObjectInfo {
    /// The `compressed_size` sentinel for an object of 4 GiB or more.
    pub const SIZE_UNKNOWN: u32 = 0xFFFF_FFFF;

    /// The object's size, where the 32-bit field can state it.
    pub fn known_size(&self) -> Option<u64> {
        (self.compressed_size != Self::SIZE_UNKNOWN).then_some(u64::from(self.compressed_size))
    }

    /// The capture date, where it is present and well-formed.
    pub fn captured_at(&self) -> Option<PtpDateTime> {
        PtpDateTime::parse(&self.capture_date)
    }

    /// Decode a `GetObjectInfo` data phase.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        Ok(Self {
            storage_id: StorageId(r.u32()?),
            format: ObjectFormat(r.u16()?),
            protection_status: r.u16()?,
            compressed_size: r.u32()?,
            thumb_format: r.u16()?,
            thumb_compressed_size: r.u32()?,
            thumb_pix_width: r.u32()?,
            thumb_pix_height: r.u32()?,
            image_pix_width: r.u32()?,
            image_pix_height: r.u32()?,
            image_bit_depth: r.u32()?,
            parent: ObjectHandle(r.u32()?),
            association_type: r.u16()?,
            association_desc: r.u32()?,
            sequence_number: r.u32()?,
            filename: r.string()?,
            capture_date: r.string()?,
            modification_date: r.string()?,
            keywords: r.string()?,
        })
    }

    /// Encode as a `GetObjectInfo` data phase.
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        let mut w = Writer::new();
        w.u32(self.storage_id.0)
            .u16(self.format.0)
            .u16(self.protection_status)
            .u32(self.compressed_size)
            .u16(self.thumb_format)
            .u32(self.thumb_compressed_size)
            .u32(self.thumb_pix_width)
            .u32(self.thumb_pix_height)
            .u32(self.image_pix_width)
            .u32(self.image_pix_height)
            .u32(self.image_bit_depth)
            .u32(self.parent.0)
            .u16(self.association_type)
            .u32(self.association_desc)
            .u32(self.sequence_number)
            .string(&self.filename)?
            .string(&self.capture_date)?
            .string(&self.modification_date)?
            .string(&self.keywords)?;
        Ok(w.into_bytes())
    }
}

/// A PTP `DateTime` string decoded: `YYYYMMDDThhmmss`, optional `.s` tenths, then optionally `Z`
/// or `±hhmm`. Cameras almost always omit the zone, so [`offset`](Self::offset) is usually
/// `None` and the time is the camera clock's wall time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PtpDateTime {
    /// The wall-clock date and time.
    pub civil: DateTime,
    /// The UTC offset, when the string carried one.
    pub offset: Option<Offset>,
}

impl PtpDateTime {
    /// Decode a PTP `DateTime` string; `None` for an empty or malformed one.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim_end_matches('\0');
        let bytes = raw.as_bytes();
        if bytes.len() < 15 || bytes[8] != b'T' || !raw.is_ascii() {
            return None;
        }
        let num = |range: std::ops::Range<usize>| raw.get(range)?.parse::<i32>().ok();
        let date = Date::new(
            i16::try_from(num(0..4)?).ok()?,
            i8::try_from(num(4..6)?).ok()?,
            i8::try_from(num(6..8)?).ok()?,
        )
        .ok()?;
        let mut rest = &raw[15..];
        let mut subsec_nanos = 0;
        if let Some(after_dot) = rest.strip_prefix('.') {
            let tenth = after_dot.get(..1)?.parse::<i32>().ok()?;
            subsec_nanos = tenth * 100_000_000;
            rest = &after_dot[1..];
        }
        let time = Time::new(
            i8::try_from(num(9..11)?).ok()?,
            i8::try_from(num(11..13)?).ok()?,
            i8::try_from(num(13..15)?).ok()?,
            subsec_nanos,
        )
        .ok()?;
        let offset = match rest {
            "" => None,
            "Z" => Some(Offset::UTC),
            zone => {
                let sign = match zone.as_bytes().first()? {
                    b'+' => 1,
                    b'-' => -1,
                    _ => return None,
                };
                if zone.len() != 5 {
                    return None;
                }
                let hours: i32 = zone[1..3].parse().ok()?;
                let minutes: i32 = zone[3..5].parse().ok()?;
                Some(Offset::from_seconds(sign * (hours * 3600 + minutes * 60)).ok()?)
            }
        };
        Some(Self {
            civil: date.to_datetime(time),
            offset,
        })
    }
}

impl fmt::Display for PtpDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.civil)?;
        if let Some(offset) = self.offset {
            write!(f, "{offset}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
