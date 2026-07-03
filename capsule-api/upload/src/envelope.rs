//! The refuse-by-default envelope gate — the seam wiring `capsule_core::validation` into
//! every write path (contract skeleton; slice `S-C1` in the repo-root `SLICES.md`; SSoT:
//! <https://docs/design/threat-model/validation/>).
//!
//! The pure, key-free invariants (the protocol handshake, the manifest-envelope checks,
//! the idempotency keys) are implemented and exhaustively tested in
//! `capsule_core::validation` — [`protocol_gate`] and [`check_manifest_envelope`] are
//! ready to consume. What this middleware adds when `S-C1` lands is the transport glue:
//! read the `X-Capsule-*` negotiation headers, run the gate before any state is written,
//! and map each rejection to its HTTP status plus `error.*` code (see the design docs'
//! API-surfaces rejection mapping). Until then it is an unmounted stub.
//!
//! [`protocol_gate`]: capsule_core::validation::protocol_gate
//! [`check_manifest_envelope`]: capsule_core::validation::check_manifest_envelope

use salvo::prelude::*;

/// Salvo middleware running the fail-closed protocol handshake (invariant 1 and the
/// universal-headers rules) ahead of every write handler it is hooped onto.
#[allow(dead_code)]
pub(crate) struct EnvelopeGate {
    /// The lowest protocol version this server accepts (`X-Capsule-Protocol-Min`).
    pub min_protocol: String,
    /// The highest protocol version this server accepts (`X-Capsule-Protocol-Max`).
    pub max_protocol: String,
}

#[async_trait]
impl Handler for EnvelopeGate {
    async fn handle(
        &self,
        _req: &mut Request,
        _depot: &mut Depot,
        _res: &mut Response,
        _ctrl: &mut FlowCtrl,
    ) {
        // S-C1 wires capsule_core::validation::protocol_gate here: parse
        // X-Capsule-Protocol, reject outside [min, max] with 426 + the error.* code and
        // the advertised range, and short-circuit before any handler writes state.
        todo!("S-C1: envelope gate wiring — see SLICES.md")
    }
}
