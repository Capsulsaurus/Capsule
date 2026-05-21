use std::collections::HashSet;
use std::path::{self, PathBuf};

/// Check if there are nested paths in the given list of paths.
/// You probably want to see Ok(false).
/// Note: Paths are not canonicalized using filesystem and so things like
/// ("../") won't be resolved.
pub fn are_there_nested_paths(paths: &[PathBuf]) -> std::io::Result<bool> {
    // Due to sparsity of the paths, we want to have a flat structure (e.g. HashSet)
    // and fail early
    // Do not resolve symlinks/canonicalize

    let mut ancestors = HashSet::new();

    for path in paths {
        let absolute_path = path::absolute(path)?;
        // Ensure the path is absolute
        debug_assert!(
            absolute_path.is_absolute(),
            "Path is not absolute: {}",
            absolute_path.display()
        );

        // Check if the path is already in the ancestors
        if ancestors.contains(&absolute_path) {
            return Ok(true);
        }

        // Add ancestors of the path to the set
        let current_path_ancestors = absolute_path.ancestors();
        for ancestor in current_path_ancestors.into_iter() {
            ancestors.insert(ancestor.to_path_buf());
        }
    }

    Ok(false)
}

// TODO: Unit test ^^
// - PathBufs that are relative
// - PathBufs that are absolute
// - PathBufs that are nested
// - PathBufs should still fail even if the actual files don't exist
// - Two paths created from different file separators ("/" and "\") should fail
// - Two paths, one with trailing slash and one without should fail
// - Ensure it does not follow paths that point to a symlink
// - Unicode normalization should not affect the result
// - Should case sensitivity be taken into account?
