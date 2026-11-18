//! Nod was called revwarden before. On the first start after the rename, the old installation is carried over so
//! nothing has to be set up again:
//!
//! - settings and third-party plugins (config directory), review records and tracked PRs (data directory) are
//!   copied, never moved, so the old copy stays as it was;
//! - stored secrets are copied from the old keychain service to the new one;
//! - start at sign-in is moved to the new app;
//! - on Windows the old app is stopped and its uninstaller run silently, so the two never review side by side.
//!
//! It runs once: as soon as the new configuration exists, it does nothing.

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths;

/// The previous name, as used for its directories, its keychain service and its sign-in entry.
pub const OLD: &str = "revwarden";

/// Data files worth keeping; the rest (logs, locks, pids, checkouts, the program itself) is recreated.
const SKIP: [&str; 9] = [
    "revwarden.exe",
    "uninstall.exe",
    "daemon.lock",
    "daemon.pid",
    "daemon.out",
    "heartbeat.json",
    "autoreview.log",
    "autoreview.log.1",
    "worker.out",
];

/// `dir` with its last component "nod" replaced by the old name.
pub fn old_dir(dir: &Path) -> Option<PathBuf> {
    (dir.file_name()? == paths::APP).then(|| dir.with_file_name(OLD))
}

/// What happened, for the log. `true` in the second value when the old app started at sign-in.
pub fn run() -> (Vec<String>, bool) {
    if crate::app::isolated() {
        return (vec![], false);
    }
    let (Some(old_cfg), Some(old_data)) =
        (old_dir(&paths::config_dir()), old_dir(&paths::data_dir()))
    else {
        return (vec![], false);
    };
    carry_over(
        &old_cfg,
        &paths::config_dir(),
        &old_data,
        &paths::data_dir(),
    )
}

pub fn carry_over(
    old_cfg: &Path,
    new_cfg: &Path,
    old_data: &Path,
    new_data: &Path,
) -> (Vec<String>, bool) {
    let mut msgs = vec![];
    if new_cfg.join("config.toml").exists() || !old_cfg.join("config.toml").exists() {
        return (msgs, false); // already carried over, or nothing to carry
    }
    match copy_tree(old_cfg, new_cfg) {
        Ok(n) => msgs.push(format!(
            "copied {n} settings files from {}",
            old_cfg.display()
        )),
        Err(e) => {
            msgs.push(format!(
                "could not copy the settings from {}: {e:#}",
                old_cfg.display()
            ));
            return (msgs, false);
        }
    }
    let _ = fs::create_dir_all(new_data);
    let mut copied = 0;
    for entry in fs::read_dir(old_data).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_file()
            && !SKIP.contains(&name.as_str())
            && !new_data.join(&name).exists()
            && fs::copy(entry.path(), new_data.join(&name)).is_ok()
        {
            copied += 1;
        }
    }
    msgs.push(format!(
        "copied {copied} review-record files from {}",
        old_data.display()
    ));
    match crate::config::load_from(&new_cfg.join("config.toml"), false) {
        Ok(c) => {
            let n = crate::secrets::copy_from(OLD, &c);
            msgs.push(format!(
                "copied {n} stored secrets from the old keychain entry"
            ));
        }
        Err(e) => msgs.push(format!(
            "settings copied but not readable, secrets not copied: {e:#}"
        )),
    }
    let autostart = old_start_at_sign_in();
    #[cfg(windows)]
    {
        stop_old(old_data);
        if autostart {
            let _ = std::process::Command::new("reg")
                .args(["delete", RUN_KEY, "/v", OLD, "/f"])
                .output();
        }
        let uninstaller = old_data.join("uninstall.exe");
        if uninstaller.exists() {
            use std::os::windows::process::CommandExt;
            let ok = std::process::Command::new(&uninstaller)
                .arg("/S")
                .creation_flags(crate::util::CREATE_NO_WINDOW)
                .spawn()
                .is_ok();
            msgs.push(if ok {
                "removing the old revwarden app".into()
            } else {
                "could not start the old app's uninstaller; remove revwarden in Settings > Apps"
                    .into()
            });
        }
    }
    (msgs, autostart)
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Whether the old app was set to start at sign-in.
fn old_start_at_sign_in() -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("reg")
            .args(["query", RUN_KEY, "/v", OLD])
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Stop every process started from the old installation directory (the app and its worker).
#[cfg(windows)]
fn stop_old(old_data: &Path) {
    let dir = old_data.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Get-Process -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -and $_.Path.StartsWith('{dir}\\', \
         [StringComparison]::OrdinalIgnoreCase) }} | Stop-Process -Force"
    );
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(crate::util::CREATE_NO_WINDOW)
        .output();
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<usize> {
    fs::create_dir_all(to)?;
    let mut n = 0;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            n += copy_tree(&entry.path(), &target)?;
        } else if !target.exists() {
            fs::copy(entry.path(), &target)?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_directories_are_found_by_name() {
        assert_eq!(
            old_dir(Path::new("/x/nod")),
            Some(PathBuf::from("/x/revwarden"))
        );
        assert_eq!(old_dir(Path::new("/x/custom")), None);
    }

    #[test]
    fn the_old_installation_is_copied_once() {
        crate::secrets::use_fake();
        let t = tempfile::tempdir().unwrap();
        let (oc, nc, od, nd) = (
            t.path().join("oc"),
            t.path().join("nc"),
            t.path().join("od"),
            t.path().join("nd"),
        );
        fs::create_dir_all(oc.join("plugins/p1")).unwrap();
        fs::write(oc.join("config.toml"), "language = \"en\"\n").unwrap();
        fs::write(oc.join("plugins/p1/plugin.toml"), "name = \"p1\"\n").unwrap();
        fs::create_dir_all(od.join("repos")).unwrap();
        fs::write(od.join("state.json"), "{}").unwrap();
        fs::write(od.join("track.txt"), "o/r#1\n").unwrap();
        fs::write(od.join("daemon.lock"), "").unwrap();
        fs::write(od.join("autoreview.log"), "old log").unwrap();
        // a Feishu app secret kept by the old app in its keychain service
        fs::write(
            oc.join("config.toml"),
            "language = \"en\"
[notify.feishu]
app_id = \"cli_x\"
",
        )
        .unwrap();
        crate::secrets::fake_put("revwarden/notify.feishu.app_secret", "S3CRET");
        let (msgs, _) = carry_over(&oc, &nc, &od, &nd);
        assert!(
            msgs.iter().any(|m| m.contains("copied 1 stored secrets")),
            "{msgs:?}"
        );
        assert!(
            nc.join("config.toml").exists() && nc.join("plugins/p1/plugin.toml").exists(),
            "{msgs:?}"
        );
        assert!(nd.join("state.json").exists() && nd.join("track.txt").exists());
        assert!(
            !nd.join("daemon.lock").exists()
                && !nd.join("autoreview.log").exists()
                && !nd.join("repos").exists()
        );
        assert!(
            oc.join("config.toml").exists(),
            "the old copy is left as it was"
        );
        // a second start: the new configuration exists, nothing happens
        fs::write(nc.join("config.toml"), "language = \"zh-CN\"\n").unwrap();
        assert!(carry_over(&oc, &nc, &od, &nd).0.is_empty());
        assert!(fs::read_to_string(nc.join("config.toml"))
            .unwrap()
            .contains("zh-CN"));
    }
}
