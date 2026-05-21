use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecialStatus {
    /// Special file
    File(SpecialFileStatus),
    /// Special directory
    Directory(SpecialDirectoryStatus),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecialFileStatus {
    Dxo,
}

impl SpecialFileStatus {
    /// Detects from file path if it is a special file.
    /// Does not check if it is actually a file itself.
    pub fn from_path(path: &Path) -> Option<Self> {
        let filename = path.file_name()?.to_str()?;
        match filename {
            "dxo" => Some(SpecialFileStatus::Dxo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecialDirectoryStatus {
    DavinciResolve,
    Git,
}

impl SpecialDirectoryStatus {
    /// Detects from directory path if it is a special directory.
    /// Does not check if it is actually a directory itself.
    pub fn from_path(path: &Path) -> Option<Self> {
        let filename = path.file_name()?.to_str()?;
        match filename {
            ".git" => Some(SpecialDirectoryStatus::Git),
            ".dra" => Some(SpecialDirectoryStatus::DavinciResolve),
            _ => None,
        }
    }
}
