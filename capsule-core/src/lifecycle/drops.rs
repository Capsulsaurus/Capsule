//! Guest (web-upload) drops: issuing upload links, staging sealed drops, and adopting them
//! in place. The only `lifecycle` file that reaches [`crate::drop`].

use std::collections::BTreeMap;

use uuid::Uuid;

use super::{Workspace, now_rfc3339};
use crate::crypto::encryption::keywrap::seal_file_key;
use crate::crypto::encryption::{blob_ciphertext_hash, seal_metadata_blob, stream};
use crate::crypto::hash;
use crate::crypto::keys::{Amk, AmkVersion, DekKeypair};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::AssetManifest;
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::manifest::{
    ASSET_MANIFEST_VERSION, KeyMode, ManifestCore, WrappedFileKey,
};
use crate::crypto::verify_asset::{
    MetadataBinding, VerifyOutcome, verify_asset, verify_metadata_binding,
};
// The `UploadLinkIssuer` / `DropAdopter` traits are referenced by full path in their impl
// headers below and pulled into scope locally at their (UFCS) call sites — keeping them out
// of module scope avoids a `create_link`/`revoke_link` name clash with `ShareLinkIssuer`.
use crate::drop::{
    DropDescriptor, DropError, DropId, LinkCaps, PassphraseVerifier, PendingDrop, SealedDrop,
    UploadLink, UploadLinkId, generate_opaque_id as generate_drop_id, open_drop_key,
};
use crate::metadata::crdt::Lww;
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

/// The issuer-held state for one live upload link. The Drop Key private half is **escrowed**
/// (sealed under the account master key) so it never sits in the clear at rest, mirroring
/// how the account file seals device keys; the public half is returned to the caller for the
/// URL fragment and never persisted server-side.
pub(super) struct IssuedLink {
    /// The random 128-bit opaque URL-path id.
    opaque_id: [u8; 16],
    /// The per-link caps the serve path (S-C5) enforces at drop-session creation.
    caps: LinkCaps,
    /// The Drop Key public half (KEM encapsulation key; travels in the URL fragment only).
    drop_pubkey: Vec<u8>,
    /// The Drop Key private half (X-Wing 32-byte seed) sealed under the account master key.
    escrowed_drop_seed: Vec<u8>,
    /// Optional Argon2id write-abuse-gate verifier.
    passphrase: Option<PassphraseVerifier>,
    /// RFC 3339 issuance time.
    created_at: String,
    /// Set once revoked; the serve path then refuses the link.
    revoked_at: Option<String>,
}

/// One pending drop staged in the owner's inbox: the guest's unsigned descriptor, the sealed
/// ciphertext (the server-held blob, referenced in place on adoption), and the link it
/// arrived through (so adoption can unwrap the right Drop Key).
pub(super) struct InboxEntry {
    descriptor: DropDescriptor,
    ciphertext: Vec<u8>,
    via_link: UploadLinkId,
    received_at: String,
}

impl Workspace {
    /// The public (fragment) half of an issued upload link, if this workspace issued it.
    /// Exposed for tests and clients rebuilding the `.../u/{opaque-id}#{drop_pubkey}` URL.
    pub fn upload_link_pubkey(&self, link: &UploadLinkId) -> Option<&[u8]> {
        self.upload_links
            .get(link)
            .map(|l| l.drop_pubkey.as_slice())
    }

    /// The opaque URL-path id of an issued upload link (the `.../u/{opaque-id}` component).
    pub fn upload_link_opaque_id(&self, link: &UploadLinkId) -> Option<[u8; 16]> {
        self.upload_links.get(link).map(|l| l.opaque_id)
    }

    /// The optional Argon2id write-abuse verifier of an issued link — the record the serve
    /// path (S-C5) consults to gate a drop-session on passphrase possession.
    pub fn upload_link_passphrase(&self, link: &UploadLinkId) -> Option<&PassphraseVerifier> {
        self.upload_links
            .get(link)
            .and_then(|l| l.passphrase.as_ref())
    }

    /// RFC 3339 issuance time of an issued upload link.
    pub fn upload_link_created_at(&self, link: &UploadLinkId) -> Option<&str> {
        self.upload_links.get(link).map(|l| l.created_at.as_str())
    }

    /// Whether an issued upload link is still live at `now` (not revoked, not past expiry).
    pub fn upload_link_is_live(&self, link: &UploadLinkId, now: jiff::Timestamp) -> bool {
        self.upload_links.get(link).is_some_and(|l| {
            l.revoked_at.is_none()
                && l.caps
                    .expires_at
                    .as_ref()
                    .is_none_or(|ts| ts.parse::<jiff::Timestamp>().is_ok_and(|exp| now < exp))
        })
    }

    /// Stage a guest's [`SealedDrop`] into this owner's inbox, modeling the server's stage
    /// step (S-C5) so the offline core can drive the full seal → stage → adopt path. Refuses
    /// a drop through an unknown or revoked link (a fail-closed stand-in for the serve
    /// path's cap/quota checks). Returns the new inbox drop id.
    pub fn receive_drop(
        &mut self,
        via_link: UploadLinkId,
        sealed: SealedDrop,
    ) -> std::result::Result<DropId, DropError> {
        let link = self
            .upload_links
            .get(&via_link)
            .ok_or(DropError::NotFound)?;
        if link.revoked_at.is_some() {
            return Err(DropError::LinkRefused("link revoked"));
        }
        let drop_id = DropId(Uuid::now_v7());
        self.inbox.insert(
            drop_id,
            InboxEntry {
                descriptor: sealed.descriptor,
                ciphertext: sealed.ciphertext,
                via_link,
                received_at: now_rfc3339(),
            },
        );
        Ok(drop_id)
    }

    /// Recover the Drop Key keypair for an issued link by unsealing its escrowed seed.
    fn drop_keypair(&self, link: &UploadLinkId) -> std::result::Result<DekKeypair, DropError> {
        let issued = self.upload_links.get(link).ok_or(DropError::NotFound)?;
        let seed = self
            .account
            .master
            .open(&issued.escrowed_drop_seed)
            .map_err(|_| DropError::Crypto("drop key escrow unseal failed"))?;
        let seed: [u8; 32] = seed
            .as_slice()
            .try_into()
            .map_err(|_| DropError::Crypto("drop key seed wrong length"))?;
        Ok(DekKeypair::from_seed(&seed))
    }
}

impl crate::drop::UploadLinkIssuer for Workspace {
    /// Provision an upload link: mint a fresh [Drop Key](crate::crypto::keys::DekKeypair)
    /// (X-Wing KEM), escrow its private half under the account master key (the OGK re-wrap
    /// that lets *any* enrolled device decapsulate rides the multi-device escrow seam),
    /// draw a 128-bit opaque id, derive the optional Argon2id write-abuse verifier, and
    /// record the link for revocation. The public half is returned for the URL fragment and
    /// never persisted server-side. SSoT: [Web Upload].
    ///
    /// [Web Upload]: https://docs/design/web-upload/
    fn create_link(
        &mut self,
        caps: LinkCaps,
        passphrase: Option<&str>,
    ) -> std::result::Result<UploadLink, DropError> {
        let drop_key = DekKeypair::generate();
        let drop_pubkey = drop_key.public_bytes();
        let escrowed_drop_seed = self.account.master.seal(&drop_key.to_seed_bytes());
        let opaque_id = generate_drop_id();
        let passphrase = passphrase
            .map(|pw| PassphraseVerifier::derive(pw, self.argon2_params))
            .transpose()?;
        let link_id = UploadLinkId(Uuid::now_v7());
        self.upload_links.insert(
            link_id,
            IssuedLink {
                opaque_id,
                caps: caps.clone(),
                drop_pubkey: drop_pubkey.clone(),
                escrowed_drop_seed,
                passphrase,
                created_at: now_rfc3339(),
                revoked_at: None,
            },
        );
        Ok(UploadLink {
            link_id,
            opaque_id,
            drop_pubkey,
            caps,
        })
    }

    /// Revoke a link; the serve path refuses it within its fail-closed cache window.
    /// Revoking an unknown link is [`DropError::NotFound`]; re-revocation keeps the original
    /// timestamp (idempotent).
    fn revoke_link(&mut self, link: UploadLinkId) -> std::result::Result<(), DropError> {
        let issued = self
            .upload_links
            .get_mut(&link)
            .ok_or(DropError::NotFound)?;
        if issued.revoked_at.is_none() {
            issued.revoked_at = Some(now_rfc3339());
        }
        Ok(())
    }
}

impl crate::drop::DropAdopter for Workspace {
    /// This owner's pending drops, newest-arrival order unspecified.
    fn list_inbox(&self) -> std::result::Result<Vec<PendingDrop>, DropError> {
        Ok(self
            .inbox
            .iter()
            .map(|(drop_id, e)| PendingDrop {
                drop_id: *drop_id,
                descriptor: e.descriptor.clone(),
                via_link: e.via_link,
                received_at: e.received_at.clone(),
            })
            .collect())
    }

    /// Adopt a pending drop into `album_id` **in place** (no byte re-upload): decapsulate the
    /// guest-chosen `K`, rewrap it under the album AMK (`asset-keywrap/v1`), author + sign a
    /// `create` manifest with `key_mode = wrapped` whose `ciphertext_hash` references the
    /// staged blob, self-verify through [`verify_asset`], and drop the inbox row. Returns the
    /// signed adopting manifest. SSoT: [Web Upload] — Review and adopt-in-place.
    ///
    /// [Web Upload]: https://docs/design/web-upload/
    fn adopt(
        &mut self,
        drop: DropId,
        album_id: Uuid,
    ) -> std::result::Result<AssetManifest, DropError> {
        let entry = self.inbox.get(&drop).ok_or(DropError::NotFound)?;
        let descriptor = entry.descriptor.clone();
        let ciphertext = entry.ciphertext.clone();
        let via_link = entry.via_link;

        // Decapsulate the guest-chosen K with the link's escrowed Drop Key private half.
        let drop_key = self.drop_keypair(&via_link)?;
        let file_key = open_drop_key(&drop_key, &descriptor.kem_ct)?;

        let album = self
            .album(&album_id)
            .map_err(|_| DropError::LinkRefused("destination album not found"))?;
        let epoch = album.current_epoch;
        let amk = Amk::from_bytes(album.amks[&epoch]);
        let file_id = Uuid::now_v7();

        // The adopter can decrypt the staged bytes (it now holds K) to author the sidecar's
        // plaintext hash and preview; the bulk ciphertext itself never moves.
        let plaintext = stream::decrypt_asset_vec(&file_key, &descriptor.nonce_prefix, &ciphertext)
            .map_err(|_| DropError::Crypto("staged drop ciphertext failed to decrypt"))?;

        // Author + sign the sidecar. `created_by`/author is the adopter — the guest is never
        // a signer; its origin is an unverified, self-asserted descriptive note.
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: file_id,
            hash: hash::hash_bytes(&plaintext),
            capture_timestamp: now_rfc3339(),
            import_timestamp: now_rfc3339(),
            content_type: descriptor.content_type.clone(),
            dimensions: None,
            lqip: None,
            tags_user: Default::default(),
            tags_ai: Default::default(),
            caption: Default::default(),
            rating: Default::default(),
            stack_membership: Lww::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: self.account.device.device_id,
            session_id: Uuid::now_v7(),
            gps: None,
            provenance_chain_hash: None,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);

        // Seal the sidecar into the metadata blob (fresh nonce folded into the blob key; the
        // adoption is a create, so there is no prior nonce to refuse).
        let (metadata_blob, blob_key) =
            seal_metadata_blob(&amk, &file_id, &sidecar.to_canonical_vec(), None)
                .map_err(|_| DropError::Crypto("adopted metadata blob seal failed"))?;
        let metadata_blob_hash = blob_ciphertext_hash(&metadata_blob);

        // Rewrap K under the album AMK (`asset-keywrap/v1`) — K is *carried* wrapped, not
        // derived, because an external party chose it.
        let wrapped_file_key = seal_file_key(&amk, &file_id, &file_key);

        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id,
            album_id,
            amk_version: AmkVersion(epoch),
            ciphertext_hash: descriptor.ciphertext_hash,
            plaintext_size: descriptor.plaintext_size,
            chunk_size: descriptor.chunk_size,
            nonce_prefix: descriptor.nonce_prefix,
            key_mode: KeyMode::Wrapped,
            wrapped_file_key: Some(WrappedFileKey(wrapped_file_key)),
            metadata_blob_hash: Some(metadata_blob_hash),
            created_by_user: self.account.user_id,
            created_by_device: self.account.device.device_id,
            client_version: self.client_version.clone(),
            timestamp: now_rfc3339(),
            action: Action::Create,
            prior_provenance_hash: None,
            upgraded_from: None,
            retention_until: None,
        };
        let manifest = core
            .sign(
                self.device_signer.as_ref(),
                album
                    .write_tier_signer()
                    .map_err(|_| DropError::Crypto("album holds no write capability"))?,
            )
            .map_err(|_| DropError::Crypto("adopting manifest signing failed"))?;

        // Self-verify through the one chokepoint against the unchanged staged ciphertext, and
        // confirm the sealed metadata blob binds to the signed sidecar, before committing.
        let authority = self
            .authority(&album_id)
            .map_err(|_| DropError::Crypto("album has no attested authority"))?;
        if verify_asset(&manifest, &ciphertext, &self.directory, authority, None)
            != VerifyOutcome::Accept
        {
            return Err(DropError::Crypto(
                "adopting manifest failed self-verification",
            ));
        }
        if verify_metadata_binding(
            &manifest,
            &metadata_blob,
            &blob_key,
            &sidecar.to_canonical_vec(),
        ) != MetadataBinding::Bound
        {
            return Err(DropError::Crypto(
                "adopted metadata blob failed its binding",
            ));
        }

        // The drop is now a library asset — remove the inbox row (atomic on the server side).
        self.inbox.remove(&drop);
        Ok(manifest)
    }

    /// Discard a pending drop the owner rejects; its bytes are GC'd and the quota freed.
    fn discard(&mut self, drop: DropId) -> std::result::Result<(), DropError> {
        self.inbox.remove(&drop).ok_or(DropError::NotFound)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;

    /// S-A6: the full guest-drop path through the `Workspace` — issue an upload link, seal a
    /// drop to its Drop Key (the WASM/browser half), stage it, and adopt it into an album.
    /// The adopting `create` manifest is `key_mode = wrapped`, self-verifies, and a second
    /// member (holding the same AMK) unwraps `wrapped_file_key` and decrypts the unchanged
    /// ciphertext. Also covers revocation, the passphrase verifier, and discard.
    #[test]
    fn upload_link_seal_stage_and_adopt() {
        use crate::crypto::encryption::keywrap::unseal_file_key;
        use crate::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Guest contributions").unwrap();

        // Provision an upload link with a passphrase abuse gate.
        let caps = LinkCaps {
            max_file_count: Some(10),
            single_use: false,
            ..Default::default()
        };
        let link = UploadLinkIssuer::create_link(&mut ws, caps, Some("open sesame")).unwrap();
        assert_eq!(link.drop_pubkey.len(), crate::crypto::keys::DEK_PUBLIC_LEN);
        assert_eq!(
            ws.upload_link_opaque_id(&link.link_id).unwrap(),
            link.opaque_id
        );
        assert!(ws.upload_link_created_at(&link.link_id).is_some());
        assert!(
            ws.upload_link_passphrase(&link.link_id)
                .unwrap()
                .verify("open sesame")
        );
        assert!(ws.upload_link_is_live(&link.link_id, Timestamp::now()));

        // A guest seals an asset to the link's Drop Key public half (no account, no album key).
        let plaintext = b"\xFF\xD8\xFF a guest-uploaded photo, sealed in the browser";
        let sealed = seal_drop(plaintext, &link.drop_pubkey, "image/jpeg").unwrap();
        let ciphertext = sealed.ciphertext.clone();

        // The server stages it into the owner's inbox; the owner sees it awaiting review.
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
        assert_eq!(DropAdopter::list_inbox(&ws).unwrap().len(), 1);

        // The owner adopts it into the album on their trusted device.
        let manifest = DropAdopter::adopt(&mut ws, drop_id, album).unwrap();
        assert_eq!(manifest.core.key_mode, KeyMode::Wrapped);
        assert!(manifest.core.wrapped_file_key.is_some());
        assert_eq!(manifest.core.album_id, album);
        // The bytes stayed put: the manifest references the drop's ciphertext hash unchanged.
        assert_eq!(manifest.core.ciphertext_hash, hash::hash_bytes(&ciphertext));
        // Adoption emptied the inbox.
        assert!(DropAdopter::list_inbox(&ws).unwrap().is_empty());

        // A second album member unwraps K under the AMK and decrypts the unchanged ciphertext.
        let epoch = manifest.core.amk_version.0;
        let amk = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch]);
        let k = unseal_file_key(
            &amk,
            &manifest.core.file_id,
            &manifest.core.wrapped_file_key.as_ref().unwrap().0,
        )
        .unwrap();
        assert_eq!(
            stream::decrypt_asset_vec(&k, &manifest.core.nonce_prefix, &ciphertext).unwrap(),
            plaintext,
        );

        // Revocation makes the link non-live and refuses further drops.
        UploadLinkIssuer::revoke_link(&mut ws, link.link_id).unwrap();
        assert!(!ws.upload_link_is_live(&link.link_id, Timestamp::now()));
        let sealed2 = seal_drop(b"late drop", &link.drop_pubkey, "image/jpeg").unwrap();
        assert!(matches!(
            ws.receive_drop(link.link_id, sealed2),
            Err(DropError::LinkRefused(_))
        ));
        // Revoking an unknown link is NotFound; re-revocation is idempotent.
        assert!(matches!(
            UploadLinkIssuer::revoke_link(&mut ws, UploadLinkId(Uuid::now_v7())),
            Err(DropError::NotFound)
        ));
        UploadLinkIssuer::revoke_link(&mut ws, link.link_id).unwrap();
    }

    /// S-A6: a rejected drop is discarded — removed from the inbox with no library trace.
    #[test]
    fn discarded_drop_leaves_no_trace() {
        use crate::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let link = UploadLinkIssuer::create_link(&mut ws, LinkCaps::default(), None).unwrap();
        let sealed = seal_drop(b"unwanted", &link.drop_pubkey, "image/jpeg").unwrap();
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();

        assert_eq!(DropAdopter::list_inbox(&ws).unwrap().len(), 1);
        DropAdopter::discard(&mut ws, drop_id).unwrap();
        assert!(DropAdopter::list_inbox(&ws).unwrap().is_empty());
        // Discarding again (or an unknown drop) is NotFound.
        assert!(matches!(
            DropAdopter::discard(&mut ws, drop_id),
            Err(DropError::NotFound)
        ));
    }
}
