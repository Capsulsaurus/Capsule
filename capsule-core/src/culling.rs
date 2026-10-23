//! The client-side **culling** workflow engine (SSoT: [Organization — Culling]).
//!
//! Culling is the review pass a photographer makes after a shoot: keep, undecided, toss.
//! Capsule models it as a trinary [`CullFlag`] per asset
//! stored in the sidecar's `cull` LWW register. This module owns the *derived* pieces the
//! schema deliberately does **not** store, so there is no second source of truth to diverge:
//!
//! - [`GroupCullState`] — a stack/burst has no stored flag of its own; its cull state is
//!   **derived** from its members (all-rejected, any-pick, else mixed).
//!
//! The stateful surface (writing flags as signed `metadata-update`s, the cull-filtered views,
//! flagging a whole stack, and the reject-sweep) lives on
//! `Workspace`, which ties these predicates to the on-disk
//! signed data plane. Flagging never touches bytes and is fully reversible; the reject-sweep's
//! batch-move to trash is the *only* destructive step, and it is soft-per-retention like any
//! delete.
//!
//! [Organization — Culling]: https://docs/design/organization/#culling

use crate::sidecar::sidecar_v1::CullFlag;

/// The **derived** cull state of a stack/burst (owner: [Organization — Culling]). A group
/// never stores a flag of its own — this is computed from its members every time it is read,
/// so grouping cannot diverge from the per-asset flags.
///
/// The precedence is exactly the doc's: `all-rejected → any-pick → else mixed`.
///
/// [Organization — Culling]: https://docs/design/organization/#culling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCullState {
    /// Every member is [`CullFlag::Reject`] — the whole group is a sweep candidate.
    AllRejected,
    /// At least one member is [`CullFlag::Pick`] (and not every member is rejected).
    AnyPick,
    /// Neither all-rejected nor any-pick (e.g. some rejects mixed with neutrals, or an
    /// entirely undecided group).
    Mixed,
}

impl GroupCullState {
    /// Derive a group's state from its members' flags, per the doc precedence
    /// (`all-rejected → any-pick → else mixed`). Returns `None` for an empty group — there
    /// are no members to derive a state from, and a vacuous "all rejected" would be wrong.
    pub fn derive(flags: impl IntoIterator<Item = CullFlag>) -> Option<Self> {
        let mut count = 0usize;
        let mut all_reject = true;
        let mut any_pick = false;
        for flag in flags {
            count += 1;
            match flag {
                CullFlag::Reject => {}
                CullFlag::Pick => {
                    any_pick = true;
                    all_reject = false;
                }
                CullFlag::Neutral => all_reject = false,
            }
        }
        if count == 0 {
            return None;
        }
        Some(if all_reject {
            Self::AllRejected
        } else if any_pick {
            Self::AnyPick
        } else {
            Self::Mixed
        })
    }
}

#[cfg(test)]
mod tests;
