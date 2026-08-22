//! Framework-free wire contract (slice `S-C27`).
//!
//! The server's response taxonomy — which outcome carries which status, which body, and
//! which published sentence — used to exist only as framework trait impls, written twice per
//! response enum (once to render it, once to document it) with nothing keeping the two
//! halves in agreement. That made the transport a load-bearing part of the contract: the
//! contract could not outlive the framework.
//!
//! This crate owns that taxonomy as plain data ([`ResponseSpec`], [`BodyShape`],
//! [`WireResponses`]) plus the protocol [`headers`], and generates the framework impls from
//! it. It depends on nothing but `serde`, and must keep depending on nothing but `serde`:
//! it is the piece of the server that survives the transport swap unchanged.
//!
//! The Salvo adapter lives in [`salvo_responses!`] — a macro, because the orphan rule allows
//! no other way to implement foreign traits for the server's types, and because a macro can
//! name `::salvo::…` paths without this crate ever linking them.

pub mod headers;
mod response;
mod salvo_adapter;

pub use response::{BodyShape, ResponseSpec, is_valid_status};

/// The neutral response taxonomy of one endpoint's response enum.
///
/// Implemented by [`salvo_responses!`] from the same table its framework impls are generated
/// from, so a consumer that is not the current server — the replacement transport, a
/// conformance test, a documentation generator — reads the contract without the framework.
pub trait WireResponses {
    /// Every outcome this response enum can produce, in declaration order.
    const RESPONSES: &'static [ResponseSpec];

    /// The outcomes that reach the published API document.
    fn documented() -> impl Iterator<Item = &'static ResponseSpec> {
        Self::RESPONSES.iter().filter(|spec| spec.is_documented())
    }

    /// The outcomes deliberately absent from the published API document.
    ///
    /// A non-empty result is a real documentation gap, recorded rather than hidden.
    fn undocumented() -> impl Iterator<Item = &'static ResponseSpec> {
        Self::RESPONSES
            .iter()
            .filter(|spec| spec.status.is_some() && spec.description.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sample;

    impl WireResponses for Sample {
        const RESPONSES: &'static [ResponseSpec] = &[
            ResponseSpec {
                status: Some(200),
                body: BodyShape::Json,
                description: Some("Success"),
                schema: Some("TokenResponse"),
            },
            ResponseSpec {
                status: Some(429),
                body: BodyShape::Json,
                description: None,
                schema: None,
            },
            ResponseSpec {
                status: None,
                body: BodyShape::Delegated,
                description: None,
                schema: None,
            },
        ];
    }

    #[test]
    fn documented_outcomes_are_the_published_ones() {
        let statuses: Vec<_> = Sample::documented()
            .filter_map(|spec| spec.status)
            .collect();
        assert_eq!(statuses, vec![200]);
    }

    #[test]
    fn undocumented_outcomes_exclude_delegating_rows() {
        let statuses: Vec<_> = Sample::undocumented()
            .filter_map(|spec| spec.status)
            .collect();
        assert_eq!(statuses, vec![429]);
    }
}
