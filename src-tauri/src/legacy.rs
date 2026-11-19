//! Clean-up after the command-line edition (0.1 previews), which installed its own background service:
//!
//! - Windows: a Task Scheduler task `revwarden` running `revwarden ensure` every 2 minutes;
//! - macOS: a launchd agent `~/Library/LaunchAgents/io.github.revwarden.plist`;
//! - Linux: a systemd user unit `revwarden.service`.
//!
//! The desktop app starts reviews itself, so on start-up it removes that service and stops its daemon. The
//! configuration and state are in the same places and are used as they are.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::paths;

pub const TASK_NAME: &str = "revwarden";
pub const LAUNCHD_LABEL: &str = "io.github.revwarden";
pub const SYSTEMD_UNIT: &str = "revwarden.service";

pub fn launchd_path() -> PathBuf {
    paths::home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

pub fn systemd_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home_dir().join(".config"))
        .join("systemd")
        .join("user")
        .join(SYSTEMD_UNIT)
}

fn sh(args: &[&str]) -> Option<(bool, String)> {
    let mut c = Command::new(args[0]);
    c.args(&args[1..]).stdin(Stdio::null());
    crate::util::hide(&mut c);
    let o = c.output().ok()?;
    Some((
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
    ))
}

fn powershell(script: &str) -> Option<(bool, String)> {
    sh(&[
        "powershell",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ])
}

/// The command that tells whether the old task exists (exit code 0 = it does). `schtasks` answers in milliseconds;
/// starting PowerShell for the same question took seconds at every launch, and the worker waits for it.
pub fn task_query() -> [&'static str; 4] {
    ["schtasks", "/Query", "/TN", TASK_NAME]
}

/// Whether the command-line edition's background service is installed.
pub fn service_installed() -> bool {
    if cfg!(windows) {
        sh(&task_query()).is_some_and(|(found, _)| found)
    } else if cfg!(target_os = "macos") {
        launchd_path().exists()
    } else {
        systemd_path().exists()
    }
}

/// Remove the old service and stop its daemon. Returns what was done (empty when there was nothing to do).
///
/// Must run before the app starts its own worker: the old daemon holds `daemon.lock`.
pub fn remove_service() -> Result<Vec<String>> {
    let mut done = vec![];
    if overridden() {
        return Ok(done); // a side-by-side test instance: never touch the real installation
    }
    if service_installed() {
        if cfg!(windows) {
            powershell(&format!(
                "Unregister-ScheduledTask -TaskName '{TASK_NAME}' -Confirm:$false -ErrorAction SilentlyContinue"
            ));
            done.push(format!("removed the Task Scheduler task '{TASK_NAME}'"));
        } else if cfg!(target_os = "macos") {
            let p = launchd_path();
            if let Some((_, uid)) = sh(&["id", "-u"]) {
                sh(&[
                    "launchctl",
                    "bootout",
                    &format!("gui/{}", uid.trim()),
                    &p.to_string_lossy(),
                ]);
            }
            let _ = fs::remove_file(&p);
            done.push(format!("removed the launchd agent {}", p.display()));
        } else {
            sh(&["systemctl", "--user", "disable", "--now", SYSTEMD_UNIT]);
            let _ = fs::remove_file(systemd_path());
            sh(&["systemctl", "--user", "daemon-reload"]);
            done.push(format!("removed the systemd user unit {SYSTEMD_UNIT}"));
        }
    }
    // Whatever is still holding the lock now is a daemon this app did not start (we have not started ours yet).
    let pid = crate::daemon::running_pid();
    if pid > 0 && pid as u32 != std::process::id() {
        crate::util::kill_tree(pid as u32);
        done.push(format!("stopped the old daemon (pid {pid})"));
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    for m in &done {
        crate::info!("legacy: {m}");
    }
    Ok(done)
}

fn overridden() -> bool {
    overridden_by(|v| std::env::var_os(v))
}

fn overridden_by(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    ["NOD_HOME", "NOD_CONFIG_DIR"]
        .iter()
        .any(|v| get(v).is_some_and(|x| !x.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_match_the_cli_edition() {
        assert!(launchd_path().ends_with("Library/LaunchAgents/io.github.revwarden.plist"));
        assert!(systemd_path().ends_with("systemd/user/revwarden.service"));
        assert_eq!(task_query(), ["schtasks", "/Query", "/TN", "revwarden"]);
    }

    #[test]
    fn test_instances_leave_the_real_service_alone() {
        assert!(overridden_by(|v| (v == "NOD_HOME").then(|| "/tmp/x".into())));
        assert!(!overridden_by(|v| (v == "NOD_HOME").then(|| "".into())));
        assert!(!overridden_by(|_| None));
    }
}
