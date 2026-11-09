//! Organization: the trash state machine ([`trash`]), the culling workflow
//! (flag → filter → sweep), and stack membership + derived group cull state.

use std::collections::BTreeSet;

use capsule_core::crypto::keys::Amk;
use capsule_core::crypto::provenance::Action;
use capsule_core::culling::GroupCullState;
use capsule_core::metadata::crdt::Lww;
use capsule_core::sidecar::sidecar_v1::{CullFlag, StackMembership};
use capsule_core::sidecar::{
    Region, RegionShape, Regions, STACK_DOC_SCHEMA_V1, StackDocV1, live_region, merge_regions,
    normalize_label, write_region,
};
use uuid::Uuid;

use super::album::{read_doc_file, remove_label, stack_doc_path, write_doc_file};
use super::{AssetState, LifecycleError, Result, Workspace, asset_is_deleted, now_rfc3339};

mod trash;

pub use trash::{FeedDrain, PurgeOutcome, TrashEntry};

impl Workspace {
    // ── Culling workflow ────────────────────────────────────────────────
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

    // ── Hidden assets ─────────────────────────────────────────────
    //
    // Hiding is metadata, never a copy or a move: the `hidden` LWW register is written into the
    // signed sidecar as a `metadata-update`, so it is durable, reversible, and survives a
    // `rebuild_index`. Every default projection excludes a hidden asset; the gated Hidden view
    // ([`GateKeeper::query_hidden`](crate::library::auth_gate::GateKeeper::query_hidden)) is the
    // only surface that serves it (owner: [Organization — Hidden Assets]).
    //
    // [Organization — Hidden Assets]: https://docs/design/organization/#hidden-assets

    /// Hide (`true`) or unhide (`false`) an asset: write the `hidden` LWW register and emit a
    /// `metadata-update`. Stamped with this device id + now, so concurrent writes from two
    /// devices converge under the LWW `(ts, device_id)` rule.
    ///
    /// The index row is refreshed by the same `append_lifecycle` write, so the asset leaves the
    /// default projections and enters the gated Hidden view immediately — no rebuild required.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id))]
    pub fn set_hidden(&mut self, asset_id: &Uuid, hidden: bool) -> Result<()> {
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            s.hidden.set(hidden, ts, device);
        })
    }

    /// Whether an asset is currently hidden — `false` if never written or the asset is unknown
    /// (the wire-absent default is *visible*).
    pub fn is_hidden(&self, asset_id: &Uuid) -> bool {
        self.assets
            .get(asset_id)
            .and_then(|a| a.sidecar.hidden.get().copied())
            .unwrap_or_default()
    }

    /// Apply a peer device's `hidden` register into the local one (the CRDT sync-apply path) and
    /// emit a `metadata-update`. The merge is the [`Lww`] merge, so the flag converges to the
    /// same value regardless of which device's write arrives first.
    pub fn apply_remote_hidden(&mut self, asset_id: &Uuid, remote: &Lww<bool>) -> Result<()> {
        let remote = remote.clone();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            s.hidden.merge(&remote);
        })
    }

    /// The current `hidden` register for an asset (for driving a sync/merge with a peer replica).
    pub fn hidden_register(&self, asset_id: &Uuid) -> Option<&Lww<bool>> {
        self.assets.get(asset_id).map(|a| &a.sidecar.hidden)
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
        if let Some(m) = &membership {
            self.check_stack_album(asset_id, m.stack_id)?;
        }
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            s.stack_membership.set(membership, ts, device);
        })
    }

    /// Refuse a join that would split `stack_id` across container albums: every member of a
    /// stack belongs to one album (owner: [Organization — Labels]), the one whose AMK seals
    /// the stack's label record.
    ///
    /// [Organization — Labels]: https://docs/design/organization/#labels
    fn check_stack_album(&self, asset_id: &Uuid, stack_id: Uuid) -> Result<()> {
        let asset_album = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .album_id;
        let stack_album = match self.stack_album(stack_id) {
            Ok(album) => album,
            Err(LifecycleError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        if stack_album == asset_album {
            Ok(())
        } else {
            tracing::warn!(
                stack_id = %stack_id,
                stack_album = %stack_album,
                asset_album = %asset_album,
                "stack join refused: it would split the stack across albums"
            );
            Err(LifecycleError::StackAlbumMismatch {
                stack_id,
                stack_album,
                asset_album,
            })
        }
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

    // ── Image regions (owner: Organization — Labels) ────────────────────────────────────
    //
    // Regions live in the asset's signed sidecar, so every edit is a `metadata-update` like a
    // tag or a caption. Deleting a region is final: the region map merges as a semilattice in
    // which a tombstone absorbs every live write (`sidecar::region`).

    /// The live region `region_id` of `asset_id`, or [`LifecycleError::NotFound`].
    fn live_region_of(&self, asset_id: &Uuid, region_id: &Uuid) -> Result<Region> {
        self.assets
            .get(asset_id)
            .and_then(|a| a.sidecar.regions.get(region_id))
            .and_then(live_region)
            .cloned()
            .ok_or_else(|| LifecycleError::NotFound(format!("region {region_id} of {asset_id}")))
    }

    /// Mark a region of `asset_id` and return its new id (UUIDv7). The shape is in coordinates
    /// normalized to `[0, 1]` over the original's display orientation.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id))]
    pub fn add_region(&mut self, asset_id: &Uuid, shape: RegionShape) -> Result<Uuid> {
        shape.validate()?;
        let region_id = Uuid::now_v7();
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            let register = s.regions.entry(region_id).or_default();
            write_region(register, Some(Region::new(shape)), ts, device);
        })?;
        tracing::info!(region_id = %region_id, "region added");
        Ok(region_id)
    }

    /// Delete a region. Final: a concurrent or later edit of it on another device does not
    /// bring it back once the two merge.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id, region_id = %region_id))]
    pub fn delete_region(&mut self, asset_id: &Uuid, region_id: &Uuid) -> Result<()> {
        self.live_region_of(asset_id, region_id)?;
        let region_id = *region_id;
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            let register = s.regions.entry(region_id).or_default();
            write_region(register, None, ts, device);
        })
    }

    /// Add a label to a region.
    #[tracing::instrument(skip(self, label), fields(asset_id = %asset_id, region_id = %region_id))]
    pub fn add_region_label(
        &mut self,
        asset_id: &Uuid,
        region_id: &Uuid,
        label: &str,
    ) -> Result<()> {
        let label = normalize_label(label)?;
        let mut region = self.live_region_of(asset_id, region_id)?;
        let region_id = *region_id;
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, add_id| {
            region.labels.add(label, add_id);
            let register = s.regions.entry(region_id).or_default();
            write_region(register, Some(region), ts, device);
        })
    }

    /// Remove a label from a region: every add of it this device has observed. Returns how
    /// many adds were removed; 0 writes nothing.
    #[tracing::instrument(skip(self, label), fields(asset_id = %asset_id, region_id = %region_id))]
    pub fn remove_region_label(
        &mut self,
        asset_id: &Uuid,
        region_id: &Uuid,
        label: &str,
    ) -> Result<usize> {
        let label = normalize_label(label)?;
        let mut region = self.live_region_of(asset_id, region_id)?;
        let removed = remove_label(&mut region.labels, &label);
        if removed == 0 {
            return Ok(0);
        }
        let region_id = *region_id;
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            let register = s.regions.entry(region_id).or_default();
            write_region(register, Some(region), ts, device);
        })?;
        Ok(removed)
    }

    /// The asset's live regions, by id. Empty for an unknown asset.
    pub fn regions(&self, asset_id: &Uuid) -> Vec<(Uuid, Region)> {
        self.assets
            .get(asset_id)
            .map(|a| {
                a.sidecar
                    .regions
                    .iter()
                    .filter_map(|(id, reg)| live_region(reg).map(|r| (*id, r.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Merge a peer device's region map into the asset's (the CRDT sync-apply path) and emit a
    /// `metadata-update`.
    pub fn apply_remote_regions(&mut self, asset_id: &Uuid, remote: &Regions) -> Result<()> {
        let remote = remote.clone();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _| {
            merge_regions(&mut s.regions, &remote);
        })
    }

    // ── Stack labels: the stack's `StackDocV1` (owner: Organization — Labels) ─────────────

    /// The container album of `stack_id`: the album its members share, else the one its label
    /// record names.
    fn stack_album(&self, stack_id: Uuid) -> Result<Uuid> {
        if let Some(album) = self
            .stack_members(stack_id)
            .first()
            .and_then(|id| self.assets.get(id))
            .map(|a| a.album_id)
        {
            return Ok(album);
        }
        match read_doc_file(&stack_doc_path(&self.root, &stack_id))? {
            Some(bytes) => {
                Ok(StackDocV1::from_canonical_slice(&bytes, STACK_DOC_SCHEMA_V1)?.album_id)
            }
            None => Err(LifecycleError::NotFound(format!("stack {stack_id}"))),
        }
    }

    /// `stack_id`'s label record: the persisted one, or an empty one in its container album.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::NotFound`] for a stack with no members and no record;
    /// [`LifecycleError::UnverifiedDocument`] when the persisted record does not verify.
    pub fn stack_doc(&self, stack_id: Uuid) -> Result<StackDocV1> {
        if let Some(bytes) = read_doc_file(&stack_doc_path(&self.root, &stack_id))? {
            let doc = StackDocV1::from_canonical_slice(&bytes, STACK_DOC_SCHEMA_V1)?;
            if doc.stack_id != stack_id || !doc.verify(&self.account.user_ik.verifying_key()) {
                return Err(LifecycleError::UnverifiedDocument(stack_id));
            }
            return Ok(doc);
        }
        Ok(StackDocV1::new(stack_id, self.stack_album(stack_id)?))
    }

    fn store_stack_doc(&self, mut doc: StackDocV1) -> Result<()> {
        if !doc.verify(&self.account.user_ik.verifying_key()) {
            doc.sign(&self.account.user_ik);
        }
        write_doc_file(
            &stack_doc_path(&self.root, &doc.stack_id),
            &doc.to_canonical_vec(),
        )?;
        tracing::debug!(
            stack_id = %doc.stack_id,
            labels = doc.labels.value().len(),
            "stack label record persisted"
        );
        Ok(())
    }

    /// Add a label to a stack.
    #[tracing::instrument(skip(self, label), fields(stack_id = %stack_id))]
    pub fn add_stack_label(&mut self, stack_id: Uuid, label: &str) -> Result<()> {
        let label = normalize_label(label)?;
        let mut doc = self.stack_doc(stack_id)?;
        let add_id = self.issue_add_id_for(&doc.labels);
        doc.labels.add(label, add_id);
        doc.signature = None;
        self.store_stack_doc(doc)
    }

    /// Remove a label from a stack: every add of it this device has observed. Returns how many
    /// adds were removed; 0 writes nothing.
    #[tracing::instrument(skip(self, label), fields(stack_id = %stack_id))]
    pub fn remove_stack_label(&mut self, stack_id: Uuid, label: &str) -> Result<usize> {
        let label = normalize_label(label)?;
        let mut doc = self.stack_doc(stack_id)?;
        let removed = remove_label(&mut doc.labels, &label);
        if removed > 0 {
            doc.signature = None;
            self.store_stack_doc(doc)?;
        }
        Ok(removed)
    }

    /// The stack's current labels.
    pub fn stack_labels(&self, stack_id: Uuid) -> Result<BTreeSet<String>> {
        Ok(self.stack_doc(stack_id)?.labels.value())
    }

    /// Seal the stack's record under its container album's current-epoch AMK.
    pub fn seal_stack_doc(&self, stack_id: Uuid) -> Result<Vec<u8>> {
        let doc = self.stack_doc(stack_id)?;
        let album = self.album(&doc.album_id)?;
        let amk = Amk::from_bytes(album.amks[&album.current_epoch]);
        Ok(doc.seal(&amk)?)
    }

    /// Open a sealed stack record another device of this account sent, verify it under this
    /// account's identity key, and merge it into the local one. Every epoch of `album_id` this
    /// workspace holds is tried, newest first.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::Document`] when no held AMK opens it, it does not decode, or it places
    /// the stack in another album than the local record; [`LifecycleError::UnverifiedDocument`]
    /// when it does not verify. Nothing is merged.
    #[tracing::instrument(skip(self, wire), fields(stack_id = %stack_id, album_id = %album_id))]
    pub fn apply_remote_stack_doc(
        &mut self,
        stack_id: Uuid,
        album_id: Uuid,
        wire: &[u8],
    ) -> Result<()> {
        let album = self.album(&album_id)?;
        let mut result = Err(LifecycleError::NotFound(format!("album {album_id} keys")));
        for amk in album.amks.values().rev() {
            result = StackDocV1::open_sealed(
                wire,
                &Amk::from_bytes(*amk),
                &stack_id,
                STACK_DOC_SCHEMA_V1,
            )
            .map_err(LifecycleError::from);
            if result.is_ok() {
                break;
            }
        }
        let remote = result?;
        if remote.album_id != album_id || !remote.verify(&self.account.user_ik.verifying_key()) {
            tracing::warn!("remote stack label record does not verify; refused");
            return Err(LifecycleError::UnverifiedDocument(stack_id));
        }
        self.merge_stack_doc(&remote)
    }

    /// Merge an already-trusted stack record into the local one and persist the result.
    pub(super) fn merge_stack_doc(&mut self, remote: &StackDocV1) -> Result<()> {
        let mut doc = match self.stack_doc(remote.stack_id) {
            Ok(doc) => doc,
            Err(LifecycleError::NotFound(_)) => StackDocV1::new(remote.stack_id, remote.album_id),
            Err(e) => return Err(e),
        };
        doc.merge(remote)?;
        self.store_stack_doc(doc)?;
        tracing::info!(stack_id = %remote.stack_id, "stack label record merged");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
