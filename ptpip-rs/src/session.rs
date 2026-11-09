//! A blocking PTP/IP initiator session over a command and an event channel.
//!
//! The connection state machine (CIPA DC-005 §2.4):
//!
//! 1. On the command channel the initiator sends `InitCommandRequest`; the responder answers
//!    `InitCommandAck` with a connection number, or `InitFail` ([`Session::begin`]).
//! 2. The initiator opens the event channel and quotes that number in `InitEventRequest`; the
//!    responder answers `InitEventAck` or `InitFail` ([`PendingSession::attach_event`]).
//! 3. PTP operations follow on the command channel: `OpenSession`, then the transactions.
//!
//! Each transaction is an `OperationRequest`, an optional data phase from the responder
//! (`StartData`, `Data`…, `EndData`) and an `OperationResponse`, all quoting one transaction id.
//! The session checks that every packet quotes the id in flight and that the data phase delivers
//! exactly the length `StartData` declared, and streams data to a caller-supplied sink so an
//! object of any size is held in memory only one packet at a time.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::dataset::{DeviceInfo, ObjectInfo};
use crate::error::Error;
use crate::packet::{DataPhase, MAX_PARAMS, PROTOCOL_VERSION, Packet, read_packet, write_packet};
use crate::ptp::{ObjectHandle, OperationCode, ResponseCode, StorageId};

/// Who the initiator is. A camera that pairs (Sony's "Remote/PC connection") remembers the GUID,
/// so the caller persists it in its client-local pairing config and reuses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitiatorIdentity {
    /// A stable 16-byte GUID for this initiator.
    pub guid: [u8; 16],
    /// The name the camera shows for this initiator.
    pub name: String,
}

/// Who the responder said it is in `InitCommandAck`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponderIdentity {
    /// The connection number the event channel quotes.
    pub connection: u32,
    /// The responder's GUID.
    pub guid: [u8; 16],
    /// The responder's name.
    pub name: String,
    /// The responder's PTP/IP protocol version.
    pub version: u32,
}

/// A command channel that completed `InitCommandRequest`, waiting for its event channel.
#[derive(Debug)]
pub struct PendingSession<C> {
    command: C,
    responder: ResponderIdentity,
}

/// What an operation's `OperationResponse` returned, once its code was `OK`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completed {
    /// The response parameters.
    pub params: Vec<u32>,
    /// The data phase's byte count; zero when there was none.
    pub data_len: u64,
}

/// An initiator session.
#[derive(Debug)]
pub struct Session<C, E> {
    command: C,
    event: E,
    responder: ResponderIdentity,
    /// The id the next in-session transaction uses. Zero until `OpenSession` succeeds, which is
    /// what the standard requires of `OpenSession` itself and of `GetDeviceInfo` before it.
    next_transaction: u32,
    session_id: Option<u32>,
}

impl<C: Read + Write> Session<C, ()> {
    /// Step 1: open the command channel. Sends `InitCommandRequest` and waits for its answer.
    pub fn begin(
        mut command: C,
        initiator: &InitiatorIdentity,
    ) -> Result<PendingSession<C>, Error> {
        tracing::debug!(initiator = %initiator.name, "ptpip: init command channel");
        write_packet(
            &mut command,
            &Packet::InitCommandRequest {
                guid: initiator.guid,
                name: initiator.name.clone(),
                version: PROTOCOL_VERSION,
            },
        )?;
        match read_packet(&mut command)? {
            Packet::InitCommandAck {
                connection,
                guid,
                name,
                version,
            } => {
                tracing::info!(
                    responder = %name,
                    connection,
                    version = format_args!("{version:#010x}"),
                    "ptpip: command channel accepted"
                );
                Ok(PendingSession {
                    command,
                    responder: ResponderIdentity {
                        connection,
                        guid,
                        name,
                        version,
                    },
                })
            }
            Packet::InitFail { reason } => {
                tracing::warn!(
                    reason = format_args!("{reason:#010x}"),
                    "ptpip: command channel refused"
                );
                Err(Error::InitFailed { reason })
            }
            other => Err(Error::UnexpectedPacket {
                expected: "InitCommandAck",
                received: other.kind(),
            }),
        }
    }
}

impl<C: Read + Write> PendingSession<C> {
    /// The responder's identity from `InitCommandAck`.
    pub fn responder(&self) -> &ResponderIdentity {
        &self.responder
    }

    /// Step 2: attach the event channel, opened after the command channel was accepted.
    pub fn attach_event<E: Read + Write>(self, mut event: E) -> Result<Session<C, E>, Error> {
        let connection = self.responder.connection;
        write_packet(&mut event, &Packet::InitEventRequest { connection })?;
        match read_packet(&mut event)? {
            Packet::InitEventAck => {
                tracing::debug!(connection, "ptpip: event channel accepted");
                Ok(Session {
                    command: self.command,
                    event,
                    responder: self.responder,
                    next_transaction: 0,
                    session_id: None,
                })
            }
            Packet::InitFail { reason } => Err(Error::InitFailed { reason }),
            other => Err(Error::UnexpectedPacket {
                expected: "InitEventAck",
                received: other.kind(),
            }),
        }
    }
}

impl<C: Read + Write, E: Read + Write> Session<C, E> {
    /// The responder's identity from the handshake.
    pub fn responder(&self) -> &ResponderIdentity {
        &self.responder
    }

    /// The open PTP session's id, if `OpenSession` succeeded.
    pub fn session_id(&self) -> Option<u32> {
        self.session_id
    }

    /// The event channel, for a caller that services events or probes itself.
    pub fn event_channel(&mut self) -> &mut E {
        &mut self.event
    }

    /// Give up the session and take back its command and event channels.
    pub fn into_channels(self) -> (C, E) {
        (self.command, self.event)
    }

    fn take_transaction(&mut self) -> u32 {
        let id = self.next_transaction;
        if self.session_id.is_some() {
            // 0 and 0xFFFFFFFF are reserved; wrap past both.
            self.next_transaction = match id.wrapping_add(1) {
                0 | 0xFFFF_FFFF => 1,
                next => next,
            };
        }
        id
    }

    /// Run one transaction whose data phase, if any, flows from the responder into `sink`.
    ///
    /// This is the primitive every operation below is built on, public so a
    /// [`VendorExtension`](crate::vendor::VendorExtension) can issue its own operation codes.
    /// A data phase with no `sink` supplied is a protocol error.
    pub fn transaction(
        &mut self,
        operation: OperationCode,
        params: &[u32],
        mut sink: Option<&mut dyn Write>,
    ) -> Result<Completed, Error> {
        debug_assert!(
            params.len() <= MAX_PARAMS,
            "PTP carries at most five params"
        );
        let transaction = self.take_transaction();
        let started = Instant::now();
        tracing::debug!(%operation, transaction, ?params, "ptpip: operation request");
        write_packet(
            &mut self.command,
            &Packet::OperationRequest {
                phase: DataPhase::NoneOrIn,
                code: operation,
                transaction,
                params: params.to_vec(),
            },
        )?;

        let check = |received: u32| {
            if received == transaction {
                Ok(())
            } else {
                Err(Error::TransactionMismatch {
                    expected: transaction,
                    received,
                })
            }
        };
        // `Some(total)` once StartData arrived; `u64::MAX` is "length unknown".
        let mut declared: Option<u64> = None;
        let mut delivered: u64 = 0;
        let mut ended = false;
        loop {
            let packet = read_packet(&mut self.command)?;
            let is_end = matches!(packet, Packet::EndData { .. });
            match packet {
                Packet::StartData {
                    transaction: id,
                    total,
                } if declared.is_none() => {
                    check(id)?;
                    if sink.is_none() {
                        return Err(Error::UnexpectedPacket {
                            expected: "OperationResponse",
                            received: crate::packet::PacketType::StartData,
                        });
                    }
                    tracing::trace!(transaction, total, "ptpip: data phase starts");
                    declared = Some(total);
                }
                Packet::Data {
                    transaction: id,
                    payload,
                }
                | Packet::EndData {
                    transaction: id,
                    payload,
                } if declared.is_some() && !ended => {
                    check(id)?;
                    ended = is_end;
                    delivered += payload.len() as u64;
                    if let Some(total) = declared
                        && total != u64::MAX
                        && delivered > total
                    {
                        return Err(Error::DataLength {
                            declared: total,
                            delivered,
                        });
                    }
                    if let Some(sink) = sink.as_deref_mut() {
                        sink.write_all(&payload).map_err(Error::Sink)?;
                    }
                }
                Packet::OperationResponse {
                    code,
                    transaction: id,
                    params,
                } => {
                    check(id)?;
                    if let Some(total) = declared
                        && total != u64::MAX
                        && delivered != total
                    {
                        return Err(Error::DataLength {
                            declared: total,
                            delivered,
                        });
                    }
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    if code != ResponseCode::OK {
                        tracing::debug!(%operation, %code, elapsed_ms, "ptpip: operation refused");
                        return Err(Error::Response { operation, code });
                    }
                    tracing::debug!(
                        %operation,
                        transaction,
                        data_len = delivered,
                        elapsed_ms,
                        "ptpip: operation complete"
                    );
                    return Ok(Completed {
                        params,
                        data_len: delivered,
                    });
                }
                Packet::ProbeRequest => {
                    write_packet(&mut self.command, &Packet::ProbeResponse)?;
                }
                other => {
                    return Err(Error::UnexpectedPacket {
                        expected: if declared.is_some() && !ended {
                            "Data, EndData or OperationResponse"
                        } else {
                            "StartData or OperationResponse"
                        },
                        received: other.kind(),
                    });
                }
            }
        }
    }

    fn dataset(&mut self, operation: OperationCode, params: &[u32]) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        self.transaction(operation, params, Some(&mut bytes))?;
        Ok(bytes)
    }

    /// `GetDeviceInfo`. Valid before `OpenSession`.
    pub fn device_info(&mut self) -> Result<DeviceInfo, Error> {
        let bytes = self.dataset(OperationCode::GET_DEVICE_INFO, &[])?;
        Ok(DeviceInfo::decode(&bytes)?)
    }

    /// `OpenSession(session_id)`. A responder that answers `Session_Already_Open` is treated as
    /// open: a reconnecting initiator commonly meets its own earlier session.
    pub fn open_session(&mut self, session_id: u32) -> Result<(), Error> {
        self.next_transaction = 0;
        match self.transaction(OperationCode::OPEN_SESSION, &[session_id], None) {
            Ok(_)
            | Err(Error::Response {
                code: ResponseCode::SESSION_ALREADY_OPEN,
                ..
            }) => {
                self.session_id = Some(session_id);
                self.next_transaction = 1;
                tracing::info!(session_id, "ptpip: session open");
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// `CloseSession`.
    pub fn close_session(&mut self) -> Result<(), Error> {
        self.transaction(OperationCode::CLOSE_SESSION, &[], None)?;
        tracing::info!(session_id = ?self.session_id, "ptpip: session closed");
        self.session_id = None;
        self.next_transaction = 0;
        Ok(())
    }

    /// `GetStorageIDs`.
    pub fn storage_ids(&mut self) -> Result<Vec<StorageId>, Error> {
        let bytes = self.dataset(OperationCode::GET_STORAGE_IDS, &[])?;
        let ids = crate::codec::Reader::new(&bytes).u32_array()?;
        Ok(ids.into_iter().map(StorageId).collect())
    }

    /// `GetObjectHandles(storage, 0, 0)`: every object on `storage`, folders included, at any
    /// depth.
    pub fn object_handles(&mut self, storage: StorageId) -> Result<Vec<ObjectHandle>, Error> {
        let bytes = self.dataset(OperationCode::GET_OBJECT_HANDLES, &[storage.0, 0, 0])?;
        let handles = crate::codec::Reader::new(&bytes).u32_array()?;
        Ok(handles.into_iter().map(ObjectHandle).collect())
    }

    /// `GetObjectInfo(handle)`.
    pub fn object_info(&mut self, handle: ObjectHandle) -> Result<ObjectInfo, Error> {
        let bytes = self.dataset(OperationCode::GET_OBJECT_INFO, &[handle.0])?;
        Ok(ObjectInfo::decode(&bytes)?)
    }

    /// `GetObject(handle)`, streamed into `sink`. Returns the bytes delivered.
    pub fn get_object(&mut self, handle: ObjectHandle, sink: &mut dyn Write) -> Result<u64, Error> {
        Ok(self
            .transaction(OperationCode::GET_OBJECT, &[handle.0], Some(sink))?
            .data_len)
    }

    /// `GetPartialObject(handle, offset, max_bytes)`, streamed into `sink`. Returns the bytes
    /// delivered, which is fewer than `max_bytes` at the object's end.
    pub fn get_partial_object(
        &mut self,
        handle: ObjectHandle,
        offset: u32,
        max_bytes: u32,
        sink: &mut dyn Write,
    ) -> Result<u64, Error> {
        Ok(self
            .transaction(
                OperationCode::GET_PARTIAL_OBJECT,
                &[handle.0, offset, max_bytes],
                Some(sink),
            )?
            .data_len)
    }
}

/// One asynchronous event the responder sent on the event channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The PTP event code.
    pub code: u16,
    /// The transaction it relates to, or zero.
    pub transaction: u32,
    /// Up to three parameters.
    pub params: Vec<u32>,
}

/// What [`serve_events`] handled before it stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventsServed {
    /// `Event` packets delivered to the caller.
    pub events: u64,
    /// `ProbeRequest` packets answered with a `ProbeResponse`.
    pub probes: u64,
}

/// Service an event channel until it closes or `stop` is set: answer every `ProbeRequest` with a
/// `ProbeResponse`, so a responder that probes a quiet initiator does not drop it during a long
/// transfer, and hand every `Event` to `on_event`.
///
/// Built to run on its own thread over a second handle to the event channel (for TCP,
/// [`TcpStream::try_clone`](std::net::TcpStream::try_clone) of
/// [`Session::event_channel`]): the session itself never reads that channel once it is attached.
/// A read that times out before the first byte of a packet is idleness, not an error: `stop` is
/// checked and the wait goes on, so a channel with a read timeout stops within one timeout of
/// `stop` being set. A clean end of the channel (the caller shut it down, or the responder closed
/// it) between packets returns the tally.
///
/// # Errors
///
/// The channel failed mid-packet, or the responder sent a packet the event channel does not carry.
/// Either way the channel can no longer be trusted; the command channel is unaffected.
pub fn serve_events<E: Read + Write>(
    mut event: E,
    stop: &AtomicBool,
    mut on_event: impl FnMut(Event),
) -> Result<EventsServed, Error> {
    let mut served = EventsServed::default();
    tracing::debug!("ptpip: event channel reader started");
    loop {
        if stop.load(Ordering::Relaxed) {
            tracing::debug!(?served, "ptpip: event channel reader stopped");
            return Ok(served);
        }
        let mut first = [0u8; 1];
        match event.read(&mut first) {
            Ok(0) => {
                tracing::debug!(?served, "ptpip: event channel closed");
                return Ok(served);
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                tracing::trace!("ptpip: event channel idle");
                continue;
            }
            Err(error) => return Err(Error::Io(error)),
        }
        match read_packet(&mut (&first[..]).chain(&mut event))? {
            Packet::ProbeRequest => {
                write_packet(&mut event, &Packet::ProbeResponse)?;
                served.probes += 1;
                tracing::trace!(
                    probes = served.probes,
                    "ptpip: answered event-channel probe"
                );
            }
            Packet::Event {
                code,
                transaction,
                params,
            } => {
                served.events += 1;
                tracing::debug!(
                    code = format_args!("{code:#06x}"),
                    transaction,
                    ?params,
                    "ptpip: event"
                );
                on_event(Event {
                    code,
                    transaction,
                    params,
                });
            }
            // An answer to a probe the initiator never sends here; harmless.
            Packet::ProbeResponse => {}
            other => {
                return Err(Error::UnexpectedPacket {
                    expected: "Event or ProbeRequest",
                    received: other.kind(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;
