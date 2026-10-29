use super::*;
use crate::packet::PacketType;
use crate::testkit::{Scripted, ack, data_then_ok, ok, respond};

fn initiator() -> InitiatorIdentity {
    InitiatorIdentity {
        guid: [0x11; 16],
        name: "Capsule".into(),
    }
}

/// A session past the handshake, whose command channel replays `script`.
fn open_with(script: Vec<Packet>) -> Session<Scripted, Scripted> {
    let mut command = vec![ack(5)];
    command.extend(script);
    Session::begin(Scripted::new(&command), &initiator())
        .unwrap()
        .attach_event(Scripted::new(&[Packet::InitEventAck]))
        .unwrap()
}

fn sent_requests(session: &Session<Scripted, Scripted>) -> Vec<(OperationCode, u32, Vec<u32>)> {
    session
        .command
        .sent_packets()
        .into_iter()
        .filter_map(|packet| match packet {
            Packet::OperationRequest {
                code,
                transaction,
                params,
                ..
            } => Some((code, transaction, params)),
            _ => None,
        })
        .collect()
}

#[test]
fn handshake_sends_identity_and_quotes_the_connection_number_on_the_event_channel() {
    let session = open_with(vec![]);
    assert_eq!(session.responder().connection, 5);
    assert_eq!(session.responder().name, "ILCE-7M4");
    assert_eq!(
        session.command.sent_packets(),
        vec![Packet::InitCommandRequest {
            guid: [0x11; 16],
            name: "Capsule".into(),
            version: PROTOCOL_VERSION,
        }]
    );
    assert_eq!(
        session.event.sent_packets(),
        vec![Packet::InitEventRequest { connection: 5 }]
    );
}

#[test]
fn init_fail_on_either_channel_is_a_typed_refusal() {
    let refused = Session::begin(
        Scripted::new(&[Packet::InitFail { reason: 0x2 }]),
        &initiator(),
    );
    assert!(matches!(refused, Err(Error::InitFailed { reason: 0x2 })));

    let pending = Session::begin(Scripted::new(&[ack(1)]), &initiator()).unwrap();
    let refused = pending.attach_event(Scripted::new(&[Packet::InitFail { reason: 0x3 }]));
    assert!(matches!(refused, Err(Error::InitFailed { reason: 0x3 })));
}

#[test]
fn unexpected_handshake_packet_is_an_error() {
    let result = Session::begin(Scripted::new(&[Packet::InitEventAck]), &initiator());
    assert!(matches!(
        result,
        Err(Error::UnexpectedPacket {
            received: PacketType::InitEventAck,
            ..
        })
    ));
}

#[test]
fn open_session_uses_transaction_zero_then_numbering_starts_at_one() {
    let mut script = vec![ok(0)];
    script.extend(data_then_ok(
        1,
        &crate::testkit::u32_array(&[0x0001_0001]),
        64,
    ));
    let mut session = open_with(script);
    session.open_session(7).unwrap();
    assert_eq!(session.session_id(), Some(7));
    assert_eq!(session.storage_ids().unwrap(), vec![StorageId(0x0001_0001)]);
    assert_eq!(
        sent_requests(&session),
        vec![
            (OperationCode::OPEN_SESSION, 0, vec![7]),
            (OperationCode::GET_STORAGE_IDS, 1, vec![]),
        ]
    );
}

#[test]
fn session_already_open_counts_as_open() {
    let mut session = open_with(vec![respond(ResponseCode::SESSION_ALREADY_OPEN, 0, vec![])]);
    session.open_session(1).unwrap();
    assert_eq!(session.session_id(), Some(1));
}

#[test]
fn get_object_streams_every_chunk_into_the_sink() {
    let object: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    let mut script = vec![ok(0)];
    script.extend(data_then_ok(1, &object, 1500));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    let mut sink = Vec::new();
    let delivered = session.get_object(ObjectHandle(0x42), &mut sink).unwrap();
    assert_eq!(delivered, 10_000);
    assert_eq!(sink, object);
    assert_eq!(
        sent_requests(&session)[1],
        (OperationCode::GET_OBJECT, 1, vec![0x42])
    );
}

#[test]
fn short_data_phase_is_a_length_error_not_a_short_object() {
    let mut script = vec![ok(0)];
    script.push(Packet::StartData {
        transaction: 1,
        total: 100,
    });
    script.push(Packet::EndData {
        transaction: 1,
        payload: vec![0; 60],
    });
    script.push(ok(1));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    let result = session.get_object(ObjectHandle(1), &mut Vec::new());
    assert!(matches!(
        result,
        Err(Error::DataLength {
            declared: 100,
            delivered: 60
        })
    ));
}

#[test]
fn overlong_data_phase_fails_as_soon_as_it_overruns() {
    let mut script = vec![ok(0)];
    script.push(Packet::StartData {
        transaction: 1,
        total: 10,
    });
    script.push(Packet::Data {
        transaction: 1,
        payload: vec![0; 11],
    });
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    let mut sink = Vec::new();
    let result = session.get_object(ObjectHandle(1), &mut sink);
    assert!(matches!(
        result,
        Err(Error::DataLength {
            declared: 10,
            delivered: 11
        })
    ));
    assert!(sink.is_empty(), "the overrun chunk never reaches the sink");
}

#[test]
fn unknown_total_length_accepts_whatever_arrives() {
    let mut script = vec![ok(0)];
    script.push(Packet::StartData {
        transaction: 1,
        total: u64::MAX,
    });
    script.push(Packet::EndData {
        transaction: 1,
        payload: vec![9; 33],
    });
    script.push(ok(1));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    assert_eq!(
        session
            .get_object(ObjectHandle(1), &mut Vec::new())
            .unwrap(),
        33
    );
}

#[test]
fn a_packet_for_another_transaction_is_refused() {
    let mut script = vec![ok(0)];
    script.extend(data_then_ok(9, b"abc", 8));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    assert!(matches!(
        session.get_object(ObjectHandle(1), &mut Vec::new()),
        Err(Error::TransactionMismatch {
            expected: 1,
            received: 9
        })
    ));
}

#[test]
fn a_refused_operation_is_a_non_fatal_response_error() {
    let mut script = vec![ok(0)];
    script.push(respond(ResponseCode::INVALID_OBJECT_HANDLE, 1, vec![]));
    script.extend(data_then_ok(2, b"ok", 8));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    let error = session
        .get_object(ObjectHandle(1), &mut Vec::new())
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Response {
            operation: OperationCode::GET_OBJECT,
            code: ResponseCode::INVALID_OBJECT_HANDLE
        }
    ));
    assert!(!error.is_fatal());
    // The session is still usable: the next transaction proceeds with the next id.
    assert_eq!(
        session
            .get_object(ObjectHandle(2), &mut Vec::new())
            .unwrap(),
        2
    );
}

#[test]
fn data_phase_without_a_sink_is_a_protocol_error() {
    let mut script = vec![ok(0)];
    script.extend(data_then_ok(1, b"x", 8));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    assert!(matches!(
        session.close_session(),
        Err(Error::UnexpectedPacket {
            received: PacketType::StartData,
            ..
        })
    ));
}

#[test]
fn data_before_start_data_is_a_protocol_error() {
    let mut session = open_with(vec![
        ok(0),
        Packet::Data {
            transaction: 1,
            payload: vec![1],
        },
    ]);
    session.open_session(1).unwrap();
    assert!(matches!(
        session.get_object(ObjectHandle(1), &mut Vec::new()),
        Err(Error::UnexpectedPacket {
            received: PacketType::Data,
            ..
        })
    ));
}

#[test]
fn probe_during_a_transaction_is_answered() {
    let mut script = vec![ok(0), Packet::ProbeRequest];
    script.extend(data_then_ok(1, b"abc", 8));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    session
        .get_object(ObjectHandle(1), &mut Vec::new())
        .unwrap();
    assert!(
        session
            .command
            .sent_packets()
            .contains(&Packet::ProbeResponse)
    );
}

#[test]
fn early_eof_mid_transfer_is_fatal_io() {
    let mut script = vec![ok(0)];
    script.push(Packet::StartData {
        transaction: 1,
        total: 100,
    });
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    let error = session
        .get_object(ObjectHandle(1), &mut Vec::new())
        .unwrap_err();
    assert!(matches!(error, Error::Io(_)));
    assert!(error.is_fatal());
}

#[test]
fn enumeration_and_info_operations_send_the_standard_parameters() {
    let info = ObjectInfo {
        storage_id: StorageId(0x0001_0001),
        format: crate::ptp::ObjectFormat(0x3801),
        protection_status: 0,
        compressed_size: 3,
        thumb_format: 0,
        thumb_compressed_size: 0,
        thumb_pix_width: 0,
        thumb_pix_height: 0,
        image_pix_width: 0,
        image_pix_height: 0,
        image_bit_depth: 0,
        parent: ObjectHandle(0),
        association_type: 0,
        association_desc: 0,
        sequence_number: 0,
        filename: "A.JPG".into(),
        capture_date: String::new(),
        modification_date: String::new(),
        keywords: String::new(),
    };
    let mut script = vec![ok(0)];
    script.extend(data_then_ok(1, &crate::testkit::u32_array(&[3, 4]), 64));
    script.extend(data_then_ok(2, &info.encode().unwrap(), 64));
    script.extend(data_then_ok(3, b"ab", 64));
    script.push(ok(4));
    let mut session = open_with(script);
    session.open_session(1).unwrap();
    assert_eq!(
        session.object_handles(StorageId(0x0001_0001)).unwrap(),
        vec![ObjectHandle(3), ObjectHandle(4)]
    );
    assert_eq!(session.object_info(ObjectHandle(3)).unwrap(), info);
    assert_eq!(
        session
            .get_partial_object(ObjectHandle(3), 1, 2, &mut Vec::new())
            .unwrap(),
        2
    );
    session.close_session().unwrap();
    assert_eq!(session.session_id(), None);
    assert_eq!(
        sent_requests(&session)[1..],
        [
            (
                OperationCode::GET_OBJECT_HANDLES,
                1,
                vec![0x0001_0001, 0, 0]
            ),
            (OperationCode::GET_OBJECT_INFO, 2, vec![3]),
            (OperationCode::GET_PARTIAL_OBJECT, 3, vec![3, 1, 2]),
            (OperationCode::CLOSE_SESSION, 4, vec![]),
        ]
    );
}

#[test]
fn device_info_before_open_session_uses_transaction_zero() {
    let device = DeviceInfo {
        model: "ILCE-7M4".into(),
        ..DeviceInfo::default()
    };
    let mut session = open_with(data_then_ok(0, &device.encode().unwrap(), 16));
    assert_eq!(session.device_info().unwrap(), device);
    assert_eq!(
        sent_requests(&session),
        vec![(OperationCode::GET_DEVICE_INFO, 0, vec![])]
    );
}

#[test]
fn transaction_ids_wrap_past_the_reserved_values() {
    let mut session = open_with(vec![]);
    session.session_id = Some(1);
    session.next_transaction = 0xFFFF_FFFE;
    assert_eq!(session.take_transaction(), 0xFFFF_FFFE);
    assert_eq!(session.take_transaction(), 1);
}

/// An event channel whose first `timeouts` reads time out before the script replays, setting
/// `stop_on_timeout` (when given) as they do.
struct Idle<'a> {
    timeouts: usize,
    stop_on_timeout: Option<&'a AtomicBool>,
    inner: Scripted,
}

impl Read for Idle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.timeouts > 0 {
            self.timeouts -= 1;
            if let Some(stop) = self.stop_on_timeout {
                stop.store(true, Ordering::Relaxed);
            }
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.inner.read(buf)
    }
}

impl Write for Idle<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn event(code: u16) -> Packet {
    Packet::Event {
        code,
        transaction: 0,
        params: vec![7],
    }
}

#[test]
fn the_event_reader_answers_probes_and_hands_over_events_until_the_channel_closes() {
    let mut channel = Scripted::new(&[
        Packet::ProbeRequest,
        event(0x4002),
        Packet::ProbeRequest,
        event(0x400D),
    ]);
    let mut seen = Vec::new();
    let served = serve_events(&mut channel, &AtomicBool::new(false), |e| seen.push(e)).unwrap();
    assert_eq!(
        served,
        EventsServed {
            events: 2,
            probes: 2
        }
    );
    assert_eq!(
        seen.iter().map(|e| e.code).collect::<Vec<_>>(),
        [0x4002, 0x400D]
    );
    assert_eq!(seen[0].params, [7]);
    assert_eq!(
        channel.sent_packets(),
        [Packet::ProbeResponse, Packet::ProbeResponse]
    );
}

#[test]
fn the_event_reader_waits_through_idle_timeouts() {
    let mut channel = Idle {
        timeouts: 3,
        stop_on_timeout: None,
        inner: Scripted::new(&[Packet::ProbeRequest]),
    };
    let served = serve_events(&mut channel, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(served.probes, 1);
    assert_eq!(channel.inner.sent_packets(), [Packet::ProbeResponse]);
}

#[test]
fn the_event_reader_stops_within_one_idle_timeout_of_being_asked() {
    let stop = AtomicBool::new(false);
    let mut channel = Idle {
        timeouts: 1,
        stop_on_timeout: Some(&stop),
        // Never reached: the stop is seen before the next read.
        inner: Scripted::new(&[Packet::ProbeRequest]),
    };
    let served = serve_events(&mut channel, &stop, |_| {}).unwrap();
    assert_eq!(served, EventsServed::default());
    assert!(channel.inner.sent.is_empty());
}

#[test]
fn the_event_reader_refuses_a_packet_the_event_channel_does_not_carry() {
    let mut channel = Scripted::new(&[ok(1)]);
    assert!(matches!(
        serve_events(&mut channel, &AtomicBool::new(false), |_| {}),
        Err(Error::UnexpectedPacket {
            received: PacketType::OperationResponse,
            ..
        })
    ));
}

#[test]
fn the_event_reader_fails_on_a_packet_cut_off_midway() {
    let mut bytes = Packet::ProbeRequest.encode();
    bytes.extend_from_slice(&event(0x4002).encode()[..6]);
    let mut channel = Scripted::from_bytes(bytes);
    assert!(matches!(
        serve_events(&mut channel, &AtomicBool::new(false), |_| {}),
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
    ));
}
