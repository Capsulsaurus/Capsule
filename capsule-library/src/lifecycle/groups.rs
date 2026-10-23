//! Aggregated federated albums — album-group assertions, their AMK-sealed op path,
//! and the local constituent view. The only `lifecycle` file that reaches [`capsule_core::federation`].

use capsule_core::cbor;
use capsule_core::crypto::encryption::{blob_nonce, open_blob, seal_metadata_blob};
use capsule_core::crypto::keys::Amk;
use capsule_core::federation::{
    AggregateAsset, AlbumGroupAssertion, AlbumGroupInvite, Constituent, MemberHint, mint_group_id,
};
use uuid::Uuid;

use super::{LifecycleError, Result, Workspace, asset_is_deleted, now_rfc3339};

impl Workspace {
    // ── Aggregated federated albums ──────────────────────────────────────
    //
    // An album-group assertion is written into *this* workspace's own container album's
    // collaborative-metadata stream — a device-signed operation, AMK-sealed so no server ever
    // learns a group exists (zero new server surface). Reading the *other* constituents rides
    // the existing per-album feed/pull; the aggregate itself is a computed view.
    // SSoT: [Federation — Federated Shared Albums].
    //
    // [Federation — Federated Shared Albums]: https://docs/design/federation/#federated-shared-albums-aggregated-albums

    /// Create an aggregated album by minting a fresh `group_id` and writing this workspace's own
    /// `album_id` constituent's [`AlbumGroupAssertion`] (device-signed, stamped for LWW
    /// convergence). Returns the [`AlbumGroupInvite`] the creator hands to contributors — the
    /// `group_id` plus the creator's own constituent as a sibling hint. The interactive
    /// membership ceremony is not performed here; this produces the invite *data*.
    /// `home_server` is this constituent's origin identity.
    #[tracing::instrument(skip(self), fields(album_id = %album_id))]
    pub fn create_album_group(
        &mut self,
        album_id: Uuid,
        group_name: &str,
        home_server: &str,
    ) -> Result<AlbumGroupInvite> {
        self.album(&album_id)?; // must be an album this workspace holds keys for.
        let group_id = mint_group_id();
        let mut assertion = AlbumGroupAssertion::new(
            group_id,
            group_name,
            now_rfc3339(),
            self.account.device.device_id,
            vec![MemberHint {
                album_id,
                home_server: home_server.to_string(),
            }],
        );
        assertion.sign(&self.account.user_ik);
        self.group_assertions.insert(album_id, assertion);
        tracing::info!(%group_id, "album-group: created");
        Ok(AlbumGroupInvite {
            group_id,
            group_name: group_name.to_string(),
            siblings: vec![MemberHint {
                album_id,
                home_server: home_server.to_string(),
            }],
        })
    }

    /// Join an existing group as a contributor: write this workspace's own `album_id` constituent's
    /// assertion (asserting the invite's `group_id`, seeded with the invite's siblings + this
    /// constituent), device-signed. Rides the existing album invite as data; the
    /// membership ceremony itself is not performed here. `home_server` is this constituent's
    /// origin.
    #[tracing::instrument(skip(self, invite), fields(album_id = %album_id, group_id = %invite.group_id))]
    pub fn join_album_group(
        &mut self,
        album_id: Uuid,
        invite: &AlbumGroupInvite,
        home_server: &str,
    ) -> Result<()> {
        self.album(&album_id)?;
        let mut assertion = invite.to_assertion(
            album_id,
            home_server,
            now_rfc3339(),
            self.account.device.device_id,
        );
        assertion.sign(&self.account.user_ik);
        self.group_assertions.insert(album_id, assertion);
        tracing::info!("album-group: joined");
        Ok(())
    }

    /// LWW-rename the group on this workspace's own `album_id` constituent (a fresh stamped write),
    /// re-signing the assertion. Concurrent renames on sibling constituents converge by LWW.
    pub fn rename_album_group(&mut self, album_id: Uuid, new_name: &str) -> Result<()> {
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        let assertion = self.group_assertions.get_mut(&album_id).ok_or_else(|| {
            LifecycleError::NotFound(format!("group assertion for album {album_id}"))
        })?;
        assertion.rename(new_name, ts, device);
        let assertion = self
            .group_assertions
            .get_mut(&album_id)
            .expect("assertion present above");
        assertion.sign(&self.account.user_ik);
        Ok(())
    }

    /// Fold a peer constituent's assertion (read from the feed for `their_album_id`) into the
    /// reconciled local view: LWW-converges the group name and unions the advisory hints. If the
    /// local user already holds a reconciled assertion for that album it merges in place; otherwise
    /// the peer assertion is adopted verbatim. A peer assertion for a *different* group than an
    /// existing local one is refused (no cross-talk). Returns whether it was applied.
    pub fn merge_album_group_assertion(
        &mut self,
        their_album_id: Uuid,
        remote: &AlbumGroupAssertion,
    ) -> bool {
        if let Some(local) = self.group_assertions.get_mut(&their_album_id) {
            local.merge(remote)
        } else {
            self.group_assertions.insert(their_album_id, remote.clone());
            true
        }
    }

    /// Leave the group from this workspace's `album_id` constituent: **remove the assertion**, so
    /// the constituent drops out of every participant's aggregate on their next sync. Optionally
    /// `unshare` (bump the AMK epoch + rotate the write tier) to cut read access to the historical
    /// photos too — the honest limitation being that this only stops others from seeing *your*
    /// photos, never removes anyone else's constituent (a true group kick is the v2 problem).
    /// Returns whether an assertion was present to remove.
    #[tracing::instrument(skip(self), fields(album_id = %album_id, unshare))]
    pub fn leave_album_group(&mut self, album_id: Uuid, unshare: bool) -> Result<bool> {
        let removed = self.group_assertions.remove(&album_id).is_some();
        if unshare {
            self.rotate_epoch(album_id)?;
        }
        tracing::info!(removed, "album-group: left");
        Ok(removed)
    }

    /// The reconciled group assertion for `album_id`, if the workspace holds one.
    pub fn album_group_assertion(&self, album_id: &Uuid) -> Option<&AlbumGroupAssertion> {
        self.group_assertions.get(album_id)
    }

    /// Seal `album_id`'s current group assertion into the AMK-sealed operation the op path carries
    /// — the wire bytes a server stores. AEAD-encrypted under the album's current-epoch AMK, so the
    /// server (and any non-member) learns nothing of the group: the plaintext `group_id`/name never
    /// appears in these bytes. This is the write half of the op path.
    pub fn seal_album_group_op(&self, album_id: &Uuid) -> Result<Vec<u8>> {
        let assertion = self.group_assertions.get(album_id).ok_or_else(|| {
            LifecycleError::NotFound(format!("group assertion for album {album_id}"))
        })?;
        let album = self.album(album_id)?;
        let amk = Amk::from_bytes(album.amks[&album.current_epoch]);
        let plaintext =
            cbor::to_canonical_vec(assertion).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        let (wire, _key) = seal_metadata_blob(&amk, album_id, &plaintext, None)?;
        Ok(wire)
    }

    /// Open an AMK-sealed group-assertion operation for `album_id` — the read half. Requires the
    /// album's AMK: a non-member (no AMK) physically cannot decrypt, which is exactly why a
    /// stranger's assertion can never inject itself into the view. Restricted to the current epoch.
    pub fn open_album_group_op(&self, album_id: &Uuid, wire: &[u8]) -> Result<AlbumGroupAssertion> {
        let album = self.album(album_id)?;
        let amk = Amk::from_bytes(album.amks[&album.current_epoch]);
        let nonce = blob_nonce(wire)
            .ok_or_else(|| LifecycleError::Cbor("group-op wire too short".into()))?;
        let blob_key = amk.derive_blob_key(album_id, &nonce);
        let plaintext = open_blob(&blob_key, wire)?;
        cbor::from_slice(&plaintext).map_err(|e| LifecycleError::Cbor(e.to_string()))
    }

    /// Build the [`Constituent`] view of one of this workspace's own albums for the aggregate
    /// renderer: it is a member (holds the AMK), carries its reconciled assertion (if any), and
    /// contributes its non-trashed assets keyed by capture time. `reachable` / `blocked` are the
    /// per-origin flags the caller supplies (own albums are normally reachable + unblocked).
    pub fn local_constituent(
        &self,
        album_id: Uuid,
        home_server: &str,
        reachable: bool,
        blocked: bool,
    ) -> Constituent {
        let assets = self
            .assets
            .values()
            .filter(|a| a.album_id == album_id && !asset_is_deleted(a))
            .map(|a| AggregateAsset {
                asset_id: a.asset_id,
                capture_timestamp: a.capture_utc,
            })
            .collect();
        Constituent {
            album_id,
            home_server: home_server.to_string(),
            is_member: true,
            assertion: self.group_assertions.get(&album_id).cloned(),
            reachable,
            blocked,
            assets,
        }
    }
}

#[cfg(test)]
mod tests;
