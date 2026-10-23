use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Primary,
    Raw,
    Video,
    Audio,
    DepthMap,
    Processed,
    Source,
    Alternate,
    Sidecar,
    Proxy,
    Master,
}

#[cfg(test)]
mod tests;
