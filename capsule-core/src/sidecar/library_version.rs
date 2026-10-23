use serde::{Deserialize, Serialize};

pub const CURRENT_LIBRARY_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryVersionCbor {
    pub version: u8,
}

impl Default for LibraryVersionCbor {
    fn default() -> Self {
        Self {
            version: CURRENT_LIBRARY_VERSION,
        }
    }
}

#[cfg(test)]
mod tests;
