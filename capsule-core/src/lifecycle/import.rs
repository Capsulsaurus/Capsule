//! The signed import path: EXIF scan, encrypt, sign the create manifest, seal the sidecar,
//! self-verify, and write through to the queryable index.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use jiff::Timestamp;
use uuid::Uuid;

use super::{
    AssetState, LifecycleError, Result, SidecarEnrichment, SignedImport, SignedImportOptions,
    StackPlacement, StreamedImport, Workspace, asset_is_deleted, media_dir, now_rfc3339,
};
use crate::cbor;
use crate::crypto::encryption::{blob_ciphertext_hash, encrypt_asset_rekey, seal_metadata_blob};
use crate::crypto::hash;
use crate::crypto::keys::{Amk, AmkVersion};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use crate::crypto::provenance::{ProvenanceChain, ProvenanceRecord};
use crate::crypto::verify_asset::{
    MetadataBinding, VerifyOutcome, verify_asset, verify_metadata_binding,
};
use crate::db::{AssetRow, CachedRepresentationRow};
use crate::exif::extract::extract_exif;
use crate::exif::timezone::resolve_timezone;
use crate::metadata::crdt::{Lww, OrSet};
#[cfg(not(feature = "media"))]
use crate::sidecar::sidecar_v1::Dimensions;
use crate::sidecar::sidecar_v1::{
    Gps, GpsSource, SIDECAR_SCHEMA_V1, SidecarV1, StackMembership, StackRole,
};

/// Render a Unix-second capture time as the sidecar's RFC 3339 `capture_timestamp`.
fn capture_rfc3339(secs: i64) -> String {
    Timestamp::from_second(secs)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_string()
}

/// The sidecar `stack_membership` register an import starts life with (`S-B15`).
///
/// A standalone import returns the **never-written** register, which is wire-absent — the
/// absent-key discipline every signed-struct field carries, and what keeps an unstacked
/// asset's sidecar bytes identical to a pre-`S-B15` one. A stacked member returns the register
/// with `membership` stamped `(now, device_id)`.
fn stack_membership_register(
    membership: Option<StackMembership>,
    device_id: Uuid,
) -> Lww<Option<StackMembership>> {
    let mut register = Lww::new();
    if membership.is_some() {
        register.set(membership, now_rfc3339(), device_id);
    }
    register
}

/// Which side the sidecar's `capture_timestamp` was taken from — logged so an import decision
/// can be reconstructed after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureSource {
    /// This file's own EXIF, resolved to a UTC instant at the write site.
    Embedded,
    /// The adapter's folded value (`S-B10`): an EXIF time the write site could not resolve on
    /// its own, else the exporter's taken-time. Which of the two is recorded by the executor's
    /// `taken_time_source` log line for the same file.
    Folded,
    /// Neither side carried a capture time — the import's own clock.
    ImportTime,
}

impl CaptureSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded-exif",
            Self::Folded => "adapter-fold",
            Self::ImportTime => "import-time",
        }
    }
}

/// Capture-time [precedence] at the write site: the file's own embedded EXIF wins; the
/// adapter's folded value is the fallback; the import clock is the last resort.
///
/// [precedence]: https://docs/design/import/pipeline/#third-party-importers
fn folded_capture(embedded_utc: Option<i64>, folded: Option<Timestamp>) -> (i64, CaptureSource) {
    match (embedded_utc, folded) {
        (Some(secs), _) => (secs, CaptureSource::Embedded),
        (None, Some(t)) => (t.as_second(), CaptureSource::Folded),
        (None, None) => (Timestamp::now().as_second(), CaptureSource::ImportTime),
    }
}

/// GPS [precedence](folded_capture) at the write site: this file's own EXIF fix wins over the
/// exporter's record, which fills in only where the bytes carried none.
fn folded_gps(embedded: Option<Gps>, folded: Option<&Gps>) -> Option<Gps> {
    embedded.or_else(|| folded.cloned())
}

fn content_type_for(ext: &str) -> String {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "heic" => "image/heic",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn asset_type_for(content_type: &str) -> String {
    if content_type.starts_with("video/") {
        "video"
    } else {
        "photo"
    }
    .to_string()
}

fn rfc3339_to_secs(s: &str) -> i64 {
    s.parse::<Timestamp>().map_or(0, Timestamp::as_second)
}

/// Map a managed asset's in-memory state to its queryable `assets` index row. Deletion state is
/// derived from the provenance chain's lifecycle actions; media-derived fields (dimensions,
/// duration, chromahash) stay NULL — they are out of scope in this offline core.
fn asset_row_from_state(asset: &AssetState) -> AssetRow {
    let mut is_deleted = false;
    let mut deleted_at = None;
    for rec in asset.chain.records() {
        match rec.manifest.core.action {
            Action::Delete => {
                is_deleted = true;
                deleted_at = Some(rfc3339_to_secs(&rec.manifest.core.timestamp));
            }
            Action::TrashRestore => {
                is_deleted = false;
                deleted_at = None;
            }
            _ => {}
        }
    }
    debug_assert_eq!(is_deleted, asset_is_deleted(asset));
    // Stack columns are a projection of the signed `stack_membership` register (`S-B15`) — the
    // same projection `library::rebuild::signed_asset_row` applies, so the write path and a
    // rebuild agree on what the views show. A *written* register is authoritative in both arms:
    // `Some(m)` is a placement, a stamped `None` is an explicit departure from a stack. Only a
    // never-written register falls back to `AssetState::stack`, the index-only placement a
    // pre-`S-B15` import left behind.
    let (stack_id, is_stack_hidden) = match asset.sidecar.stack_membership.get() {
        Some(membership) => membership.as_ref().map_or((None, false), |m| {
            (Some(m.stack_id.to_string()), m.role != StackRole::Primary)
        }),
        None => asset
            .stack
            .as_ref()
            .map_or((None, false), |s| (Some(s.stack_id.clone()), s.hidden)),
    };
    AssetRow {
        uuid: asset.asset_id.to_string(),
        asset_type: asset_type_for(&asset.sidecar.content_type),
        capture_timestamp: asset.capture_utc,
        capture_utc: Some(asset.capture_utc),
        capture_tz_source: None,
        import_timestamp: rfc3339_to_secs(&asset.sidecar.import_timestamp),
        hash_sha256: asset.sidecar.hash.to_hex(),
        width: asset.sidecar.dimensions.as_ref().map(|d| d.width as i64),
        height: asset.sidecar.dimensions.as_ref().map(|d| d.height as i64),
        duration_ms: None,
        stack_id,
        is_stack_hidden,
        chromahash: None,
        dominant_color: None,
        album_id: Some(asset.album_id.to_string()),
        rating: asset.sidecar.rating.get().copied().unwrap_or(0) as i64,
        is_deleted,
        deleted_at,
        // Projection of the sidecar `hidden` LWW register (S-D19): a never-written register
        // means visible, the wire-absent default.
        is_hidden: asset.sidecar.hidden.get().copied().unwrap_or(false),
    }
}

impl Workspace {
    pub(super) fn write_asset_files(&self, asset: &AssetState, plaintext: &[u8]) -> Result<()> {
        let dir = media_dir(&self.root, asset.capture_utc);
        fs::create_dir_all(&dir).map_err(|e| LifecycleError::Io(e.to_string()))?;
        fs::write(self.media_path(asset), plaintext)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        fs::write(self.sidecar_path(asset), asset.sidecar.to_canonical_vec())
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        let prov = cbor::to_canonical_vec(&asset.chain.records().to_vec())
            .map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        fs::write(self.provenance_path(asset), prov)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        // The sealed metadata blob (`S-A10`). Un-regenerable (its nonce is folded into the blob
        // key), AMK ciphertext, and required by `export_backup` and upload — so it is written
        // beside the chain rather than held only in memory. An action that mints no blob
        // (`delete` / `trash-restore`) leaves the previous file in place.
        if !asset.metadata_blob.is_empty() {
            fs::write(self.metadata_blob_path(asset), &asset.metadata_blob)
                .map_err(|e| LifecycleError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Write the queryable index row + user tags for `asset` into `library.sqlite`. Re-syncs on
    /// every change (import, metadata edit, soft-delete/restore), so the index reflects the
    /// asset's current rating, tags, and deletion state. Upsert keeps it conflict-safe even
    /// though the legacy importer shares the same `assets` table.
    pub(super) fn index_asset_row(&self, asset: &AssetState) -> Result<()> {
        self.library
            .db
            .upsert_asset(&asset_row_from_state(asset))
            .map_err(|e| LifecycleError::Db(e.to_string()))?;
        let tags: Vec<String> = asset.sidecar.tags_user.value().into_iter().collect();
        self.library
            .db
            .replace_asset_tags(&asset.asset_id.to_string(), &tags)
            .map_err(|e| LifecycleError::Db(e.to_string()))
    }

    /// Record the asset's own original as a device-owned cache representation — exempt from the
    /// automatic eviction sweep, and the real lifecycle data that sweep then operates on.
    pub(super) fn index_original_representation(
        &self,
        asset: &AssetState,
        bytes: usize,
    ) -> Result<()> {
        self.library
            .db
            .upsert_representation(&CachedRepresentationRow {
                uuid: asset.asset_id.to_string(),
                tier: "original".to_string(),
                format: Some(asset.ext.clone()),
                bytes: bytes as i64,
                path: self.media_path(asset).to_string_lossy().into_owned(),
                last_accessed_at: Timestamp::now().as_second(),
                pinned: false,
                is_owned_original: true,
            })
            .map_err(|e| LifecycleError::Db(e.to_string()))
    }

    /// Import a file into `album_id`: encrypt, build the signed create manifest + provenance,
    /// write the signed sidecar, and self-verify through `verify_asset` **and** the
    /// metadata↔manifest binding. Follows the [sealing order] so the manifest commits to the
    /// content address of the sidecar it seals, without a cycle. Returns the asset id.
    ///
    /// [sealing order]: https://docs/design/metadata/#provenance-binding-and-sealing-order
    pub fn import_asset(&mut self, album_id: Uuid, src: &Path) -> Result<Uuid> {
        Ok(self
            .import_asset_with(album_id, src, &SignedImportOptions::default())?
            .asset_id)
    }

    /// Import **bytes already in memory** into `album_id` under the file name `file_name`
    /// (consulted for its extension and nothing else), returning the asset id.
    ///
    /// This is the entry point a platform app needs and the CLI does not: PhotoKit, the Android
    /// MediaStore, and a browser upload all hand over a byte buffer rather than a path the
    /// library may read at leisure. It stages those bytes in the library's own scratch
    /// directory and drives the **one** signed write path —
    /// [`import_asset_with`](Self::import_asset_with) in Move mode, so the staged copy is
    /// released by the same durable-commit rule that releases any other moved source. There is
    /// no second sealing implementation here and none is wanted: the file key derivation, STREAM
    /// encryption, manifest, sidecar, provenance chain, and `verify_asset` self-check are
    /// byte-for-byte the path a file import takes.
    ///
    /// EXIF is read back off the staged file, so a JPEG carrying capture time and GPS is scanned
    /// exactly as it would be on a file import.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, file_name, bytes = bytes.len()))]
    pub fn import_bytes(&mut self, album_id: Uuid, file_name: &str, bytes: &[u8]) -> Result<Uuid> {
        let ext = Path::new(file_name)
            .extension()
            .map_or_else(|| "bin".to_string(), |e| e.to_string_lossy().to_lowercase());
        let staging = self.root().join(".library").join("staging");
        fs::create_dir_all(&staging)
            .map_err(|e| LifecycleError::Io(format!("create staging dir: {e}")))?;
        let staged = staging.join(format!("{}.{ext}", Uuid::now_v7().simple()));
        fs::write(&staged, bytes)
            .map_err(|e| LifecycleError::Io(format!("stage {}: {e}", staged.display())))?;

        let opts = SignedImportOptions {
            // The staged copy is scratch, and the caller still holds the buffer it handed over,
            // so releasing on the local durable commit is right: nothing is the only copy.
            move_source: true,
            defer_source_release: false,
            stack: None,
            enrichment: None,
        };
        let imported = self.import_asset_with(album_id, &staged, &opts);
        if imported.is_err() {
            // A failed import must not leave staged plaintext behind.
            let _ = fs::remove_file(&staged);
        }
        let asset_id = imported?.asset_id;
        tracing::info!(asset_id = %asset_id, "imported in-memory bytes as a signed asset");
        Ok(asset_id)
    }

    /// As [`import_asset`](Self::import_asset) but with executor-supplied [`SignedImportOptions`]
    /// (Move-mode source release + stack placement). This is the single signed write path the
    /// import executor drives (S-B2): every imported member lands as a signed `SidecarV1` +
    /// manifest + append-only provenance, self-verified through [`verify_asset`], and — behind
    /// the `media` feature, when a [`StillEncoder`](crate::media::image::derivative::StillEncoder)
    /// is attached — with signed thumbnail/preview derivatives + an LQIP in the sidecar.
    ///
    /// Returns a [`SignedImport`]: the asset id, plus the [`DerivativeStatus`](super::DerivativeStatus)
    /// saying whether derivatives were generated and, if not, why. A format this build has no
    /// codec for **still imports** — the original is the backup, the thumbnail is a bonus — so
    /// the status is a report, never a rejection (slice `S-B13`).
    #[tracing::instrument(skip_all, fields(album_id = %album_id, src = %src.display()))]
    pub fn import_asset_with(
        &mut self,
        album_id: Uuid,
        src: &Path,
        opts: &SignedImportOptions,
    ) -> Result<SignedImport> {
        let plaintext = fs::read(src)
            .map_err(|e| LifecycleError::Io(format!("read {}: {e}", src.display())))?;
        let ext = src
            .extension()
            .map_or_else(|| "bin".into(), |e| e.to_string_lossy().to_lowercase());
        let asset_id = Uuid::now_v7();

        // Scan & extract: capture time, dimensions, and GPS from the file's EXIF. Missing values
        // degrade cleanly (capture → now; dimensions/GPS → absent).
        let exif = extract_exif(src).unwrap_or_default();
        let tz = resolve_timezone(&exif);
        // `S-B10`: a third-party import arrives with the adapter's folded exporter record. Its
        // capture time and GPS are *fallbacks* — the file's own EXIF wins wherever it yields a
        // value, which is the pipeline doc's precedence rule applied at the write site.
        let enrichment = opts.enrichment.as_ref();
        let (capture_utc, capture_source) =
            folded_capture(tz.capture_utc, enrichment.and_then(|e| e.capture_time));
        // EXIF GPS is the near-universal WGS-84 camera datum (metadata doc, Geolocation);
        // stored verbatim, so the wire-absent default datum applies.
        let embedded_gps = exif.gps_lat.zip(exif.gps_lon).map(|(lat, lon)| Gps {
            lat,
            lon,
            source: GpsSource::Exif,
            datum: crate::domain::GpsDatum::Wgs84,
        });
        let gps = folded_gps(embedded_gps, enrichment.and_then(|e| e.gps.as_ref()));

        // Exporter-authoritative registers (`S-B10`): the description, favorite flag, and album
        // membership the file bytes never carried. Each is stamped `(now, device_id)` exactly as
        // the `stack_membership` register is, so a later edit on any device converges under the
        // same LWW rule; each album title gets its own `add_id` so it stays individually
        // removable. Nothing folded ⇒ every register stays at its default, and the sidecar
        // encodes byte-identically to a plain filesystem import's.
        let device_id = self.account.device.device_id;
        let stamp = now_rfc3339();
        let mut caption = Lww::new();
        if let Some(text) = enrichment.and_then(|e| e.caption.as_deref()) {
            caption.set(text.to_string(), stamp.clone(), device_id);
        }
        let mut rating = Lww::new();
        if let Some(stars) = enrichment.and_then(|e| e.rating) {
            rating.set(stars, stamp.clone(), device_id);
        }
        let mut tags_user = OrSet::new();
        for tag in enrichment.map_or(&[][..], |e| e.tags.as_slice()) {
            let add_id = self.counter.issue();
            tags_user.add(tag.clone(), add_id);
        }
        // Logged for *every* import, enriched or not: which side each contested field came from
        // is what makes a surprising capture time or location explainable after the fact. User
        // content stays out — sizes and counts are what a decision has to be reconstructed from,
        // not the caption text or the album titles.
        tracing::debug!(
            asset_id = %asset_id,
            enriched = enrichment.is_some(),
            capture_source = capture_source.as_str(),
            capture_utc,
            gps_source = ?gps.as_ref().map(|g| g.source),
            caption_bytes = enrichment.and_then(|e| e.caption.as_ref()).map_or(0, String::len),
            rating = ?enrichment.and_then(|e| e.rating),
            tags = enrichment.map_or(0, |e| e.tags.len()),
            "import: sidecar metadata resolved"
        );

        // Still-derived sidecar metadata (dimensions + LQIP) and the derivatives to persist
        // after the commit. Behind `media` this decodes the still once and generates the signed
        // derivatives; without it, dimensions come from EXIF and no derivatives are generated.
        // Either way the import proceeds: a still this build cannot decode is still backed up as
        // a signed, encrypted original — `derivative_status` records the gap so it is reportable
        // rather than silent (S-B13).
        #[cfg(feature = "media")]
        let (dimensions, lqip, pending_derivatives, derivative_status) = {
            let prepared = self.prepare_still(&plaintext, &ext, src, &exif, asset_id, album_id)?;
            (
                prepared.dimensions,
                prepared.lqip,
                prepared.derivatives,
                prepared.status,
            )
        };
        // No `media` feature means no codecs at all, so every still is a deferral.
        #[cfg(not(feature = "media"))]
        let (dimensions, lqip, derivative_status) = (
            exif.width
                .zip(exif.height)
                .map(|(width, height)| Dimensions { width, height }),
            None::<crate::sidecar::sidecar_v1::Lqip>,
            super::DerivativeStatus::DeferredNoCodec,
        );

        let album = self.album(&album_id)?;
        let epoch = album.current_epoch;
        let amk = Amk::from_bytes(album.amks[&epoch]);
        // First write: draw a fresh nonce prefix and derive the folded file key together
        // (nothing to replace on a create).
        let (enc, ciphertext, _file_key) = encrypt_asset_rekey(&amk, &asset_id, &plaintext, None)?;

        // Sealing order (1) the prior head `H` is `None` on a create; (2) author + sign the
        // sidecar with `provenance_chain_hash = H`.
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: asset_id,
            hash: hash::hash_bytes(&plaintext),
            capture_timestamp: capture_rfc3339(capture_utc),
            import_timestamp: now_rfc3339(),
            content_type: content_type_for(&ext),
            dimensions,
            lqip,
            tags_user,
            tags_ai: Default::default(),
            caption,
            rating,
            // `S-B15`: an importer-formed stack is written into the signed sidecar exactly as
            // the manual `set_stack_membership` path writes it, stamped with this device id +
            // now so it converges under the same `(ts, device_id)` LWW rule. A standalone
            // import leaves the register at its default, which is wire-absent — a sidecar for
            // an unstacked asset encodes byte-identically to one written before this slice.
            stack_membership: stack_membership_register(
                opts.stack.clone(),
                self.account.device.device_id,
            ),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: self.account.device.device_id,
            session_id: Uuid::now_v7(),
            gps,
            provenance_chain_hash: None,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);

        // (3) Seal the sidecar into the metadata blob (fresh nonce folded into the blob key;
        // nothing to replace on a create); compute its content hash.
        let (metadata_blob, blob_key) =
            seal_metadata_blob(&amk, &asset_id, &sidecar.to_canonical_vec(), None)?;
        let metadata_blob_hash = blob_ciphertext_hash(&metadata_blob);

        // (4) Build + sign the manifest with `prior_provenance_hash = H` (None) and the
        // `metadata_blob_hash` from (3); append it as the new chain head.
        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: asset_id,
            album_id,
            amk_version: AmkVersion(epoch),
            ciphertext_hash: enc.ciphertext_hash,
            plaintext_size: enc.plaintext_size,
            chunk_size: enc.chunk_size,
            nonce_prefix: enc.nonce_prefix,
            key_mode: KeyMode::Derived,
            wrapped_file_key: None,
            metadata_blob_hash: Some(metadata_blob_hash),
            created_by_user: self.account.user_id,
            created_by_device: self.account.device.device_id,
            client_version: self.client_version.clone(),
            timestamp: now_rfc3339(),
            action: Action::Create,
            prior_provenance_hash: None,
            retention_until: None,
        };
        let manifest = core.sign(self.device_signer.as_ref(), album.write_tier_signer()?)?;

        let mut chain = ProvenanceChain::new();
        chain
            .append(ProvenanceRecord {
                asset_id,
                manifest: manifest.clone(),
                prior_provenance_hash: None,
            })
            .map_err(|e| LifecycleError::Cbor(format!("chain: {e}")))?;

        // Self-check: the asset must verify through the one chokepoint, and the sealed metadata
        // blob must round-trip to the signed sidecar and the committed hash, before we accept it.
        let authority = self.authority(&album_id)?;
        let outcome = verify_asset(&manifest, &ciphertext, &self.directory, authority, None);
        if outcome != VerifyOutcome::Accept {
            return Err(LifecycleError::SelfVerify(outcome));
        }
        let binding = verify_metadata_binding(
            &manifest,
            &metadata_blob,
            &blob_key,
            &sidecar.to_canonical_vec(),
        );
        if binding != MetadataBinding::Bound {
            return Err(LifecycleError::MetadataUnbound(binding));
        }

        let asset = AssetState {
            asset_id,
            album_id,
            ext,
            capture_utc,
            chain,
            sidecar,
            metadata_blob,
            // Kept in step with the register it is projected from; only a pre-`S-B15` asset
            // reaches `AssetState::stack` by any other route.
            stack: opts.stack.as_ref().map(StackPlacement::from_membership),
        };
        self.write_asset_files(&asset, &plaintext)?;
        self.index_asset_row(&asset)?;
        self.index_original_representation(&asset, plaintext.len())?;

        // Persist the signed still derivatives generated pre-commit (media + encoder attached).
        #[cfg(feature = "media")]
        self.persist_derivatives(&asset, &pending_derivatives)?;

        // Move mode: release the source only after the durable, self-verified commit — unless
        // the caller defers release to its server-side verify-before-destroy gate (S-D4/S-B3),
        // where the source is the only copy until the *server* durably holds it.
        if opts.move_source && !opts.defer_source_release {
            let _ = fs::remove_file(src);
        }

        self.assets.insert(asset_id, asset);
        Ok(SignedImport {
            asset_id,
            derivatives: derivative_status,
        })
    }

    /// Import a file on the signed path for a **streaming** import: identical to
    /// [`import_asset_with`](Self::import_asset_with) but with `defer_source_release` forced on,
    /// and returning the [`StreamedImport`] descriptor the streaming window drives its
    /// per-asset upload → verify → release step from. The local original (and any Move-mode
    /// source) is left in place — the [streaming executor](crate::import::streaming) releases it
    /// only after the server's `durable` verdict + custody receipt clear the `S-D4` gate.
    ///
    /// `enrichment` carries the folded third-party exporter metadata for this file exactly as
    /// [`import_asset_with`](Self::import_asset_with) takes it (`S-B11`). It is a parameter and
    /// not a hard-wired `None` because the enrichment is written *inside the signed sidecar*:
    /// dropping it here would make a streamed Takeout import silently lossier than a bulk one,
    /// and the difference would be unrecoverable without re-importing.
    #[tracing::instrument(
        skip_all,
        fields(album_id = %album_id, src = %src.display(), move_source, enriched = enrichment.is_some())
    )]
    pub fn import_asset_streaming(
        &mut self,
        album_id: Uuid,
        src: &Path,
        move_source: bool,
        stack: Option<StackMembership>,
        enrichment: Option<SidecarEnrichment>,
    ) -> Result<StreamedImport> {
        let opts = SignedImportOptions {
            move_source,
            defer_source_release: true,
            stack,
            enrichment,
        };
        let asset_id = self.import_asset_with(album_id, src, &opts)?.asset_id;
        let asset = self
            .assets
            .get(&asset_id)
            .expect("asset just inserted by import_asset_with");
        let head = &asset
            .chain
            .records()
            .last()
            .expect("provenance chain is never empty")
            .manifest
            .core;
        // The always-present required blobs: original ciphertext + sealed metadata blob.
        let mut blob_hashes = vec![head.ciphertext_hash];
        if let Some(h) = head.metadata_blob_hash {
            blob_hashes.push(h);
        }
        Ok(StreamedImport {
            asset_id,
            blob_hashes,
            local_original: self.media_path(asset),
            move_source: move_source.then(|| src.to_path_buf()),
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::keys::HybridSigningKey;

    // ── Write-site precedence (S-B10) ───────────────────────────────────────
    //
    // The [precedence rule] the pipeline doc fixes: embedded EXIF wins over an exporter-side
    // record for capture time and GPS. The adapter resolves it once at extraction; these two
    // helpers are where the *write* path honours it, and they are the reason an exporter
    // record can never overwrite what the file bytes themselves say.
    //
    // [precedence rule]: https://docs/design/import/pipeline/#third-party-importers

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).expect("in-range timestamp")
    }

    fn point(lat: f64, lon: f64, source: GpsSource) -> Gps {
        Gps {
            lat,
            lon,
            source,
            datum: crate::domain::GpsDatum::Wgs84,
        }
    }

    /// Both sides present and disagreeing — the case that was unreachable while `extract_exif`
    /// returned `None` for every well-formed EXIF file (`S-B16`).
    #[test]
    fn embedded_exif_capture_wins_over_the_folded_record() {
        assert_eq!(
            folded_capture(Some(1_622_505_600), Some(ts(1_000_000_000))),
            (1_622_505_600, CaptureSource::Embedded)
        );
    }

    #[test]
    fn the_folded_record_fills_capture_when_the_bytes_resolve_none() {
        assert_eq!(
            folded_capture(None, Some(ts(1_609_502_400))),
            (1_609_502_400, CaptureSource::Folded)
        );
    }

    #[test]
    fn capture_falls_back_to_import_time_when_neither_side_has_one() {
        let before = Timestamp::now().as_second();
        let (secs, source) = folded_capture(None, None);
        assert_eq!(source, CaptureSource::ImportTime);
        assert!(secs >= before, "the import clock, not the epoch");
    }

    #[test]
    fn embedded_exif_gps_wins_over_the_folded_record() {
        let embedded = point(48.8584, 2.2945, GpsSource::Exif);
        let exporter = point(40.0, -70.0, GpsSource::Manual);
        assert_eq!(
            folded_gps(Some(embedded.clone()), Some(&exporter)),
            Some(embedded)
        );
    }

    #[test]
    fn the_folded_record_fills_gps_when_the_bytes_carry_none() {
        let exporter = point(21.3, -157.8, GpsSource::Manual);
        assert_eq!(folded_gps(None, Some(&exporter)), Some(exporter));
        assert_eq!(folded_gps(None, None), None);
    }

    /// `import_bytes` is the platform-app entry point (PhotoKit / MediaStore hand over a
    /// buffer, not a path). It must land an asset indistinguishable from a file import — same
    /// signed artifacts, same `verify_asset` verdict, same plaintext readable back — and must
    /// leave no staged plaintext behind.
    #[test]
    fn import_bytes_lands_a_signed_asset_and_clears_its_staging_copy() {
        let lib = TempDir::new().unwrap();
        let bytes = b"\xFF\xD8\xFF\xE0 in-memory jpeg bytes \x00\x01\x02".to_vec();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Camera Roll").unwrap();
        let asset = ws.import_bytes(album, "IMG_0042.JPG", &bytes).unwrap();

        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
        assert_eq!(ws.read_plaintext(&asset).unwrap(), bytes);
        // The extension came off the file name, lowercased, exactly as a file import derives it.
        assert_eq!(ws.asset(&asset).unwrap().ext, "jpg");
        assert!(ws.asset(&asset).unwrap().sidecar.signature.is_some());

        // Move mode released the staged copy: no plaintext is left in the scratch directory.
        let staging = lib.path().join(".library").join("staging");
        let leftovers = std::fs::read_dir(&staging)
            .map(|dir| dir.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "the staged plaintext copy must not survive");
    }

    /// A name with no extension still imports — the extension degrades to `bin` rather than
    /// the import failing, because a backup tool never refuses bytes it can store.
    #[test]
    fn import_bytes_without_an_extension_degrades_to_bin() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Camera Roll").unwrap();
        let asset = ws
            .import_bytes(album, "no-extension", b"raw bytes")
            .unwrap();
        assert_eq!(ws.asset(&asset).unwrap().ext, "bin");
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    }

    // ── Importer-formed stacks (S-B15) ──────────────────────────────────────────

    /// Write `n` distinct fixture files into `dir` and return their paths.
    fn fixture_files(dir: &Path, n: usize) -> Vec<std::path::PathBuf> {
        (0..n)
            .map(|i| {
                let p = dir.join(format!("stacked-{i}.jpg"));
                let mut bytes = vec![0xFF, 0xD8, 0xFF];
                bytes.extend_from_slice(format!("stack fixture asset {i}").as_bytes());
                fs::write(&p, &bytes).unwrap();
                p
            })
            .collect()
    }

    /// **The `S-B15` acceptance case.** A stack formed by the *importer* — never touched by
    /// hand — is written into the signed sidecar, so it survives losing `index/library.sqlite`
    /// entirely and rebuilding from the artifacts on disk.
    ///
    /// Before this slice the placement existed only as index columns, so this test's `rebuild`
    /// came back with both members loose in the timeline and no `asset_stacks` row at all.
    #[test]
    fn importer_formed_stack_survives_index_loss_and_rebuild() {
        use crate::library::{open_library, rebuild_index};
        use crate::sidecar::sidecar_v1::StackRole;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let root = lib.path().to_path_buf();
        let files = fixture_files(src.path(), 2);
        let stack_id = Uuid::now_v7();

        let mut ws = fast_workspace(&root);
        let album = ws.create_album("Trip").unwrap();
        let mut ids = Vec::new();
        for (seq, path) in files.iter().enumerate() {
            let opts = SignedImportOptions {
                stack: Some(StackMembership {
                    stack_id,
                    stack_type: crate::domain::StackType::RawJpeg,
                    role: if seq == 0 {
                        StackRole::Primary
                    } else {
                        StackRole::Member
                    },
                    member_index: Some(seq as u32),
                }),
                ..Default::default()
            };
            ids.push(ws.import_asset_with(album, path, &opts).unwrap().asset_id);
        }
        let (primary, member) = (ids[0], ids[1]);

        // The register is on disk, inside the signed sidecar, and the asset still verifies.
        let sidecar = &ws.asset(&member).unwrap().sidecar;
        let membership = sidecar
            .stack_membership
            .get()
            .and_then(Option::as_ref)
            .expect("the importer wrote the stack register");
        assert_eq!(membership.stack_id, stack_id);
        assert_eq!(membership.role, StackRole::Member);
        assert_eq!(membership.member_index, Some(1));
        assert_eq!(ws.verify(&member).unwrap(), VerifyOutcome::Accept);

        // ...and it is projected onto the index the same way a rebuild projects it.
        let timeline = ws.db().query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1, "only the primary is in the timeline");
        assert_eq!(timeline[0].uuid, primary.to_string());

        // Recovery: delete the index outright and rebuild it from the artifacts on disk.
        drop(ws);
        fs::remove_file(root.join("index/library.sqlite")).unwrap();
        let library = open_library(&root).unwrap();
        rebuild_index(&library).unwrap();

        for (id, hidden) in [(primary, false), (member, true)] {
            let row = library
                .db
                .find_by_uuid(&id.to_string())
                .unwrap()
                .expect("asset is back in the rebuilt index");
            assert_eq!(row.stack_id.as_deref(), Some(stack_id.to_string().as_str()));
            assert_eq!(row.is_stack_hidden, hidden);
        }
        let rebuilt = library.db.query_timeline(0, 100).unwrap();
        assert_eq!(rebuilt.len(), 1, "the stack still collapses to its primary");
        assert_eq!(rebuilt[0].uuid, primary.to_string());
        assert_eq!(
            library
                .db
                .list_stack_members(&stack_id.to_string())
                .unwrap()
                .len(),
            2,
            "the `stack_members` rows are reconstructed from the registers"
        );

        // Reopening the workspace reads the placement off the sidecar, not the index.
        drop(library);
        let ws = Workspace::open(
            &root,
            b"passphrase",
            crate::crypto::primitives::Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();
        let placement = ws.asset(&member).unwrap().stack.as_ref().unwrap();
        assert_eq!(placement.stack_id, stack_id.to_string());
        assert!(placement.hidden);
    }

    /// The absent-key discipline, at the import path. An asset imported **without** a stack
    /// leaves `stack_membership` at its never-written default, which is wire-absent — so its
    /// signed sidecar encodes byte-identically to one written before `S-B15`.
    #[test]
    fn import_without_a_stack_leaves_the_stack_register_wire_absent() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("standalone.jpg");
        fs::write(&img, b"\xFF\xD8\xFF unstacked photo").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();

        let bytes = fs::read(ws.sidecar_path(ws.asset(&id).unwrap())).unwrap();
        for needle in [
            b"stack_membership".as_slice(),
            b"cull".as_slice(),
            b"hidden".as_slice(),
        ] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "a never-written register must not reach the wire"
            );
        }

        let parsed = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(parsed.stack_membership, Lww::new());

        // The proof itself: substituting the literal pre-`S-B15` field value (`Lww::new()`)
        // re-encodes to the same bytes, so the register contributes nothing to an unstacked
        // asset's signed sidecar and no existing signature is invalidated.
        let mut pre_change = parsed.clone();
        pre_change.stack_membership = Lww::new();
        assert_eq!(pre_change.to_canonical_vec(), bytes);
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    }

    /// The register is only *written* when there is a stack: `stack_membership_register` is the
    /// single decision point, and its `None` arm is the wire-absent default.
    #[test]
    fn stack_membership_register_is_absent_without_a_stack() {
        let device = Uuid::from_u128(0xD1);
        assert_eq!(stack_membership_register(None, device), Lww::new());

        let membership = StackMembership {
            stack_id: Uuid::now_v7(),
            stack_type: crate::domain::StackType::Burst,
            role: crate::sidecar::sidecar_v1::StackRole::Primary,
            member_index: Some(0),
        };
        let written = stack_membership_register(Some(membership.clone()), device);
        assert_eq!(written.get(), Some(&Some(membership)));
    }

    #[test]
    fn end_to_end_data_plane() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(
            &img,
            b"\xFF\xD8\xFF\xE0 fake jpeg bytes for the e2e test \x00\x01\x02",
        )
        .unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();

        // Import → encrypt → manifest+provenance+signed sidecar → verify_asset(Accept).
        let asset = ws.import_asset(album, &img).unwrap();
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);

        // The signed sidecar + provenance + plaintext exist on disk.
        let st = ws.asset(&asset).unwrap();
        assert!(ws.media_path(st).exists());
        assert!(ws.sidecar_path(st).exists());
        assert!(ws.provenance_path(st).exists());
        assert!(st.sidecar.verify(&ws.account.user_ik.verifying_key()));

        // CRDT metadata edits advance the chain and re-sign the sidecar.
        ws.tag_add(&asset, "vacation").unwrap();
        ws.set_caption(&asset, "sunset over the bay").unwrap();
        let st = ws.asset(&asset).unwrap();
        assert!(st.sidecar.tags_user.value().contains("vacation"));
        assert_eq!(st.sidecar.caption.get().unwrap(), "sunset over the bay");
        assert_eq!(st.chain.records().len(), 3); // create + 2 metadata-update
        ProvenanceChain::verify_walk(st.chain.records()).unwrap();

        // Soft delete + restore append lifecycle records.
        ws.soft_delete(&asset, 30).unwrap();
        ws.restore(&asset).unwrap();
        let st = ws.asset(&asset).unwrap();
        assert_eq!(st.chain.records().len(), 5);
        // The delete record carries a retention window; it remains in the chain after restore.
        let actions: Vec<_> = st
            .chain
            .records()
            .iter()
            .map(|r| r.manifest.core.action)
            .collect();
        assert_eq!(
            actions,
            vec![
                Action::Create,
                Action::MetadataUpdate,
                Action::MetadataUpdate,
                Action::Delete,
                Action::TrashRestore
            ]
        );

        // Backup → restore into a FRESH library (new device, verifying against the
        // exporter's published key) → byte-equal plaintext.
        let backup_path = src.path().join("backup.tar");
        ws.export_backup(&backup_path, b"recovery-pass").unwrap();
        let exporter_pub = ws.exporter_verifying_key();

        let fresh = TempDir::new().unwrap();
        let mut ws2 = fast_workspace(fresh.path());
        let added = ws2
            .import_backup(&backup_path, b"recovery-pass", &exporter_pub)
            .unwrap();
        assert_eq!(added, 1);
        assert_eq!(
            ws2.read_plaintext(&asset).unwrap(),
            ws.read_plaintext(&asset).unwrap(),
            "restored library must be byte-equal to the source"
        );

        // A wrong exporter key (untrusted device) is refused.
        let imposter = HybridSigningKey::generate().verifying_key();
        let mut ws3 = fast_workspace(TempDir::new().unwrap().path());
        assert!(
            ws3.import_backup(&backup_path, b"recovery-pass", &imposter)
                .is_err()
        );
    }

    #[test]
    fn crypto_lifecycle_writes_through_to_the_index() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF indexed photo").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        let uuid = id.to_string();

        // The import is queryable in the timeline, tagged to its album.
        let timeline = ws.db().query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].uuid, uuid);
        assert_eq!(
            timeline[0].album_id.as_deref(),
            Some(album.to_string().as_str())
        );

        // It recorded a device-owned `original` representation, exempt from eviction.
        let reps = ws.db().representations_for(&uuid).unwrap();
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].tier, "original");
        assert!(reps[0].is_owned_original);
        assert!(
            ws.db().eviction_candidates(0).unwrap().is_empty(),
            "an owned original is never an eviction candidate"
        );

        // A tag edit re-syncs into the index.
        ws.tag_add(&id, "vacation").unwrap();
        assert_eq!(
            ws.db().tags_for(&uuid).unwrap(),
            vec!["vacation".to_string()]
        );

        // Soft-delete hides it from the timeline; restore brings it back (deletion state is
        // derived from the provenance chain).
        ws.soft_delete(&id, 30).unwrap();
        assert!(ws.db().query_timeline(0, 100).unwrap().is_empty());
        ws.restore(&id).unwrap();
        assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 1);
    }
}
