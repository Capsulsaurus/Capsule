use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryConfigCbor {
    pub schema_version: u8,
    pub library_name: String,
    pub last_opened_at: i64,
    pub last_scrubbed_at: Option<i64>,
}

#[cfg(test)]
mod tests;
