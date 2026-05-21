use std::path::Path;

use globset::{Glob, GlobSetBuilder};

/// Detect whether file is ignored based on rules similar to .gitignore
pub fn is_ignored_file(path: &Path, ignore_rules: &[String]) -> bool {
    let mut builder = GlobSetBuilder::new();
    for rule in ignore_rules {
        // Skip empty rules
        if rule.trim().is_empty() {
            continue;
        }
        // Add each rule as a glob pattern
        if let Ok(glob) = Glob::new(rule) {
            builder.add(glob);
        }
    }
    let globset = match builder.build() {
        Ok(gs) => gs,
        Err(_) => return false, // If rules are invalid, don't ignore
    };
    globset.is_match(path)
}
