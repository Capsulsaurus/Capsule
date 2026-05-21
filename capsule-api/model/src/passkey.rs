use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passkey {
    /// Unique identifier for the passkey.
    pub id: String,

    /// The ID of the user this key belongs to.
    pub user_id: String,

    /// The raw Credential ID.
    pub cred_id: Vec<u8>,

    /// The public key for signature verification.
    pub public_key: Vec<u8>,

    /// Current signature counter value.
    pub counter: i64,

    /// Human-readable name given to the key.
    pub name: String,

    /// When this key was created.
    pub created_at: DateTime<Utc>,

    /// When this key was last used, if ever.
    pub last_used_at: Option<DateTime<Utc>>,

    /// The AAGUID identifying the authenticator model.
    pub aaguid: Option<uuid::Uuid>,

    /// Indicates if the key can be synced to a cloud provider (e.g., iCloud, Google).
    pub backup_eligible: bool,

    /// Indicates if the key is currently backed up.
    pub backup_state: bool,
}

impl From<entity::passkey::Model> for Passkey {
    fn from(model: entity::passkey::Model) -> Self {
        Passkey {
            id: model.id,
            user_id: model.user_id,
            cred_id: model.cred_id,
            public_key: model.public_key,
            counter: model.counter,
            name: model.name,
            created_at: model.created_at,
            last_used_at: model.last_used_at,
            aaguid: model.aaguid,
            backup_eligible: model.backup_eligible,
            backup_state: model.backup_state,
        }
    }
}
