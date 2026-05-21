pub mod mutation;
pub mod query;

use serde::{Deserialize, Serialize};

// Re-export specific structs that consumers expect under service::user::X
pub use mutation::Mutation;
pub use query::Query;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUserArgs {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password_hash: String,
    pub registered_via: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateUserArgs {
    pub username: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub password_hash: Option<String>,
}

// Validation Helpers (Restored)
pub fn is_valid_username(username: &str) -> bool {
    let len = username.len();
    // Allow alphanumeric + underscore/dash. 3-30 chars.
    (3..=30).contains(&len)
        && username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Returns true if the string is a valid email address
pub fn is_valid_email(email: &str) -> bool {
    // TODO: Replace this incomplete check
    email.contains('@') && email.contains('.')
}

pub fn is_valid_password(password: &str) -> bool {
    // TODO: Replace this incomplete check
    password.len() >= 8
}
