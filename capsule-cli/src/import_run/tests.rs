//! The TUI's import over a real workspace: what lands, what a second run skips, and a source
//! that is not there (that file's outcome, not a failed run).

use capsule_core::crypto::primitives::Argon2Params;

use super::*;
use crate::import_report::FileImportStatus;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-import-run-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A library at a fast KDF cost, with its default album, as the CLI opens one.
fn workspace(scratch: &Scratch) -> Workspace {
    let lib = scratch.0.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let mut ws = Workspace::create_with_params(
        &lib,
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    ws
}

/// A folder of two photos.
fn sources(scratch: &Scratch) -> PathBuf {
    let dir = scratch.0.join("photos");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.jpg"), b"first photo bytes").unwrap();
    std::fs::write(dir.join("b.jpg"), b"second photo bytes").unwrap();
    dir
}

#[test]
fn a_folder_is_imported_and_a_second_run_skips_it_as_duplicates() {
    let scratch = Scratch::new();
    let mut ws = workspace(&scratch);
    let folder = sources(&scratch);

    let first = import_sources(&mut ws, None, std::slice::from_ref(&folder)).unwrap();
    assert_eq!(first.imported, 2, "{first:?}");
    assert_eq!(ws.asset_ids().len(), 2, "both landed in the library");
    assert!(
        first
            .outcomes
            .iter()
            .all(|o| o.status == FileImportStatus::Imported && o.asset_id.is_some()),
        "{first:?}"
    );

    let second = import_sources(&mut ws, None, &[folder]).unwrap();
    assert_eq!(second.imported, 0);
    assert_eq!(second.duplicates, 2, "{second:?}");
    assert_eq!(ws.asset_ids().len(), 2, "nothing was imported twice");
}

#[test]
fn a_source_that_is_not_there_is_reported_failed_not_skipped() {
    let scratch = Scratch::new();
    let mut ws = workspace(&scratch);
    let missing = scratch.0.join("no-such-folder");

    let report = import_sources(&mut ws, None, &[missing]).unwrap();

    assert_eq!(report.errors, 1, "{report:?}");
    let [outcome] = report.outcomes.as_slice() else {
        panic!("one outcome: {report:?}");
    };
    assert_eq!(outcome.status, FileImportStatus::Failed);
    assert!(outcome.error.is_some());
    assert!(ws.asset_ids().is_empty());
}
