//! Aggregated federated albums — the album-group **view** (SSoT: [Federation — Federated Shared
//! Albums] and [Organization — views]).
//!
//! An *aggregated album* is N ordinary container albums — one per contributor, each homed on
//! its contributor's own server and single-writer-domain as always — that clients present as
//! **one logical album**. There is **zero new server surface**: servers never learn a group
//! exists. The group is client-side metadata, asserted per constituent:
//!
//! - The creator mints a `group_id` (UUIDv7) and shares it in the invite ([`AlbumGroupInvite`]).
//! - Each contributor's client writes an [`AlbumGroupAssertion`] into *their own* container
//!   album's encrypted collaborative-metadata stream (the [operation path]) — a device-signed,
//!   AMK-sealed operation, so the assertion is never legible to any server.
//!
//! There is deliberately **no shared mutable group object**: a union of per-album assertions
//! needs no cross-server consensus. The aggregate is a **computed view** ([`render_aggregate`]),
//! holding no keys and forming no access-control boundary, exactly like every [view album].
//!
//! # Inclusion is injection-proof by construction
//!
//! A constituent appears in the local aggregate **only if** the local user is a *member* of that
//! album (holds its AMK) **and** it asserts the `group_id` ([`Constituent::admits`]). A stranger's
//! album cannot inject itself into anyone's view: without an invite its assertion is never even
//! decryptable, so it is never a member; `member_hint` only tells the client where to *ask*,
//! membership does the admitting.
//!
//! [Federation — Federated Shared Albums]: https://docs/design/federation/#federated-shared-albums-aggregated-albums
//! [Organization — views]: https://docs/design/organization/#system--smart-albums-views
//! [operation path]: https://docs/design/metadata/#how-operations-travel
//! [view album]: https://docs/design/organization/#system--smart-albums-views

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ambient::IdSource;
use crate::cbor;
use crate::crypto::keys::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use crate::metadata::crdt::Lww;

/// A sibling/member hint: where a constituent of the group is *claimed* to live. **Advisory
/// discovery only, never trusted** — it tells a client where to *ask* for an album, but
/// membership (holding the AMK) is what admits a constituent into the view. A hint can name an
/// album a stranger controls and it changes nothing: without an invite that album is not a
/// member, so it is never rendered.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MemberHint {
    /// The constituent album id the hint points at.
    pub album_id: Uuid,
    /// The home server that album is claimed to live on.
    pub home_server: String,
}

/// The album-group assertion — client-side group metadata, asserted **per constituent** and
/// written into that album's collaborative-metadata stream. This type owns the schema (SSoT:
/// [Federation — The Album-Group Assertion]).
///
/// The union of per-album assertions *is* the group; there is no shared mutable group object.
/// `group_name` converges across participants by LWW exactly like `caption_lww`, so a concurrent
/// rename on two constituents reconciles to one name regardless of arrival order.
///
/// [Federation — The Album-Group Assertion]: https://docs/design/federation/#the-album-group-assertion
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumGroupAssertion {
    /// The group this constituent asserts membership of (UUIDv7, minted by the creator).
    pub group_id: Uuid,
    /// The group's display name — an LWW register that converges across every participant's
    /// assertion, like `caption_lww` (SSoT: Metadata — Collaborative Metadata).
    pub group_name: Lww<String>,
    /// Advisory sibling hints — where the *other* constituents are claimed to live. Never
    /// trusted for admission; only used to know where to ask.
    pub member_hint: Vec<MemberHint>,
    /// Device signature over the assertion's canonical bytes — the assertion is a device-signed
    /// operation on the collaborative-metadata op path, provenance-tracked like a
    /// `metadata-update`. `None` until [`sign`](Self::sign) is called.
    pub signature: Option<HybridSignature>,
}

/// The signature-excluded canonical view of an assertion (what [`AlbumGroupAssertion::sign`]
/// covers). A separate struct keeps the covered bytes byte-stable and independent of the
/// `signature` field's presence.
#[derive(Serialize)]
struct AssertionSigningView<'a> {
    group_id: &'a Uuid,
    group_name: &'a Lww<String>,
    member_hint: &'a [MemberHint],
}

impl AlbumGroupAssertion {
    /// A fresh assertion for `group_id` naming the group `name` (stamped by `(ts, device)` for
    /// LWW convergence) with the given advisory sibling hints. Unsigned — the caller
    /// [`sign`](Self::sign)s it.
    pub fn new(
        group_id: Uuid,
        name: impl Into<String>,
        ts: impl Into<String>,
        device: Uuid,
        member_hint: Vec<MemberHint>,
    ) -> Self {
        let mut group_name = Lww::new();
        group_name.set(name.into(), ts, device);
        Self {
            group_id,
            group_name,
            member_hint: sorted_dedup(member_hint),
            signature: None,
        }
    }

    /// The canonical bytes the signature covers (everything except `signature`).
    pub fn signing_bytes(&self) -> Vec<u8> {
        cbor::to_canonical_vec(&AssertionSigningView {
            group_id: &self.group_id,
            group_name: &self.group_name,
            member_hint: &self.member_hint,
        })
        .expect("assertion signing view serializes")
    }

    /// Sign the assertion with the author's identity key, setting `signature`. Re-signable after
    /// a mutation (a rename or hint merge) — the caller clears and re-signs.
    pub fn sign(&mut self, ik: &HybridSigningKey) {
        self.signature = None;
        self.signature = Some(ik.sign(&self.signing_bytes()));
    }

    /// Verify the assertion's signature against the author's identity public key.
    pub fn verify(&self, ik_public: &HybridVerifyingKey) -> bool {
        match &self.signature {
            Some(sig) => ik_public.verify(&self.signing_bytes(), sig),
            None => false,
        }
    }

    /// The converged group name, if any writer has set one.
    pub fn name(&self) -> Option<&str> {
        self.group_name.get().map(String::as_str)
    }

    /// Whether this assertion asserts `group_id` — the *asserts-group* half of the inclusion
    /// rule. An assertion for a different group id (or none) never pulls its album into `group_id`'s
    /// view.
    pub fn asserts(&self, group_id: Uuid) -> bool {
        self.group_id == group_id
    }

    /// Merge another constituent's assertion **for the same group** into this one: LWW-merge the
    /// group name (rename convergence) and union the advisory sibling hints. A mismatched
    /// `group_id` is ignored (returns `false`) — assertions for different groups never cross-talk.
    /// The result is unsigned (the merged value is this reader's reconciled local state, not a new
    /// authored write); [`sign`](Self::sign) before re-emitting it as an operation.
    pub fn merge(&mut self, other: &Self) -> bool {
        if self.group_id != other.group_id {
            return false;
        }
        self.group_name.merge(&other.group_name);
        let mut hints = std::mem::take(&mut self.member_hint);
        hints.extend(other.member_hint.iter().cloned());
        self.member_hint = sorted_dedup(hints);
        self.signature = None;
        true
    }

    /// LWW-rename the group, stamped `(ts, device)`. Clears the signature (the caller re-signs).
    pub fn rename(&mut self, name: impl Into<String>, ts: impl Into<String>, device: Uuid) {
        self.group_name.set(name.into(), ts, device);
        self.signature = None;
    }
}

/// Mint a fresh `group_id` — a UUIDv7 from `ids`, per the assertion schema. The creator mints it
/// once and carries it in the [`AlbumGroupInvite`].
pub fn mint_group_id(ids: IdSource<'_>) -> Uuid {
    ids.uuid_v7()
}

/// Group-aware invite **data** riding the existing album invite: the `group_id` plus the sibling
/// hints a joiner seeds its own assertion from. This carries the group facts as data; the
/// interactive membership ceremony (delivering the AMK, the MLS `Welcome`) belongs to the
/// MLS authority's membership layer and is blocked — same caveat as the organization doc's
/// invitation surface. A joiner turns this into its own constituent's [`AlbumGroupAssertion`] with [`to_assertion`](Self::to_assertion).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumGroupInvite {
    /// The group being joined.
    pub group_id: Uuid,
    /// The group name at invite time (seeds the joiner's LWW register; later renames converge).
    pub group_name: String,
    /// The sibling constituents the joiner should try to read (advisory; membership still admits).
    pub siblings: Vec<MemberHint>,
}

impl AlbumGroupInvite {
    /// Build the assertion a joining contributor writes into **their own** constituent
    /// (`own_album` / `own_server`): asserts the invited `group_id`, seeds the group name, and
    /// carries the invite's siblings plus the joiner's own constituent as advisory hints. Unsigned
    /// — the caller signs it before sealing it onto the op path.
    pub fn to_assertion(
        &self,
        own_album: Uuid,
        own_server: impl Into<String>,
        ts: impl Into<String>,
        device: Uuid,
    ) -> AlbumGroupAssertion {
        let mut hints = self.siblings.clone();
        hints.push(MemberHint {
            album_id: own_album,
            home_server: own_server.into(),
        });
        AlbumGroupAssertion::new(self.group_id, self.group_name.clone(), ts, device, hints)
    }
}

fn sorted_dedup(mut hints: Vec<MemberHint>) -> Vec<MemberHint> {
    hints.sort();
    hints.dedup();
    hints
}

// ── The aggregate view renderer ─────────────────────────────────────────────────────────────
//
// The aggregate is a *computed view*: merged ordering is `capture_timestamp` with `asset_id` as
// the tiebreak, computed at render, nothing stored — so it is idempotent under the
// grouping-convergence requirement by definition. It holds no keys and is no access-control
// boundary, exactly like every view album.

/// One asset a constituent contributes, as the local index (or federated breadcrumb index) holds
/// it. `capture_timestamp` is the merge-ordering key; `asset_id` breaks ties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateAsset {
    /// The asset id.
    pub asset_id: Uuid,
    /// Capture time (Unix seconds) — the primary merge-ordering key.
    pub capture_timestamp: i64,
}

/// One constituent album's contribution to an aggregate, as the local client sees it. Built from
/// the local library for own albums and from a federated read for remote ones; the renderer
/// applies the inclusion rule uniformly.
#[derive(Debug, Clone)]
pub struct Constituent {
    /// The constituent album id.
    pub album_id: Uuid,
    /// The home server this constituent lives on (the per-origin identity).
    pub home_server: String,
    /// Whether the local user is a **member** (holds this album's AMK) — the admitting fact.
    /// A non-member's assertion is undecryptable, so this is `false` and the album is dropped.
    pub is_member: bool,
    /// The reconciled assertion this constituent carries, if the local user can read it.
    pub assertion: Option<AlbumGroupAssertion>,
    /// Whether the constituent's origin is currently reachable. When `false` its entries still
    /// render (from the local index) but are flagged degraded — nothing is removed.
    pub reachable: bool,
    /// Whether the viewer has moderation-**blocked** this origin. A blocked origin's constituent
    /// is dropped from the viewer's aggregate (per-origin moderation), independently of others.
    pub blocked: bool,
    /// This constituent's assets, from the local index / breadcrumb index.
    pub assets: Vec<AggregateAsset>,
}

impl Constituent {
    /// Whether this constituent is admitted into `group_id`'s aggregate: the local user is a
    /// member (holds the AMK) **and** the origin is not moderation-blocked **and** the constituent
    /// asserts `group_id`. This is the injection-proof `member-of ∧ asserts-group` rule (with the
    /// per-origin moderation drop folded in): a stranger's album — not a member — can never inject
    /// itself, and an album that does not assert the group cannot be pulled into the view.
    pub fn admits(&self, group_id: Uuid) -> bool {
        self.is_member
            && !self.blocked
            && self.assertion.as_ref().is_some_and(|a| a.asserts(group_id))
    }
}

/// One merged entry in the rendered aggregate. Carries its origin so a client can group by
/// server and honor the per-origin degraded indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateEntry {
    /// The asset id.
    pub asset_id: Uuid,
    /// The constituent album the asset lives in.
    pub album_id: Uuid,
    /// The origin server the asset is homed on.
    pub home_server: String,
    /// Capture time (Unix seconds) — the merge-ordering key.
    pub capture_timestamp: i64,
    /// `true` when this entry's origin is currently unreachable: it renders from the local index
    /// but the client shows the per-origin "currently unavailable" indicator. Never removed.
    pub degraded: bool,
}

/// The per-origin partial-view indicator for one included constituent. Structured data — the
/// client formats the localized "photos from {home_server} currently unavailable" string from an
/// i18n key; the core never carries the user-facing string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginStatus {
    /// The origin server.
    pub home_server: String,
    /// The constituent album on that origin.
    pub album_id: Uuid,
    /// Whether the origin is currently reachable.
    pub reachable: bool,
}

/// The computed aggregate view over the constituents of one group. Nothing here is stored: it is
/// recomputed at render and is identical across devices given the same inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateView {
    /// The group rendered.
    pub group_id: Uuid,
    /// The LWW-converged group name across every included constituent's assertion.
    pub group_name: Option<String>,
    /// Merged, capture-time-ordered entries (`asset_id` tiebreak) from every included
    /// constituent — including unreachable origins, whose entries are flagged `degraded`.
    pub entries: Vec<AggregateEntry>,
    /// Per-origin reachability for the included constituents (the partial-view indicator).
    pub origins: Vec<OriginStatus>,
    /// The cover asset: the per-viewer override when it names an included asset, else the newest
    /// included asset (max `(capture_timestamp, asset_id)`); `None` for an empty aggregate.
    pub cover: Option<Uuid>,
    /// `true` when at least one included origin is currently unreachable — the aggregate is a
    /// partial view.
    pub partial: bool,
}

/// Render the aggregate for `group_id` over `constituents`, honoring the per-viewer `cover_override`
/// (a `group_id → asset_id` choice from the library-settings document; `None` falls back to the
/// newest included asset).
///
/// Pure and deterministic: membership + assertion admit each constituent, entries merge by
/// `(capture_timestamp, asset_id)`, and the group name is the LWW-convergence of the included
/// assertions. Recomputation is idempotent and identical across devices (grouping-convergence).
#[tracing::instrument(skip(constituents), fields(group_id = %group_id, constituents = constituents.len()))]
pub fn render_aggregate(
    group_id: Uuid,
    constituents: &[Constituent],
    cover_override: Option<Uuid>,
) -> AggregateView {
    let included: Vec<&Constituent> = constituents.iter().filter(|c| c.admits(group_id)).collect();

    // LWW-converge the group name across every included assertion (rename convergence).
    let mut name_reg: Lww<String> = Lww::new();
    for c in &included {
        if let Some(a) = &c.assertion {
            name_reg.merge(&a.group_name);
        }
    }
    let group_name = name_reg.get().cloned();

    // Merge entries; an unreachable origin's entries render degraded, never removed.
    let mut entries: Vec<AggregateEntry> = included
        .iter()
        .flat_map(|c| {
            c.assets.iter().map(move |asset| AggregateEntry {
                asset_id: asset.asset_id,
                album_id: c.album_id,
                home_server: c.home_server.clone(),
                capture_timestamp: asset.capture_timestamp,
                degraded: !c.reachable,
            })
        })
        .collect();
    entries.sort_by_key(|e| (e.capture_timestamp, e.asset_id));

    let mut origins: Vec<OriginStatus> = included
        .iter()
        .map(|c| OriginStatus {
            home_server: c.home_server.clone(),
            album_id: c.album_id,
            reachable: c.reachable,
        })
        .collect();
    // Sort so the view is identical regardless of constituent arrival order (grouping-convergence).
    origins.sort_by(|a, b| (&a.home_server, a.album_id).cmp(&(&b.home_server, b.album_id)));
    let partial = origins.iter().any(|o| !o.reachable);

    // Cover: the per-viewer override iff it names an included asset, else the newest included one.
    let cover = cover_override
        .filter(|id| entries.iter().any(|e| e.asset_id == *id))
        .or_else(|| {
            entries
                .iter()
                .max_by_key(|e| (e.capture_timestamp, e.asset_id))
                .map(|e| e.asset_id)
        });

    AggregateView {
        group_id,
        group_name,
        entries,
        origins,
        cover,
        partial,
    }
}

#[cfg(test)]
mod tests;
