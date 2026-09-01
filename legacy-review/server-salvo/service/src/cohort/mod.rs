//! The durable device-cohort map (slice `S-C13`).
//!
//! An advisory, client-asserted `cohort_hash` groups a physical device's sessions across app
//! reinstalls (each reinstall re-enrolls with a fresh, security-bearing `device_id`). This
//! module owns the small durable `device_cohorts(user_id, cohort_hash, first_seen, last_seen)`
//! table that persists the grouping **beyond session expiry** — a session-store-only cohort
//! would be forgotten exactly when "have I seen this device before?" matters.
//!
//! **Advisory-only, structurally.** The value is unverifiable, so nothing here feeds any
//! authorization or capability decision. Authorization in `capsule-api-auth` is driven solely
//! by the JWT [`Claims`], which carry no cohort field; this map is read only to surface
//! grouping in the session listing. The security-bearing identity is `device_id`/the DSK,
//! kept in a wholly separate identifier space.
//!
//! - [`Mutation::observe`] records a `(user_id, cohort_hash)` sighting: it pins `first_seen`
//!   on the first observation and bumps `last_seen` on every re-observation, in one guarded
//!   upsert. Calling it is *advisory* — a failure must never fail the auth ceremony.
//! - [`Query::for_user`] returns a user's cohort map for the session-listing surface.
//!
//! [`Claims`]: (the auth crate's JWT claims — no cohort field by construction)

mod mutation;
mod query;

pub use mutation::Mutation;
pub use query::{CohortObservation, Query};
use thiserror::Error;

/// The maximum accepted length of an advisory cohort hash, in characters.
///
/// A well-formed value is a 64-char hex SHA-256; the bound leaves generous headroom for
/// alternate encodings while refusing an unbounded client-supplied blob. It is a structural
/// guard, not a validity check — an over-long value is treated as **absent**, which the
/// contract explicitly permits ("absent or garbage ... behaves identically").
pub const MAX_COHORT_HASH_LEN: usize = 128;

/// Normalize a client-asserted cohort value into what may be stored, or `None`.
///
/// The cohort is advisory and opaque, so normalization is deliberately minimal: trim
/// surrounding whitespace, then drop it if it is empty or exceeds [`MAX_COHORT_HASH_LEN`].
/// A dropped value is indistinguishable from an absent one downstream — the server behaves
/// identically for absent, empty, over-long, and well-formed inputs, differing only in which
/// (harmless, non-authoritative) grouping row it records.
pub fn normalize(raw: Option<String>) -> Option<String> {
    let trimmed = raw?.trim().to_string();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_COHORT_HASH_LEN {
        return None;
    }
    Some(trimmed)
}

/// A device-cohort store failure. The advisory nature of cohorts means callers **log and
/// continue** rather than surfacing this to the user — it never produces a new error path in
/// the auth ceremony.
#[derive(Debug, Error)]
pub enum CohortError {
    /// A database failure recording or reading the cohort map.
    #[error("device cohort store error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_drops_absent_empty_and_oversized() {
        assert_eq!(normalize(None), None);
        assert_eq!(normalize(Some(String::new())), None);
        assert_eq!(normalize(Some("   ".to_string())), None);
        let too_long = "a".repeat(MAX_COHORT_HASH_LEN + 1);
        assert_eq!(normalize(Some(too_long)), None);
    }

    #[test]
    fn normalize_keeps_and_trims_well_formed() {
        assert_eq!(
            normalize(Some("  deadbeef  ".to_string())),
            Some("deadbeef".to_string())
        );
        let hex = "a".repeat(64);
        assert_eq!(normalize(Some(hex.clone())), Some(hex));
        // A short "garbage" value is retained verbatim: it groups as its own cohort, which is
        // behaviourally identical to any other cohort (advisory, non-authoritative).
        assert_eq!(
            normalize(Some("garbage".to_string())),
            Some("garbage".to_string())
        );
    }
}
