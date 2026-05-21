use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct C2PAManifest {
    pub data: Vec<u8>,
}

impl std::fmt::Debug for C2PAManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("C2PAManifest")
            .field("data", &format!("<{} bytes>", self.data.len()))
            .finish()
    }
}
