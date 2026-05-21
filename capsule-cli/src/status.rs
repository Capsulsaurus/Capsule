use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use colored::*;
use eyre::{Result, eyre};
use humansize::{BINARY, format_size};

use crate::config::Config;
use crate::utils::directories::{
    get_cache_dir, get_config_dir, get_config_file_path, get_data_dir,
};
use crate::utils::files::get_available_space;

pub struct StatusInfo {
    pub auth_status: AuthStatus,
    pub local_env_status: LocalEnvStatus,
    pub server_status: ServerStatus,
    pub sync_status: SyncStatus,
}

pub struct AuthStatus {
    /// Username of logged in user
    pub username: Option<String>,
    /// Whether token is valid according to server
    pub token_valid: bool,
    /// Expiry time of the authentication token
    pub token_expires_at: Option<DateTime<Utc>>,
}

pub struct LocalEnvStatus {
    /// Configuration directory
    pub config_dir: Option<PathBuf>,
    /// Path to the configuration file
    pub config_file_path: Option<PathBuf>,
    /// Whether the config file exists
    pub config_file_exists: bool,
    /// Data directory
    pub data_dir: Option<PathBuf>,
    /// Available disk space in data directory
    pub available_disk_space: Option<u64>,
    /// Directory for ephemeral cache files
    pub cache_dir: Option<PathBuf>,
}

pub struct ServerStatus {
    pub api_endpoint: String,
    pub connection_status: ConnectionStatus,
    pub api_version: Option<String>,
    pub response_time: Option<u64>,
    pub server_health: Option<String>,
}

pub struct SyncStatus {
    /// Last sync time based on local system time
    pub last_sync: Option<SystemTime>,
    /// Number of files pending upload
    pub pending_uploads: u32,
    /// Number of files pending download
    pub pending_downloads: u32,
    /// Number of sync conflicts
    pub sync_conflicts: u32,
    /// Number of local files
    pub local_file_count: u32,
    /// Number of remote files
    pub remote_file_count: Option<u32>,
}

#[allow(dead_code)]
pub struct ConfigStatus {
    pub cli_version: String,
    pub config_valid: bool,
    pub config_errors: Vec<String>,
    pub env_vars_status: Vec<(String, bool)>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum ConnectionStatus {
    Connected,
    Disconnected,
    Unknown, // TODO: Is this needed?
    Error(String),
}

impl StatusInfo {
    pub async fn collect() -> Result<Self> {
        let config = Config::from_default_path().map_err(|e| eyre!(e))?;

        Ok(StatusInfo {
            auth_status: AuthStatus::check(&config).await?,
            local_env_status: LocalEnvStatus::check().await?,
            server_status: ServerStatus::check(&config).await?,
            sync_status: SyncStatus::check(&config).await?,
        })
    }

    pub fn display(&self) {
        println!("{}", "=== Capsule CLI Status ===".bright_blue().bold());
        println!();

        self.auth_status.display();
        println!();

        self.local_env_status.display();
        println!();

        self.server_status.display();
        println!();

        self.sync_status.display();
    }
}

impl AuthStatus {
    // TODO: Implement properly
    pub async fn check(config: &Config) -> Result<Self> {
        // Since backend is not implemented, we'll mock the data
        let username = config.user_id.clone(); // TODO: Fetch from server, otherwise use cache
        let token_valid = config.auth_token.is_some(); // Assume token is valid if it exists
        let token_expires_at = if token_valid {
            Some(Utc::now() + chrono::Duration::days(30)) // Mock expiry 30 days from now
        } else {
            None
        };

        Ok(AuthStatus {
            username,
            token_valid,
            token_expires_at,
        })
    }

    pub fn display(&self) {
        println!("{}", "Authentication Status:".bright_yellow().bold());

        if self.username.is_some() {
            println!("  {} {}", "Status:".dimmed(), "Logged in".green());
            if let Some(user) = &self.username {
                println!("  {} {}", "User:".dimmed(), user.cyan());
            }
            if self.token_valid {
                println!("  {} {}", "Token:".dimmed(), "Valid".green());
            } else {
                println!("  {} {}", "Token:".dimmed(), "Invalid".red());
            }
            if let Some(expires) = &self.token_expires_at {
                println!(
                    "  {} {}",
                    "Expires:".dimmed(),
                    expires.to_rfc3339().dimmed()
                );
            }
        } else {
            println!("  {} {}", "Status:".dimmed(), "Not logged in".red());
        }
    }
}

impl LocalEnvStatus {
    pub async fn check() -> Result<Self> {
        let config_dir = get_config_dir();
        let config_file_path = get_config_file_path();
        let config_file_exists = config_file_path
            .clone()
            .map(|dir| dir.join("config.toml"))
            .is_some_and(|path| path.exists());
        let data_dir = get_data_dir();
        let available_disk_space = data_dir.clone().and_then(|dir| get_available_space(&dir));
        let cache_dir = get_cache_dir();

        Ok(LocalEnvStatus {
            config_dir,
            config_file_path,
            config_file_exists,
            data_dir,
            available_disk_space,
            cache_dir,
        })
    }

    pub fn display(&self) {
        println!("{}", "Local Environment Status:".bright_yellow().bold());

        if let Some(config_dir) = &self.config_dir {
            println!(
                "  {} {}",
                "Config Directory:".dimmed(),
                config_dir.display().to_string().cyan()
            );
            if self.config_file_exists {
                println!("  {} {}", "Config File:".dimmed(), "Found".green());
            } else {
                println!("  {} {}", "Config File:".dimmed(), "Not found".yellow());
            }
        } else {
            println!("  {} {}", "Config Directory:".dimmed(), "Not found".red());
        }

        if let Some(config_file_path) = &self.config_file_path {
            println!(
                "  {} {}",
                "Config File Path:".dimmed(),
                config_file_path.display().to_string().dimmed()
            );
        }

        if let Some(data_dir) = &self.data_dir {
            println!(
                "  {} {}",
                "Data Directory:".dimmed(),
                data_dir.display().to_string().dimmed()
            );
        }

        if let Some(cache_dir) = &self.cache_dir {
            println!(
                "  {} {}",
                "Cache Directory:".dimmed(),
                cache_dir.display().to_string().dimmed()
            );
        }

        if let Some(space) = self.available_disk_space {
            println!(
                "  {} {}",
                "Available Space:".dimmed(),
                format_size(space, BINARY).cyan()
            );
        }
    }
}

impl ServerStatus {
    pub async fn check(config: &Config) -> Result<Self> {
        let api_endpoint = config.api_endpoint.clone();

        // TODO: Implement this properly
        // Since backend is not implemented, we'll mock the connection status
        let connection_status = ConnectionStatus::Disconnected;
        let api_version = None;
        let response_time = None;
        let server_health = Some("Backend not implemented".to_string());

        Ok(ServerStatus {
            api_endpoint,
            connection_status,
            api_version,
            response_time,
            server_health,
        })
    }

    pub fn display(&self) {
        println!("{}", "Server/API Status:".bright_yellow().bold());

        println!(
            "  {} {}",
            "API Endpoint:".dimmed(),
            self.api_endpoint.cyan()
        );

        match &self.connection_status {
            ConnectionStatus::Connected => {
                println!("  {} {}", "Connection:".dimmed(), "Connected".green());
            }
            ConnectionStatus::Disconnected => {
                println!("  {} {}", "Connection:".dimmed(), "Disconnected".red());
            }
            ConnectionStatus::Unknown => {
                println!("  {} {}", "Connection:".dimmed(), "Unknown".yellow());
            }
            ConnectionStatus::Error(err) => {
                println!(
                    "  {} {}",
                    "Connection:".dimmed(),
                    format!("Error: {}", err).red()
                );
            }
        }

        if let Some(version) = &self.api_version {
            println!("  {} {}", "API Version:".dimmed(), version.cyan());
        } else {
            println!("  {} {}", "API Version:".dimmed(), "Unknown".dimmed());
        }

        if let Some(time) = self.response_time {
            println!(
                "  {} {}ms",
                "Response Time:".dimmed(),
                time.to_string().cyan()
            );
        }

        if let Some(health) = &self.server_health {
            println!("  {} {}", "Server Health:".dimmed(), health.dimmed());
        }
    }
}

impl SyncStatus {
    pub async fn check(_config: &Config) -> Result<Self> {
        // TODO: Implement this properly
        // Since backend is not implemented, we'll mock the sync status
        Ok(SyncStatus {
            last_sync: None,
            pending_uploads: 0,
            pending_downloads: 0,
            sync_conflicts: 0,
            local_file_count: 0,
            remote_file_count: None,
        })
    }

    pub fn display(&self) {
        println!("{}", "Sync Status:".bright_yellow().bold());

        if let Some(last_sync) = self.last_sync {
            println!(
                "  {} {}",
                "Last Sync:".dimmed(),
                format!("{:?}", last_sync).cyan()
            );
        } else {
            println!("  {} {}", "Last Sync:".dimmed(), "Never".dimmed());
        }

        println!(
            "  {} {}",
            "Pending Uploads:".dimmed(),
            self.pending_uploads.to_string().cyan()
        );
        println!(
            "  {} {}",
            "Pending Downloads:".dimmed(),
            self.pending_downloads.to_string().cyan()
        );

        if self.sync_conflicts > 0 {
            println!(
                "  {} {}",
                "Sync Conflicts:".dimmed(),
                self.sync_conflicts.to_string().red()
            );
        } else {
            println!("  {} {}", "Sync Conflicts:".dimmed(), "None".green());
        }

        println!(
            "  {} {}",
            "Local Files:".dimmed(),
            self.local_file_count.to_string().cyan()
        );

        if let Some(remote_count) = self.remote_file_count {
            println!(
                "  {} {}",
                "Remote Files:".dimmed(),
                remote_count.to_string().cyan()
            );
        } else {
            println!("  {} {}", "Remote Files:".dimmed(), "Unknown".dimmed());
        }
    }
}
