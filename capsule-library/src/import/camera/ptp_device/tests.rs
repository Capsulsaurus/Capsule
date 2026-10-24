use std::io::{self, Cursor};

use ptpip::packet::{Packet, read_packet};
use ptpip::{InitiatorIdentity, ObjectFormat, ObjectInfo, ResponseCode};

use super::*;

/// A channel replaying a scripted responder and capturing what the initiator sends.
struct Scripted {
    input: Cursor<Vec<u8>>,
    sent: Vec<u8>,
}

impl Scripted {
    fn new(script: &[Packet]) -> Self {
        Self {
            input: Cursor::new(script.iter().flat_map(Packet::encode).collect()),
            sent: Vec::new(),
        }
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

fn ok(transaction: u32) -> Packet {
    Packet::OperationResponse {
        code: ResponseCode::OK,
        transaction,
        params: vec![],
    }
}

fn data(transaction: u32, bytes: &[u8]) -> Vec<Packet> {
    vec![
        Packet::StartData {
            transaction,
            total: bytes.len() as u64,
        },
        Packet::EndData {
            transaction,
            payload: bytes.to_vec(),
        },
        ok(transaction),
    ]
}

fn u32_array(values: &[u32]) -> Vec<u8> {
    let mut w = ptpip::codec::Writer::new();
    w.u32_array(values);
    w.into_bytes()
}

/// A camera whose session is open (transaction 0) and whose command channel then replays
/// `script` from transaction 1 on.
fn camera(script: Vec<Packet>, operations: &[OperationCode]) -> PtpCamera<Scripted, Scripted> {
    let mut command = vec![
        Packet::InitCommandAck {
            connection: 1,
            guid: [0; 16],
            name: "camera".into(),
            version: ptpip::packet::PROTOCOL_VERSION,
        },
        ok(0),
    ];
    command.extend(script);
    let initiator = InitiatorIdentity {
        guid: [1; 16],
        name: "Capsule".into(),
    };
    let mut session = Session::begin(Scripted::new(&command), &initiator)
        .unwrap()
        .attach_event(Scripted::new(&[Packet::InitEventAck]))
        .unwrap();
    session.open_session(1).unwrap();
    let device = DeviceInfo {
        operations_supported: operations.iter().map(|op| op.0).collect(),
        ..DeviceInfo::default()
    };
    PtpCamera::new(session, device)
}

fn requests(camera: PtpCamera<Scripted, Scripted>) -> Vec<(OperationCode, Vec<u32>)> {
    let (command, _event) = camera.into_session().into_channels();
    let bytes = command.sent;
    let mut cursor = Cursor::new(bytes.clone());
    let mut out = Vec::new();
    while (cursor.position() as usize) < bytes.len() {
        if let Packet::OperationRequest { code, params, .. } = read_packet(&mut cursor).unwrap() {
            out.push((code, params));
        }
    }
    out
}

fn object_info(format: ObjectFormat, size: u32, date: &str) -> ObjectInfo {
    ObjectInfo {
        storage_id: StorageId(0x0001_0001),
        format,
        protection_status: 0,
        compressed_size: size,
        thumb_format: 0,
        thumb_compressed_size: 0,
        thumb_pix_width: 0,
        thumb_pix_height: 0,
        image_pix_width: 0,
        image_pix_height: 0,
        image_bit_depth: 0,
        parent: ObjectHandle(0x10),
        association_type: 0,
        association_desc: 0,
        sequence_number: 0,
        filename: "DSC00001.ARW".into(),
        capture_date: date.into(),
        modification_date: String::new(),
        keywords: String::new(),
    }
}

#[test]
fn enumeration_maps_ptp_records_onto_the_port() {
    let mut script = data(1, &u32_array(&[0x0001_0001]));
    script.extend(data(2, &u32_array(&[0x20, 0x10])));
    script.extend(data(
        3,
        &object_info(ObjectFormat(0xB101), 4096, "20260412T153012")
            .encode()
            .unwrap(),
    ));
    script.extend(data(
        4,
        &object_info(ObjectFormat::ASSOCIATION, 0, "")
            .encode()
            .unwrap(),
    ));
    let mut camera = camera(script, &[]);

    assert_eq!(camera.storage_ids().unwrap(), vec![0x0001_0001]);
    assert_eq!(
        camera.object_handles(0x0001_0001).unwrap(),
        vec![0x20, 0x10]
    );
    let file = camera.object(0x20).unwrap();
    assert_eq!(
        file,
        CameraObject {
            storage: 0x0001_0001,
            handle: 0x20,
            parent: 0x10,
            filename: "DSC00001.ARW".into(),
            size: Some(4096),
            captured_at: Some(jiff::civil::date(2026, 4, 12).at(15, 30, 12, 0)),
            is_folder: false,
        }
    );
    assert!(camera.object(0x10).unwrap().is_folder);
}

#[test]
fn the_four_gib_sentinel_is_an_unknown_size() {
    let script = data(
        1,
        &object_info(ObjectFormat(0x300B), ObjectInfo::SIZE_UNKNOWN, "")
            .encode()
            .unwrap(),
    );
    let mut camera = camera(script, &[]);
    assert_eq!(camera.object(0x20).unwrap().size, None);
}

#[test]
fn a_whole_read_is_one_get_object() {
    let mut camera = camera(data(1, b"abcdef"), &[]);
    let object = CameraObject {
        storage: 1,
        handle: 0x20,
        parent: 0,
        filename: "a".into(),
        size: Some(6),
        captured_at: None,
        is_folder: false,
    };
    let mut sink = Vec::new();
    assert_eq!(camera.read_object(&object, 0, &mut sink).unwrap(), 6);
    assert_eq!(sink, b"abcdef");
    assert_eq!(
        requests(camera),
        vec![
            (OperationCode::OPEN_SESSION, vec![1]),
            (OperationCode::GET_OBJECT, vec![0x20])
        ]
    );
}

#[test]
fn a_resumed_read_asks_for_the_remaining_range() {
    let mut camera = camera(data(1, b"def"), &[OperationCode::GET_PARTIAL_OBJECT]);
    assert!(camera.supports_partial_reads());
    let object = CameraObject {
        storage: 1,
        handle: 0x20,
        parent: 0,
        filename: "a".into(),
        size: Some(6),
        captured_at: None,
        is_folder: false,
    };
    let mut sink = Vec::new();
    assert_eq!(camera.read_object(&object, 3, &mut sink).unwrap(), 3);
    assert_eq!(sink, b"def");
    assert_eq!(
        requests(camera)[1],
        (OperationCode::GET_PARTIAL_OBJECT, vec![0x20, 3, 3])
    );
}

#[test]
fn a_resumed_read_without_partial_support_or_size_is_refused() {
    let object = CameraObject {
        storage: 1,
        handle: 0x20,
        parent: 0,
        filename: "a".into(),
        size: Some(6),
        captured_at: None,
        is_folder: false,
    };
    let mut no_partial = camera(vec![], &[]);
    assert!(matches!(
        no_partial.read_object(&object, 3, &mut Vec::new()),
        Err(CameraError::ResumeUnsupported { offset: 3, .. })
    ));
    let mut unknown_size = camera(vec![], &[OperationCode::GET_PARTIAL_OBJECT]);
    let big = CameraObject {
        size: None,
        ..object
    };
    assert!(matches!(
        unknown_size.read_object(&big, 3, &mut Vec::new()),
        Err(CameraError::ResumeUnsupported { .. })
    ));
}

#[test]
fn protocol_errors_map_onto_fatal_and_per_object_camera_errors() {
    let refused = CameraError::from(ptpip::Error::Response {
        operation: OperationCode::GET_OBJECT,
        code: ResponseCode::ACCESS_DENIED,
    });
    assert!(matches!(refused, CameraError::Refused { .. }));
    assert!(!refused.is_fatal());

    let dropped = CameraError::from(ptpip::Error::Io(io::Error::from(
        io::ErrorKind::ConnectionReset,
    )));
    assert!(matches!(dropped, CameraError::Disconnected(_)));
    assert!(dropped.is_fatal());

    let sink = CameraError::from(ptpip::Error::Sink(io::Error::other("disk full")));
    assert!(matches!(sink, CameraError::Sink(_)));
}

#[test]
fn a_dropped_connection_mid_read_is_disconnected() {
    let mut camera = camera(
        vec![Packet::StartData {
            transaction: 1,
            total: 10,
        }],
        &[],
    );
    let object = CameraObject {
        storage: 1,
        handle: 0x20,
        parent: 0,
        filename: "a".into(),
        size: Some(10),
        captured_at: None,
        is_folder: false,
    };
    assert!(matches!(
        camera.read_object(&object, 0, &mut Vec::new()),
        Err(CameraError::Disconnected(_))
    ));
}
