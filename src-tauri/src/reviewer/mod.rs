//! Review engines: Claude Code, OpenAI Codex CLI, Gemini CLI, any OpenAI-compatible API, and external plugins.
//!
//! Every engine gets the same checkout (made from the PR's code host) and must return the same structured verdict;
//! what happens with the verdict (comment or approve) is decided by the fixed rules in `review::decide`.

pub mod api;
pub mod claude;
pub mod codex;
pub mod gemini;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::config::Config;
use crate::forge::{Forge, Pr, Remote};
use crate::paths;
use crate::review::Verdict;
use crate::util::{run, run_raw, RunOpts};

/// Where checkouts live: the data directory, or a separate one for dry runs (see [`Workspace::root`]).
pub fn workspace_root(dry_run: bool) -> PathBuf {
    if dry_run {
        paths::data_dir().join("dry-run")
    } else {
        paths::data_dir()
    }
}

fn ctx_of(wt: &Path) -> PathBuf {
    wt.with_file_name(format!("{}-ctx", wt.file_name().unwrap().to_string_lossy()))
}

/// git in `cwd`. `env` carries the auth header for hosts that need one: a partial clone downloads file contents
/// lazily (worktree, diff), so every git command of a checkout must be able to authenticate, not only clone/fetch.
fn git(
    args: &[&str],
    cwd: &Path,
    secs: u64,
    env: Option<&HashMap<String, String>>,
) -> Result<String> {
    let mut v = vec!["git"];
    v.extend_from_slice(args);
    run(
        &v,
        RunOpts {
            cwd: Some(cwd),
            timeout: Duration::from_secs(secs),
            env,
            ..Default::default()
        },
    )
}

/// The environment for git commands against `remote`: the auth header goes in through GIT_CONFIG_* variables, so it
/// is never on a command line (visible to other processes) or saved in the mirror's config.
fn git_env(remote: &Remote) -> Option<HashMap<String, String>> {
    match remote {
        Remote::Git {
            header: Some(h), ..
        } => {
            let mut env: HashMap<String, String> = std::env::vars().collect();
            env.insert("GIT_CONFIG_COUNT".into(), "1".into());
            env.insert("GIT_CONFIG_KEY_0".into(), "http.extraHeader".into());
            env.insert("GIT_CONFIG_VALUE_0".into(), h.clone());
            env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
            Some(env)
        }
        // still never wait for an interactive credential prompt in the background
        _ => {
            let mut env: HashMap<String, String> = std::env::vars().collect();
            env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
            Some(env)
        }
    }
}

/// Checkouts of pull requests from one code-host account: a partial mirror per repository under `repos/`, and a
/// worktree per review under `work/` with the diff and commit list next to it.
pub struct Workspace {
    pub forge: Arc<dyn Forge>,
    /// Account id; the original GitHub account ("github") keeps the original directory names.
    pub account: String,
    /// Holds `repos/` and `work/`. A dry run from the window uses its own root: it runs in another process at the
    /// same time as the worker, and sharing the worktree of a PR would delete the files the worker is reviewing.
    pub root: PathBuf,
}

impl Workspace {
    fn prefix(&self, pr: &Pr) -> String {
        let r = pr.repo.replace('/', "__");
        if self.account == "github" {
            r
        } else {
            format!("{}__{r}", self.account)
        }
    }

    fn mirror(&self, pr: &Pr) -> PathBuf {
        self.root.join("repos").join(self.prefix(pr))
    }

    pub fn checkout(&self, pr: &Pr) -> Result<(PathBuf, PathBuf)> {
        fs::create_dir_all(self.root.join("repos"))?;
        fs::create_dir_all(self.root.join("work"))?;
        let mirror = self.mirror(pr);
        let remote = self.forge.remote(&pr.repo)?;
        let env = git_env(&remote);
        if !mirror.join(".git").exists() {
            let _ = fs::remove_dir_all(&mirror);
            let m = mirror.to_string_lossy().to_string();
            match &remote {
                Remote::Gh { host } => {
                    let repo = if host.is_empty() {
                        pr.repo.clone()
                    } else {
                        format!("{host}/{}", pr.repo)
                    };
                    run(
                        &[
                            "gh",
                            "repo",
                            "clone",
                            &repo,
                            &m,
                            "--",
                            "--no-checkout",
                            "--filter=blob:none",
                        ],
                        RunOpts {
                            timeout: Duration::from_secs(1800),
                            ..Default::default()
                        },
                    )?;
                }
                Remote::Git { url, .. } => {
                    run(
                        &[
                            "git",
                            "clone",
                            "-q",
                            "--no-checkout",
                            "--filter=blob:none",
                            url,
                            &m,
                        ],
                        RunOpts {
                            timeout: Duration::from_secs(1800),
                            env: env.as_ref(),
                            ..Default::default()
                        },
                    )?;
                }
            }
        }
        run(
            &[
                "git",
                "fetch",
                "-q",
                "origin",
                &format!("+refs/heads/{0}:refs/remotes/origin/{0}", pr.base),
                &format!(
                    "+{}:refs/autoreview/pr-{}",
                    self.forge.head_ref(pr.number),
                    pr.number
                ),
            ],
            RunOpts {
                cwd: Some(&mirror),
                timeout: Duration::from_secs(1800),
                env: env.as_ref(),
                ..Default::default()
            },
        )?;
        let wt = self
            .root
            .join("work")
            .join(format!("{}-{}", self.prefix(pr), pr.number));
        self.cleanup(pr, &wt);
        // Symlinks in the PR are checked out as plain files, so they cannot point the reviewer outside the checkout.
        git(
            &["config", "core.symlinks", "false"],
            &mirror,
            60,
            env.as_ref(),
        )?;
        let wts = wt.to_string_lossy().to_string();
        git(
            &["worktree", "add", "-q", "--detach", &wts, &pr.head],
            &mirror,
            1800,
            env.as_ref(),
        )?;
        let base = git(
            &["merge-base", "HEAD", &format!("origin/{}", pr.base)],
            &wt,
            120,
            env.as_ref(),
        )?
        .trim()
        .to_string();
        let ctx = ctx_of(&wt);
        let _ = fs::remove_dir_all(&ctx);
        fs::create_dir_all(&ctx)?;
        fs::write(ctx.join("merge_base.txt"), &base)?;
        fs::write(
            ctx.join("diffstat.txt"),
            git(
                &["diff", "--stat=200", &base, "HEAD"],
                &wt,
                600,
                env.as_ref(),
            )?,
        )?;
        fs::write(
            ctx.join("diff.patch"),
            git(&["diff", "-U8", &base, "HEAD"], &wt, 600, env.as_ref())?,
        )?;
        fs::write(
            ctx.join("commits.txt"),
            git(
                &["log", "--format=%h %an %s", &format!("{base}..HEAD")],
                &wt,
                120,
                env.as_ref(),
            )?,
        )?;
        Ok((wt, ctx))
    }

    pub fn cleanup(&self, pr: &Pr, wt: &Path) {
        let _ = fs::remove_dir_all(ctx_of(wt));
        let mirror = self.mirror(pr);
        if wt.exists() {
            let w = wt.to_string_lossy().to_string();
            let _ = run_raw(
                &["git", "worktree", "remove", "--force", &w],
                RunOpts {
                    cwd: Some(&mirror),
                    ..Default::default()
                },
            );
            let _ = fs::remove_dir_all(wt);
        }
        if mirror.exists() {
            let _ = run_raw(
                &["git", "worktree", "prune"],
                RunOpts {
                    cwd: Some(&mirror),
                    ..Default::default()
                },
            );
        }
    }
}

pub trait Reviewer: Send + Sync {
    /// Returns (checkout, context dir with diff/commit files).
    fn checkout(&self, pr: &Pr) -> Result<(PathBuf, PathBuf)>;
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
    ) -> Result<Verdict>;
    fn cleanup(&self, pr: &Pr, wt: &Path);
}

/// The environment an engine runs with: everything except variables that look like credentials, minus `keep`
/// (the engine's own login, e.g. OPENAI_API_KEY for Codex).
pub fn safe_env(keep: &[&str]) -> HashMap<String, String> {
    let secret = |k: &str| {
        let u = k.to_uppercase();
        if keep.iter().any(|x| x.eq_ignore_ascii_case(k)) {
            return false;
        }
        [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "API_KEY",
            "APIKEY",
            "CREDENTIAL",
        ]
        .iter()
        .any(|w| u.contains(w))
            || [
                "GH_", "GITHUB_", "GITLAB_", "GITEA_", "LARK", "FEISHU", "NOD_",
            ]
            .iter()
            .any(|p| u.starts_with(p))
    };
    std::env::vars().filter(|(k, _)| !secret(k)).collect()
}

/// Display name of the code host, for the prompt.
pub fn platform(kind: &str) -> &'static str {
    match kind {
        "gitee" => "Gitee",
        "gitea" => "Gitea",
        "github" => "GitHub",
        _ => "Git",
    }
}

/// Picks the engine for each pull request (per-repository rules, then the default) and runs it on the shared
/// workspace.
pub struct Router {
    pub ws: Arc<Workspace>,
    pub platform: &'static str,
    pub me: String,
}

impl Router {
    pub fn engine(&self, cfg: &Config, pr: &Pr) -> Result<Box<dyn Engine>> {
        let name = cfg.engine_for(&pr.repo, &self.me);
        Ok(match name.as_str() {
            "claude" => Box::new(claude::Claude),
            "codex" => Box::new(codex::Codex),
            "gemini" => Box::new(gemini::Gemini),
            "api" => Box::new(api::Api),
            n if n.starts_with("ext:") => {
                Box::new(crate::plugins::ExternalEngine::new(cfg, &n[4..])?)
            }
            n => return Err(anyhow!("unknown review engine {n:?}")),
        })
    }
}

/// One review engine: turn a prompt and a checkout into a verdict.
pub trait Engine {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict>;
}

impl Reviewer for Router {
    fn checkout(&self, pr: &Pr) -> Result<(PathBuf, PathBuf)> {
        self.ws.checkout(pr)
    }

    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
    ) -> Result<Verdict> {
        self.engine(cfg, pr)?
            .ask(cfg, pr, wt, ctx, discussion, self.platform)
    }

    fn cleanup(&self, pr: &Pr, wt: &Path) {
        self.ws.cleanup(pr, wt)
    }
}

/// For engines without a native structured-output option: how to answer.
pub fn json_only(lang: &str) -> String {
    let lead = if lang == "en" {
        "Reply with exactly one JSON object that follows this JSON Schema, with no other text:"
    } else {
        "只回复一个符合下面 JSON Schema 的 JSON 对象，不要输出任何其他文字："
    };
    format!(
        "{lead}
```json
{}
```",
        crate::review::SCHEMA.trim()
    )
}

/// Read a verdict an engine wrote as JSON text (possibly wrapped in a ```json fence or surrounded by prose).
pub fn verdict_from_text(text: &str) -> Result<Verdict> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .map(|x| x.trim_end().trim_end_matches("```"))
        .unwrap_or(t);
    let start = t
        .find('{')
        .ok_or_else(|| anyhow!("no JSON verdict in the reply"))?;
    let end = t
        .rfind('}')
        .ok_or_else(|| anyhow!("no JSON verdict in the reply"))?;
    let v: serde_json::Value = serde_json::from_str(&t[start..=end])
        .map_err(|e| anyhow!("the verdict is not valid JSON: {e}"))?;
    crate::review::validate_verdict(&v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (code review): a dry run from the window used the worker's directories, and checking out deleted
    /// the worktree the worker was reviewing in.
    #[test]
    fn dry_runs_check_out_apart_from_the_worker() {
        let (real, dry) = (workspace_root(false), workspace_root(true));
        assert_ne!(real, dry);
        assert!(
            !real.join("work").starts_with(dry.join("work"))
                && !dry.join("work").starts_with(real.join("work"))
        );
    }

    #[test]
    fn discussion_keeps_its_newest_part() {
        assert_eq!(crate::util::keep_last("评论一二三", 3), "一二三");
        assert_eq!(crate::util::keep_last("ab", 5), "ab");
    }

    #[test]
    fn verdicts_from_text() {
        let v = verdict_from_text(
            "Here you go:\n```json\n{\"summary\":\"ok\",\"findings\":[],\"approve_recommended\":true,\"unable_reason\":\"\"}\n```",
        )
        .unwrap();
        assert!(v.approve_recommended);
        assert!(verdict_from_text("no json").is_err());
        assert!(verdict_from_text("{\"summary\": 1}").is_err());
    }

    #[test]
    fn env_hides_credentials_but_keeps_the_engines_login() {
        std::env::set_var("RW_TEST_API_KEY", "x");
        std::env::set_var("RW_TEST_OTHER_TOKEN", "y");
        let env = safe_env(&["RW_TEST_API_KEY"]);
        assert!(env.contains_key("RW_TEST_API_KEY"));
        assert!(!env.contains_key("RW_TEST_OTHER_TOKEN"));
    }

    #[test]
    fn git_auth_stays_off_the_command_line() {
        let env = git_env(&Remote::Git {
            url: "https://x/y.git".into(),
            header: Some("Authorization: Basic abc".into()),
        })
        .unwrap();
        assert_eq!(env["GIT_CONFIG_KEY_0"], "http.extraHeader");
        assert_eq!(env["GIT_CONFIG_VALUE_0"], "Authorization: Basic abc");
        let gh = git_env(&Remote::Gh {
            host: String::new(),
        })
        .unwrap();
        assert!(!gh.contains_key("GIT_CONFIG_COUNT"));
        assert_eq!(gh["GIT_TERMINAL_PROMPT"], "0");
    }
}
