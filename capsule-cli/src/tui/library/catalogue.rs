//! What the library browser shows: a plain snapshot of one library, read once when it opens.
//!
//! [`collect`] is the only code here that touches a [`Workspace`]; it runs inside the port, and
//! what it returns is a [`Catalogue`] of owned values the reducer and the view work from without
//! any I/O. The workspace is closed again as soon as the snapshot is taken: it holds the
//! library's lock, which the import and push screens need to take while the browser is open.
//!
//! The per-asset rows come from [`crate::show::rows`], so the detail pane and `capsule show`
//! word every field the same way.

use std::path::PathBuf;

use capsule_core::crypto::provenance::Action as ProvenanceAction;
use capsule_core::sidecar::StackRole;
use capsule_library::lifecycle::Workspace;
use jiff::Timestamp;
use uuid::Uuid;

/// One library as the browser shows it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Catalogue {
    /// Every asset the workspace manages, newest capture first (ties by id), trashed and
    /// hidden ones included: each view decides what it leaves out.
    pub assets: Vec<AssetEntry>,
    /// Every album the workspace holds keys for, by name then id.
    pub albums: Vec<AlbumEntry>,
    /// Every stack an asset is a member of, newest first by its primary's capture.
    pub stacks: Vec<StackEntry>,
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
    // Counts only: every action is traced, and a whole library's rows would swamp the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalogue")
            .field("assets", &self.assets.len())
            .field("albums", &self.albums.len())
            .field("stacks", &self.stacks.len())
            .finish()
    }
}

impl Catalogue {
    /// The asset with `asset_id`, if the library has it.
    #[must_use]
    pub fn asset(&self, asset_id: Uuid) -> Option<&AssetEntry> {
        self.assets.iter().find(|a| a.asset_id == asset_id)
    }

    /// The album with `album_id`, if the workspace holds its keys.
    #[must_use]
    pub fn album(&self, album_id: Uuid) -> Option<&AlbumEntry> {
        self.albums.iter().find(|a| a.album_id == album_id)
    }

    /// The stack with `stack_id`, if any asset is placed in it.
    #[must_use]
    pub fn stack(&self, stack_id: Uuid) -> Option<&StackEntry> {
        self.stacks.iter().find(|s| s.stack_id == stack_id)
    }

    /// The timeline: every asset [`AssetEntry::in_timeline`] admits, newest first.
    #[must_use]
    pub fn timeline(&self) -> Vec<Uuid> {
        self.assets
            .iter()
            .filter(|a| a.in_timeline())
            .map(|a| a.asset_id)
            .collect()
    }

    /// An album's assets that are not in trash, newest first.
    #[must_use]
    pub fn album_assets(&self, album_id: Uuid) -> Vec<Uuid> {
        self.assets
            .iter()
            .filter(|a| a.album_id == album_id && !a.in_trash)
            .map(|a| a.asset_id)
            .collect()
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
    for asset in &assets {
        let Some((stack_id, role)) = asset.stack else {
            continue;
        };
        let member = (asset.asset_id, role);
        match stacks.iter_mut().find(|s| s.stack_id == stack_id) {
            Some(stack) if role == StackRole::Primary => stack.members.insert(0, member),
            Some(stack) => stack.members.push(member),
            None => stacks.push(StackEntry {
                stack_id,
                members: vec![member],
                labels: ws
                    .stack_labels(stack_id)
                    .map(|labels| labels.into_iter().collect())
                    .map_err(|e| e.to_string()),
            }),
        }
    }

    tracing::debug!(
        target: "capsule_cli::tui",
        assets = assets.len(),
        albums = albums.len(),
        stacks = stacks.len(),
        "library catalogue collected"
    );
    Catalogue {
        assets,
        albums,
        stacks,
    }
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
    })
}

#[cfg(test)]
mod tests;
