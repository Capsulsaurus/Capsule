//! User + AI metadata edits — the CRDT writes that ride a signed `metadata-update` record.
//! The only `lifecycle` file that reaches [`crate::ml`].

use uuid::Uuid;

use super::{LifecycleError, Result, Workspace, now_rfc3339};
use crate::crypto::provenance::action::Action;
use crate::metadata::crdt::AddId;
use crate::ml::{ModelId, Registry};
use crate::sidecar::sidecar_v1::AiTag;

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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

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
        let album = ws.create_album("Trip");
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
        let album = ws.create_album("A");
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
}
