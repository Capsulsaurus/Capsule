use std::net::TcpListener;
use std::thread;

use super::*;
use crate::packet::{Packet, read_packet, write_packet};
use crate::testkit::{ack, data_then_ok, ok};

fn expect_request(stream: &mut TcpStream) -> u32 {
    match read_packet(stream).expect("request") {
        Packet::OperationRequest { transaction, .. } => transaction,
        other => panic!("expected an operation request, got {other:?}"),
    }
}

#[test]
fn connect_brings_up_both_channels_and_opens_the_session() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let device = DeviceInfo {
        manufacturer: "Canon Inc.".into(),
        model: "EOS R5".into(),
        ..DeviceInfo::default()
    };
    let served = device.clone();
    let responder = thread::spawn(move || {
        let (mut command, _) = listener.accept().expect("command channel");
        assert!(matches!(
            read_packet(&mut command).expect("init"),
            Packet::InitCommandRequest { .. }
        ));
        write_packet(&mut command, &ack(9)).expect("ack");
        let (mut event, _) = listener.accept().expect("event channel");
        assert_eq!(
            read_packet(&mut event).expect("init event"),
            Packet::InitEventRequest { connection: 9 }
        );
        write_packet(&mut event, &Packet::InitEventAck).expect("event ack");

        let id = expect_request(&mut command);
        for packet in data_then_ok(id, &served.encode().expect("encode"), 32) {
            write_packet(&mut command, &packet).expect("device info");
        }
        let id = expect_request(&mut command);
        write_packet(&mut command, &ok(id)).expect("open session");
        // Hold both channels open until the initiator is done with them.
        let _ = read_packet(&mut command);
        drop(event);
    });

    let initiator = InitiatorIdentity {
        guid: [1; 16],
        name: "Capsule".into(),
    };
    let camera = connect(addr, &initiator, 1, Timeouts::default()).expect("connect");
    assert_eq!(camera.device, device);
    assert_eq!(camera.extension.name(), "generic");
    assert_eq!(camera.session.session_id(), Some(1));
    assert_eq!(camera.session.responder().connection, 9);
    drop(camera);
    responder.join().expect("responder thread");
}

#[test]
fn connect_to_a_closed_port_is_an_io_error() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    let initiator = InitiatorIdentity {
        guid: [1; 16],
        name: "Capsule".into(),
    };
    let timeouts = Timeouts {
        connect: Duration::from_secs(2),
        io: Duration::from_secs(2),
    };
    assert!(matches!(
        connect(addr, &initiator, 1, timeouts),
        Err(Error::Io(_))
    ));
}
