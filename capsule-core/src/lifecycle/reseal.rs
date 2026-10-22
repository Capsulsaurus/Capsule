//! Re-sealing a **never-published** create at the album's current epoch (issue #822).
//!
//! The server refuses a create whose manifest names an album epoch below the album's high-water
//! mark (invariant 18): an asset imported offline before a rotation, or one whose push lost the
//! race to a sibling written under the newer epoch, would otherwise be stranded on the device.
//! The refusal is final for that envelope, so the only way forward is a new one.
//!
//! [`Workspace::reseal_create`] writes it. A create the server refused was never published —
//! the refusal fires only on a row with no chain head, and writes nothing — so replacing its
//! genesis record breaks no chain anybody holds. Everything sealed under the old epoch is
//! sealed again under the current one, because a rotation is a confidentiality boundary:
//!
//! - the **original** is re-encrypted under a fresh `nonce_prefix` (the same `file_id`), so its
//!   ciphertext, content address and folded file key all change;
//! - the **metadata blob** — the same signed sidecar, whose plaintext does not name an epoch —
//!   is sealed again under a fresh nonce;
//! - the **derivatives** are regenerated, because each embeds the original's content address in
//!   its signed `original` reference and each is encrypted under the old epoch's key. They start
//!   fresh chains: the old ones were never published and must not be linked to.
//!
//! No nonce is reused: every prefix the asset has spent (the old original's and every old
//! derivative's) is excluded from the new draws, and the old metadata blob nonce is refused for
//! the new one. Nothing old stays reachable: the old derivative bundle is removed before the new
//! one is written, and the old chain and metadata blob are replaced in the same atomic renames
//! every signed write uses.
//!
//! What a re-seal cannot do is reach an epoch this device does not hold: when this device's
//! current epoch is no newer than the asset's, the refusal is [`ResealRefusal::NoNewerEpoch`]
//! and the asset stays local. That covers a refusal that had nothing to do with the epoch, and
//! one where another device rotated the album and this one has not been handed the new key.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};

use thiserror::Error;
use uuid::Uuid;

use super::derivatives::{ExistingDerivatives, PreparedStill, StillSource, existing_derivatives};
use super::import::{MAX_HELD_STILL_BYTES, Tee, read_head};
use super::{AssetState, LifecycleError, Result, Workspace, media_dir, now_rfc3339};
use crate::crypto::encryption::{
    NONCE_PREFIX_LEN, RekeyStreamError, blob_ciphertext_hash, blob_nonce,
    encrypt_asset_rekey_stream_with_prefix, seal_metadata_blob,
};
use crate::crypto::hash::Sha256Hasher;
use crate::crypto::keys::{Amk, AmkVersion};
use crate::crypto::provenance::{Action, KeyMode, ManifestCore, ProvenanceChain, ProvenanceRecord};
use crate::crypto::verify_asset::{
    MetadataBinding, VerifyOutcome, verify_asset_hashed, verify_metadata_binding,
};
use crate::crypto::{CryptoError, rng};
use crate::exif::ExifExtract;
use crate::library::load_receipts;
use crate::media::StillFormat;

/// How many candidate nonce prefixes are drawn before the draw itself is called broken. A
/// 7-byte prefix collides with a handful of spent ones at about 1 in 2^50, so running out is a
/// broken CSPRNG, not bad luck.
const MAX_PREFIX_DRAWS: usize = 8;

/// Why [`Workspace::reseal_create`] refused an asset. Each is a state in which writing a new
/// genesis record would be wrong or impossible; none of them writes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResealRefusal {
    /// The asset's provenance chain is not a lone `create`. A later record means the asset was
    /// edited, and an edit is only ever authored on top of a create this device already holds as
    /// its chain; replacing that genesis would orphan the records that follow it.
    #[error("its provenance chain is not a lone create")]
    NotALoneCreate,
    /// The library holds a custody receipt for the asset, so the server published it. A
    /// published genesis is append-only history; it is never replaced.
    #[error("the server holds a custody receipt for it, so it was published")]
    Published,
    /// The original's file key is **wrapped** (an adopted guest drop), not derived from the album
    /// key. Re-sealing it would need the key re-wrapped as well as the bytes re-encrypted, which
    /// this path does not do.
    #[error("its original's file key is wrapped, not derived from the album key")]
    WrappedFileKey,
    /// The album's current epoch on this device is not newer than the one the asset is sealed
    /// under, so there is no epoch to re-seal at. This does not say why the server refused the
    /// create: the album's epoch may not have moved at all (a revoked device, withdrawn write
    /// access), or another device rotated it and this one does not hold the new key yet.
    #[error(
        "it is sealed at album epoch {sealed} and this device's current epoch is {current}: \
         there is no newer epoch to re-seal it at"
    )]
    NoNewerEpoch {
        /// The epoch the asset's create manifest names.
        sealed: u32,
        /// The album's current epoch on this device.
        current: u32,
    },
}

/// What one re-seal did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResealOutcome {
    /// The re-sealed asset (its `file_id`, unchanged).
    pub asset_id: Uuid,
    /// The asset's album.
    pub album_id: Uuid,
    /// The epoch the refused create was sealed under.
    pub from_epoch: u32,
    /// The epoch it is sealed under now: the album's current one.
    pub to_epoch: u32,
    /// How many derivative manifests were regenerated at the new epoch (zero for an asset this
    /// build makes no derivatives for).
    pub derivatives: usize,
}

impl Workspace {
    /// Re-seal a never-published `create` at the album's current epoch, replacing its genesis
    /// record, so a create the server refused because the album's epoch moved past it can be
    /// pushed again instead of being stranded on the device.
    ///
    /// An epoch rotation is a confidentiality boundary, so everything sealed under the old epoch
    /// is sealed again under the current one: the original under a fresh `nonce_prefix`, the
    /// unchanged signed sidecar under a fresh metadata-blob nonce, and the derivatives
    /// regenerated as fresh chains. No prefix or nonce the asset has spent is drawn again, and
    /// the old derivative bundle is removed before the new one is written.
    ///
    /// The caller is a push that the server refused with an epoch regression; this re-checks
    /// every precondition locally rather than trusting it, and refuses with
    /// [`LifecycleError::ResealRefused`] when the asset is not a lone create, holds a custody
    /// receipt, has a wrapped file key, or when this device holds no newer epoch. A read-only
    /// album is [`LifecycleError::AlbumReadOnly`]. A plaintext on disk that no longer hashes to
    /// the signed sidecar is [`LifecycleError::CiphertextMismatch`].
    ///
    /// The new create self-verifies through `verify_asset` and the metadata binding before
    /// anything is written. On success the asset's next [`upload_bundle`](Self::upload_bundle)
    /// carries the new epoch, the new original ciphertext, the new metadata blob and the new
    /// derivatives, and nothing sealed under the old epoch.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id))]
    pub fn reseal_create(&mut self, asset_id: &Uuid) -> Result<ResealOutcome> {
        let refuse = |reason: ResealRefusal| {
            tracing::warn!(asset_id = %asset_id, %reason, "reseal: refused; nothing written");
            LifecycleError::ResealRefused {
                asset_id: *asset_id,
                reason,
            }
        };
        let asset = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?;
        let album_id = asset.album_id;

        // (1) Only a lone, derived-key create, and only one the server never published.
        let [genesis] = asset.chain.records() else {
            return Err(refuse(ResealRefusal::NotALoneCreate));
        };
        let old = genesis.manifest.core.clone();
        if old.action != Action::Create || old.prior_provenance_hash.is_some() {
            return Err(refuse(ResealRefusal::NotALoneCreate));
        }
        if old.key_mode != KeyMode::Derived {
            return Err(refuse(ResealRefusal::WrappedFileKey));
        }
        let receipts = load_receipts(&self.root, asset_id, Some(asset.capture_utc))
            .map_err(|e| LifecycleError::Cbor(format!("reading the receipt log: {e}")))?;
        if !receipts.is_empty() {
            return Err(refuse(ResealRefusal::Published));
        }

        // (2) Only to an epoch this device holds and that is newer than the sealed one.
        let album = self.album(&album_id)?;
        let sealed = old.amk_version.0;
        let current = album.current_epoch;
        if current <= sealed {
            return Err(refuse(ResealRefusal::NoNewerEpoch { sealed, current }));
        }
        let write_tier = album.write_tier_signer()?;
        let amk = Amk::from_bytes(album.amks[&current]);
        tracing::info!(
            album_id = %album_id,
            from_epoch = sealed,
            to_epoch = current,
            "reseal: re-sealing a never-published create at the album's current epoch"
        );

        // Every prefix this `file_id` has spent: the old original's and every old derivative's.
        let derivative_dir = media_dir(&self.root, asset.capture_utc).join("derivatives");
        let ExistingDerivatives {
            used_prefixes: mut spent,
            ..
        } = existing_derivatives(&derivative_dir, *asset_id);
        spent.insert(old.nonce_prefix);

        // (3) Re-encrypt the original under a fresh prefix, hashing the plaintext as it passes:
        // the signed sidecar commits to that hash, so bytes that changed on disk since the
        // import are refused rather than sealed under a manifest that would misdescribe them.
        let media_path = self.media_path(asset);
        let read_err =
            |e: io::Error| LifecycleError::Io(format!("read {}: {e}", media_path.display()));
        let mut source = fs::File::open(&media_path).map_err(read_err)?;
        let size = source.metadata().map_err(read_err)?.len();
        let head = read_head(&mut source).map_err(read_err)?;
        let decodable =
            StillFormat::detect(&head, &asset.ext).is_some_and(StillFormat::is_decodable);
        let held = if decodable && size <= MAX_HELD_STILL_BYTES {
            let mut whole = Vec::new();
            source.read_to_end(&mut whole).map_err(read_err)?;
            Some(whole)
        } else {
            None
        };
        let mut held_reader = held.as_deref().unwrap_or_default();
        let reader: &mut dyn Read = if held.is_some() {
            &mut held_reader
        } else {
            &mut source
        };
        let mut tee = Tee {
            inner: reader,
            hasher: Sha256Hasher::new(),
            copy: None,
        };
        let prefix = draw_unspent_prefix(&spent, rng::random_array::<NONCE_PREFIX_LEN>)?;
        let (enc, _file_key) = encrypt_asset_rekey_stream_with_prefix(
            &amk,
            asset_id,
            &mut tee,
            io::sink(),
            prefix,
            Some(old.nonce_prefix),
        )
        .map_err(|e| match e {
            RekeyStreamError::Crypto(e) => LifecycleError::Crypto(e),
            RekeyStreamError::Stream(e) => {
                LifecycleError::Io(format!("encrypt {}: {e}", media_path.display()))
            }
        })?;
        let plaintext_hash = tee.hasher.finalize();
        if plaintext_hash != asset.sidecar.hash || enc.plaintext_size != old.plaintext_size {
            tracing::error!(
                asset_id = %asset_id,
                expected_plaintext_size = old.plaintext_size,
                actual_plaintext_size = enc.plaintext_size,
                "reseal: the original on disk no longer matches its signed sidecar; refusing"
            );
            return Err(LifecycleError::CiphertextMismatch(*asset_id));
        }
        tracing::debug!(
            plaintext_size = enc.plaintext_size,
            spent_prefixes = spent.len(),
            "reseal: original re-encrypted under a fresh nonce prefix"
        );

        // (4) Seal the same signed sidecar again under the current epoch, refusing the old
        // blob's nonce for the new draw.
        let sidecar_bytes = asset.sidecar.to_canonical_vec();
        let (metadata_blob, blob_key) = seal_metadata_blob(
            &amk,
            asset_id,
            &sidecar_bytes,
            blob_nonce(&asset.metadata_blob),
        )?;
        let metadata_blob_hash = blob_ciphertext_hash(&metadata_blob);

        // (5) Regenerate the derivatives at the current epoch, as fresh chains, excluding every
        // spent prefix. Never fatal over pixels, exactly as at import.
        spent.insert(enc.nonce_prefix);
        let PreparedStill { derivatives, .. } = self.prepare_still_with(
            &StillSource {
                plaintext: held.as_deref().unwrap_or(&head),
                size,
                ext: &asset.ext,
                src: &media_path,
                exif: &ExifExtract::default(),
            },
            *asset_id,
            album_id,
            &amk,
            &enc,
            ExistingDerivatives {
                heads: HashMap::new(),
                used_prefixes: spent,
            },
        )?;
        drop(held);

        // (6) The new genesis: the old create's identity and declarations, with every field the
        // epoch decides re-minted, signed by this device under the current write-tier key.
        let core = ManifestCore {
            amk_version: AmkVersion(current),
            ciphertext_hash: enc.ciphertext_hash,
            plaintext_size: enc.plaintext_size,
            chunk_size: enc.chunk_size,
            nonce_prefix: enc.nonce_prefix,
            metadata_blob_hash: Some(metadata_blob_hash),
            created_by_user: self.account.user_id,
            created_by_device: self.account.device.device_id,
            client_version: self.client_version.clone(),
            timestamp: now_rfc3339(),
            ..old
        };
        let manifest = core.sign(self.device_signer.as_ref(), write_tier)?;
        let mut chain = ProvenanceChain::new();
        chain
            .append(ProvenanceRecord::new(*asset_id, manifest.clone(), None))
            .map_err(|e| LifecycleError::Cbor(format!("chain: {e}")))?;

        // (7) Self-check before anything is written.
        let outcome = verify_asset_hashed(
            &manifest,
            &enc.ciphertext_hash,
            &self.directory,
            self.authority(&album_id)?,
            None,
        );
        if outcome != VerifyOutcome::Accept {
            return Err(LifecycleError::SelfVerify(outcome));
        }
        let binding = verify_metadata_binding(&manifest, &metadata_blob, &blob_key, &sidecar_bytes);
        if binding != MetadataBinding::Bound {
            return Err(LifecycleError::MetadataUnbound(binding));
        }

        let resealed = AssetState {
            asset_id: *asset_id,
            album_id,
            ext: asset.ext.clone(),
            capture_utc: asset.capture_utc,
            chain,
            sidecar: asset.sidecar.clone(),
            metadata_blob,
            stack: asset.stack.clone(),
        };

        // (8) Write. The old derivative bundle goes first: once the chain names the new epoch
        // no manifest sealed under the old one may remain readable, and a bundle that cannot be
        // removed fails the re-seal before anything else moves. The new derivatives then land
        // (a failure there is logged and costs only a regenerable thumbnail), and the signed
        // artifacts last, the chain renamed after the blob, as every signed write orders them.
        let old_bundle = derivative_dir.join(format!("{}.derivatives.cbor", asset_id.simple()));
        match fs::remove_file(&old_bundle) {
            Ok(()) => tracing::debug!("reseal: old-epoch derivative bundle removed"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(LifecycleError::Io(format!(
                    "remove {}: {e}",
                    old_bundle.display()
                )));
            }
        }
        self.persist_derivatives(&resealed, &derivatives);
        self.write_signed_artifacts(&resealed)?;
        self.index_asset_row(&resealed)?;
        self.assets.insert(*asset_id, resealed);

        tracing::info!(
            album_id = %album_id,
            from_epoch = sealed,
            to_epoch = current,
            derivatives = derivatives.len(),
            "reseal: create re-sealed and self-verified at the album's current epoch"
        );
        Ok(ResealOutcome {
            asset_id: *asset_id,
            album_id,
            from_epoch: sealed,
            to_epoch: current,
            derivatives: derivatives.len(),
        })
    }
}

/// Draw a nonce prefix from `draw` that is not in `spent`, redrawing a collision.
///
/// The CSPRNG makes a collision vanishingly unlikely; the refusal is the encryption doc's
/// defense in depth on top of it, because a prefix is folded into the file-key salt and a
/// reused one would reuse the key as well as the nonce.
fn draw_unspent_prefix(
    spent: &std::collections::HashSet<[u8; NONCE_PREFIX_LEN]>,
    mut draw: impl FnMut() -> [u8; NONCE_PREFIX_LEN],
) -> Result<[u8; NONCE_PREFIX_LEN]> {
    for attempt in 0..MAX_PREFIX_DRAWS {
        let prefix = draw();
        if !spent.contains(&prefix) {
            return Ok(prefix);
        }
        tracing::warn!(attempt, "reseal: drew a spent nonce prefix; redrawing");
    }
    Err(LifecycleError::Crypto(CryptoError::NonceReuse))
}

#[cfg(test)]
mod tests;
