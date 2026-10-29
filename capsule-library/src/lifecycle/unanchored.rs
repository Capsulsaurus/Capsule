//! Sidecars that no provenance chain anchors.
//!
//! [`Workspace::open`] restores an asset from its `.provenance.cbor`; a `{uuid}.cbor` under
//! `media/` with no chain beside it is invisible to every signed operation — it cannot be
//! verified, exported, or uploaded. Such a file is debris: the signed sidecar of a write that
//! died after the sidecar rename and before the chain rename (the chain lands last, so a
//! crash leaves this rather than a torn chain), a torn or foreign file, or a retired unsigned
//! pre-signed-path record. None of them is restorable without discarding or inventing signed
//! state, so each is reported — by `open`, and keylessly by [`unanchored_sidecars`] — and
//! never touched.

use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::sidecar::shape::{self, SidecarShape};
use uuid::Uuid;
use walkdir::WalkDir;

use super::Workspace;

/// The shape of a `{uuid}.cbor` under `media/` that no `.provenance.cbor` anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnanchoredShape {
    /// A signed sidecar whose chain is missing — a write interrupted before its chain landed
    /// — with the `sidecar_schema` it carries.
    SignedWithoutChain {
        /// The value at the sidecar's integer key `0`.
        schema: u16,
    },
    /// Not a signed sidecar: not CBOR, not a map, a torn write, or a retired unsigned
    /// pre-signed-path record. Reported so it is never silently skipped, and never touched.
    Unknown,
}

/// One sidecar file under `media/` that no provenance chain anchors — an asset the workspace
/// cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnanchoredSidecar {
    /// The `{uuid}.cbor` file.
    pub path: PathBuf,
    /// The asset id its stem names, when the stem parses as one.
    pub asset_id: Option<Uuid>,
    /// Which shape the bytes have.
    pub shape: UnanchoredShape,
}

/// Every `{uuid}.cbor` under `root/media` — the sidecars, never the sibling
/// `.provenance.cbor` / `.receipts.cbor` / `.attestations.cbor` logs or the
/// `.derivatives.cbor` bundle. The same filter `rebuild_index` and the add-id sweep apply.
fn sidecar_files(root: &Path) -> Vec<PathBuf> {
    let media = root.join("media");
    if !media.exists() {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = WalkDir::new(&media)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().is_file())
        .filter(|e| crate::library::paths::is_sidecar_file_name(&e.file_name().to_string_lossy()))
        .map(|e| e.path().to_path_buf())
        .collect();
    // Deterministic order regardless of directory-walk order.
    out.sort();
    out
}

/// The sidecars under `root/media` with no `.provenance.cbor` sibling, each probed for its
/// shape, in path order. Used by [`Workspace::open`] to report them.
pub(super) fn find_unanchored(root: &Path) -> Vec<UnanchoredSidecar> {
    let mut out = Vec::new();
    for path in sidecar_files(root) {
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".cbor"))
        else {
            continue;
        };
        let dir = path.parent().unwrap_or(root);
        if dir.join(format!("{stem}.provenance.cbor")).exists() {
            continue;
        }
        let asset_id = Uuid::parse_str(stem).ok();
        let shape = match fs::read(&path) {
            Ok(bytes) => match shape::probe(&bytes) {
                SidecarShape::Signed { schema } => UnanchoredShape::SignedWithoutChain { schema },
                SidecarShape::Unknown => UnanchoredShape::Unknown,
            },
            Err(e) => {
                tracing::warn!(sidecar = %path.display(), error = %e, "unreadable sidecar file");
                UnanchoredShape::Unknown
            }
        };
        out.push(UnanchoredSidecar {
            path,
            asset_id,
            shape,
        });
    }
    out
}

/// The sidecars under the library at `root` that no provenance chain anchors — the set
/// [`Workspace::open`] reports through [`Workspace::unanchored_sidecars`], found without a
/// passphrase or any key material.
///
/// The scan only walks `root/media` and probes each unanchored file's shape, so a keyless
/// caller (`capsule library info`, which opens the library without unlocking it) can report
/// it. A missing `media/` directory is an empty library, not an error; an unreadable sidecar
/// is reported as [`UnanchoredShape::Unknown`], never skipped.
#[must_use]
pub fn unanchored_sidecars(root: &Path) -> Vec<UnanchoredSidecar> {
    let found = find_unanchored(root);
    tracing::debug!(
        library = %root.display(),
        count = found.len(),
        "keyless unanchored-sidecar scan"
    );
    found
}

impl Workspace {
    /// The sidecar files [`open`](Self::open) found under `media/` that no provenance chain
    /// anchors — assets this workspace cannot see, verify, export, or upload. Empty for a
    /// library with no debris.
    pub fn unanchored_sidecars(&self) -> &[UnanchoredSidecar] {
        &self.unanchored
    }
}

#[cfg(test)]
mod tests;
