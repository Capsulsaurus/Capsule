use std::path::PathBuf;

use directories::ProjectDirs;

/// Returns project directory for the Capsule CLI.
fn get_project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "Capsule", "Capsule CLI")
}

/// Returns the default configuration directory for the Capsule CLI.
pub fn get_config_dir() -> Option<PathBuf> {
    get_project_dirs().map(|proj_dirs| proj_dirs.config_dir().to_path_buf())
}

/// Returns the config file path for the Capsule CLI.
pub fn get_config_file_path() -> Option<PathBuf> {
    get_config_dir().map(|proj_dirs| proj_dirs.join("config.toml"))
}

/// Returns the default data directory for the Capsule CLI.
pub fn get_data_dir() -> Option<PathBuf> {
    get_project_dirs().map(|proj_dirs| proj_dirs.data_dir().to_path_buf())
}

/// Returns SQlite database file path for the Capsule CLI.
pub fn get_sqlite_db_path() -> Option<PathBuf> {
    get_data_dir().map(|dir| dir.join("capsule.sqlite"))
}

/// Returns the sync directory for the Capsule CLI.
#[allow(dead_code)]
pub fn get_sync_dir() -> Option<PathBuf> {
    get_data_dir().map(|dir| dir.join("sync"))
}

/// Returns the default cache directory for the Capsule CLI.
pub fn get_cache_dir() -> Option<PathBuf> {
    get_project_dirs().map(|proj_dirs| proj_dirs.cache_dir().to_path_buf())
}
