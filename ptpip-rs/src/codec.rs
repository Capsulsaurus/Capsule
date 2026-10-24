//! Little-endian PTP primitives.
//!
//! PTP encodes every integer little-endian. A string is one byte counting UTF-16 code units
//! **including** the terminating NUL (zero for the empty string), then that many UCS-2LE units.
//! An array is a `u32` element count, then the elements.

use thiserror::Error;

/// A PTP value that did not decode.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DecodeError {
    /// The input ended before the value did.
    #[error("truncated: needed {needed} bytes at offset {offset}, {available} available")]
    Truncated {
        /// Offset the read started at.
        offset: usize,
        /// Bytes the value needed.
        needed: usize,
        /// Bytes left in the input.
        available: usize,
    },
    /// A string's units were not valid UTF-16, or its last unit was not NUL.
    #[error("invalid PTP string at offset {offset}")]
    InvalidString {
        /// Offset of the string's count byte.
        offset: usize,
    },
    /// An array declared more elements than the input could possibly hold.
    #[error("array of {count} elements at offset {offset} exceeds the input")]
    ArrayTooLong {
        /// Offset of the array's count.
        offset: usize,
        /// The declared element count.
        count: u32,
    },
    /// A string is too long to encode (PTP caps a string at 255 code units with its NUL).
    #[error("string of {units} UTF-16 units exceeds the PTP limit of 254")]
    StringTooLong {
        /// The string's length in UTF-16 units, without the NUL.
        units: usize,
    },
}

/// A cursor over a PTP payload.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    /// A reader at the start of `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    /// The current offset into the payload.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Consume and return the next `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < n {
            return Err(DecodeError::Truncated {
                offset: self.offset,
                needed: n,
                available: self.remaining(),
            });
        }
        let slice = &self.bytes[self.offset..self.offset + n];
        self.offset += n;
        Ok(slice)
    }

    /// Consume everything left.
    pub fn rest(&mut self) -> &'a [u8] {
        let slice = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        slice
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// A `u8`.
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    /// A little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    /// A little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// A little-endian `u64`.
    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// A PTP string (count byte, then UCS-2LE units ending in NUL).
    pub fn string(&mut self) -> Result<String, DecodeError> {
        let start = self.offset;
        let count = usize::from(self.u8()?);
        if count == 0 {
            return Ok(String::new());
        }
        let raw = self.take(count * 2)?;
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let Some((&0, body)) = units.split_last() else {
            return Err(DecodeError::InvalidString { offset: start });
        };
        String::from_utf16(body).map_err(|_| DecodeError::InvalidString { offset: start })
    }

    /// A NUL-terminated UCS-2LE string with no count prefix, as the PTP/IP init packets carry
    /// the friendly name. Consumes through the NUL; an unterminated tail is an error.
    pub fn nul_terminated_utf16(&mut self) -> Result<String, DecodeError> {
        let start = self.offset;
        let mut units = Vec::new();
        loop {
            if self.remaining() < 2 {
                return Err(DecodeError::InvalidString { offset: start });
            }
            let unit = self.u16()?;
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        String::from_utf16(&units).map_err(|_| DecodeError::InvalidString { offset: start })
    }

    fn array_count(&mut self, element_size: usize) -> Result<usize, DecodeError> {
        let start = self.offset;
        let count = self.u32()?;
        let fits = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(element_size))
            .is_some_and(|bytes| bytes <= self.remaining());
        if !fits {
            return Err(DecodeError::ArrayTooLong {
                offset: start,
                count,
            });
        }
        Ok(count as usize)
    }

    /// A `u16` array.
    pub fn u16_array(&mut self) -> Result<Vec<u16>, DecodeError> {
        let count = self.array_count(2)?;
        (0..count).map(|_| self.u16()).collect()
    }

    /// A `u32` array.
    pub fn u32_array(&mut self) -> Result<Vec<u32>, DecodeError> {
        let count = self.array_count(4)?;
        (0..count).map(|_| self.u32()).collect()
    }
}

/// A builder for a PTP payload.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    /// An empty payload.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes written so far.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Append raw bytes.
    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(bytes);
        self
    }

    /// Append a `u8`.
    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.bytes.push(value);
        self
    }

    /// Append a little-endian `u16`.
    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    /// Append a little-endian `u32`.
    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    /// Append a little-endian `u64`.
    pub fn u64(&mut self, value: u64) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    /// Append a PTP string. The empty string is the single byte `0`.
    pub fn string(&mut self, value: &str) -> Result<&mut Self, DecodeError> {
        let units: Vec<u16> = value.encode_utf16().collect();
        if units.is_empty() {
            return Ok(self.u8(0));
        }
        let count = u8::try_from(units.len() + 1)
            .map_err(|_| DecodeError::StringTooLong { units: units.len() })?;
        self.u8(count);
        for unit in units {
            self.u16(unit);
        }
        Ok(self.u16(0))
    }

    /// Append a NUL-terminated UCS-2LE string with no count prefix.
    pub fn nul_terminated_utf16(&mut self, value: &str) -> &mut Self {
        for unit in value.encode_utf16() {
            self.u16(unit);
        }
        self.u16(0)
    }

    /// Append a `u16` array.
    pub fn u16_array(&mut self, values: &[u16]) -> &mut Self {
        self.u32(values.len() as u32);
        for &value in values {
            self.u16(value);
        }
        self
    }

    /// Append a `u32` array.
    pub fn u32_array(&mut self, values: &[u32]) -> &mut Self {
        self.u32(values.len() as u32);
        for &value in values {
            self.u32(value);
        }
        self
    }
}

#[cfg(test)]
mod tests;
