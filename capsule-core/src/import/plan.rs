use std::path::PathBuf;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::import::{
    GroupingResult, ScanResult, UploadExecutionPlan, UploadPriorityConfig, apply_grouping_rules,
    get_upload_ordering,
};
use crate::metadata::AssetType;
use crate::utils::file::are_there_nested_paths;

/// <path> -> (<selected action>, <reason for action>)
pub type ImportActionMapping = IndexMap<PathBuf, (Option<ImportAction>, ScanResult)>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportActionPlan {
    /// A list of files to import, along with their intended actions.
    mapping: ImportActionMapping,
}

impl ImportActionPlan {
    /// Creates a new import plan with an empty mapping.
    pub fn new(mapping: ImportActionMapping) -> Self {
        ImportActionPlan { mapping }
    }

    /// Returns the list of files and their actions in the import plan.
    pub fn mapping(&self) -> &ImportActionMapping {
        &self.mapping
    }

    /// Returns a mutable reference to the mapping.
    pub fn mapping_mut(&mut self) -> &mut ImportActionMapping {
        &mut self.mapping
    }

    // /// Update the action for a specific file in the import plan.
    // pub fn update_action(&mut self, file: &PathBuf, action: Option<ImportAction>)
    // {     if let Some(entry) = self.mapping.iter_mut().find(|(f, _)| f ==
    // file)     {
    //         entry.1 = action;
    //     }
    //     else
    //     {
    //         self.add_file(file.clone(), action);
    //     }
    // }

    // /// Checks if import plan is ready
    // pub fn is_ready(&self) -> bool {
    //     !self.mapping.is_empty() && self.mapping.iter().all(|(_, action)|
    // action.is_some()) }

    /// Returns the number of files in the import plan.
    pub fn len(&self) -> usize {
        self.mapping.len()
    }

    /// Checks if the import plan is empty.
    pub fn is_empty(&self) -> bool {
        self.mapping.is_empty()
    }

    /// Applies standard grouping rules to the import action plan.
    pub fn apply_grouping_rules(&mut self) -> GroupingResult<()> {
        apply_grouping_rules(self)
    }
}

impl Default for ImportActionPlan {
    fn default() -> Self {
        ImportActionPlan::new(IndexMap::new())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportAction {
    /// Import the file into collection
    New(NewImportConfig),
    // /// Update an existing entry with the file.
    // Update,
    /// Skip the file, leaving it unchanged.
    Skip,
}

pub type ImportExecutionMapping = Vec<(PathBuf, ImportAction)>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportExecutionPlan {
    /// A list of files to import, along with their intended actions.
    mapping: ImportExecutionMapping,
}

impl ImportExecutionPlan {
    /// Normalize mapping (e.g. order by file name)
    pub fn normalize(&mut self) -> &mut Self {
        // Sort by file name
        self.mapping.sort_by_key(|(path, _)| path.clone());
        // self.mapping.sort_by(|(path_a, _), (path_b, _)| {
        //     path_a.cmp(path_b)
        // }); // TODO: See if this is faster than sort_by_key
        self
    }

    /// Returns the list of files and their actions in the import plan.
    pub fn mapping(&self) -> &ImportExecutionMapping {
        &self.mapping
    }

    /// Get uploadable paths only
    pub fn get_uploadable_paths(&self) -> impl Iterator<Item = PathBuf> {
        self.mapping
            .iter()
            .filter_map(|(path, action)| match action {
                ImportAction::New(_) => Some(path.clone()),
                ImportAction::Skip => None,
            })
    }

    /// Returns upload ordering
    pub fn get_upload_ordering(
        &self,
        upload_priority_config: Option<UploadPriorityConfig>,
    ) -> UploadExecutionPlan {
        get_upload_ordering(self, upload_priority_config)
    }
}

#[derive(Debug, Error)]
pub enum ImportExecutionPlanError {
    #[error("Import action plan is empty")]
    EmptyPlan,
    #[error("There are nested paths in the import plan")]
    NestedPaths,
    #[error("No action specified for file: {0}")]
    NoActionForFile(PathBuf),
    #[error("Invalid import action mapping: {0}")]
    Io(#[from] std::io::Error),
}

impl TryFrom<ImportActionPlan> for ImportExecutionPlan {
    type Error = ImportExecutionPlanError;

    fn try_from(plan: ImportActionPlan) -> Result<Self, Self::Error> {
        // No empty plans allowed
        if plan.is_empty() {
            return Err(ImportExecutionPlanError::EmptyPlan);
        }

        // Ensure there are no paths that nest with each other
        // Note: We don't need to worry about following symlinks
        let path_keys: Vec<_> = plan.mapping.keys().map(|p| p.to_path_buf()).collect();
        if !(are_there_nested_paths(&path_keys)?) {
            return Err(ImportExecutionPlanError::NestedPaths);
        }

        // Ensure all paths have an action
        let mut mapping: ImportExecutionMapping = Vec::new();
        for (file, (action, _result)) in plan.mapping.into_iter() {
            if let Some(action) = action {
                mapping.push((file, action));
            } else {
                return Err(ImportExecutionPlanError::NoActionForFile(file));
            }
        }

        let mut plan = ImportExecutionPlan { mapping };
        plan.normalize();

        Ok(plan)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewImportConfig {
    /// Asset type
    pub asset_type: AssetType,
    /// Album ID to import into
    pub album_id: Option<String>,
    /// Group ID to import into
    pub group_id: Option<String>,
}

impl NewImportConfig {
    /// Creates a new import configuration with the specified file type.
    pub fn new(asset_type: AssetType) -> Self {
        NewImportConfig {
            asset_type,
            album_id: None,
            group_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportExecutionSummary(pub Vec<(PathBuf, ImportResult)>);

impl ImportExecutionSummary {
    /// Returns number of successful imports.
    pub fn success_count(&self) -> usize {
        self.0
            .iter()
            .filter(|(_, result)| matches!(result, ImportResult::Success))
            .count()
    }

    /// Returns total number of imports.
    pub fn total_count(&self) -> usize {
        self.0.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportResult {
    /// The file was successfully imported.
    Success,
    /// The file was skipped.
    Skipped,
    /// An error occurred during import.
    Error(String), // TODO: Use more specific error type
}
