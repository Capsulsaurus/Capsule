//! Server-to-server federation — capabilities and compartmentalized pulls (slice `S-E2`).
//!
//! Federation lets an album owned on one Capsule server be shared to users on another. It
//! introduces **no new data protocol**: a peer fetches the exact same content-addressed primitives
//! a client does — the [sync feed](crate::feed), `GET /blob/{hash}`, the manifest envelope — over
//! the same transports. The only new things are a **capability token** (the contract gating which
//! peers may fetch what) and a **per-peer compartmentalization layer**. SSoT: the
//! [Federation design doc](https://docs/design/federation/); the boundary invariants 19–21 are
//! owned by [Threat Model — Validation](https://docs/design/threat-model/validation/#on-federation-pull-server-to-server).
//!
//! This module holds the **runtime** federation logic; the durable stores (peer identity,
//! revocation list) live in [`service::federation`]. The pieces:
//!
//! - [`capability`] — capability issuance/verification (invariant 19), scope by blob role.
//! - [`revocation`] — the verifier-side revocation view with 15-minute fail-closed staleness.
//! - [`compartment`] — per-peer budgets, circuit breaker, probation (invariant 21).
//! - [`rejected`] — the bounded, LRU-by-last-reference soft-fail rejected-hash table.
//! - [`pull`] — the pull path: serve-side authorization + ingest-side re-validation (invariant 20)
//!   of every pulled manifest against `capsule_core`'s keyless battery (invariants 1–18 + 25).
//!
//! **Threat model:** a remote server is hostile until proven otherwise. Every claim a peer makes is
//! untrusted input until a signature or content hash says otherwise; peers *pull*, they never push
//! into Capsule's database.

pub mod capability;
pub mod compartment;
pub mod pull;
pub mod rejected;
pub mod revocation;

#[cfg(test)]
mod tests;

pub use capability::{
    CapabilityClaims, CapabilityIssuer, CapabilityReject, FederationScope, IssueParams,
    MintedCapability, VerifyContext, verify_capability,
};
use capsule_i18n::error_codes;
pub use compartment::{CompartmentReject, PeerLimits, PeerRegistry, PeerTier, PullCost};
pub use pull::{PullBoundaryReject, PullValidationContext, PulledEnvelope, revalidate_pulled};
pub use rejected::RejectedHashTable;
pub use revocation::{RevocationList, RevocationVerdict};
use thiserror::Error;

/// A federation pull refusal that carries a stable `error.*` code and its transport status — the
/// unified gate outcome for invariants 19 and 21. (Invariant 20's pulled-content refusals are
/// [`PullBoundaryReject`], tagged by the specific invariant tripped.)
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FederationReject {
    /// A capability-token verification failure (invariant 19).
    #[error("federation capability rejected: {0:?}")]
    Capability(CapabilityReject),
    /// A per-peer compartment refusal (invariant 21 / circuit breaker).
    #[error("federation request contained: {0:?}")]
    Compartment(CompartmentReject),
}

impl From<CapabilityReject> for FederationReject {
    fn from(reject: CapabilityReject) -> Self {
        FederationReject::Capability(reject)
    }
}

impl From<CompartmentReject> for FederationReject {
    fn from(reject: CompartmentReject) -> Self {
        FederationReject::Compartment(reject)
    }
}

impl FederationReject {
    /// The stable `error.federation.*` code this refusal surfaces (clients switch on the code, not
    /// the status).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            FederationReject::Capability(c) => match c {
                CapabilityReject::Expired => error_codes::FEDERATION_CAPABILITY_EXPIRED,
                CapabilityReject::Revoked | CapabilityReject::RevocationUnverifiable => {
                    error_codes::FEDERATION_CAPABILITY_REVOKED
                }
                CapabilityReject::AudienceMismatch => error_codes::FEDERATION_AUDIENCE_MISMATCH,
                CapabilityReject::ScopeInsufficient => error_codes::FEDERATION_SCOPE_INSUFFICIENT,
                CapabilityReject::MissingClaim(_)
                | CapabilityReject::BadSignature
                | CapabilityReject::Malformed(_)
                | CapabilityReject::WrongIssuer
                | CapabilityReject::NotYetValid
                | CapabilityReject::ExpiryTooFar => error_codes::FEDERATION_CAPABILITY_INVALID,
            },
            FederationReject::Compartment(c) => match c {
                CompartmentReject::CircuitOpen { .. } => error_codes::FEDERATION_CIRCUIT_OPEN,
                CompartmentReject::EventBudgetExceeded
                | CompartmentReject::ByteBudgetExceeded
                | CompartmentReject::CpuBudgetExceeded => {
                    error_codes::FEDERATION_RATE_BUDGET_EXCEEDED
                }
            },
        }
    }

    /// The HTTP status this refusal maps to (invariant 19: 401/403; invariant 21: 429).
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            FederationReject::Capability(c) => match c {
                CapabilityReject::Expired
                | CapabilityReject::MissingClaim(_)
                | CapabilityReject::BadSignature
                | CapabilityReject::Malformed(_)
                | CapabilityReject::WrongIssuer
                | CapabilityReject::NotYetValid
                | CapabilityReject::ExpiryTooFar => 401,
                CapabilityReject::Revoked
                | CapabilityReject::RevocationUnverifiable
                | CapabilityReject::AudienceMismatch
                | CapabilityReject::ScopeInsufficient => 403,
            },
            FederationReject::Compartment(_) => 429,
        }
    }
}
