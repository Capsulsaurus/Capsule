//! Aggregated federated albums (`S-E4`) — album-group assertions, their AMK-sealed op path,
//! and the local constituent view. The only `lifecycle` file that reaches [`crate::federation`].

use uuid::Uuid;

use super::{LifecycleError, Result, Workspace, asset_is_deleted, now_rfc3339};
use crate::cbor;
use crate::crypto::encryption::{blob_nonce, open_blob, seal_metadata_blob};
use crate::crypto::keys::Amk;
use crate::federation::{
    AggregateAsset, AlbumGroupAssertion, AlbumGroupInvite, Constituent, MemberHint, mint_group_id,
};

impl Workspace {
    // ── Aggregated federated albums (S-E4) ──────────────────────────────────────
    //
    // An album-group assertion is written into *this* workspace's own container album's
    // collaborative-metadata stream — a device-signed operation, AMK-sealed so no server ever
    // learns a group exists (zero new server surface). Reading the *other* constituents rides
    // the existing per-album feed/pull (S-D2/S-E2); the aggregate itself is a computed view.
    // SSoT: [Federation — Federated Shared Albums].
    //
    // [Federation — Federated Shared Albums]: https://docs/design/federation/#federated-shared-albums-aggregated-albums

    /// Create an aggregated album by minting a fresh `group_id` and writing this workspace's own
    /// `album_id` constituent's [`AlbumGroupAssertion`] (device-signed, stamped for LWW
    /// convergence). Returns the [`AlbumGroupInvite`] the creator hands to contributors — the
    /// `group_id` plus the creator's own constituent as a sibling hint. The interactive
    /// membership ceremony is `S-X2`'s; this produces the invite *data*. `home_server` is this
    /// constituent's origin identity.
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
    /// constituent), device-signed. Rides the existing album invite as data (`S-X2` owns the
    /// membership ceremony). `home_server` is this constituent's origin.
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
mod tests {
    use tempfile::TempDir;

    use super::super::fast_workspace;

    // ── Aggregated federated albums (S-E4) ──────────────────────────────────────

    /// The op-path write half is **server-opaque**: the sealed group-assertion operation the
    /// server would store carries no plaintext group facts — they live only inside the AMK
    /// AEAD. A member re-derives the AMK and opens it; a non-member cannot.
    #[test]
    fn album_group_assertion_op_is_amk_sealed_and_opaque() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Alice trip");

        let invite = ws
            .create_album_group(album, "Summer-Roadtrip-Secret", "alice.tld")
            .unwrap();
        let wire = ws.seal_album_group_op(&album).unwrap();

        // The server-stored ciphertext never exposes the group id or name in the clear.
        let gid_bytes = invite.group_id.as_bytes();
        assert!(
            !wire.windows(gid_bytes.len()).any(|w| w == gid_bytes),
            "group_id must not appear in the sealed op"
        );
        assert!(
            !wire
                .windows(b"Summer-Roadtrip-Secret".len())
                .any(|w| w == b"Summer-Roadtrip-Secret"),
            "group name must not appear in the sealed op"
        );

        // A member (holds the AMK) re-opens it to the exact reconciled assertion.
        let opened = ws.open_album_group_op(&album, &wire).unwrap();
        assert_eq!(opened.group_id, invite.group_id);
        assert_eq!(opened.name(), Some("Summer-Roadtrip-Secret"));
        assert!(opened.verify(&ws.user_ik_public()));

        // A non-member workspace (different account → different album keys) cannot open it: it
        // does not even hold the album, which is exactly why a stranger can never inject.
        let lib2 = TempDir::new().unwrap();
        let ws2 = fast_workspace(lib2.path());
        assert!(ws2.open_album_group_op(&album, &wire).is_err());
    }

    /// End-to-end composition + injection-refusal + partial-view + leave through the workspace:
    /// two constituents assert the same group and compose into one capture-time-ordered view; a
    /// third member album that never asserts the group is refused; an unreachable origin degrades
    /// visibly without removal; leaving removes the assertion so the constituent drops out.
    #[test]
    fn album_group_view_composes_refuses_degrades_and_leaves() {
        let alice_lib = TempDir::new().unwrap();
        let bob_lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("p.jpg");
        std::fs::write(&img, b"\xFF\xD8\xFF group photo").unwrap();

        // Alice creates the group on her constituent and imports one asset.
        let mut alice = fast_workspace(alice_lib.path());
        let a_album = alice.create_album("Alice");
        let a_other = alice.create_album("Alice-unrelated"); // a member album NOT in the group.
        alice.import_asset(a_album, &img).unwrap();
        alice.import_asset(a_other, &img).unwrap();
        let invite = alice
            .create_album_group(a_album, "Trip", "alice.tld")
            .unwrap();

        // Bob joins the group on his own constituent (the invite rides as data) and imports.
        let mut bob = fast_workspace(bob_lib.path());
        let b_album = bob.create_album("Bob");
        bob.import_asset(b_album, &img).unwrap();
        bob.join_album_group(b_album, &invite, "bob.tld").unwrap();

        // Alice reads Bob's assertion off the feed and folds it in (LWW name + hint union).
        let bob_assertion = bob.album_group_assertion(&b_album).unwrap().clone();
        assert!(alice.merge_album_group_assertion(b_album, &bob_assertion));

        // Alice renders the aggregate over her constituents + Bob's (a remote constituent she is
        // a member of). Bob's origin is momentarily unreachable → partial view.
        let a_c = alice.local_constituent(a_album, "alice.tld", true, false);
        let a_other_c = alice.local_constituent(a_other, "alice.tld", true, false); // no assertion.
        let bob_remote = crate::federation::Constituent {
            album_id: b_album,
            home_server: "bob.tld".into(),
            is_member: true, // Alice holds Bob's AMK via the album invite (S-X2 ceremony).
            assertion: Some(bob_assertion),
            reachable: false, // bob.tld is down.
            blocked: false,
            assets: vec![crate::federation::AggregateAsset {
                asset_id: bob.asset_ids()[0],
                capture_timestamp: 10,
            }],
        };
        let view = crate::federation::render_aggregate(
            invite.group_id,
            &[a_c, a_other_c, bob_remote],
            None,
        );

        // Composition: Alice's group asset + Bob's asset; the unrelated album is refused.
        assert_eq!(
            view.entries.len(),
            2,
            "only the two group constituents compose"
        );
        assert!(view.entries.iter().any(|e| e.home_server == "alice.tld"));
        assert!(view.entries.iter().any(|e| e.home_server == "bob.tld"));
        // Partial view: Bob's origin is degraded but not removed.
        assert!(view.partial);
        assert!(
            view.entries
                .iter()
                .find(|e| e.home_server == "bob.tld")
                .unwrap()
                .degraded
        );
        // Group name converges by LWW across the two assertions.
        assert_eq!(view.group_name.as_deref(), Some("Trip"));

        // Leave: Alice removes her assertion → her constituent no longer admits into the group.
        assert!(alice.leave_album_group(a_album, false).unwrap());
        assert!(alice.album_group_assertion(&a_album).is_none());
        let a_c_after = alice.local_constituent(a_album, "alice.tld", true, false);
        assert!(
            !a_c_after.admits(invite.group_id),
            "a constituent with no assertion drops out of the group"
        );
    }
}
