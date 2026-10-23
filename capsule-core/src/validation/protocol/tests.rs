use super::*;
use crate::crypto::primitives::CRYPTO_SUITE_ID;

#[test]
fn protocol_in_range_accepts() {
    assert!(protocol_gate("2026-05-31", "2026-01-01", "2026-12-31").is_ok());
    // Boundaries are inclusive.
    assert!(protocol_gate("2026-01-01", "2026-01-01", "2026-12-31").is_ok());
    assert!(protocol_gate("2026-12-31", "2026-01-01", "2026-12-31").is_ok());
}

#[test]
fn protocol_out_of_range_is_426() {
    let below = protocol_gate("2025-12-31", "2026-01-01", "2026-12-31");
    let above = protocol_gate("2027-01-01", "2026-01-01", "2026-12-31");
    assert_eq!(below, Err(HandshakeReject::ProtocolOutOfRange));
    assert_eq!(above, Err(HandshakeReject::ProtocolOutOfRange));
    assert_eq!(below.unwrap_err().http_status(), 426);
}

#[test]
fn malformed_protocol_is_400() {
    for bad in ["2026/05/31", "v1", "2026-5-31", "", "2026-05-31T00:00:00Z"] {
        assert_eq!(
            protocol_gate(bad, "2026-01-01", "2026-12-31"),
            Err(HandshakeReject::ProtocolMalformed)
        );
    }
}

#[test]
fn suite_inventory_check() {
    assert!(check_suite(CRYPTO_SUITE_ID).is_ok());
    assert_eq!(check_suite(0x9999), Err(HandshakeReject::UnknownSuite));
    assert_eq!(check_suite(0x9999).unwrap_err().http_status(), 400);
}

#[test]
fn sidecar_schema_too_new_rejected() {
    assert!(check_sidecar_schema(1, 1).is_ok());
    assert!(check_sidecar_schema(1, 2).is_ok());
    assert_eq!(
        check_sidecar_schema(3, 2),
        Err(HandshakeReject::SidecarSchemaTooNew)
    );
}
