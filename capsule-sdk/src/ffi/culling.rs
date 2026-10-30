//! The **cull verbs** of the `capsule_sdk` uniffi namespace: flag an asset, flag a whole stack,
//! read a stack's derived state, list the assets carrying a flag, and the reject sweep — over
//! `capsule-library`'s [`Workspace`] culling workflow ([Organization — Culling]).
//!
//! Every verb here is one call into the workspace under its lock. Flagging writes the trinary
//! `cull` LWW register as a signed `metadata-update` and never touches bytes, so it is fully
//! reversible; the sweep is the one destructive step, and it is a soft delete carrying the
//! signed retention window like any [`FfiWorkspace::delete_asset`]. A stack has no stored flag:
//! [`FfiWorkspace::stack_cull_state`] derives it from the members on every read.
//!
//! A separate `#[uniffi::export] impl` block on the same object, so the culling surface reads
//! as one place rather than a fourth section of the workspace module.
//!
//! [`Workspace`]: capsule_library::lifecycle::Workspace
//! [Organization — Culling]: https://docs/design/organization/#culling

use capsule_core::culling::GroupCullState;
use capsule_core::sidecar::sidecar_v1::CullFlag;

use super::FfiError;
use super::workspace::{FfiWorkspace, parse_uuid, trash_error};

/// The trinary culling flag, mirrored from core's `CullFlag`. `Neutral` is the never-flagged
/// state and what an unwritten register reads as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCullFlag {
    /// A keeper.
    Pick,
    /// Not yet culled either way.
    Neutral,
    /// Marked for rejection: a candidate for the reject sweep.
    Reject,
}

impl From<FfiCullFlag> for CullFlag {
    fn from(flag: FfiCullFlag) -> Self {
        match flag {
            FfiCullFlag::Pick => Self::Pick,
            FfiCullFlag::Neutral => Self::Neutral,
            FfiCullFlag::Reject => Self::Reject,
        }
    }
}

impl From<CullFlag> for FfiCullFlag {
    fn from(flag: CullFlag) -> Self {
        match flag {
            CullFlag::Pick => Self::Pick,
            CullFlag::Neutral => Self::Neutral,
            CullFlag::Reject => Self::Reject,
        }
    }
}

/// A stack's derived cull state, by the doc's precedence `all-rejected → any-pick → else mixed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiStackCullState {
    /// Every member is rejected: the whole stack is a sweep candidate.
    AllRejected,
    /// At least one member is picked.
    AnyPick,
    /// Neither: undecided, or rejects mixed with neutrals.
    Mixed,
}

impl From<GroupCullState> for FfiStackCullState {
    fn from(state: GroupCullState) -> Self {
        match state {
            GroupCullState::AllRejected => Self::AllRejected,
            GroupCullState::AnyPick => Self::AnyPick,
            GroupCullState::Mixed => Self::Mixed,
        }
    }
}

#[uniffi::export]
impl FfiWorkspace {
    /// Flag one asset. `Neutral` clears a prior flag. Signs one `metadata-update`, stamped with
    /// this device and now, so concurrent flags from two devices converge under LWW.
    pub fn set_cull(&self, asset_id: String, flag: FfiCullFlag) -> Result<(), FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| {
            ws.set_cull(&asset_id, flag.into())?;
            tracing::debug!(%asset_id, ?flag, "ffi: asset flagged");
            Ok(())
        })
    }

    /// The asset's current flag: `Neutral` when it was never flagged, and for an asset this
    /// workspace does not hold.
    pub fn cull_flag(&self, asset_id: String) -> Result<FfiCullFlag, FfiError> {
        let asset_id = parse_uuid("asset_id", &asset_id)?;
        self.with(|ws| Ok(ws.cull_flag(&asset_id).into()))
    }

    /// The managed, non-trashed assets currently carrying `flag`, sorted. `Neutral` includes
    /// the never-flagged ones.
    pub fn assets_by_cull(&self, flag: FfiCullFlag) -> Result<Vec<String>, FfiError> {
        self.with(|ws| {
            Ok(ws
                .assets_by_cull(flag.into())
                .iter()
                .map(ToString::to_string)
                .collect())
        })
    }

    /// Flag every non-trashed member of a stack, one `metadata-update` each — flagging a
    /// collapsed stack flags its members, since a stack stores no flag of its own. Returns the
    /// members flagged, sorted; empty for a stack with no members.
    pub fn flag_stack(&self, stack_id: String, flag: FfiCullFlag) -> Result<Vec<String>, FfiError> {
        let stack_id = parse_uuid("stack_id", &stack_id)?;
        self.with(|ws| {
            let members = ws.flag_stack(stack_id, flag.into())?;
            tracing::debug!(%stack_id, ?flag, members = members.len(), "ffi: stack flagged");
            Ok(members.iter().map(ToString::to_string).collect())
        })
    }

    /// A stack's cull state, derived from its members on this read. `None` for a stack with no
    /// non-trashed members.
    pub fn stack_cull_state(
        &self,
        stack_id: String,
    ) -> Result<Option<FfiStackCullState>, FfiError> {
        let stack_id = parse_uuid("stack_id", &stack_id)?;
        self.with(|ws| Ok(ws.group_cull_state(stack_id).map(Into::into)))
    }

    /// The reject sweep: move every rejected, not-yet-trashed asset to the trash with a
    /// retention window of `retain_days` days, and return the swept ids, sorted. Each is a
    /// signed `delete`, restorable from the trash until its window closes. A refusal partway
    /// is [`FfiError::Trash`]; the assets swept before it stay in the trash, and a retried
    /// sweep moves the rest.
    pub fn reject_sweep(&self, retain_days: u32) -> Result<Vec<String>, FfiError> {
        self.with(|ws| {
            let swept = ws
                .reject_sweep(i64::from(retain_days))
                .map_err(trash_error)?;
            tracing::info!(
                swept = swept.len(),
                retain_days,
                "ffi: reject sweep moved assets to the trash"
            );
            Ok(swept.iter().map(ToString::to_string).collect())
        })
    }
}

#[cfg(test)]
mod tests;
