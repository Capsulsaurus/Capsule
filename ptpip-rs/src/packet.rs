//! PTP/IP framing (CIPA DC-005 §2.3).
//!
//! Every packet is `length: u32` (the whole packet, header included), `type: u32`, then a
//! type-specific payload, all little-endian. [`read_packet`] bounds the declared length before it
//! allocates, so a hostile or confused responder cannot make the initiator reserve gigabytes.

use std::fmt;
use std::io::{Read, Write};

use crate::codec::{DecodeError, Reader, Writer};
use crate::error::Error;
use crate::ptp::{OperationCode, ResponseCode};

/// The header's size: length and type, four bytes each.
pub const HEADER_LEN: u32 = 8;

/// The largest packet [`read_packet`] accepts. Cameras send object data in `Data` packets of a
/// few hundred KiB to a few MiB; 64 MiB is far above any observed chunk and still a bounded
/// allocation.
pub const MAX_PACKET_LEN: u32 = 64 * 1024 * 1024;

/// The PTP/IP protocol version this initiator speaks (1.0).
pub const PROTOCOL_VERSION: u32 = 0x0001_0000;

/// The at-most-five parameters an operation request or response carries.
pub const MAX_PARAMS: usize = 5;

/// The packet types of CIPA DC-005, plus any type this crate does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    /// 1 — initiator opens the command channel.
    InitCommandRequest,
    /// 2 — responder accepts the command channel.
    InitCommandAck,
    /// 3 — initiator opens the event channel.
    InitEventRequest,
    /// 4 — responder accepts the event channel.
    InitEventAck,
    /// 5 — responder refuses either channel.
    InitFail,
    /// 6 — an operation request.
    OperationRequest,
    /// 7 — an operation's response.
    OperationResponse,
    /// 8 — an asynchronous event.
    Event,
    /// 9 — a data phase begins.
    StartData,
    /// 10 — a data phase chunk.
    Data,
    /// 11 — cancel a transaction.
    Cancel,
    /// 12 — a data phase's final chunk.
    EndData,
    /// 13 — keep-alive probe.
    ProbeRequest,
    /// 14 — keep-alive answer.
    ProbeResponse,
    /// A type number outside the standard.
    Unknown(u32),
}

impl PacketType {
    /// The wire number.
    pub fn code(self) -> u32 {
        match self {
            Self::InitCommandRequest => 1,
            Self::InitCommandAck => 2,
            Self::InitEventRequest => 3,
            Self::InitEventAck => 4,
            Self::InitFail => 5,
            Self::OperationRequest => 6,
            Self::OperationResponse => 7,
            Self::Event => 8,
            Self::StartData => 9,
            Self::Data => 10,
            Self::Cancel => 11,
            Self::EndData => 12,
            Self::ProbeRequest => 13,
            Self::ProbeResponse => 14,
            Self::Unknown(code) => code,
        }
    }

    /// The type a wire number names.
    pub fn from_code(code: u32) -> Self {
        match code {
            1 => Self::InitCommandRequest,
            2 => Self::InitCommandAck,
            3 => Self::InitEventRequest,
            4 => Self::InitEventAck,
            5 => Self::InitFail,
            6 => Self::OperationRequest,
            7 => Self::OperationResponse,
            8 => Self::Event,
            9 => Self::StartData,
            10 => Self::Data,
            11 => Self::Cancel,
            12 => Self::EndData,
            13 => Self::ProbeRequest,
            14 => Self::ProbeResponse,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for PacketType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Whether an operation's data phase, if any, flows to the responder (CIPA DC-005 §2.3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataPhase {
    /// No data phase, or data from the responder to the initiator (`1`).
    NoneOrIn,
    /// Data from the initiator to the responder (`2`).
    Out,
    /// Any other value a responder might send back in a decoded request.
    Unknown(u32),
}

impl DataPhase {
    fn code(self) -> u32 {
        match self {
            Self::NoneOrIn => 1,
            Self::Out => 2,
            Self::Unknown(code) => code,
        }
    }

    fn from_code(code: u32) -> Self {
        match code {
            1 => Self::NoneOrIn,
            2 => Self::Out,
            other => Self::Unknown(other),
        }
    }
}

/// One decoded PTP/IP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Initiator → responder, command channel.
    InitCommandRequest {
        /// The initiator's 16-byte GUID; a responder that pairs remembers it.
        guid: [u8; 16],
        /// The initiator's display name.
        name: String,
        /// [`PROTOCOL_VERSION`].
        version: u32,
    },
    /// Responder → initiator, command channel.
    InitCommandAck {
        /// The connection number the event channel must quote.
        connection: u32,
        /// The responder's GUID.
        guid: [u8; 16],
        /// The responder's display name.
        name: String,
        /// The responder's protocol version.
        version: u32,
    },
    /// Initiator → responder, event channel.
    InitEventRequest {
        /// The connection number from [`Packet::InitCommandAck`].
        connection: u32,
    },
    /// Responder → initiator, event channel.
    InitEventAck,
    /// Responder → initiator, either channel.
    InitFail {
        /// Why the responder refused.
        reason: u32,
    },
    /// An operation request.
    OperationRequest {
        /// The data phase's direction.
        phase: DataPhase,
        /// The operation.
        code: OperationCode,
        /// The transaction id.
        transaction: u32,
        /// Up to [`MAX_PARAMS`] parameters.
        params: Vec<u32>,
    },
    /// An operation's response.
    OperationResponse {
        /// The response code.
        code: ResponseCode,
        /// The transaction it answers.
        transaction: u32,
        /// Up to [`MAX_PARAMS`] parameters.
        params: Vec<u32>,
    },
    /// An asynchronous event.
    Event {
        /// The event code.
        code: u16,
        /// The transaction it relates to, or zero.
        transaction: u32,
        /// Up to three parameters.
        params: Vec<u32>,
    },
    /// A data phase begins.
    StartData {
        /// The transaction.
        transaction: u32,
        /// The data phase's total length; `u64::MAX` when the sender does not know it.
        total: u64,
    },
    /// A data phase chunk.
    Data {
        /// The transaction.
        transaction: u32,
        /// The chunk.
        payload: Vec<u8>,
    },
    /// Cancel a transaction.
    Cancel {
        /// The transaction.
        transaction: u32,
    },
    /// A data phase's last chunk.
    EndData {
        /// The transaction.
        transaction: u32,
        /// The chunk (possibly empty).
        payload: Vec<u8>,
    },
    /// Keep-alive probe.
    ProbeRequest,
    /// Keep-alive answer.
    ProbeResponse,
    /// A packet of an unknown type, kept so the caller can name it in an error.
    Unknown {
        /// The type number.
        kind: u32,
        /// Its payload.
        payload: Vec<u8>,
    },
}

impl Packet {
    /// The packet's type.
    pub fn kind(&self) -> PacketType {
        match self {
            Self::InitCommandRequest { .. } => PacketType::InitCommandRequest,
            Self::InitCommandAck { .. } => PacketType::InitCommandAck,
            Self::InitEventRequest { .. } => PacketType::InitEventRequest,
            Self::InitEventAck => PacketType::InitEventAck,
            Self::InitFail { .. } => PacketType::InitFail,
            Self::OperationRequest { .. } => PacketType::OperationRequest,
            Self::OperationResponse { .. } => PacketType::OperationResponse,
            Self::Event { .. } => PacketType::Event,
            Self::StartData { .. } => PacketType::StartData,
            Self::Data { .. } => PacketType::Data,
            Self::Cancel { .. } => PacketType::Cancel,
            Self::EndData { .. } => PacketType::EndData,
            Self::ProbeRequest => PacketType::ProbeRequest,
            Self::ProbeResponse => PacketType::ProbeResponse,
            Self::Unknown { kind, .. } => PacketType::Unknown(*kind),
        }
    }

    /// The packet's payload, without the header.
    pub fn encode_payload(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Self::InitCommandRequest {
                guid,
                name,
                version,
            } => {
                w.bytes(guid).nul_terminated_utf16(name).u32(*version);
            }
            Self::InitCommandAck {
                connection,
                guid,
                name,
                version,
            } => {
                w.u32(*connection)
                    .bytes(guid)
                    .nul_terminated_utf16(name)
                    .u32(*version);
            }
            Self::InitEventRequest { connection } => {
                w.u32(*connection);
            }
            Self::InitFail { reason } => {
                w.u32(*reason);
            }
            Self::OperationRequest {
                phase,
                code,
                transaction,
                params,
            } => {
                w.u32(phase.code()).u16(code.0).u32(*transaction);
                for &param in params {
                    w.u32(param);
                }
            }
            Self::OperationResponse {
                code,
                transaction,
                params,
            } => {
                w.u16(code.0).u32(*transaction);
                for &param in params {
                    w.u32(param);
                }
            }
            Self::Event {
                code,
                transaction,
                params,
            } => {
                w.u16(*code).u32(*transaction);
                for &param in params {
                    w.u32(param);
                }
            }
            Self::StartData { transaction, total } => {
                w.u32(*transaction).u64(*total);
            }
            Self::Data {
                transaction,
                payload,
            }
            | Self::EndData {
                transaction,
                payload,
            } => {
                w.u32(*transaction).bytes(payload);
            }
            Self::Cancel { transaction } => {
                w.u32(*transaction);
            }
            Self::Unknown { payload, .. } => {
                w.bytes(payload);
            }
            Self::InitEventAck | Self::ProbeRequest | Self::ProbeResponse => {}
        }
        w.into_bytes()
    }

    /// The whole packet: header, then payload.
    pub fn encode(&self) -> Vec<u8> {
        let payload = self.encode_payload();
        let mut w = Writer::new();
        w.u32(HEADER_LEN + payload.len() as u32)
            .u32(self.kind().code())
            .bytes(&payload);
        w.into_bytes()
    }

    /// Decode a payload of the given type.
    pub fn decode(kind: PacketType, payload: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(payload);
        let packet = match kind {
            PacketType::InitCommandRequest => {
                let guid = guid(&mut r)?;
                let name = r.nul_terminated_utf16()?;
                Self::InitCommandRequest {
                    guid,
                    name,
                    version: r.u32()?,
                }
            }
            PacketType::InitCommandAck => {
                let connection = r.u32()?;
                let guid = guid(&mut r)?;
                let name = r.nul_terminated_utf16()?;
                Self::InitCommandAck {
                    connection,
                    guid,
                    name,
                    version: r.u32()?,
                }
            }
            PacketType::InitEventRequest => Self::InitEventRequest {
                connection: r.u32()?,
            },
            PacketType::InitEventAck => Self::InitEventAck,
            PacketType::InitFail => Self::InitFail { reason: r.u32()? },
            PacketType::OperationRequest => {
                let phase = DataPhase::from_code(r.u32()?);
                let code = OperationCode(r.u16()?);
                let transaction = r.u32()?;
                Self::OperationRequest {
                    phase,
                    code,
                    transaction,
                    params: params(&mut r)?,
                }
            }
            PacketType::OperationResponse => {
                let code = ResponseCode(r.u16()?);
                let transaction = r.u32()?;
                Self::OperationResponse {
                    code,
                    transaction,
                    params: params(&mut r)?,
                }
            }
            PacketType::Event => {
                let code = r.u16()?;
                let transaction = r.u32()?;
                Self::Event {
                    code,
                    transaction,
                    params: params(&mut r)?,
                }
            }
            PacketType::StartData => Self::StartData {
                transaction: r.u32()?,
                total: r.u64()?,
            },
            PacketType::Data => Self::Data {
                transaction: r.u32()?,
                payload: r.rest().to_vec(),
            },
            PacketType::Cancel => Self::Cancel {
                transaction: r.u32()?,
            },
            PacketType::EndData => Self::EndData {
                transaction: r.u32()?,
                payload: r.rest().to_vec(),
            },
            PacketType::ProbeRequest => Self::ProbeRequest,
            PacketType::ProbeResponse => Self::ProbeResponse,
            PacketType::Unknown(kind) => Self::Unknown {
                kind,
                payload: r.rest().to_vec(),
            },
        };
        Ok(packet)
    }
}

fn guid(r: &mut Reader<'_>) -> Result<[u8; 16], DecodeError> {
    let mut out = [0u8; 16];
    out.copy_from_slice(r.take(16)?);
    Ok(out)
}

/// The trailing `u32` parameters of a request, response or event. Responders pad inconsistently,
/// so up to [`MAX_PARAMS`] whole parameters are read and any trailing partial word is ignored.
fn params(r: &mut Reader<'_>) -> Result<Vec<u32>, DecodeError> {
    let mut out = Vec::new();
    while r.remaining() >= 4 && out.len() < MAX_PARAMS {
        out.push(r.u32()?);
    }
    Ok(out)
}

/// Write one packet to a channel.
pub fn write_packet<W: Write + ?Sized>(channel: &mut W, packet: &Packet) -> Result<(), Error> {
    tracing::trace!(kind = %packet.kind(), "ptpip: send packet");
    channel.write_all(&packet.encode())?;
    channel.flush()?;
    Ok(())
}

/// Read one packet from a channel, refusing a declared length outside `8..=MAX_PACKET_LEN`.
pub fn read_packet<R: Read + ?Sized>(channel: &mut R) -> Result<Packet, Error> {
    let mut header = [0u8; HEADER_LEN as usize];
    channel.read_exact(&mut header)?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let kind = PacketType::from_code(u32::from_le_bytes([
        header[4], header[5], header[6], header[7],
    ]));
    if !(HEADER_LEN..=MAX_PACKET_LEN).contains(&length) {
        return Err(Error::PacketTooLarge {
            length,
            max: MAX_PACKET_LEN,
        });
    }
    let mut payload = vec![0u8; (length - HEADER_LEN) as usize];
    channel.read_exact(&mut payload)?;
    tracing::trace!(%kind, length, "ptpip: received packet");
    Ok(Packet::decode(kind, &payload)?)
}

#[cfg(test)]
mod tests;
