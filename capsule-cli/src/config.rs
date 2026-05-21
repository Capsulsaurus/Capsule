use std::path::Path;

use crate::utils::directories::get_config_file_path;

pub struct Config {
    /// API server endpoint
    pub api_endpoint: String,
    /// Authentication token for API access
    pub auth_token: Option<String>,
    /// User ID
    pub user_id: Option<String>,
}

impl Config {
    /// Creates a new Config instance with default values.
    pub fn from_default_path() -> Result<Self, String> {
        // Get default path
        let config_file_path =
            get_config_file_path().ok_or("Failed to get configuration directory")?;

        // Load configuration from the default path if it exists
        Config::from_path(&config_file_path)
    }

    /// Creates a Config instance from a specific path.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        // For now, we'll just check if the file exists and return a default config
        // In a real implementation, you'd parse a TOML file here
        let config_exists = path.exists();

        // TODO: Implement this for real vv
        Ok(Config {
            api_endpoint: std::env::var("CAPSULE_API_ENDPOINT")
                .unwrap_or_else(|_| "https://api.capsule.com".to_string()),
            auth_token: std::env::var("CAPSULE_AUTH_TOKEN").ok(),
            user_id: if config_exists {
                Some("user@example.com".to_string()) // Mock user
            } else {
                None
            },
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_endpoint: "https://api.capsule.com".to_string(),
            auth_token: None,
            user_id: None,
        }
    }
}
