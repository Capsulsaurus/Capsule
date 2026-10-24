use std::io::Cursor;

use super::*;

fn round_trip(packet: &Packet) -> Packet {
    let bytes = packet.encode();
    read_packet(&mut Cursor::new(bytes)).expect("decodes")
}

#[test]
fn every_packet_type_round_trips() {
    let packets = [
        Packet::InitCommandRequest {
            guid: [7; 16],
            name: "Capsule".into(),
            version: PROTOCOL_VERSION,
        },
        Packet::InitCommandAck {
            connection: 3,
            guid: [9; 16],
            name: "ILCE-7M4".into(),
            version: PROTOCOL_VERSION,
        },
        Packet::InitEventRequest { connection: 3 },
        Packet::InitEventAck,
        Packet::InitFail { reason: 0x1 },
        Packet::OperationRequest {
            phase: DataPhase::NoneOrIn,
            code: OperationCode::GET_OBJECT,
            transaction: 12,
            params: vec![0x42],
        },
        Packet::OperationResponse {
            code: ResponseCode::OK,
            transaction: 12,
            params: vec![],
        },
        Packet::Event {
            code: 0x4002,
            transaction: 0,
            params: vec![5],
        },
        Packet::StartData {
            transaction: 12,
            total: 1 << 33,
        },
        Packet::Data {
            transaction: 12,
            payload: vec![1, 2, 3],
        },
        Packet::Cancel { transaction: 12 },
        Packet::EndData {
            transaction: 12,
            payload: vec![],
        },
        Packet::ProbeRequest,
        Packet::ProbeResponse,
        Packet::Unknown {
            kind: 99,
            payload: vec![0xAA],
        },
    ];
    for packet in packets {
        assert_eq!(round_trip(&packet), packet, "{:?}", packet.kind());
    }
}

#[test]
fn header_carries_the_whole_length_and_type() {
    let bytes = Packet::InitEventRequest { connection: 1 }.encode();
    assert_eq!(&bytes[..8], &[12, 0, 0, 0, 3, 0, 0, 0]);
}

#[test]
fn operation_request_layout_matches_the_standard() {
    let bytes = Packet::OperationRequest {
        phase: DataPhase::NoneOrIn,
        code: OperationCode::OPEN_SESSION,
        transaction: 0,
        params: vec![1],
    }
    .encode();
    assert_eq!(
        bytes,
        vec![
            22, 0, 0, 0, 6, 0, 0, 0, // header
            1, 0, 0, 0, // data phase
            0x02, 0x10, // OpenSession
            0, 0, 0, 0, // transaction
            1, 0, 0, 0, // session id
        ]
    );
}

#[test]
fn declared_length_below_the_header_is_refused() {
    let bytes = [4u8, 0, 0, 0, 1, 0, 0, 0];
    assert!(matches!(
        read_packet(&mut Cursor::new(bytes)),
        Err(Error::PacketTooLarge { length: 4, .. })
    ));
}

#[test]
fn declared_length_above_the_bound_is_refused_before_allocating() {
    let mut bytes = (MAX_PACKET_LEN + 1).to_le_bytes().to_vec();
    bytes.extend_from_slice(&10u32.to_le_bytes());
    assert!(matches!(
        read_packet(&mut Cursor::new(bytes)),
        Err(Error::PacketTooLarge { .. })
    ));
}

#[test]
fn early_eof_is_an_io_error() {
    let mut bytes = Packet::Data {
        transaction: 1,
        payload: vec![0; 32],
    }
    .encode();
    bytes.truncate(20);
    assert!(matches!(
        read_packet(&mut Cursor::new(bytes)),
        Err(Error::Io(_))
    ));
}

#[test]
fn response_parameters_ignore_trailing_partial_words_and_cap_at_five() {
    let mut payload = vec![0x01, 0x20, 7, 0, 0, 0];
    for i in 0..6u32 {
        payload.extend_from_slice(&i.to_le_bytes());
    }
    payload.push(0xFF);
    let packet = Packet::decode(PacketType::OperationResponse, &payload).unwrap();
    assert_eq!(
        packet,
        Packet::OperationResponse {
            code: ResponseCode::OK,
            transaction: 7,
            params: vec![0, 1, 2, 3, 4],
        }
    );
}

#[test]
fn packet_type_codes_are_a_bijection_over_the_standard_range() {
    for code in 0..=16 {
        assert_eq!(PacketType::from_code(code).code(), code);
    }
}
