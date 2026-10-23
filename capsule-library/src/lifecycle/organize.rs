//! Organization: trash (soft-delete / restore), the culling workflow (flag → filter → sweep),
//! and stack membership + derived group cull state.

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
use jiff::Timestamp;
use uuid::Uuid;

use super::album::{read_doc_file, remove_label, stack_doc_path, write_doc_file};
use super::{AssetState, LifecycleError, Result, Workspace, asset_is_deleted, now_rfc3339};

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

    // ── Hidden assets (S-D19/S-D25) ─────────────────────────────────────────────
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
mod tests {
    use std::fs;
    use std::path::Path;

    use capsule_core::crypto::verify_asset::VerifyOutcome;
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;

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
        use capsule_core::sidecar::sidecar_v1::CullFlag;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot").unwrap();
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
        use capsule_core::metadata::crdt::Lww;
        use capsule_core::sidecar::sidecar_v1::CullFlag;

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
        let album = ws.create_album("Shoot").unwrap();
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

    // ── Hidden assets (S-D25) ───────────────────────────────────────────────────

    /// A [`LocalAuthGate`] that always grants — the platform adapter's decision is not what is
    /// under test here; what the Hidden view *contains* is.
    struct AllowGate;
    impl crate::library::auth_gate::LocalAuthGate for AllowGate {
        fn authenticate(
            &self,
            _view: crate::library::auth_gate::GatedView,
        ) -> std::result::Result<(), crate::library::auth_gate::LocalAuthError> {
            Ok(())
        }
    }

    /// **The `S-D25` acceptance case.** Hiding an asset through the public API takes it out of
    /// every default projection, puts it in the gated Hidden view, and — because the register
    /// is written into the signed sidecar, not just the index — it is *still* hidden after the
    /// index is deleted and rebuilt from disk.
    ///
    /// Before this slice there was no `set_hidden` at all: the column, the gate and the
    /// projections all read a register nothing wrote.
    #[test]
    fn hidden_written_through_the_api_leaves_default_views_and_survives_rebuild() {
        use crate::library::auth_gate::{GateKeeper, GatedView};
        use crate::library::{open_library, rebuild_index};

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let root = lib.path().to_path_buf();

        let mut ws = fast_workspace(&root);
        let album = ws.create_album("Shoot").unwrap();
        let [secret, visible, _] = import_trio(&mut ws, album, src.path());

        // Both start visible: the wire-absent default.
        assert!(!ws.is_hidden(&secret));
        assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 3);

        ws.set_hidden(&secret, true).unwrap();

        // The register is written into the signed sidecar, which still verifies.
        assert!(ws.is_hidden(&secret));
        assert!(!ws.is_hidden(&visible));
        assert_eq!(ws.verify(&secret).unwrap(), VerifyOutcome::Accept);
        assert_eq!(
            ws.asset(&secret)
                .unwrap()
                .chain
                .records()
                .last()
                .unwrap()
                .manifest
                .core
                .action,
            Action::MetadataUpdate
        );

        // Immediately — no rebuild — the default projection drops it and the gated Hidden view
        // serves it. `append_lifecycle` re-indexes the row as part of the same write.
        let timeline: Vec<String> = ws
            .db()
            .query_timeline(0, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.uuid)
            .collect();
        assert_eq!(timeline.len(), 2);
        assert!(!timeline.contains(&secret.to_string()));

        let mut gk = GateKeeper::new();
        gk.open(GatedView::Hidden, &AllowGate).unwrap();
        let hidden = gk.query_hidden(ws.db(), 0, 100).unwrap();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].uuid, secret.to_string());

        // Recovery: lose the whole index and rebuild it from the artifacts on disk.
        drop(ws);
        fs::remove_file(root.join("index/library.sqlite")).unwrap();
        let library = open_library(&root).unwrap();
        rebuild_index(&library).unwrap();

        let row = library
            .db
            .find_by_uuid(&secret.to_string())
            .unwrap()
            .expect("the hidden asset is back in the index");
        assert!(row.is_hidden, "the sidecar `hidden` register survived");
        let timeline: Vec<String> = library
            .db
            .query_timeline(0, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.uuid)
            .collect();
        assert_eq!(timeline.len(), 2);
        assert!(!timeline.contains(&secret.to_string()));

        let mut gk = GateKeeper::new();
        gk.open(GatedView::Hidden, &AllowGate).unwrap();
        assert_eq!(gk.query_hidden(&library.db, 0, 100).unwrap().len(), 1);
    }

    /// Hiding is fully reversible and never touches asset bytes: unhiding writes `false` into
    /// the same register and the asset returns to the default projections.
    #[test]
    fn unhiding_returns_the_asset_to_the_default_views() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot").unwrap();
        let [a, _, _] = import_trio(&mut ws, album, src.path());

        let bytes = ws.read_plaintext(&a).unwrap();
        ws.set_hidden(&a, true).unwrap();
        assert_eq!(ws.db().query_hidden(0, 100).unwrap().len(), 1);

        ws.set_hidden(&a, false).unwrap();
        assert!(!ws.is_hidden(&a));
        // An explicit un-hide is a *written* `false`, not the absent default — it must beat a
        // stale remote `true` on merge, which an absent register could not.
        assert_eq!(ws.hidden_register(&a).unwrap().get(), Some(&false));
        assert!(ws.db().query_hidden(0, 100).unwrap().is_empty());
        assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 3);
        assert_eq!(ws.read_plaintext(&a).unwrap(), bytes, "bytes untouched");
        assert_eq!(ws.verify(&a).unwrap(), VerifyOutcome::Accept);
    }

    /// The sync-apply arm: a peer's `hidden` register merges under the same `(ts, device_id)`
    /// rule as `cull`, so two devices converge regardless of arrival order.
    #[test]
    fn remote_hidden_register_merges_and_converges() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot").unwrap();
        let [a, _, _] = import_trio(&mut ws, album, src.path());

        // A peer hid it at 11:00; the local device un-hid it at 10:00. Later write wins.
        let mut early: Lww<bool> = Lww::new();
        early.set(false, "2026-08-29T10:00:00Z", Uuid::from_u128(0xA));
        let mut late: Lww<bool> = Lww::new();
        late.set(true, "2026-08-29T11:00:00Z", Uuid::from_u128(0xB));

        ws.apply_remote_hidden(&a, &early).unwrap();
        ws.apply_remote_hidden(&a, &late).unwrap();
        assert!(ws.is_hidden(&a));
        // Re-applying the stale register changes nothing (idempotent convergence).
        ws.apply_remote_hidden(&a, &early).unwrap();
        assert!(ws.is_hidden(&a));
        assert_eq!(ws.db().query_hidden(0, 100).unwrap().len(), 1);
        assert_eq!(ws.verify(&a).unwrap(), VerifyOutcome::Accept);
    }

    /// The manual stacking path writes through to the index the same way the importer does
    /// (`S-B15`): the `assets` stack columns are a projection of the signed register, so
    /// `set_stack_membership` immediately suppresses a non-primary member from the timeline,
    /// and leaving the stack (a stamped `None`) puts it back.
    #[test]
    fn set_stack_membership_projects_onto_the_index_both_ways() {
        use capsule_core::domain::StackType;
        use capsule_core::sidecar::sidecar_v1::StackRole;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot").unwrap();
        let [a, b, _] = import_trio(&mut ws, album, src.path());

        let stack_id = Uuid::now_v7();
        for (id, role, index) in [(a, StackRole::Primary, 0), (b, StackRole::Member, 1)] {
            ws.set_stack_membership(
                &id,
                Some(StackMembership {
                    stack_id,
                    stack_type: StackType::Burst,
                    role,
                    member_index: Some(index),
                }),
            )
            .unwrap();
        }

        let row = ws.db().find_by_uuid(&b.to_string()).unwrap().unwrap();
        assert_eq!(row.stack_id.as_deref(), Some(stack_id.to_string().as_str()));
        assert!(
            row.is_stack_hidden,
            "a non-primary member leaves the timeline"
        );
        assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 2);

        // Leaving the stack is a *stamped* `None`, and the register stays authoritative: the
        // columns clear rather than falling back to a stale placement.
        ws.set_stack_membership(&b, None).unwrap();
        let row = ws.db().find_by_uuid(&b.to_string()).unwrap().unwrap();
        assert_eq!(row.stack_id, None);
        assert!(!row.is_stack_hidden);
        assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 3);
    }

    /// S-D13: a stack/group's cull state is *derived* from its members (all-rejected → any-pick
    /// → mixed), and flagging a collapsed stack applies the flag to each member.
    #[test]
    fn group_cull_state_derives_from_stack_members() {
        use capsule_core::culling::GroupCullState;
        use capsule_core::domain::StackType;
        use capsule_core::sidecar::sidecar_v1::{CullFlag, StackMembership, StackRole};

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Shoot").unwrap();
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

    // ── Labels on regions and stacks (#667) ─────────────────────────────────────────────

    fn face() -> RegionShape {
        RegionShape::Rect {
            x: 0.2,
            y: 0.1,
            w: 0.3,
            h: 0.4,
        }
    }

    fn burst(stack_id: Uuid) -> Option<StackMembership> {
        use capsule_core::domain::StackType;
        use capsule_core::sidecar::sidecar_v1::StackRole;
        Some(StackMembership {
            stack_id,
            stack_type: StackType::Burst,
            role: StackRole::Member,
            member_index: None,
        })
    }

    /// Regions are signed `metadata-update`s: add, label, unlabel and delete each land in the
    /// sidecar, the asset still verifies, and the regions survive a reopen.
    #[test]
    fn region_labels_round_trip_through_signed_metadata_updates() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Faces").unwrap();
        let [asset, _, _] = import_trio(&mut ws, album, src.path());

        let region = ws.add_region(&asset, face()).unwrap();
        ws.add_region_label(&asset, &region, "Alice").unwrap();
        ws.add_region_label(&asset, &region, "Bob").unwrap();
        assert_eq!(ws.remove_region_label(&asset, &region, "Bob").unwrap(), 1);
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);

        let other = ws.add_region(&asset, face()).unwrap();
        ws.delete_region(&asset, &other).unwrap();
        assert!(matches!(
            ws.add_region_label(&asset, &other, "late"),
            Err(LifecycleError::NotFound(_))
        ));
        assert!(matches!(
            ws.add_region(
                &asset,
                RegionShape::Circle {
                    cx: 2.0,
                    cy: 0.5,
                    r: 0.1
                }
            ),
            Err(LifecycleError::InvalidRegion(_))
        ));

        drop(ws);
        let ws = Workspace::open(lib.path(), b"passphrase", super::super::FAST_PARAMS).unwrap();
        let regions = ws.regions(&asset);
        assert_eq!(regions.len(), 1, "the deleted region is not live");
        assert_eq!(regions[0].0, region);
        assert_eq!(
            regions[0].1.labels.value().into_iter().collect::<Vec<_>>(),
            vec!["Alice".to_string()]
        );
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    }

    /// The open-time counter reseed counts region labels, so a reopened device never re-issues
    /// an `add_id` a region already holds.
    #[test]
    fn counter_reseed_counts_region_labels() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Faces").unwrap();
        let [asset, _, _] = import_trio(&mut ws, album, src.path());
        let region = ws.add_region(&asset, face()).unwrap();
        ws.add_region_label(&asset, &region, "Alice").unwrap();
        let issued = ws
            .asset(&asset)
            .unwrap()
            .sidecar
            .max_add_counter_for(&ws.device_id());

        drop(ws);
        let ws = Workspace::open(lib.path(), b"passphrase", super::super::FAST_PARAMS).unwrap();
        assert!(ws.counter.peek() > issued.unwrap());
    }

    /// Two replicas of one asset's regions converge whichever merges first: concurrent labels
    /// on one region both survive, and a delete on one side wins over a label on the other.
    #[test]
    fn remote_regions_converge_in_either_order() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Faces").unwrap();
        let [asset, _, _] = import_trio(&mut ws, album, src.path());
        let kept = ws.add_region(&asset, face()).unwrap();
        let doomed = ws.add_region(&asset, face()).unwrap();
        let base = ws.asset(&asset).unwrap().sidecar.regions.clone();

        // Replica A: label both regions. Replica B (built from the same base): label `kept`
        // differently and delete `doomed`.
        ws.add_region_label(&asset, &kept, "Alice").unwrap();
        ws.add_region_label(&asset, &doomed, "Carol").unwrap();
        let replica_a = ws.asset(&asset).unwrap().sidecar.regions.clone();

        let mut replica_b = base;
        let device_b = Uuid::from_u128(0xB);
        let mut kept_b = live_region(&replica_b[&kept]).unwrap().clone();
        kept_b.labels.add(
            "Bob".into(),
            capsule_core::metadata::crdt::AddId {
                device: device_b,
                counter: 0,
            },
        );
        write_region(
            replica_b.get_mut(&kept).unwrap(),
            Some(kept_b),
            now_rfc3339(),
            device_b,
        );
        write_region(
            replica_b.get_mut(&doomed).unwrap(),
            None,
            "2000-01-01T00:00:00Z".into(),
            device_b,
        );

        let mut ab = replica_a.clone();
        merge_regions(&mut ab, &replica_b);
        let mut ba = replica_b.clone();
        merge_regions(&mut ba, &replica_a);
        assert_eq!(ab, ba);

        ws.apply_remote_regions(&asset, &replica_b).unwrap();
        let live = ws.regions(&asset);
        assert_eq!(
            live.len(),
            1,
            "the delete wins even though it is stamped earlier"
        );
        assert_eq!(
            live[0].1.labels.value(),
            std::collections::BTreeSet::from(["Alice".to_string(), "Bob".to_string()])
        );
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    }

    /// A stack's labels live in its signed record, sealed under its album's AMK; every member
    /// shares that album, so a join from another album is refused.
    #[test]
    fn stack_labels_live_in_the_record_and_members_share_one_album() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Burst").unwrap();
        let elsewhere = ws.create_album("Elsewhere").unwrap();
        let [a, b, _] = import_trio(&mut ws, album, src.path());
        let p = src.path().join("outsider.jpg");
        fs::write(&p, b"\xFF\xD8\xFF an asset in another album").unwrap();
        let outsider = ws.import_asset(elsewhere, &p).unwrap();

        let stack = Uuid::now_v7();
        assert!(matches!(
            ws.add_stack_label(stack, "empty"),
            Err(LifecycleError::NotFound(_))
        ));
        ws.set_stack_membership(&a, burst(stack)).unwrap();
        ws.set_stack_membership(&b, burst(stack)).unwrap();
        assert!(matches!(
            ws.set_stack_membership(&outsider, burst(stack)),
            Err(LifecycleError::StackAlbumMismatch { stack_album, asset_album, .. })
                if stack_album == album && asset_album == elsewhere
        ));

        ws.add_stack_label(stack, "hdr").unwrap();
        ws.add_stack_label(stack, "best").unwrap();
        assert_eq!(ws.remove_stack_label(stack, "best").unwrap(), 1);
        assert_eq!(
            ws.stack_labels(stack).unwrap(),
            std::collections::BTreeSet::from(["hdr".to_string()])
        );
        let doc = ws.stack_doc(stack).unwrap();
        assert_eq!(doc.album_id, album);
        assert!(doc.verify(&ws.user_ik_public()));

        // The sealed record opens under the album's key and merges idempotently.
        let wire = ws.seal_stack_doc(stack).unwrap();
        ws.apply_remote_stack_doc(stack, album, &wire).unwrap();
        assert_eq!(
            ws.stack_labels(stack).unwrap(),
            std::collections::BTreeSet::from(["hdr".to_string()])
        );
        assert!(ws.apply_remote_stack_doc(stack, elsewhere, &wire).is_err());
    }

    /// A stack in `album` holding one member, labelled `hdr`.
    fn labelled_stack(ws: &mut Workspace, album: Uuid, src: &Path) -> Uuid {
        let [member, _, _] = import_trio(ws, album, src);
        let stack = Uuid::now_v7();
        ws.set_stack_membership(&member, burst(stack)).unwrap();
        ws.add_stack_label(stack, "hdr").unwrap();
        stack
    }

    /// A remote stack record that opens under the album's AMK is still refused when another
    /// account's identity key signed it, or when it places the stack in another album; nothing
    /// is merged.
    #[test]
    fn a_remote_stack_record_another_account_signed_is_refused() {
        use capsule_core::crypto::keys::HybridSigningKey;
        use capsule_core::metadata::crdt::AddId;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Burst").unwrap();
        let elsewhere = ws.create_album("Elsewhere").unwrap();
        let stack = labelled_stack(&mut ws, album, src.path());
        let keys = ws.album(&album).unwrap();
        let amk = Amk::from_bytes(keys.amks[&keys.current_epoch]);
        let forge = |album_id: Uuid, signer: &HybridSigningKey| {
            let mut doc = StackDocV1::new(stack, album_id);
            doc.labels.add(
                "forged".into(),
                AddId {
                    device: Uuid::from_u128(0xC),
                    counter: 0,
                },
            );
            doc.sign(signer);
            doc.seal(&amk).unwrap()
        };

        let foreign = forge(album, &HybridSigningKey::generate());
        assert!(matches!(
            ws.apply_remote_stack_doc(stack, album, &foreign),
            Err(LifecycleError::UnverifiedDocument(id)) if id == stack
        ));
        let misplaced = forge(elsewhere, &ws.account.user_ik);
        assert!(matches!(
            ws.apply_remote_stack_doc(stack, album, &misplaced),
            Err(LifecycleError::UnverifiedDocument(id)) if id == stack
        ));
        assert_eq!(
            ws.stack_labels(stack).unwrap(),
            std::collections::BTreeSet::from(["hdr".to_string()])
        );
    }

    /// A persisted stack record that was altered after signing, or filed under another stack's
    /// id, is refused when read rather than trusted.
    #[test]
    fn a_tampered_persisted_stack_record_is_refused() {
        use capsule_core::metadata::crdt::AddId;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Burst").unwrap();
        let stack = labelled_stack(&mut ws, album, src.path());
        let path = stack_doc_path(lib.path(), &stack);

        let mut doc = ws.stack_doc(stack).unwrap();
        doc.labels.add(
            "injected".into(),
            AddId {
                device: Uuid::from_u128(0xC),
                counter: 0,
            },
        );
        fs::write(&path, doc.to_canonical_vec()).unwrap();
        assert!(matches!(
            ws.stack_doc(stack),
            Err(LifecycleError::UnverifiedDocument(id)) if id == stack
        ));

        let mut other = StackDocV1::new(Uuid::now_v7(), album);
        other.sign(&ws.account.user_ik);
        fs::write(&path, other.to_canonical_vec()).unwrap();
        assert!(matches!(
            ws.stack_doc(stack),
            Err(LifecycleError::UnverifiedDocument(id)) if id == stack
        ));

        fs::write(&path, b"not a record").unwrap();
        assert!(matches!(
            ws.stack_doc(stack),
            Err(LifecycleError::Document(_))
        ));
    }
}
