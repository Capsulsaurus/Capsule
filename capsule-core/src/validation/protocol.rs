//! The universal, fail-closed protocol & capability handshake (SSoT: [Threat Model —
//! Protocol and Capability Negotiation]). Every versioned surface runs this one-shot
//! pre-flight before any state is written; a mismatch is a hard reject, never a degrade.
//!
//! [Threat Model — Protocol and Capability Negotiation]: https://docs/design/threat-model/validation/#protocol-and-capability-negotiation

use crate::crypto::primitives::SuiteId;

/// A handshake rejection. Each maps to a fail-closed rule and an HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeReject {
    /// `X-Capsule-Protocol` outside the server's `[Min, Max]` window → `426 Upgrade Required`.
    ProtocolOutOfRange,
    /// `X-Capsule-Protocol` not a `YYYY-MM-DD` date → `400`.
    ProtocolMalformed,
    /// `crypto_suite_id` not in the inventory → `400`.
    UnknownSuite,
    /// `sidecar_schema` above the receiver's max known → `400`.
    SidecarSchemaTooNew,
}

impl HandshakeReject {
    /// The HTTP status code a server returns for this rejection.
    pub fn http_status(self) -> u16 {
        match self {
            HandshakeReject::ProtocolOutOfRange => 426,
            _ => 400,
        }
    }
}

/// True if `v` is a well-formed `YYYY-MM-DD` date (the only grammar `protocol_version`
/// accepts). Lexicographic comparison of valid values equals chronological order.
fn is_date(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..].iter().all(u8::is_ascii_digit)
}

/// Gate a request's `protocol_version` against the server-advertised `[min, max]` window.
/// Reads succeed for any past version (callers skip this on read paths); this is the write
/// gate.
pub fn protocol_gate(client: &str, min: &str, max: &str) -> Result<(), HandshakeReject> {
    if !is_date(client) {
        return Err(HandshakeReject::ProtocolMalformed);
    }
    // For YYYY-MM-DD, bytewise/lexicographic order is chronological order.
    if client < min || client > max {
        return Err(HandshakeReject::ProtocolOutOfRange);
    }
    Ok(())
}

/// Reject a `crypto_suite_id` the receiver does not implement (invariant 2).
pub fn check_suite(suite_id: u16) -> Result<(), HandshakeReject> {
    SuiteId::from_u16(suite_id)
        .map(|_| ())
        .ok_or(HandshakeReject::UnknownSuite)
}

/// Reject a `sidecar_schema` above the receiver's max known (Postel cross-version closure).
pub fn check_sidecar_schema(schema: u16, max_known: u16) -> Result<(), HandshakeReject> {
    if schema > max_known {
        Err(HandshakeReject::SidecarSchemaTooNew)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::CRYPTO_SUITE_ID;

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
}
