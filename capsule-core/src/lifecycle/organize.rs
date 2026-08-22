//! Organization: trash (soft-delete / restore), the culling workflow (flag → filter → sweep),
//! and stack membership + derived group cull state.

use jiff::Timestamp;
use uuid::Uuid;

use super::{AssetState, Result, Workspace, asset_is_deleted, now_rfc3339};
use crate::crypto::provenance::action::Action;
use crate::culling::GroupCullState;
use crate::metadata::crdt::Lww;
use crate::sidecar::sidecar_v1::{CullFlag, StackMembership};

impl Workspace {
    /// Soft-delete: emit a `delete` record carrying a signed retention window.
    pub fn soft_delete(&mut self, asset_id: &Uuid, retain_days: i64) -> Result<()> {
        // Timestamp arithmetic is absolute, so a retention "day" is exactly 24 h — the
        // correct semantic for a UTC retention window.
        let until = (Timestamp::now()
            + jiff::SignedDuration::from_hours(retain_days.saturating_mul(24)))
        .to_string();
        self.append_lifecycle(asset_id, Action::Delete, Some(until), |_, _| {})
    }

    /// Restore a soft-deleted asset: emit a `trash-restore` record.
    pub fn restore(&mut self, asset_id: &Uuid) -> Result<()> {
        self.append_lifecycle(asset_id, Action::TrashRestore, None, |_, _| {})
    }

    // ── Culling workflow (S-D13) ────────────────────────────────────────────────
    //
    // The client review pass: flag → filter → act. Flagging writes the trinary `cull` LWW
    // register as a signed `metadata-update` (never touches bytes, fully reversible); the
    // reject-sweep batch-moves flagged rejects to trash — the *only* destructive step, soft
    // per retention like any delete. Group cull state is *derived* from members (owner:
    // [Organization — Culling]), never stored, so it cannot diverge from the per-asset flags.
    //
    // [Organization — Culling]: https://docs/design/organization/#culling

    /// Flag an asset for culling: write the trinary [`CullFlag`] LWW register and emit a
    /// `metadata-update`. `Neutral` clears a prior flag — flagging is fully reversible and
    /// never touches asset bytes. Stamped with this device id + now, so concurrent flags from
    /// two devices converge under the LWW `(ts, device_id)` rule.
    pub fn set_cull(&mut self, asset_id: &Uuid, flag: CullFlag) -> Result<()> {
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            s.cull.set(flag, ts, device);
        })
    }

    /// The asset's current culling flag — [`CullFlag::Neutral`] if never flagged or the asset
    /// is unknown (the wire-absent default).
    pub fn cull_flag(&self, asset_id: &Uuid) -> CullFlag {
        self.assets
            .get(asset_id)
            .and_then(|a| a.sidecar.cull.get().copied())
            .unwrap_or_default()
    }

    /// Apply a peer device's `cull` register into the local one (the CRDT sync-apply path) and
    /// emit a `metadata-update`. The merge is the [`Lww`] merge, so the flag converges to the
    /// same value regardless of which device's write arrives first.
    pub fn apply_remote_cull(&mut self, asset_id: &Uuid, remote: &Lww<CullFlag>) -> Result<()> {
        let remote = remote.clone();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            s.cull.merge(&remote);
        })
    }

    /// The current `cull` register for an asset (for driving a sync/merge with a peer replica).
    pub fn cull_register(&self, asset_id: &Uuid) -> Option<&Lww<CullFlag>> {
        self.assets.get(asset_id).map(|a| &a.sidecar.cull)
    }

    /// The cull-**filtered** view: managed, non-trashed assets currently carrying `flag`
    /// (sorted for determinism). Filtering by [`CullFlag::Neutral`] returns the never-flagged
    /// assets too, since an unwritten register reads as `Neutral`.
    pub fn assets_by_cull(&self, flag: CullFlag) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .assets
            .values()
            .filter(|a| {
                a.sidecar.cull.get().copied().unwrap_or_default() == flag && !asset_is_deleted(a)
            })
            .map(|a| a.asset_id)
            .collect();
        ids.sort();
        ids
    }

    /// Set (or clear, with `None`) this asset's stack membership (LWW register) and emit a
    /// `metadata-update`. The companion write to [`group_cull_state`](Self::group_cull_state):
    /// a stack is the set of assets sharing a `stack_id`, and grouping converges under the same
    /// `(ts, device_id)` rule as every LWW field.
    pub fn set_stack_membership(
        &mut self,
        asset_id: &Uuid,
        membership: Option<StackMembership>,
    ) -> Result<()> {
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            s.stack_membership.set(membership, ts, device);
        })
    }

    /// The non-trashed members of `stack_id` (assets whose current sidecar stack membership
    /// points at it), sorted.
    fn stack_members(&self, stack_id: Uuid) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .assets
            .values()
            .filter(|a| self.membership_stack_id(a) == Some(stack_id) && !asset_is_deleted(a))
            .map(|a| a.asset_id)
            .collect();
        ids.sort();
        ids
    }

    /// The stack id an asset's current sidecar membership register points at, if any.
    fn membership_stack_id(&self, asset: &AssetState) -> Option<Uuid> {
        asset
            .sidecar
            .stack_membership
            .get()
            .and_then(|m| m.as_ref())
            .map(|m| m.stack_id)
    }

    /// The **derived** cull state of a stack/group (owner: [Organization — Culling]): computed
    /// from its members' flags every time, never stored, so it cannot diverge from the
    /// per-asset flags. `None` when the stack has no (non-trashed) members.
    ///
    /// [Organization — Culling]: https://docs/design/organization/#culling
    pub fn group_cull_state(&self, stack_id: Uuid) -> Option<GroupCullState> {
        let flags = self
            .stack_members(stack_id)
            .into_iter()
            .map(|id| self.cull_flag(&id));
        GroupCullState::derive(flags)
    }

    /// Apply `flag` to every member of `stack_id` — the doc's "flagging a collapsed stack
    /// applies the flag to each member", one `metadata-update` per member. Returns the members
    /// flagged (sorted).
    pub fn flag_stack(&mut self, stack_id: Uuid, flag: CullFlag) -> Result<Vec<Uuid>> {
        let members = self.stack_members(stack_id);
        for id in &members {
            self.set_cull(id, flag)?;
        }
        Ok(members)
    }

    /// The reject-**sweep**: batch-move every `Reject`-flagged, not-already-trashed managed
    /// asset to trash — a soft delete carrying the signed `retain_days` retention window. This
    /// is the *only* destructive step of culling and is reversible via
    /// [`restore`](Self::restore) until the retention window elapses. Returns the swept asset
    /// ids (sorted).
    #[tracing::instrument(skip(self), fields(retain_days))]
    pub fn reject_sweep(&mut self, retain_days: i64) -> Result<Vec<Uuid>> {
        let targets = self.assets_by_cull(CullFlag::Reject);
        for id in &targets {
            self.soft_delete(id, retain_days)?;
        }
        tracing::info!(swept = targets.len(), "cull: reject sweep complete");
        Ok(targets)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::verify_asset::VerifyOutcome;

    // ── Culling workflow (S-D13) ────────────────────────────────────────────────

    /// Import three distinct assets into `ws`'s `album` and return their ids in import order.
    fn import_trio(ws: &mut Workspace, album: Uuid, src: &Path) -> [Uuid; 3] {
        let mut ids = [Uuid::nil(); 3];
        for (i, id) in ids.iter_mut().enumerate() {
            let p = src.join(format!("cull-{i}.jpg"));
            let mut bytes = vec![0xFF, 0xD8, 0xFF];
            bytes.extend_from_slice(format!("cull fixture asset {i}").as_bytes());
            fs::write(&p, &bytes).unwrap();
            *id = ws.import_asset(album, &p).unwrap();
        }
        ids
    }

    /// S-D13 done-when (round-trip): the flag → filter → sweep loop round-trips on a fixture
    /// library. Flag pick/reject/neutral, filter the view by each flag, sweep the rejects to
    /// trash (soft, retention-carrying, the only destructive step), and restore to prove
    /// reversibility.
    #[test]
    fn cull_flag_filter_sweep_loop_round_trips() {
        use crate::sidecar::sidecar_v1::CullFlag;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot");
        let [pick, reject, neutral] = import_trio(&mut ws, album, src.path());

        // Flag: single write per asset, never touches bytes, and the sidecar stays signed.
        ws.set_cull(&pick, CullFlag::Pick).unwrap();
        ws.set_cull(&reject, CullFlag::Reject).unwrap();
        // `neutral` is left never-flagged (the wire-absent default).
        assert_eq!(ws.cull_flag(&pick), CullFlag::Pick);
        assert_eq!(ws.cull_flag(&reject), CullFlag::Reject);
        assert_eq!(ws.cull_flag(&neutral), CullFlag::Neutral);
        let st = ws.asset(&pick).unwrap();
        assert!(st.sidecar.verify(&ws.account.user_ik.verifying_key()));
        assert_eq!(ws.verify(&pick).unwrap(), VerifyOutcome::Accept);

        // Filter: the cull-filtered views partition the library by flag.
        assert_eq!(ws.assets_by_cull(CullFlag::Pick), vec![pick]);
        assert_eq!(ws.assets_by_cull(CullFlag::Reject), vec![reject]);
        assert!(ws.assets_by_cull(CullFlag::Neutral).contains(&neutral));

        // Sweep: the reject moves to trash (soft), the pick/neutral are untouched.
        let swept = ws.reject_sweep(30).unwrap();
        assert_eq!(swept, vec![reject]);
        // The swept asset dropped out of the (non-trashed) reject view; pick/neutral remain.
        assert!(ws.assets_by_cull(CullFlag::Reject).is_empty());
        assert_eq!(ws.assets_by_cull(CullFlag::Pick), vec![pick]);
        assert!(ws.assets_by_cull(CullFlag::Neutral).contains(&neutral));

        // Sweep is a soft delete carrying a signed retention window (reversible until purge).
        let head = ws.asset(&reject).unwrap().chain.records().last().unwrap();
        assert_eq!(head.manifest.core.action, Action::Delete);
        assert!(
            head.manifest.core.retention_until.is_some(),
            "the sweep's delete must carry a retention window"
        );

        // Reversible: a restore brings the swept asset back into the reject view.
        ws.restore(&reject).unwrap();
        assert_eq!(ws.assets_by_cull(CullFlag::Reject), vec![reject]);

        // A second sweep with nothing flagged reject is a no-op.
        ws.set_cull(&reject, CullFlag::Neutral).unwrap();
        assert!(ws.reject_sweep(30).unwrap().is_empty());
    }

    /// S-D13 done-when (convergence): concurrent flags from two devices converge. Driven
    /// exactly as the CRDT tests — two register writes with distinct actors/timestamps merged
    /// in both orders — and through the engine's sync-apply path.
    #[test]
    fn concurrent_cull_flags_from_two_devices_converge() {
        use crate::metadata::crdt::Lww;
        use crate::sidecar::sidecar_v1::CullFlag;

        let dev_a = Uuid::from_u128(0xA);
        let dev_b = Uuid::from_u128(0xB);

        // Device A flags Pick; device B flags Reject as the strictly later write (a far-future
        // stamp, so it also outranks the workspace's real-now local flag below).
        let mut reg_a: Lww<CullFlag> = Lww::new();
        reg_a.set(CullFlag::Pick, "2026-05-31T10:00:00Z", dev_a);
        let mut reg_b: Lww<CullFlag> = Lww::new();
        reg_b.set(CullFlag::Reject, "2099-01-01T00:00:00Z", dev_b);

        // Both merge orders converge on the later reject — order-independent by construction.
        let mut ab = reg_a.clone();
        ab.merge(&reg_b);
        let mut ba = reg_b.clone();
        ba.merge(&reg_a);
        assert_eq!(ab.get(), ba.get());
        assert_eq!(ab.get(), Some(&CullFlag::Reject));

        // Through the engine: a workspace that locally flagged Pick, then applies device B's
        // register (the sync-apply path), converges on Reject.
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF two-device cull").unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot");
        let asset = ws.import_asset(album, &img).unwrap();

        ws.set_cull(&asset, CullFlag::Pick).unwrap();
        ws.apply_remote_cull(&asset, &reg_b).unwrap();
        assert_eq!(ws.cull_flag(&asset), CullFlag::Reject);
        // Re-applying the stale Pick register changes nothing (idempotent convergence).
        ws.apply_remote_cull(&asset, &reg_a).unwrap();
        assert_eq!(ws.cull_flag(&asset), CullFlag::Reject);
        // The merged sidecar is still signed and verifies through the chokepoint.
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    }

    /// S-D13: a stack/group's cull state is *derived* from its members (all-rejected → any-pick
    /// → mixed), and flagging a collapsed stack applies the flag to each member.
    #[test]
    fn group_cull_state_derives_from_stack_members() {
        use crate::culling::GroupCullState;
        use crate::domain::StackType;
        use crate::sidecar::sidecar_v1::{CullFlag, StackMembership, StackRole};

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot");
        let [a, b, standalone] = import_trio(&mut ws, album, src.path());

        // a + b form a burst; `standalone` stays out of the stack.
        let stack_id = Uuid::now_v7();
        ws.set_stack_membership(
            &a,
            Some(StackMembership {
                stack_id,
                stack_type: StackType::Burst,
                role: StackRole::Primary,
                member_index: Some(0),
            }),
        )
        .unwrap();
        ws.set_stack_membership(
            &b,
            Some(StackMembership {
                stack_id,
                stack_type: StackType::Burst,
                role: StackRole::Member,
                member_index: Some(1),
            }),
        )
        .unwrap();

        // All members neutral → Mixed. An empty/unknown stack has no derived state.
        assert_eq!(ws.group_cull_state(stack_id), Some(GroupCullState::Mixed));
        assert_eq!(ws.group_cull_state(Uuid::now_v7()), None);

        // Flagging the collapsed stack applies to each member (one update per member).
        let flagged = ws.flag_stack(stack_id, CullFlag::Reject).unwrap();
        assert_eq!(flagged, {
            let mut m = vec![a, b];
            m.sort();
            m
        });
        assert_eq!(ws.cull_flag(&a), CullFlag::Reject);
        assert_eq!(ws.cull_flag(&b), CullFlag::Reject);
        // The standalone asset is untouched by a stack flag.
        assert_eq!(ws.cull_flag(&standalone), CullFlag::Neutral);

        // Every member rejected → AllRejected.
        assert_eq!(
            ws.group_cull_state(stack_id),
            Some(GroupCullState::AllRejected)
        );
        // A single pick surfaces the whole group as a keeper → AnyPick.
        ws.set_cull(&a, CullFlag::Pick).unwrap();
        assert_eq!(ws.group_cull_state(stack_id), Some(GroupCullState::AnyPick));
    }
}
