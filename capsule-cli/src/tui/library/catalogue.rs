//! What the library browser shows: a plain snapshot of one library, read once when it opens.
//!
//! [`collect`] is the only code here that touches a [`Workspace`]; it runs inside the port, and
//! what it returns is a [`Catalogue`] of owned values the reducer and the view work from without
//! any I/O. The workspace is closed again as soon as the snapshot is taken: it holds the
//! library's lock, which the import and push screens need to take while the browser is open.
//!
//! The per-asset rows come from [`crate::show::rows`], so the detail pane and `capsule show`
//! word every field the same way.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use capsule_core::crypto::provenance::Action as ProvenanceAction;
use capsule_core::domain::GpsDatum;
use capsule_core::sidecar::{GpsSource, RegionShape, StackRole};
use capsule_library::lifecycle::Workspace;
use jiff::Timestamp;
use uuid::Uuid;

use crate::show::Fix;

/// One library as the browser shows it.
///
/// Built once per snapshot by [`Catalogue::new`], which also indexes it by id: every lookup a
/// frame or a key press makes ([`asset`](Self::asset), [`album`](Self::album),
/// [`stack`](Self::stack), [`album_assets`](Self::album_assets), [`timeline`](Self::timeline))
/// is a hash lookup or a borrowed slice, never a scan of the whole library.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Catalogue {
    /// Every asset the workspace manages, newest capture first (ties by id), trashed and
    /// hidden ones included: each view decides what it leaves out.
    assets: Vec<AssetEntry>,
    /// Every album the workspace holds keys for, by name then id.
    albums: Vec<AlbumEntry>,
    /// Every stack an asset is a member of, newest first by its primary's capture.
    stacks: Vec<StackEntry>,
    /// The origin ledger's line for each asset handed to another device whose custody is not
    /// confirmed yet ([`crate::peer::delegation_lines`]); a released one has none.
    delegations: BTreeMap<Uuid, String>,
    /// The lookups by id, boxed so a catalogue in flight as an action stays small.
    index: Box<Index>,
}

/// What [`Catalogue::new`] derives from the lists, once per snapshot.
#[derive(Clone, Default, PartialEq, Eq)]
struct Index {
    /// Position in `assets` by asset id.
    asset_at: HashMap<Uuid, usize>,
    /// Position in `albums` by album id.
    album_at: HashMap<Uuid, usize>,
    /// Position in `stacks` by stack id.
    stack_at: HashMap<Uuid, usize>,
    /// The timeline's asset ids, in `assets` order.
    timeline: Vec<Uuid>,
    /// Each album's assets not in trash, in `assets` order.
    album_assets: HashMap<Uuid, Vec<Uuid>>,
}

/// One asset: what the lists need, the detail rows, and its provenance history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// The asset id.
    pub asset_id: Uuid,
    /// The owning album.
    pub album_id: Uuid,
    /// The effective capture instant (a user's correction when one is recorded); `None` when
    /// the signed value does not parse as RFC 3339, which sorts it last.
    pub captured: Option<Timestamp>,
    /// The original's file extension, lowercase.
    pub ext: String,
    /// The plaintext original on disk, which the preview decodes.
    pub original: Option<PathBuf>,
    /// Hidden from default views.
    pub hidden: bool,
    /// Currently in trash.
    pub in_trash: bool,
    /// The stack this asset is placed in, and its role there.
    pub stack: Option<(Uuid, StackRole)>,
    /// The labelled rows `capsule show` prints for this asset.
    pub rows: Vec<(&'static str, String)>,
    /// The provenance chain, oldest record first.
    pub history: Vec<HistoryEntry>,
    /// The asset's own user labels, sorted.
    pub labels: Vec<String>,
    /// The asset's live regions, by id.
    pub regions: Vec<RegionEntry>,
    /// The capture instant as signed (the user's correction when one is recorded), RFC 3339.
    pub capture_text: String,
    /// The as-imported capture instant, present only beside a user correction.
    pub capture_imported: Option<String>,
    /// The effective location: the user's edit when one is recorded (which may be none),
    /// else the as-imported fix.
    pub location: Option<Place>,
    /// The as-imported location, which an edit never overwrites.
    pub location_imported: Option<Place>,
    /// Whether a user location edit is recorded.
    pub location_edited: bool,
}

/// A location fix, compared by the bits of its coordinates so a snapshot stays `Eq`.
#[derive(Debug, Clone, Copy)]
pub struct Place {
    /// Latitude, in `datum`.
    pub lat: f64,
    /// Longitude, in `datum`.
    pub lon: f64,
    /// The datum the coordinates are in, stored verbatim.
    pub datum: GpsDatum,
    /// Where the fix came from.
    pub source: GpsSource,
}

impl PartialEq for Place {
    fn eq(&self, other: &Self) -> bool {
        self.lat.to_bits() == other.lat.to_bits()
            && self.lon.to_bits() == other.lon.to_bits()
            && self.datum == other.datum
            && self.source == other.source
    }
}

impl Eq for Place {}

impl From<Fix> for Place {
    fn from(fix: Fix) -> Self {
        Self {
            lat: fix.lat,
            lon: fix.lon,
            datum: fix.datum,
            source: fix.source,
        }
    }
}

/// A region's outline, compared by the bits of its coordinates so a snapshot stays `Eq`.
#[derive(Debug, Clone, Copy)]
pub struct Outline(pub RegionShape);

impl Outline {
    fn bits(self) -> [u32; 4] {
        match self.0 {
            RegionShape::Circle { cx, cy, r } => {
                [cx.to_bits(), cy.to_bits(), r.to_bits(), u32::MAX]
            }
            RegionShape::Rect { x, y, w, h } => {
                [x.to_bits(), y.to_bits(), w.to_bits(), h.to_bits()]
            }
        }
    }
}

impl PartialEq for Outline {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self.0, other.0),
            (RegionShape::Circle { .. }, RegionShape::Circle { .. })
                | (RegionShape::Rect { .. }, RegionShape::Rect { .. })
        ) && self.bits() == other.bits()
    }
}

impl Eq for Outline {}

/// One live region of an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionEntry {
    /// The region id.
    pub region_id: Uuid,
    /// Its outline.
    pub outline: Outline,
    /// Its labels, sorted.
    pub labels: Vec<String>,
}

impl AssetEntry {
    /// Whether the timeline lists it: not in trash, not hidden, and not a stack member other
    /// than the stack's primary (the primary stands for its stack).
    #[must_use]
    pub fn in_timeline(&self) -> bool {
        !self.in_trash
            && !self.hidden
            && !matches!(self.stack, Some((_, role)) if role != StackRole::Primary)
    }
}

/// One signed record of an asset's provenance chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The record's self-asserted write time, RFC 3339 as signed.
    pub timestamp: String,
    /// The lifecycle action, as the manifest spells it (`create`, `metadata-update`, …).
    pub action: &'static str,
    /// The device that signed the record.
    pub device: Uuid,
    /// The client that wrote it.
    pub client_version: String,
    /// The trash deadline a `delete` record sets.
    pub retention_until: Option<String>,
}

/// One album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumEntry {
    /// The album id.
    pub album_id: Uuid,
    /// Its name.
    pub name: String,
    /// Its labels, or why its label record could not be read.
    pub labels: Result<Vec<String>, String>,
}

/// One stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackEntry {
    /// The stack id.
    pub stack_id: Uuid,
    /// Its members with their roles: the primary first, then by capture, newest first.
    pub members: Vec<(Uuid, StackRole)>,
    /// Its labels, or why its label record could not be read.
    pub labels: Result<Vec<String>, String>,
}

impl std::fmt::Debug for Catalogue {
    // Counts only: every action is traced, and a whole library's rows would swamp the log. The
    // index is derived from the lists and left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalogue")
            .field("assets", &self.assets.len())
            .field("albums", &self.albums.len())
            .field("stacks", &self.stacks.len())
            .field("delegations", &self.delegations.len())
            .finish_non_exhaustive()
    }
}

impl Catalogue {
    /// A catalogue of `assets` (newest first), `albums` and `stacks`, in the order each view
    /// lists them, indexed by id. The first entry wins where an id repeats.
    #[must_use]
    pub fn new(assets: Vec<AssetEntry>, albums: Vec<AlbumEntry>, stacks: Vec<StackEntry>) -> Self {
        let mut asset_at = HashMap::with_capacity(assets.len());
        let mut timeline = Vec::new();
        let mut album_assets: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for (at, asset) in assets.iter().enumerate() {
            asset_at.entry(asset.asset_id).or_insert(at);
            if asset.in_timeline() {
                timeline.push(asset.asset_id);
            }
            if !asset.in_trash {
                album_assets
                    .entry(asset.album_id)
                    .or_default()
                    .push(asset.asset_id);
            }
        }
        let mut album_at = HashMap::with_capacity(albums.len());
        for (at, album) in albums.iter().enumerate() {
            album_at.entry(album.album_id).or_insert(at);
        }
        let mut stack_at = HashMap::with_capacity(stacks.len());
        for (at, stack) in stacks.iter().enumerate() {
            stack_at.entry(stack.stack_id).or_insert(at);
        }
        Self {
            assets,
            albums,
            stacks,
            delegations: BTreeMap::new(),
            index: Box::new(Index {
                asset_at,
                album_at,
                stack_at,
                timeline,
                album_assets,
            }),
        }
    }

    /// This catalogue with `lines` as each delegated asset's status line, replacing any it had.
    #[must_use]
    pub fn with_delegations(mut self, lines: BTreeMap<Uuid, String>) -> Self {
        self.delegations = lines;
        self
    }

    /// The status line for `asset_id` while it is handed to another device and its custody is
    /// not confirmed; `None` for an asset never delegated, or one the server holds.
    #[must_use]
    pub fn delegation(&self, asset_id: Uuid) -> Option<&str> {
        self.delegations.get(&asset_id).map(String::as_str)
    }

    /// Every asset, newest capture first, trashed and hidden ones included.
    #[must_use]
    pub fn assets(&self) -> &[AssetEntry] {
        &self.assets
    }

    /// Every album, by name then id.
    #[must_use]
    pub fn albums(&self) -> &[AlbumEntry] {
        &self.albums
    }

    /// Every stack, newest first by its primary's capture.
    #[must_use]
    pub fn stacks(&self) -> &[StackEntry] {
        &self.stacks
    }

    /// The asset with `asset_id`, if the library has it.
    #[must_use]
    pub fn asset(&self, asset_id: Uuid) -> Option<&AssetEntry> {
        self.index
            .asset_at
            .get(&asset_id)
            .map(|&at| &self.assets[at])
    }

    /// The album with `album_id`, if the workspace holds its keys.
    #[must_use]
    pub fn album(&self, album_id: Uuid) -> Option<&AlbumEntry> {
        self.index
            .album_at
            .get(&album_id)
            .map(|&at| &self.albums[at])
    }

    /// The stack with `stack_id`, if any asset is placed in it.
    #[must_use]
    pub fn stack(&self, stack_id: Uuid) -> Option<&StackEntry> {
        self.index
            .stack_at
            .get(&stack_id)
            .map(|&at| &self.stacks[at])
    }

    /// The timeline: every asset [`AssetEntry::in_timeline`] admits, newest first.
    #[must_use]
    pub fn timeline(&self) -> &[Uuid] {
        &self.index.timeline
    }

    /// An album's assets that are not in trash, newest first.
    #[must_use]
    pub fn album_assets(&self, album_id: Uuid) -> &[Uuid] {
        self.index
            .album_assets
            .get(&album_id)
            .map_or(&[], Vec::as_slice)
    }
}

/// The manifest's spelling of a provenance action.
#[must_use]
pub const fn action_name(action: ProvenanceAction) -> &'static str {
    match action {
        ProvenanceAction::Create => "create",
        ProvenanceAction::Replace => "replace",
        ProvenanceAction::Delete => "delete",
        ProvenanceAction::MetadataUpdate => "metadata-update",
        ProvenanceAction::DerivativeAdd => "derivative-add",
        ProvenanceAction::DerivativeReplace => "derivative-replace",
        ProvenanceAction::TrashRestore => "trash-restore",
    }
}

/// Take the snapshot of `ws` the browser shows. Reads the in-memory sidecars and chains, and
/// the album and stack label records on disk; writes nothing.
#[tracing::instrument(level = "debug", skip_all)]
pub fn collect(ws: &Workspace) -> Catalogue {
    let mut assets: Vec<AssetEntry> = ws
        .asset_ids()
        .into_iter()
        .filter_map(|asset_id| entry(ws, asset_id))
        .collect();
    // Newest first; an unparsable capture time sorts last; ties by id so the order is stable.
    assets.sort_by(|a, b| {
        b.captured
            .cmp(&a.captured)
            .then_with(|| a.asset_id.cmp(&b.asset_id))
    });

    let mut albums: Vec<AlbumEntry> = ws
        .albums()
        .into_iter()
        .map(|(album_id, name)| AlbumEntry {
            album_id,
            name,
            labels: ws
                .album_labels(&album_id)
                .map(|labels| labels.into_iter().collect())
                .map_err(|e| e.to_string()),
        })
        .collect();
    albums.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.album_id.cmp(&b.album_id))
    });

    // Stacks in the order their first member appears in the (newest-first) asset list, with
    // the primary moved to the front.
    let mut stacks: Vec<StackEntry> = Vec::new();
    let mut stack_at: HashMap<Uuid, usize> = HashMap::new();
    for asset in &assets {
        let Some((stack_id, role)) = asset.stack else {
            continue;
        };
        let member = (asset.asset_id, role);
        match stack_at.get(&stack_id).map(|&at| &mut stacks[at]) {
            Some(stack) if role == StackRole::Primary => stack.members.insert(0, member),
            Some(stack) => stack.members.push(member),
            None => {
                stack_at.insert(stack_id, stacks.len());
                stacks.push(StackEntry {
                    stack_id,
                    members: vec![member],
                    labels: ws
                        .stack_labels(stack_id)
                        .map(|labels| labels.into_iter().collect())
                        .map_err(|e| e.to_string()),
                });
            }
        }
    }

    tracing::debug!(
        target: "capsule_cli::tui",
        assets = assets.len(),
        albums = albums.len(),
        stacks = stacks.len(),
        "library catalogue collected"
    );
    Catalogue::new(assets, albums, stacks)
}

/// One asset's entry, or `None` for an id the workspace does not manage.
fn entry(ws: &Workspace, asset_id: Uuid) -> Option<AssetEntry> {
    let view = crate::show::collect(ws, &asset_id)?;
    let asset = ws.asset(&asset_id)?;
    let history = asset
        .chain
        .records()
        .iter()
        .map(|record| {
            let core = &record.manifest.core;
            HistoryEntry {
                timestamp: core.timestamp.clone(),
                action: action_name(core.action),
                device: core.created_by_device,
                client_version: core.client_version.clone(),
                retention_until: core.retention_until.clone(),
            }
        })
        .collect();
    let mut regions: Vec<RegionEntry> = ws
        .regions(&asset_id)
        .into_iter()
        .map(|(region_id, region)| RegionEntry {
            region_id,
            outline: Outline(region.shape),
            labels: region.labels.value().into_iter().collect(),
        })
        .collect();
    // UUIDv7 ids sort by creation: the regions list in the order they were drawn.
    regions.sort_by_key(|r| r.region_id);
    Some(AssetEntry {
        asset_id,
        album_id: view.album_id,
        captured: view.capture_timestamp.parse().ok(),
        ext: asset.ext.clone(),
        original: ws.original_path(&asset_id),
        hidden: view.hidden,
        in_trash: view.in_trash,
        stack: view.stack,
        rows: crate::show::rows(&view),
        history,
        labels: view.tags_user.clone(),
        regions,
        capture_text: view.capture_timestamp.clone(),
        capture_imported: view.capture_timestamp_imported.clone(),
        location: view.gps.map(Place::from),
        location_imported: view.gps_imported.map(Place::from),
        location_edited: view.location_edited,
    })
}

#[cfg(test)]
mod tests;
