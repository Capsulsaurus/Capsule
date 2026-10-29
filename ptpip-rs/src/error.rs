//! The crate's one error type.

use std::io;

use thiserror::Error;

use crate::codec::DecodeError;
use crate::packet::PacketType;
use crate::ptp::{OperationCode, ResponseCode};

/// Everything a PTP/IP exchange can fail with.
///
/// [`Error::Io`], [`Error::Decode`], [`Error::UnexpectedPacket`], [`Error::PacketTooLarge`],
/// [`Error::TransactionMismatch`], [`Error::DataLength`] and [`Error::Sink`] mean the channel can
/// no longer be trusted (a sink failure abandons a data phase midway) and the session should be
/// dropped; [`Error::Response`] is the responder refusing one operation, after which the session
/// is still usable. [`Error::is_fatal`] tells them apart.
#[derive(Debug, Error)]
pub enum Error {
    /// The underlying channel failed (connection reset, timeout, early EOF).
    #[error("ptp/ip channel i/o: {0}")]
    Io(#[from] io::Error),
    /// A packet or dataset did not decode.
    #[error("ptp/ip decode: {0}")]
    Decode(#[from] DecodeError),
    /// The responder sent a packet the protocol does not allow at this point.
    #[error("ptp/ip: expected {expected}, received {received:?}")]
    UnexpectedPacket {
        /// What the state machine was waiting for.
        expected: &'static str,
        /// The packet type that arrived instead.
        received: PacketType,
    },
    /// A packet header declared a length above the crate's bound or below the header itself.
    #[error("ptp/ip packet length {length} outside 8..={max}")]
    PacketTooLarge {
        /// The declared length.
        length: u32,
        /// The bound in force.
        max: u32,
    },
    /// The responder refused the connection during the PTP/IP handshake.
    #[error("ptp/ip init refused by responder (reason {reason:#010x})")]
    InitFailed {
        /// The `InitFail` reason code.
        reason: u32,
    },
    /// A packet answered a transaction other than the one in flight.
    #[error("ptp/ip transaction mismatch: expected {expected}, received {received}")]
    TransactionMismatch {
        /// The transaction id the initiator sent.
        expected: u32,
        /// The id the responder echoed.
        received: u32,
    },
    /// A data phase delivered a byte count other than the one its `StartData` declared.
    #[error("ptp/ip data phase declared {declared} bytes, delivered {delivered}")]
    DataLength {
        /// The `StartData` total length.
        declared: u64,
        /// The bytes that actually arrived.
        delivered: u64,
    },
    /// The responder completed the operation with a non-`OK` response code.
    #[error("ptp operation {operation} refused: {code}")]
    Response {
        /// The operation that was refused.
        operation: OperationCode,
        /// The response code it was refused with.
        code: ResponseCode,
    },
    /// Writing received data into the caller's sink failed.
    #[error("ptp/ip sink write: {0}")]
    Sink(io::Error),
}

impl Error {
    /// Whether the session is unusable after this error. Only a responder's refusal of one
    /// operation leaves the channel in a known state.
    pub fn is_fatal(&self) -> bool {
        !matches!(self, Error::Response { .. })
    }
}
