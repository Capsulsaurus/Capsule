use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMethod {
    FilenameStem,
    ContentIdentifier,
    Timecode,
    Manual,
}

#[cfg(test)]
mod tests;
