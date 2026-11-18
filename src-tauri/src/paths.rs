//! Platform-specific locations for configuration and runtime data.
//!
//! Overrides (tests, several instances side by side):
//! - `NOD_CONFIG_DIR`: directory holding `config.toml`
//! - `NOD_HOME`: directory for state, logs, repository cache and locks

use std::env;
use std::path::PathBuf;

pub const APP: &str = "nod";

fn home() -> PathBuf {
    env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn var(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    if let Some(p) = var("NOD_CONFIG_DIR") {
        return p;
    }
    if cfg!(windows) {
        var("APPDATA")
            .unwrap_or_else(|| home().join("AppData").join("Roaming"))
            .join(APP)
    } else if cfg!(target_os = "macos") {
        home().join("Library").join("Application Support").join(APP)
    } else {
        var("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home().join(".config"))
            .join(APP)
    }
}

pub fn data_dir() -> PathBuf {
    if let Some(p) = var("NOD_HOME") {
        return p;
    }
    if cfg!(windows) {
        var("LOCALAPPDATA")
            .unwrap_or_else(|| home().join("AppData").join("Local"))
            .join(APP)
    } else if cfg!(target_os = "macos") {
        home()
            .join("Library")
            .join("Application Support")
            .join(APP)
            .join("data")
    } else {
        var("XDG_STATE_HOME")
            .unwrap_or_else(|| home().join(".local").join("state"))
            .join(APP)
    }
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn home_dir() -> PathBuf {
    home()
}
