//! User + AI metadata edits — the CRDT writes that ride a signed `metadata-update` record.
//! The only `lifecycle` file that reaches [`crate::ml`], in both directions: it reads the model
//! registry to filter stale suggestions, and it is where [`Workspace`] implements the
//! [`AiTagSink`] seam the inference orchestrator writes through. The orchestrator used to import
//! `Workspace` directly, which — with this file importing `ml` — made the two modules mutually
//! recursive; the trait inverts that edge without moving any behaviour.

use jiff::Timestamp;
use uuid::Uuid;

use super::{LifecycleError, Result, Workspace, now_rfc3339};
use crate::crypto::provenance::action::Action;
use crate::db::DatabaseDriver;
use crate::metadata::crdt::AddId;
use crate::ml::orchestrator::{AiTagSink, AssetSource};
use crate::ml::{ModelId, Registry};
use crate::sidecar::AiTag;

impl Workspace {
    /// Add a user tag (OR-set) and emit a `metadata-update` provenance record.
    pub fn tag_add(&mut self, asset_id: &Uuid, tag: &str) -> Result<()> {
        let tag = tag.to_string();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, add_id| {
            s.tags_user.add(tag, add_id);
        })
    }

    /// Set the caption (LWW register) and emit a `metadata-update` provenance record.
    pub fn set_caption(&mut self, asset_id: &Uuid, caption: &str) -> Result<()> {
        let caption = caption.to_string();
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            s.caption.set(caption, ts, device);
        })
    }

    /// Correct the asset's signed `capture_timestamp` to `capture`, as a new signed revision:
    /// one `metadata-update` record, with the sidecar re-signed and its sealed blob re-sealed
    /// under a fresh nonce, exactly as any other metadata edit lands (slice `S-B17`).
    ///
    /// The **media bundle is not relocated.** `capture_timestamp` is authoritative for the
    /// asset's date; the `media/{YYYY}/{YYYY-MM}` directory is only the shard fixed at import
    /// (`AssetState::capture_utc`), and the design treats bucket-vs-timestamp drift after a
    /// capture correction as expected rather than as a fault (Maintenance — Structural
    /// Validation). Moving the bundle here would orphan the old directory for every writer
    /// that still resolves it, so the shard stays and every path keeps resolving; the index
    /// row follows the sidecar (`asset_row_from_state`), so the timeline shows the corrected
    /// instant immediately rather than after a rebuild.
    ///
    /// Takes a [`Timestamp`] rather than raw seconds so an out-of-range instant is
    /// unrepresentable at the call site instead of being clamped to the epoch here.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id, capture = %capture))]
    pub fn set_capture_timestamp(&mut self, asset_id: &Uuid, capture: Timestamp) -> Result<()> {
        let recorded = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .sidecar
            .capture_timestamp
            .clone();
        let corrected = capture.to_string();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            s.capture_timestamp = corrected;
        })?;
        let head = self.assets[asset_id].chain.head();
        tracing::info!(
            asset_id = %asset_id,
            recorded = %recorded,
            corrected = %capture,
            chain_head = ?head,
            "capture timestamp corrected as a signed metadata-update"
        );
        Ok(())
    }

    // ── AI metadata containment (SSoT: metadata § Tag Provenance, ai § AI Output Containment) ──
    //
    // AI outputs land in a namespace **structurally** separate from user-authored metadata: the
    // `tags_ai` OR-set, never `tags_user`. An AI suggestion can therefore never overwrite a user
    // tag — the question does not arise — and a hallucinating model can only pollute its own
    // namespace. Population is a signed `metadata-update` through the lifecycle, mirroring
    // [`set_cull`](Self::set_cull)'s write-through; promotion to a user tag is explicit and signed.

    /// Add AI-suggested tags to the asset's `tags_ai` OR-set — a namespace **structurally**
    /// separate from `tags_user`, so an AI suggestion can never overwrite a user tag. Emits one
    /// `metadata-update`; each tag gets a fresh `add_id` (pre-issued from this device's monotonic
    /// counter) so it can later be dismissed or promoted individually. A no-op (no record) if
    /// `tags` is empty. Each [`AiTag`] carries its producing `(model_id, model_version)` so a
    /// superseded suggestion is excluded by [`current_ai_tags`](Self::current_ai_tags).
    pub fn add_ai_tags(&mut self, asset_id: &Uuid, tags: Vec<AiTag>) -> Result<()> {
        if tags.is_empty() {
            return Ok(());
        }
        // Pre-issue one add_id per tag so a single metadata-update can carry the whole batch, each
        // element independently addressable (the closure receives a single add_id, not the counter).
        let tagged: Vec<(AiTag, AddId)> = tags
            .into_iter()
            .map(|t| (t, self.counter.issue()))
            .collect();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            for (tag, add_id) in tagged {
                s.tags_ai.add(tag, add_id);
            }
        })
    }

    /// The asset's current AI tags paired with their `add_id`s — the surface a client uses to
    /// dismiss or promote a specific suggestion. Reads the signed sidecar OR-set (the source of
    /// truth), not a query cache.
    pub fn ai_tags(&self, asset_id: &Uuid) -> Result<Vec<(AddId, AiTag)>> {
        Ok(self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .sidecar
            .tags_ai
            .entries())
    }

    /// Dismiss an AI tag: an OR-set remove on `tags_ai` keyed by its `add_id`, emitting a
    /// `metadata-update`. Rejects an `add_id` never observed locally (a fabricated remove) rather
    /// than silently no-oping.
    pub fn dismiss_ai_tag(&mut self, asset_id: &Uuid, add_id: AddId) -> Result<()> {
        let observed = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .sidecar
            .tags_ai
            .observed(&add_id);
        if !observed {
            return Err(LifecycleError::NotFound(format!(
                "unobserved ai-tag add_id {add_id:?}"
            )));
        }
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            s.tags_ai
                .remove(add_id)
                .expect("add_id observed above; remove cannot fail");
        })
    }

    /// Promote an AI tag to a user tag: copy its text into `tags_user` with a **fresh user-scoped
    /// `add_id`**, leaving the AI entry intact (still independently dismissable). An explicit,
    /// signed lifecycle operation — never automatic (ai § AI Output Containment).
    pub fn promote_ai_tag(&mut self, asset_id: &Uuid, tag: &str) -> Result<()> {
        let present = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .sidecar
            .tags_ai
            .value()
            .iter()
            .any(|t| t.tag == tag);
        if !present {
            return Err(LifecycleError::NotFound(format!("ai tag '{tag}'")));
        }
        let tag = tag.to_string();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, add_id| {
            s.tags_user.add(tag, add_id);
        })
    }

    /// The asset's AI tags that are **current** under `registry` — their `(model_id,
    /// model_version)` is the canonical pair for the model's task. Stale suggestions (a superseded
    /// model version) are excluded until regenerated, mirroring the vector-index stale rule; the
    /// sidecar retains every AI tag regardless (it is the source of truth).
    pub fn current_ai_tags(&self, registry: &Registry, asset_id: &Uuid) -> Result<Vec<AiTag>> {
        let sidecar = &self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .sidecar;
        Ok(sidecar
            .tags_ai
            .value()
            .into_iter()
            .filter(|t| {
                registry
                    .row_for_id(&ModelId::from(t.model_id.as_str()))
                    .is_some_and(|row| row.canonical_version.as_str() == t.model_version)
            })
            .collect())
    }
}

/// The read half of the inference seam: the orchestrator decodes an asset's plaintext and indexes
/// vectors through the workspace's own catalog. A thin forward to the inherent methods — the
/// indirection exists to reverse a module edge, not to change behaviour.
impl AssetSource for Workspace {
    type Error = LifecycleError;

    fn read_plaintext(&self, asset_id: &Uuid) -> Result<Vec<u8>> {
        Workspace::read_plaintext(self, asset_id)
    }

    fn vector_index(&self) -> &DatabaseDriver {
        self.db()
    }
}

/// The write half: AI tags produced by inference land in `tags_ai` as a signed `metadata-update`,
/// exactly as a direct [`Workspace::add_ai_tags`] call would.
impl AiTagSink for Workspace {
    fn add_ai_tags(&mut self, asset_id: &Uuid, tags: Vec<AiTag>) -> Result<()> {
        Workspace::add_ai_tags(self, asset_id, tags)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::verify_asset::VerifyOutcome;
    use crate::ml::{CANONICAL_PARTITION, FixtureRunner, TaskKind};
    use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

    /// A workspace holding one imported asset with `bytes` as its plaintext.
    fn workspace_with(lib: &TempDir, src: &TempDir, bytes: &[u8]) -> (Workspace, Uuid) {
        let img = src.path().join("p.jpg");
        std::fs::write(&img, bytes).unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("A").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        (ws, id)
    }

    /// S-H3: AI tags land in the **signed sidecar** as a structurally-separate namespace and
    /// survive the seal → canonical-CBOR round-trip; promotion/dismissal are explicit and
    /// per-tag. `add_ai_tags` succeeding at all proves the metadata↔manifest binding self-check
    /// passed — the signed sidecar carrying `tags_ai` re-decrypts to the committed bytes.
    #[test]
    fn ai_tags_land_in_the_signed_sidecar_and_stay_namespace_separate() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        std::fs::write(&img, b"\xFF\xD8\xFF ai-tag provenance bytes").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();

        let ai = |tag: &str, ver: &str| AiTag {
            tag: tag.to_string(),
            model_id: "mobileclip-b".to_string(),
            model_version: ver.to_string(),
        };

        // One signed metadata-update carrying two AI tags at the current canonical version.
        ws.add_ai_tags(&id, vec![ai("beach", "1"), ai("sunset", "1")])
            .unwrap();

        let sidecar = &ws.asset(&id).unwrap().sidecar;
        assert!(sidecar.signature.is_some(), "the sidecar is signed");
        let ai_texts: std::collections::BTreeSet<String> =
            sidecar.tags_ai.value().into_iter().map(|t| t.tag).collect();
        assert_eq!(
            ai_texts,
            ["beach".to_string(), "sunset".to_string()]
                .into_iter()
                .collect()
        );
        assert!(
            sidecar.tags_user.value().is_empty(),
            "AI tags never touch tags_user"
        );

        // Canonical-CBOR round-trip: the signed sidecar decodes back with tags_ai intact.
        let bytes = sidecar.to_canonical_vec();
        let decoded = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(decoded.tags_ai, sidecar.tags_ai);
        assert!(decoded.signature.is_some());

        // Each AI tag is independently addressable by its add_id.
        assert_eq!(ws.ai_tags(&id).unwrap().len(), 2);

        // Promote "beach" to a user tag — a FRESH user-scoped add_id; the AI entry stays intact.
        ws.promote_ai_tag(&id, "beach").unwrap();
        let sidecar = &ws.asset(&id).unwrap().sidecar;
        assert!(sidecar.tags_user.value().contains("beach"));
        assert!(
            sidecar.tags_ai.value().iter().any(|t| t.tag == "beach"),
            "promotion leaves the AI entry editable"
        );

        // Dismiss the "sunset" suggestion by its add_id; "beach" remains.
        let sunset_add = ws
            .ai_tags(&id)
            .unwrap()
            .into_iter()
            .find(|(_, t)| t.tag == "sunset")
            .unwrap()
            .0;
        ws.dismiss_ai_tag(&id, sunset_add).unwrap();
        let remaining: Vec<String> = ws
            .ai_tags(&id)
            .unwrap()
            .into_iter()
            .map(|(_, t)| t.tag)
            .collect();
        assert_eq!(remaining, vec!["beach".to_string()]);
    }

    /// S-H3: `current_ai_tags` excludes a suggestion tagged with a superseded model version
    /// (mirroring the vector-index stale rule), while the sidecar retains every AI tag.
    #[test]
    fn current_ai_tags_excludes_a_superseded_model_version() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("p.jpg");
        std::fs::write(&img, b"\xFF\xD8\xFF stale ai bytes").unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("A").unwrap();
        let id = ws.import_asset(album, &img).unwrap();

        let tag = |ver: &str| AiTag {
            tag: format!("t{ver}"),
            model_id: "mobileclip-b".to_string(),
            model_version: ver.to_string(),
        };
        // One current-version suggestion ("1") and one at a superseded version ("0").
        ws.add_ai_tags(&id, vec![tag("1"), tag("0")]).unwrap();

        let reg = Registry::canonical(); // canonical mobileclip-b version is "1"
        let current: Vec<String> = ws
            .current_ai_tags(&reg, &id)
            .unwrap()
            .into_iter()
            .map(|t| t.tag)
            .collect();
        assert_eq!(
            current,
            vec!["t1".to_string()],
            "the v0 suggestion is stale-excluded from the current view"
        );
        // The sidecar retains both (it is the source of truth).
        assert_eq!(ws.ai_tags(&id).unwrap().len(), 2);
    }

    // ── Capture-time correction (S-B17) ──────────────────────────────────────────────────────

    /// The `S-B17` write, end to end: the correction lands as a new signed revision, the bundle
    /// stays in the directory the import chose, the index row follows the sidecar at once, and
    /// a rebuild from disk agrees with the live row — the two projections
    /// (`asset_row_from_state` and `rebuild::signed_asset_row`) name the same instant.
    #[test]
    fn a_capture_correction_is_a_signed_revision_that_leaves_the_bundle_in_place() {
        use crate::library::{open_library, rebuild_index};

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, id) = workspace_with(&lib, &src, b"\xFF\xD8\xFF capture correction bytes");

        // No EXIF in those bytes, so the import stamped the import clock — the pre-`S-B16`
        // shape every affected asset has.
        let before = ws.asset(&id).unwrap();
        let shard = before.capture_utc;
        let original = ws
            .original_path(&id)
            .expect("a managed asset has an original");
        assert!(original.is_file());
        assert_eq!(before.chain.records().len(), 1);
        let recorded = before.sidecar.capture_timestamp.clone();

        let corrected = Timestamp::from_second(1_000_000_000).unwrap();
        ws.set_capture_timestamp(&id, corrected).unwrap();

        // A new signed revision: one more record, a `metadata-update`, and the asset verifies.
        let after = ws.asset(&id).unwrap();
        assert_eq!(after.sidecar.capture_timestamp, "2001-09-09T01:46:40Z");
        assert_ne!(after.sidecar.capture_timestamp, recorded);
        assert_eq!(after.chain.records().len(), 2);
        assert_eq!(
            after.chain.records().last().unwrap().manifest.core.action,
            Action::MetadataUpdate
        );
        assert!(after.sidecar.signature.is_some());
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);

        // The shard — and therefore every artifact path — is exactly where it was.
        assert_eq!(after.capture_utc, shard, "the bundle is not relocated");
        assert_eq!(ws.original_path(&id).unwrap(), original);
        assert!(original.is_file());
        assert!(!original.to_string_lossy().contains("2001-09"));

        // The live index row follows the sidecar immediately, not the shard.
        let row = ws
            .db()
            .find_by_uuid(&id.to_string())
            .unwrap()
            .expect("indexed");
        assert_eq!(row.capture_timestamp, 1_000_000_000);
        assert_eq!(row.capture_utc, Some(1_000_000_000));

        // Reopened from disk, the correction holds and the files stay reachable.
        let root = lib.path().to_path_buf();
        drop(ws);
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
        let reopened = ws.asset(&id).unwrap();
        assert_eq!(reopened.sidecar.capture_timestamp, "2001-09-09T01:46:40Z");
        // `open` reconciles a sidecar that no longer names its directory by taking the shard
        // from the directory itself (the month's first instant), which is the same directory.
        assert_ne!(
            reopened.capture_utc, 1_000_000_000,
            "the shard is not the corrected instant"
        );
        assert_eq!(ws.original_path(&id).unwrap(), original);
        assert!(ws.read_plaintext(&id).is_ok());
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);

        // A rebuild from the artifacts on disk projects the same instant the live row did.
        drop(ws);
        std::fs::remove_file(root.join("index/library.sqlite")).unwrap();
        let library = open_library(&root).unwrap();
        rebuild_index(&library).unwrap();
        let rebuilt = library
            .db
            .find_by_uuid(&id.to_string())
            .unwrap()
            .expect("back in the rebuilt index");
        assert_eq!(rebuilt.capture_timestamp, 1_000_000_000);
        assert_eq!(rebuilt.capture_utc, Some(1_000_000_000));
    }

    /// `is_trashed` is the chain replay the workspace applies, exposed once: a soft delete
    /// flips it, a restore flips it back, an unknown id is simply not in trash.
    #[test]
    fn is_trashed_replays_the_chain_and_is_false_for_an_unknown_id() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, id) = workspace_with(&lib, &src, b"\xFF\xD8\xFF trash replay bytes");
        assert!(!ws.is_trashed(&id));
        ws.soft_delete(&id, 30).unwrap();
        assert!(ws.is_trashed(&id));
        ws.restore(&id).unwrap();
        assert!(!ws.is_trashed(&id));
        assert!(!ws.is_trashed(&Uuid::now_v7()));
    }

    #[test]
    fn a_capture_correction_on_an_unknown_asset_is_refused() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, _) = workspace_with(&lib, &src, b"\xFF\xD8\xFF some bytes");
        let err = ws
            .set_capture_timestamp(&Uuid::now_v7(), Timestamp::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(err, LifecycleError::NotFound(_)), "{err:?}");
    }

    // ── The inference seam (`ml::AiTagSink` / `ml::AssetSource`) ─────────────────────────────

    /// The trait forwards must land in exactly the same place the inherent methods do. This is
    /// where a silent behaviour change would hide: the orchestrator now writes through a trait,
    /// so a forward that dropped tags, wrote the wrong namespace, or skipped the signed
    /// `metadata-update` would not show up anywhere in `ml`.
    #[test]
    fn the_ai_tag_sink_impl_round_trips_tags_into_the_signed_sidecar() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, id) = workspace_with(&lib, &src, b"\xFF\xD8\xFF sink round-trip");

        let tag = |t: &str| AiTag {
            tag: t.to_string(),
            model_id: "mobileclip-b".to_string(),
            model_version: "1".to_string(),
        };
        // Written through the seam, not the inherent method.
        AiTagSink::add_ai_tags(&mut ws, &id, vec![tag("beach"), tag("sunset")]).unwrap();

        // …and read back through the inherent surface: same OR-set, still signed, still separate.
        let read_back: std::collections::BTreeSet<String> = ws
            .ai_tags(&id)
            .unwrap()
            .into_iter()
            .map(|(_, t)| t.tag)
            .collect();
        assert_eq!(
            read_back,
            ["beach".to_string(), "sunset".to_string()]
                .into_iter()
                .collect()
        );
        let sidecar = &ws.asset(&id).unwrap().sidecar;
        assert!(
            sidecar.signature.is_some(),
            "the sink emits a signed update"
        );
        assert!(sidecar.tags_user.value().is_empty());

        // An empty batch through the seam is a no-op, exactly as the inherent method is.
        let before = ws.asset(&id).unwrap().chain.records().len();
        AiTagSink::add_ai_tags(&mut ws, &id, Vec::new()).unwrap();
        assert_eq!(ws.asset(&id).unwrap().chain.records().len(), before);

        // An unknown asset is refused rather than silently dropped.
        assert!(AiTagSink::add_ai_tags(&mut ws, &Uuid::now_v7(), vec![tag("ghost")]).is_err());
    }

    /// The read half: the orchestrator reaches an asset's pixels and the workspace's own catalog
    /// through [`AssetSource`], so both must resolve to the inherent surfaces.
    #[test]
    fn the_asset_source_impl_exposes_the_same_plaintext_and_catalog() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (ws, id) = workspace_with(&lib, &src, b"\xFF\xD8\xFF source bytes");

        assert_eq!(
            AssetSource::read_plaintext(&ws, &id).unwrap(),
            ws.read_plaintext(&id).unwrap()
        );
        assert!(AssetSource::read_plaintext(&ws, &Uuid::now_v7()).is_err());
        assert_eq!(
            std::ptr::from_ref(AssetSource::vector_index(&ws)),
            std::ptr::from_ref(ws.db()),
            "the seam must hand out the workspace's own catalog, not a second one"
        );
    }

    /// End-to-end across the inverted edge: `ml::auto_tag` drives a real `Workspace` purely
    /// through the seams, and its output lands in the signed sidecar's AI namespace.
    #[test]
    fn orchestrated_auto_tag_writes_through_the_seam_into_the_signed_sidecar() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, id) = workspace_with(&lib, &src, b"beach");
        let runner = FixtureRunner::new("cpu-reference");
        let reg = Registry::canonical();

        let assigned =
            crate::ml::auto_tag(&mut ws, &runner, &reg, &id, &["beach", "city"], 0.99).unwrap();
        assert_eq!(assigned, vec!["beach".to_string()]);

        // The suggestion is in the signed sidecar, tagged with the canonical pair, and it is
        // *current* under that registry (so the stale filter admits it).
        let ai = ws.ai_tags(&id).unwrap();
        assert_eq!(ai.len(), 1);
        assert_eq!(ai[0].1.tag, "beach");
        assert!(ws.asset(&id).unwrap().sidecar.signature.is_some());
        assert_eq!(
            ws.current_ai_tags(&reg, &id)
                .unwrap()
                .into_iter()
                .map(|t| t.tag)
                .collect::<Vec<_>>(),
            vec!["beach".to_string()]
        );

        // The same workspace also serves the embedding half of the seam.
        crate::ml::embed_and_store(
            &ws,
            &runner,
            &reg,
            &id,
            TaskKind::SemanticSearch,
            CANONICAL_PARTITION,
        )
        .unwrap();
        let hits = crate::ml::semantic_search(&ws, &runner, &reg, "beach", 5, CANONICAL_PARTITION)
            .unwrap();
        assert_eq!(hits[0].asset_id, id.to_string());
    }
}
