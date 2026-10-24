//! Test doubles: a scripted responder channel and builders for its packets.

use std::io::{self, Cursor, Read, Write};

use crate::packet::{Packet, read_packet};
use crate::ptp::ResponseCode;

/// A channel whose reads replay a fixed responder script and whose writes are captured.
#[derive(Debug, Default)]
pub(crate) struct Scripted {
    input: Cursor<Vec<u8>>,
    pub(crate) sent: Vec<u8>,
}

impl Scripted {
    pub(crate) fn new(script: &[Packet]) -> Self {
        let bytes = script.iter().flat_map(Packet::encode).collect();
        Self {
            input: Cursor::new(bytes),
            sent: Vec::new(),
        }
    }

    /// Every packet the initiator wrote, decoded.
    pub(crate) fn sent_packets(&self) -> Vec<Packet> {
        let mut cursor = Cursor::new(self.sent.clone());
        let mut out = Vec::new();
        while (cursor.position() as usize) < self.sent.len() {
            out.push(read_packet(&mut cursor).expect("initiator wrote a valid packet"));
        }
        out
    }
}

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl Write for Scripted {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.sent.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn ack(connection: u32) -> Packet {
    Packet::InitCommandAck {
        connection,
        guid: [0xC0; 16],
        name: "ILCE-7M4".into(),
        version: crate::packet::PROTOCOL_VERSION,
    }
}

pub(crate) fn ok(transaction: u32) -> Packet {
    respond(ResponseCode::OK, transaction, vec![])
}

pub(crate) fn respond(code: ResponseCode, transaction: u32, params: Vec<u32>) -> Packet {
    Packet::OperationResponse {
        code,
        transaction,
        params,
    }
}

/// A PTP `u32` array, as `GetStorageIDs` and `GetObjectHandles` return.
pub(crate) fn u32_array(values: &[u32]) -> Vec<u8> {
    let mut w = crate::codec::Writer::new();
    w.u32_array(values);
    w.into_bytes()
}

/// A data phase carrying `bytes`, split into `chunk`-sized `Data` packets and a final `EndData`,
/// then an `OK` response.
pub(crate) fn data_then_ok(transaction: u32, bytes: &[u8], chunk: usize) -> Vec<Packet> {
    let mut out = vec![Packet::StartData {
        transaction,
        total: bytes.len() as u64,
    }];
    let mut chunks: Vec<&[u8]> = bytes.chunks(chunk.max(1)).collect();
    let last = chunks.pop().unwrap_or(&[]);
    for piece in chunks {
        out.push(Packet::Data {
            transaction,
            payload: piece.to_vec(),
        });
    }
    out.push(Packet::EndData {
        transaction,
        payload: last.to_vec(),
    });
    out.push(ok(transaction));
    out
}
