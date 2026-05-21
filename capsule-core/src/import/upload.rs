// Related documentations:
// - https://capsule.justinchung.net/design/import-prioritization/

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::import::ImportExecutionPlan;

pub struct UploadExecutionPlan(pub Vec<PathBuf>);

pub struct UploadPriorityConfig {
    /// Whether to prioritize smaller files first
    pub prioritize_smaller_files: bool,
    /// Whether to prioritize newer files first
    pub prioritize_newer_files: bool,
    /// Whether to prioritize files with lower directory depth first
    pub prioritize_lower_depth: bool,
}

impl Default for UploadPriorityConfig {
    fn default() -> Self {
        UploadPriorityConfig {
            prioritize_smaller_files: true,
            prioritize_newer_files: true,
            prioritize_lower_depth: true,
        }
    }
}

pub fn get_upload_ordering(
    plan: &ImportExecutionPlan,
    priority_config: Option<UploadPriorityConfig>,
) -> UploadExecutionPlan {
    // Prioritization strategy:
    // - Lowest directory depth first
    // - Last modified times (newest first), grouped by day, associated files
    // - File size (smallest first)

    let priority_config: UploadPriorityConfig = priority_config.unwrap_or_default();

    let uploadable_paths: HashSet<PathBuf> = plan.get_uploadable_paths().collect();

    // Bucket by directory depth
    let _buckets_by_depth: Vec<Vec<PathBuf>> = {
        if !priority_config.prioritize_lower_depth {
            vec![uploadable_paths.into_iter().collect::<Vec<_>>()]
        } else {
            let mut map: HashMap<usize, Vec<PathBuf>> = HashMap::new();
            for path in uploadable_paths.into_iter() {
                let depth = path.components().count();
                map.entry(depth).or_default().push(path);
            }

            // TODO: Convert into Vec sorted by key (depth)
            let mut vec: Vec<_> = map.into_iter().collect();
            vec.sort_by_key(|(depth, _)| *depth);
            vec.into_iter().map(|(_, paths)| paths).collect()
        }
    };

    // For each bucket, bucket by date modified to the day
    // TODO
    // if priority_config.prioritize_newer_files { ... } else { ... }

    todo!()
}

// TODO: This function uses a lot of overhead memory by design. Need to trace entire call tree and make sure none of the data is excessively large (e.g. 1M+ file paths).
