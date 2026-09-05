use std::path::PathBuf;
use std::time::SystemTime;

use colored::*;
use eyre::{Result, eyre};
use humansize::{BINARY, format_size};
use jiff::Timestamp;

use crate::remote::RemoteConfig;
use crate::session::SessionStore;
use crate::utils::directories::{get_cache_dir, get_config_dir, get_data_dir};
use crate::utils::files::get_available_space;

pub(crate) struct StatusInfo {
    pub auth_status: AuthStatus,
    pub local_env_status: LocalEnvStatus,
    pub server_status: ServerStatus,
    pub sync_status: SyncStatus,
}

pub(crate) struct AuthStatus {
    /// Whether a session is persisted at all. This — not the access token's freshness — is
    /// what "logged in" means: an expired access token still refreshes silently on the next
    /// command, so treating it as logged-out would be wrong.
    pub signed_in: bool,
    /// Whether the persisted access token is still within its recorded lifetime.
    pub access_token_fresh: bool,
    /// Expiry of the persisted access token.
    pub token_expires_at: Option<Timestamp>,
}

pub(crate) struct LocalEnvStatus {
    /// Configuration directory (holds the persisted session)
    pub config_dir: Option<PathBuf>,
    /// Data directory
    pub data_dir: Option<PathBuf>,
    /// Available disk space in data directory
    pub available_disk_space: Option<u64>,
    /// Directory for ephemeral cache files
    pub cache_dir: Option<PathBuf>,
}

pub(crate) struct ServerStatus {
    pub api_endpoint: String,
    pub connection_status: ConnectionStatus,
    pub api_version: Option<String>,
    pub response_time: Option<u64>,
    pub server_health: Option<String>,
}

pub(crate) struct SyncStatus {
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
pub(crate) struct ConfigStatus {
    pub cli_version: String,
    pub config_valid: bool,
    pub config_errors: Vec<String>,
    pub env_vars_status: Vec<(String, bool)>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum ConnectionStatus {
    Connected,
    Disconnected,
    Unknown, // TODO: Is this needed?
    Error(String),
}

impl StatusInfo {
    pub(crate) async fn collect() -> Result<Self> {
        let remote = RemoteConfig::from_env();
        let store = crate::session_store()?;

        Ok(StatusInfo {
            auth_status: AuthStatus::check(&store)?,
            local_env_status: LocalEnvStatus::check().await?,
            server_status: ServerStatus::check(&remote).await?,
            sync_status: SyncStatus::check().await?,
        })
    }

    pub(crate) fn display(&self) {
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
    /// Report on the session the CLI actually persisted.
    ///
    /// This used to read `CAPSULE_AUTH_TOKEN` from the environment and invent a 30-day
    /// expiry, so after a successful `capsule auth login` it still reported "Not logged in"
    /// — the status command contradicted the command that had just succeeded. It now reads
    /// `session.json`, the same store `auth login` writes and every networked command
    /// resumes from.
    ///
    /// `token_valid` is a *local* judgement: the access token's recorded expiry is in the
    /// future. It deliberately does not call the server — a status command must work
    /// offline, and an expired access token is not the same as a dead session, because the
    /// refresh token may still be good.
    pub(crate) fn check(store: &SessionStore) -> Result<Self> {
        let Some(session) = store.load()? else {
            return Ok(AuthStatus {
                signed_in: false,
                access_token_fresh: false,
                token_expires_at: None,
            });
        };

        let expires_at = Timestamp::from_second(session.access_expires_at_unix).ok();

        Ok(AuthStatus {
            signed_in: true,
            access_token_fresh: expires_at.is_some_and(|t| t > Timestamp::now()),
            token_expires_at: expires_at,
        })
    }

    pub(crate) fn display(&self) {
        println!("{}", "Authentication Status:".bright_yellow().bold());

        if !self.signed_in {
            println!("  {} {}", "Status:".dimmed(), "Not logged in".red());
            return;
        }

        println!("  {} {}", "Status:".dimmed(), "Logged in".green());
        if self.access_token_fresh {
            println!("  {} {}", "Access token:".dimmed(), "Valid".green());
        } else {
            // Not an error state: the next networked command refreshes it transparently.
            println!(
                "  {} {}",
                "Access token:".dimmed(),
                "Expired (refreshes on next use)".yellow()
            );
        }
        if let Some(expires) = &self.token_expires_at {
            println!("  {} {}", "Expires:".dimmed(), expires.to_string().dimmed());
        }
    }
}

impl LocalEnvStatus {
    pub(crate) async fn check() -> Result<Self> {
        let config_dir = get_config_dir();
        let data_dir = get_data_dir();
        let available_disk_space = data_dir.clone().and_then(|dir| get_available_space(&dir));
        let cache_dir = get_cache_dir();

        Ok(LocalEnvStatus {
            config_dir,
            data_dir,
            available_disk_space,
            cache_dir,
        })
    }

    pub(crate) fn display(&self) {
        println!("{}", "Local Environment Status:".bright_yellow().bold());

        if let Some(config_dir) = &self.config_dir {
            println!(
                "  {} {}",
                "Config Directory:".dimmed(),
                config_dir.display().to_string().cyan()
            );
        } else {
            println!("  {} {}", "Config Directory:".dimmed(), "Not found".red());
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

/// How long to wait for the server before calling it unreachable. Short on purpose: this is
/// a status command, and a hung probe is worse than a fast "Disconnected".
const SERVER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

impl ServerStatus {
    /// Probe the server's unauthenticated `/v1/version` through the SDK's generated client.
    ///
    /// This previously hardcoded `Disconnected` and "Backend not implemented" — a claim that
    /// stopped being true once the server shipped, and one a user could not tell apart from
    /// a genuine outage. The probe needs no session, so it reports honestly whether or not
    /// the user is logged in.
    pub(crate) async fn check(remote: &RemoteConfig) -> Result<Self> {
        // `sync_endpoint` is the bare server origin (see `RemoteConfig::from_env`), which is
        // exactly the base the generated operation paths hang off.
        let api_endpoint = remote.sync_endpoint.clone();

        // Over the SDK's one HTTP client rather than the generated `Client::new`, so the probe
        // carries the same protocol handshake every other request does; `/v1/version` is
        // exempt from the gate, and a probe that spoke differently from the calls it precedes
        // would tell the user nothing about them.
        let client = match capsule_sdk::net::http_client()
            .map_err(|error| error.to_string())
            .and_then(|http| {
                capsule_sdk::rest::Client::with_client(http, &api_endpoint)
                    .map_err(|error| error.to_string())
            }) {
            Ok(client) => client,
            Err(error) => {
                return Ok(ServerStatus {
                    api_endpoint,
                    connection_status: ConnectionStatus::Error(error),
                    api_version: None,
                    response_time: None,
                    server_health: None,
                });
            }
        };

        let started = std::time::Instant::now();
        let probe = tokio::time::timeout(SERVER_PROBE_TIMEOUT, client.get_version()).await;
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(match probe {
            Ok(Ok(response)) => ServerStatus {
                api_endpoint,
                connection_status: ConnectionStatus::Connected,
                api_version: Some(response.into_inner().version),
                response_time: Some(elapsed),
                server_health: None,
            },
            Ok(Err(error)) => ServerStatus {
                api_endpoint,
                connection_status: ConnectionStatus::Error(error.to_string()),
                api_version: None,
                response_time: Some(elapsed),
                server_health: None,
            },
            Err(_timed_out) => ServerStatus {
                api_endpoint,
                connection_status: ConnectionStatus::Disconnected,
                api_version: None,
                response_time: None,
                server_health: Some(format!(
                    "no response within {}s",
                    SERVER_PROBE_TIMEOUT.as_secs()
                )),
            },
        })
    }

    pub(crate) fn display(&self) {
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
                    format!("Error: {err}").red()
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
    /// Report what the local sync store actually holds.
    ///
    /// This previously returned all zeros with a "backend not implemented" comment, so a
    /// user who had just synced hundreds of assets saw a report claiming nothing existed.
    /// The store is the sync feed's local projection, so the counts come from it directly.
    ///
    /// Upload/download/conflict counts stay at zero and are honest about it: the CLI has no
    /// upload path yet (slice `S-D18`), and the sync feed is apply-only with no conflict
    /// surface, so there is nothing to count rather than something we decline to count.
    pub(crate) async fn check() -> Result<Self> {
        let db = crate::db::init_sqlite()
            .await
            .map_err(|e| eyre!("Failed to open the local sync store: {e}"))?;

        // Tombstones included: a synced-then-deleted asset is still something the store
        // knows about, and hiding it would understate what a re-sync would reconcile.
        let rows = crate::remote::list(&db, true).await?;
        let local_file_count = u32::try_from(rows.len()).unwrap_or(u32::MAX);

        Ok(SyncStatus {
            // The store records a feed cursor, not a wall-clock sync time; reporting a
            // fabricated timestamp is what this function used to do.
            last_sync: None,
            pending_uploads: 0,
            pending_downloads: 0,
            sync_conflicts: 0,
            local_file_count,
            remote_file_count: None,
        })
    }

    pub(crate) fn display(&self) {
        println!("{}", "Sync Status:".bright_yellow().bold());

        if let Some(last_sync) = self.last_sync {
            println!(
                "  {} {}",
                "Last Sync:".dimmed(),
                format!("{last_sync:?}").cyan()
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

#[cfg(test)]
mod tests {
    use capsule_sdk::auth::PersistedSession;
    use secrecy::SecretString;

    use super::*;

    /// A store at a unique temp path, matching the convention in `session.rs`'s tests
    /// (no extra dev-dependency for something the standard library covers).
    fn temp_store(tag: &str) -> SessionStore {
        SessionStore::new(std::env::temp_dir().join(format!(
            "capsule-cli-status-{tag}-{}.json",
            nanoid::nanoid!()
        )))
    }

    fn session_expiring_at(unix: i64) -> PersistedSession {
        PersistedSession {
            access_token: SecretString::from("access"),
            refresh_token: SecretString::from("refresh"),
            access_expires_at_unix: unix,
        }
    }

    #[test]
    fn auth_status_reports_logged_out_without_a_session() {
        let status = AuthStatus::check(&temp_store("logged-out")).expect("check");
        assert!(!status.signed_in);
        assert!(!status.access_token_fresh);
        assert!(status.token_expires_at.is_none());
    }

    /// The regression this slice exists for: `auth status` used to read an environment
    /// variable rather than the session store, so it reported "Not logged in" immediately
    /// after `auth login` had succeeded.
    #[test]
    fn auth_status_reflects_a_session_that_login_persisted() {
        let store = temp_store("persisted");
        let future = Timestamp::now().as_second() + 900;
        store.save(&session_expiring_at(future)).expect("save");

        let status = AuthStatus::check(&store).expect("check");
        assert!(status.signed_in);
        assert!(status.access_token_fresh);
        assert_eq!(
            status.token_expires_at.map(jiff::Timestamp::as_second),
            Some(future)
        );
    }

    /// An expired *access* token is not a logged-out session — the refresh token still
    /// works, and the next networked command renews it transparently. Reporting this as
    /// logged out would send users to re-authenticate for no reason.
    #[test]
    fn an_expired_access_token_is_still_signed_in() {
        let store = temp_store("expired");
        let past = Timestamp::now().as_second() - 60;
        store.save(&session_expiring_at(past)).expect("save");

        let status = AuthStatus::check(&store).expect("check");
        assert!(status.signed_in, "a persisted session is still a session");
        assert!(!status.access_token_fresh);
    }

    /// An unreachable server must be reported as unreachable, not as a hardcoded
    /// "Backend not implemented" — a user cannot tell that apart from a real outage.
    #[tokio::test]
    async fn server_status_reports_an_unreachable_endpoint() {
        // Port 1 on loopback refuses immediately; this asserts the failure path without
        // waiting out the probe timeout.
        let remote = RemoteConfig {
            auth_endpoint: "http://127.0.0.1:1/v1/auth".to_string(),
            sync_endpoint: "http://127.0.0.1:1".to_string(),
            upload_endpoint: "http://127.0.0.1:1/v1/upload".to_string(),
            albums_endpoint: "http://127.0.0.1:1/v1/albums".to_string(),
            protocol_version: crate::remote::DEFAULT_PROTOCOL_VERSION.to_string(),
        };
        let status = ServerStatus::check(&remote).await.expect("check");
        assert!(
            matches!(
                status.connection_status,
                ConnectionStatus::Error(_) | ConnectionStatus::Disconnected
            ),
            "expected a failure status, got {:?}",
            status.connection_status
        );
        assert!(status.api_version.is_none());
        assert!(
            status.server_health.as_deref() != Some("Backend not implemented"),
            "the mocked health string must be gone"
        );
    }
}
