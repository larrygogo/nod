//! Import an older installation:
//! - a `pr-autoreview` 0.1 install (`config.toml` with `review.approve_policy` / `repo_allowlist` / `repo_denylist`),
//! - or the original script install (`config.json` + `feishu_app.json` + the "PRAutoReview" task).
//!
//! State, logs and the repository cache are moved from the old data directory to the new one.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::config::{self, Config};
use crate::paths;

const OLD_REASONS: [(&str, &str); 10] = [
    ("有高风险问题", "high"),
    ("有 PR 引入的中风险问题", "introduced_medium"),
    ("审查结论不建议 approve", "model_not_recommended"),
    ("CI 失败", "ci_failed"),
    ("CI 还没跑完", "ci_pending"),
    ("还没有 CI 结果", "ci_none_wait"),
    ("这是你自己的 PR，GitHub 不允许 approve", "own_pr"),
    ("PR 不是打开的非草稿状态", "not_open"),
    ("配置为只评论", "approve_disabled"),
    ("配置为只在个人仓库 approve", "approve_repo"),
];
const OLD_CODES: [(&str, &str); 2] = [
    ("policy_comment_only", "approve_disabled"),
    ("policy_personal_only", "approve_repo"),
];

/// Old states stored Chinese sentences or older reason codes; convert them to the current codes.
pub fn convert_reasons(state: &Path) -> Result<usize> {
    if !state.exists() {
        return Ok(0);
    }
    let mut data: Value = serde_json::from_str(&fs::read_to_string(state)?)?;
    let mut n = 0;
    if let Some(prs) = data.get_mut("prs").and_then(Value::as_object_mut) {
        for e in prs.values_mut() {
            // get_mut, not indexing: `e["reasons"]` on a missing key would insert a null
            let Some(rs) = e.get_mut("reasons").and_then(Value::as_array_mut) else {
                continue;
            };
            for r in rs.iter_mut() {
                let s = r.as_str().unwrap_or_default().to_string();
                let new = OLD_REASONS
                    .iter()
                    .chain(OLD_CODES.iter())
                    .find(|(old, _)| *old == s)
                    .map(|(_, code)| code.to_string())
                    .or_else(|| {
                        s.strip_prefix("审查未能完成：")
                            .map(|d| format!("unable:{d}"))
                    });
                if let Some(c) = new {
                    *r = Value::String(c);
                    n += 1;
                }
            }
        }
    }
    fs::write(state, serde_json::to_string_pretty(&data)?)?;
    Ok(n)
}

fn str_list(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// pr-autoreview 0.1 config.toml → current schema.
pub fn upgrade_toml(text: &str) -> Result<Config> {
    let mut v: toml::Value = toml::from_str(text)?;
    let root = v
        .as_table_mut()
        .ok_or_else(|| anyhow!("not a TOML table"))?;
    let mut scope = toml::map::Map::new();
    let mut approve = toml::map::Map::new();
    if let Some(review) = root.get_mut("review").and_then(|r| r.as_table_mut()) {
        let allow = str_list(review.remove("repo_allowlist").as_ref());
        let deny = str_list(review.remove("repo_denylist").as_ref());
        if !allow.is_empty() {
            scope.insert("repos".into(), allow.into());
        }
        if !deny.is_empty() {
            scope.insert("exclude_repos".into(), deny.into());
        }
        let policy = review
            .remove("approve_policy")
            .and_then(|p| p.as_str().map(String::from));
        match policy.as_deref() {
            Some("comment-only") => {
                approve.insert("mode".into(), "never".into());
            }
            Some("personal-only") => {
                approve.insert("repos".into(), vec!["@me/*".to_string()].into());
            }
            _ => {}
        }
    }
    if !scope.is_empty() && !root.contains_key("scope") {
        root.insert("scope".into(), scope.into());
    }
    if !approve.is_empty() && !root.contains_key("approve") {
        root.insert("approve".into(), approve.into());
    }
    let cfg: Config = v.try_into().map_err(|e: toml::de::Error| anyhow!("{e}"))?;
    cfg.validate()
}

/// Original script install (config.json) → current schema.
pub fn from_json(old_dir: &Path) -> Result<Config> {
    let raw =
        fs::read_to_string(old_dir.join("config.json")).context("old config.json not found")?;
    let old: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}'))?;
    let mut c = Config::default();
    let s = |k: &str| old[k].as_str().map(String::from);
    match s("approve_policy").as_deref() {
        Some("comment-only") => c.approve.mode = "never".into(),
        Some("personal-only") => c.approve.repos = vec!["@me/*".into()],
        _ => {}
    }
    if let Some(p) = s("claude").filter(|x| !x.is_empty()) {
        c.review.claude = p;
    }
    c.review.claude_config_dir = s("claude_config_dir").unwrap_or_default();
    c.review.model = s("claude_model").unwrap_or_default();
    if let Some(x) = old["review_timeout_min"].as_f64() {
        c.review.timeout_min = x;
    }
    if let Some(x) = old["max_attempts"].as_u64() {
        c.review.max_attempts = x as u32;
    }
    for (k, f) in [
        ("ci_wait_max_min", &mut c.review.ci_wait_max_min),
        ("ci_none_grace_min", &mut c.review.ci_none_grace_min),
        ("rereview_days", &mut c.review.rereview_days),
    ] {
        if let Some(x) = old[k].as_f64() {
            *f = x;
        }
    }
    let list = |k: &str| -> Vec<String> {
        old[k]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    c.scope.repos = list("repo_allowlist");
    c.scope.exclude_repos = list("repo_denylist");
    let app = [
        old_dir.join("feishu_app.json"),
        old_data_dir().join("feishu_app.json"),
        paths::data_dir().join("feishu_app.json"),
    ]
    .into_iter()
    .find(|p| p.exists());
    if let (Some(app), Some(union)) = (app, s("feishu_union_id")) {
        let creds: Value = serde_json::from_str(&fs::read_to_string(app)?)?;
        c.notify.channel = "feishu".into();
        c.notify.feishu.app_id = creds["app_id"].as_str().unwrap_or_default().into();
        c.notify.feishu.app_secret = creds["app_secret"].as_str().unwrap_or_default().into();
        c.notify.feishu.recipient = format!("union_id:{union}");
    }
    c.validate()
}

/// Where the previous name kept its data: the same platform layout with the old directory name.
pub fn old_data_dir() -> PathBuf {
    let new = paths::data_dir();
    let parts: Vec<String> = new
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    match parts.iter().rposition(|c| c == "nod") {
        Some(i) => {
            let mut p: PathBuf = parts[..i].iter().collect();
            p.push("pr-autoreview");
            for rest in &parts[i + 1..] {
                p.push(rest);
            }
            p
        }
        None => new,
    }
}

pub fn old_config_dir() -> PathBuf {
    paths::config_dir().with_file_name("pr-autoreview")
}

/// Whether a daemon holds the lock in `dir` (i.e. is really running there).
fn old_daemon_running(dir: &Path) -> bool {
    let Ok(f) = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("daemon.lock"))
    else {
        return false;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Move runtime files from the old data directory (if any) into the new one, never overwriting.
fn move_data(from: &Path, to: &Path, msgs: &mut Vec<String>) -> Result<()> {
    if !from.exists() || from == to {
        return Ok(());
    }
    fs::create_dir_all(to)?;
    for name in [
        "state.json",
        "alert.json",
        "track.txt",
        "autoreview.log",
        "repos",
    ] {
        let (src, dst) = (from.join(name), to.join(name));
        if src.exists() && dst.exists() {
            msgs.push(format!(
                "kept the existing {} (not replaced by the old one)",
                dst.display()
            ));
        }
        if src.exists() && !dst.exists() {
            if fs::rename(&src, &dst).is_ok() {
                msgs.push(format!("moved {} -> {}", src.display(), dst.display()));
            } else if src.is_file() {
                fs::copy(&src, &dst)?;
                msgs.push(format!("copied {} -> {}", src.display(), dst.display()));
            }
        }
    }
    Ok(())
}

/// Whether an older installation (pr-autoreview or the original scripts) is there to import.
pub fn old_install_found() -> bool {
    let d = old_config_dir();
    d.join("config.toml").exists() || d.join("config.json").exists()
}

/// Import an older installation from OLD_DIR (default: the previous default config directory).
pub fn run(old_dir: Option<&Path>) -> Result<Vec<String>> {
    let mut msgs = vec![];
    let dir = old_dir.map(PathBuf::from).unwrap_or_else(old_config_dir);
    let cfg = if dir.join("config.toml").exists() {
        upgrade_toml(&fs::read_to_string(dir.join("config.toml"))?)?
    } else if dir.join("config.json").exists() {
        from_json(&dir)?
    } else {
        return Err(anyhow!(
            "no config.toml or config.json in {}",
            dir.display()
        ));
    };

    // Stop the old daemon (it holds files in the old data directory) and remove its scheduled tasks.
    let old_data = old_data_dir();
    let overridden = ["NOD_HOME", "NOD_CONFIG_DIR"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
    if overridden {
        msgs.push(
            "NOD_HOME/NOD_CONFIG_DIR set: not stopping processes or removing scheduled tasks"
                .into(),
        );
    } else if old_data != paths::data_dir() {
        // Only kill what is provably the old daemon: it holds the old directory's lock. A stale daemon.pid may now
        // belong to an unrelated process.
        if old_daemon_running(&old_data) {
            if let Some(pid) = fs::read_to_string(old_data.join("daemon.pid"))
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                crate::util::kill_tree(pid);
                msgs.push(format!("stopped the old daemon (pid {pid})"));
            }
        }
        let _ = fs::remove_file(old_data.join("daemon.pid"));
    }
    if cfg!(windows) && !overridden {
        for task in ["PRAutoReview", "pr-autoreview"] {
            let mut c = std::process::Command::new("powershell");
            crate::util::hide(&mut c);
            let _ = c
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &format!("Unregister-ScheduledTask -TaskName '{task}' -Confirm:$false -ErrorAction SilentlyContinue"),
                ])
                .output();
        }
        msgs.push("removed the old scheduled tasks (if present)".into());
    }
    std::thread::sleep(std::time::Duration::from_secs(1));

    msgs.push(format!("wrote {}", config::save(&cfg)?.display()));
    move_data(&old_data, &paths::data_dir(), &mut msgs)?;
    let n = convert_reasons(&paths::data_dir().join("state.json"))?;
    msgs.push(format!("converted {n} stored reason(s)"));
    Ok(msgs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_pr_autoreview_toml() {
        let c = upgrade_toml(
            "language = \"en\"\n[review]\napprove_policy = \"personal-only\"\nrepo_allowlist = [\"acme/api\"]\n\
             repo_denylist = [\"acme/secret\"]\ntimeout_min = 30\n[notify]\nchannel = \"none\"\n",
        )
        .unwrap();
        assert_eq!(c.approve.repos, vec!["@me/*"]);
        assert_eq!(c.scope.repos, vec!["acme/api"]);
        assert_eq!(c.scope.exclude_repos, vec!["acme/secret"]);
        assert_eq!(c.review.timeout_min, 30.0);
        assert_eq!(
            upgrade_toml("[review]\napprove_policy = \"comment-only\"\n")
                .unwrap()
                .approve
                .mode,
            "never"
        );
        assert_eq!(
            upgrade_toml("[review]\napprove_policy = \"approve-clean\"\n")
                .unwrap()
                .approve
                .mode,
            "auto"
        );
    }

    #[test]
    fn migrates_script_install() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("config.json"), r#"{"approve_policy":"comment-only","claude":"claude","claude_config_dir":"/p","review_timeout_min":30,"feishu_union_id":"on_x","repo_denylist":["a/b"]}"#).unwrap();
        fs::write(
            d.path().join("feishu_app.json"),
            r#"{"app_id":"cli_x","app_secret":"sec"}"#,
        )
        .unwrap();
        let c = from_json(d.path()).unwrap();
        assert_eq!(c.approve.mode, "never");
        assert_eq!(c.review.timeout_min, 30.0);
        assert_eq!(c.notify.channel, "feishu");
        assert_eq!(c.notify.feishu.recipient, "union_id:on_x");
        assert_eq!(c.scope.exclude_repos, vec!["a/b"]);
    }

    #[test]
    fn converts_reasons_without_adding_nulls() {
        let d = tempfile::tempdir().unwrap();
        let st = d.path().join("state.json");
        fs::write(&st, r#"{"prs":{"a/b#1":{"reasons":["有高风险问题","审查未能完成：太大","ci_failed","policy_personal_only"]},"a/b#2":{"status":"posted"}}}"#).unwrap();
        assert_eq!(convert_reasons(&st).unwrap(), 3);
        let text = fs::read_to_string(&st).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["prs"]["a/b#1"]["reasons"],
            serde_json::json!(["high", "unable:太大", "ci_failed", "approve_repo"])
        );
        assert!(!text.contains("null"));
    }

    #[test]
    fn stale_pid_is_not_killed() {
        let d = tempfile::tempdir().unwrap();
        // no lock held: a leftover daemon.pid must not be trusted
        fs::write(d.path().join("daemon.lock"), "").unwrap();
        assert!(!old_daemon_running(d.path()));
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(d.path().join("daemon.lock"))
            .unwrap();
        held.try_lock().unwrap();
        assert!(old_daemon_running(d.path()));
    }

    #[test]
    fn old_dir_is_a_sibling() {
        assert!(old_data_dir().to_string_lossy().contains("pr-autoreview"));
        assert!(old_config_dir().ends_with("pr-autoreview"));
    }
}
