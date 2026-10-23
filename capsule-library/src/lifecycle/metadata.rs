//! User + AI metadata edits — the CRDT writes that ride a signed `metadata-update` record.
//! The only `lifecycle` file that reaches [`capsule_core::ml`], in both directions: it reads the model
//! registry to filter stale suggestions, and it is where [`Workspace`] implements the
//! [`AiTagSink`] seam the inference orchestrator writes through. The orchestrator used to import
//! `Workspace` directly, which — with this file importing `ml` — made the two modules mutually
//! recursive; the trait inverts that edge without moving any behaviour.

use capsule_core::crypto::provenance::Action;
use capsule_core::domain::GpsDatum;
use capsule_core::metadata::crdt::AddId;
use capsule_core::ml::{AiTagSink, AssetSource, ModelId, Registry};
use capsule_core::sidecar::{AiTag, Gps, GpsSource, normalize_label};
use jiff::Timestamp;
use uuid::Uuid;

use super::album::remove_label;
use super::{LifecycleError, Result, Workspace, now_rfc3339};
use crate::db::DatabaseDriver;

impl Workspace {
    /// Add a user tag (OR-set) and emit a `metadata-update` provenance record.
    ///
    /// The tag is an asset label and follows the one label rule
    /// ([`normalize_label`]: NFC, trimmed, 1–256 bytes); a tag the rule refuses is
    /// [`LifecycleError::InvalidLabel`] and nothing is appended. Tags already stored are read
    /// as they are: only new writes are normalized.
    #[tracing::instrument(skip(self, tag), fields(asset_id = %asset_id))]
    pub fn tag_add(&mut self, asset_id: &Uuid, tag: &str) -> Result<()> {
        let tag = normalize_label(tag)?;
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, add_id| {
            s.tags_user.add(tag, add_id);
        })
    }

    /// Remove a user tag: every add of it this device has observed (OR-set observed-remove),
    /// as one `metadata-update` provenance record. Returns how many adds were removed; 0 means
    /// the asset does not carry the tag, and nothing is appended.
    ///
    /// The tag is matched both exactly as given and after the label rule ([`normalize_label`]),
    /// so it names the label [`tag_add`](Self::tag_add) stored and also a tag stored before the
    /// rule existed, spelled as it is listed. A tag the rule refuses, and that the asset does
    /// not carry verbatim, is [`LifecycleError::InvalidLabel`]. An add this device has not yet
    /// seen (a concurrent add on another device) survives the merge, as an OR-set's add-wins
    /// rule says.
    #[tracing::instrument(skip(self, tag), fields(asset_id = %asset_id))]
    pub fn tag_remove(&mut self, asset_id: &Uuid, tag: &str) -> Result<usize> {
        let normalized = normalize_label(tag);
        let raw = tag.to_owned();
        let carried = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .sidecar
            .tags_user
            .entries()
            .into_iter()
            .filter(|(_, el)| *el == raw || normalized.as_ref().is_ok_and(|n| el == n))
            .count();
        if carried == 0 {
            normalized?;
            tracing::debug!(asset_id = %asset_id, "tag remove: the asset does not carry it");
            return Ok(0);
        }
        let normalized = normalized.ok().filter(|n| *n != raw);
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            remove_label(&mut s.tags_user, &raw);
            if let Some(normalized) = normalized {
                remove_label(&mut s.tags_user, &normalized);
            }
        })?;
        tracing::info!(asset_id = %asset_id, removed = carried, "user tag removed as a signed metadata-update");
        Ok(carried)
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

    /// Record the user's capture time for the asset, as a new signed revision: one
    /// `metadata-update` record, with the sidecar re-signed and its sealed blob re-sealed under a
    /// fresh nonce, exactly as any other metadata edit lands.
    ///
    /// The correction is written to the `capture_timestamp_user` LWW register, stamped with this
    /// device id and now, so concurrent corrections from two devices converge under the
    /// `(ts, device_id)` rule. The as-imported `capture_timestamp` is **never overwritten**: it
    /// stays what the original said, and the effective instant
    /// ([`SidecarV1::effective_capture_timestamp`](capsule_core::sidecar::SidecarV1::effective_capture_timestamp))
    /// is the correction.
    ///
    /// The **media bundle is not relocated.** The `media/{YYYY}/{YYYY-MM}` directory is the
    /// shard fixed at import (`AssetState::capture_utc`) from the as-imported value, and the
    /// design treats bucket-vs-timestamp drift after a correction as expected rather than as a
    /// fault (Maintenance — Structural Validation). The index row follows the effective instant
    /// (`asset_row_from_state`), so the timeline shows the correction immediately rather than
    /// after a rebuild.
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
            .effective_capture_timestamp()
            .to_owned();
        let corrected = capture.to_string();
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            s.capture_timestamp_user.set(corrected, ts, device);
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

    /// Re-record the asset's **as-imported** capture time, leaving any user correction alone.
    ///
    /// Reserved for re-deriving what the import should have recorded from the original's own
    /// metadata — the `capsule repair capture-time` pass, for libraries imported while the EXIF
    /// parser could not read `DateTimeOriginal`. It is not a user edit: a user's correction goes
    /// through [`set_capture_timestamp`](Self::set_capture_timestamp), which this never
    /// displaces. One signed `metadata-update`; the bundle is not relocated.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id, capture = %capture))]
    pub fn correct_imported_capture_timestamp(
        &mut self,
        asset_id: &Uuid,
        capture: Timestamp,
    ) -> Result<()> {
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
        tracing::info!(
            asset_id = %asset_id,
            recorded = %recorded,
            corrected = %capture,
            chain_head = ?self.assets[asset_id].chain.head(),
            "as-imported capture timestamp re-derived as a signed metadata-update"
        );
        Ok(())
    }

    /// Record the user's location for the asset: a [`GpsSource::Manual`] fix in `datum`, written
    /// to the `gps_user` LWW register as one signed `metadata-update`, stamped with this device
    /// id and now so concurrent edits converge under the `(ts, device_id)` rule.
    ///
    /// The as-imported `gps` is never overwritten; the effective location
    /// ([`SidecarV1::effective_gps`](capsule_core::sidecar::SidecarV1::effective_gps)) is this edit.
    /// The coordinate is stored verbatim in `datum`, never converted. A latitude outside
    /// `[-90, 90]`, a longitude outside `[-180, 180]`, or a non-finite value is
    /// [`LifecycleError::InvalidLocation`] and nothing is appended.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id, ?datum))]
    pub fn set_location(
        &mut self,
        asset_id: &Uuid,
        lat: f64,
        lon: f64,
        datum: GpsDatum,
    ) -> Result<()> {
        let valid = lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon);
        if !valid {
            tracing::debug!(asset_id = %asset_id, "location edit refused: out of range");
            return Err(LifecycleError::InvalidLocation { lat, lon });
        }
        self.write_location(
            asset_id,
            Some(Gps {
                lat,
                lon,
                source: GpsSource::Manual,
                datum,
            }),
        )
    }

    /// Record that the asset has **no** location, as the user's edit: a stamped `None` in the
    /// `gps_user` register. The as-imported `gps` stays in the sidecar, hidden from the
    /// effective view, so a later edit can restore it.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id))]
    pub fn clear_location(&mut self, asset_id: &Uuid) -> Result<()> {
        self.write_location(asset_id, None)
    }

    fn write_location(&mut self, asset_id: &Uuid, location: Option<Gps>) -> Result<()> {
        if !self.assets.contains_key(asset_id) {
            return Err(LifecycleError::NotFound(format!("asset {asset_id}")));
        }
        let set = location.is_some();
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            s.gps_user.set(location, ts, device);
        })?;
        tracing::info!(
            asset_id = %asset_id,
            set,
            chain_head = ?self.assets[asset_id].chain.head(),
            "location edited as a signed metadata-update"
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
    ///
    /// The copied text becomes an asset label, so it passes through [`normalize_label`]: an AI
    /// tag whose text the label rule refuses cannot be promoted
    /// ([`LifecycleError::InvalidLabel`]), nothing is appended, and the AI entry is untouched.
    #[tracing::instrument(skip(self, tag), fields(asset_id = %asset_id))]
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
        let tag = normalize_label(tag)?;
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
    type Index = DatabaseDriver;

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
mod tests;
