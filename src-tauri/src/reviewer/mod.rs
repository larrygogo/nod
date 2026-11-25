//! Review engines: Claude Code, OpenAI Codex CLI, Gemini CLI, any OpenAI-compatible API, and external plugins.
//!
//! Every engine gets the same checkout (made from the PR's code host) and must return the same structured verdict;
//! what happens with the verdict (comment or approve) is decided by the fixed rules in `review::decide`.

pub mod api;
pub mod api_tools;
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
use crate::util::{now, run, run_raw, RunOpts};

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

/// The engine called `name`: built-in, or `ext:<plugin>`.
pub fn engine_named(cfg: &Config, name: &str) -> Result<Box<dyn Engine>> {
    Ok(match name {
        "claude" => Box::new(claude::Claude),
        "codex" => Box::new(codex::Codex),
        "gemini" => Box::new(gemini::Gemini),
        "api" => Box::new(api::Api),
        n if n.starts_with("ext:") => Box::new(crate::plugins::ExternalEngine::new(cfg, &n[4..])?),
        n => return Err(anyhow!("unknown review engine {n:?}")),
    })
}

/// Run `primary`; when it cannot run at all (out of quota, signed out, not installed) and a fallback engine is set,
/// run that instead. A wrong answer or a timeout says nothing about whether another engine would do better, and
/// trying one would only double the wait. When both fail, the error carries both messages, the primary's first.
pub fn with_fallback(
    cfg: &Config,
    key: &str,
    primary: &str,
    run: impl Fn(&str) -> Result<Verdict>,
) -> Result<Verdict> {
    let err = match run(primary) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    let fb = cfg.review.fallback.as_str();
    if fb.is_empty() || fb == primary || classify(&format!("{err:#}"), now()) == Trouble::Other {
        return Err(err);
    }
    crate::warn!("{key}: {primary} cannot review now ({err:#}); using {fb} instead");
    run(fb).map_err(|e2| {
        anyhow!(
            "{err:#}
{fb} (fallback): {e2:#}"
        )
    })
}

/// Why an engine could not give a verdict, as far as its message tells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trouble {
    /// Out of quota or rate limited: it will work again later, by itself. `until` = when, if the message says.
    Quota { until: Option<f64> },
    /// Cannot run here at all (not installed, signed out, a bad key) until someone fixes it.
    Unavailable,
    /// Anything else: a timeout, a malformed answer, a crash.
    Other,
}

/// Sort an engine's error message (`now` in unix seconds, for "try again in 2 hours").
pub fn classify(msg: &str, now: f64) -> Trouble {
    use regex_lite::Regex;
    let m = msg.to_lowercase();
    let quota = [
        "usage limit",
        "rate limit",
        "rate_limit",
        "ratelimit",
        "quota",
        "credit balance",
        "insufficient_quota",
        "insufficient balance",
        "resource_exhausted",
        "hit your limit",
        "limit reached",
        "too many requests",
        "out of credits",
        "billing",
        "余额不足",
        "额度",
        "限流",
    ];
    if quota.iter().any(|w| m.contains(w)) || Regex::new(r"\b429\b").unwrap().is_match(&m) {
        return Trouble::Quota {
            until: reset_time(&m, now),
        };
    }
    let unavailable = [
        "not found on path",
        "no such file",
        "not logged in",
        "not signed in",
        "please run /login",
        "please log in",
        "please login",
        "invalid api key",
        "invalid x-api-key",
        "invalid_api_key",
        "incorrect api key",
        "authentication",
        "unauthorized",
        "api key not valid",
        "oauth token has expired",
        // Claude Code when its sign-in expired and the refresh token no longer works:
        // "Failed to authenticate: OAuth session expired and could not be refreshed"
        "failed to authenticate",
        "session expired",
        "could not be refreshed",
        // Gemini CLI with no sign-in configured
        "please set an auth method",
    ];
    if unavailable.iter().any(|w| m.contains(w)) || Regex::new(r"\b401\b").unwrap().is_match(&m) {
        return Trouble::Unavailable;
    }
    Trouble::Other
}

/// The next time it is `h:min` on this machine's clock, after `now`.
fn next_local(now: f64, h: u32, min: u32) -> Option<f64> {
    use chrono::{Local, TimeZone};
    let base = Local.timestamp_opt(now as i64, 0).single()?;
    let mut day = base.date_naive();
    for _ in 0..2 {
        let t = day
            .and_hms_opt(h, min, 0)?
            .and_local_timezone(Local)
            .earliest()?;
        if t.timestamp() as f64 > now {
            return Some(t.timestamp() as f64);
        }
        day = day.succ_opt()?;
    }
    None
}

/// When a quota message says the limit lifts: "…|1736280000" (Claude), "try again in 2 hours 5 minutes" (Codex),
/// "retry in 23.5s" / "retry after 60 seconds" (Gemini, APIs). Kept within a minute and a day from now.
fn reset_time(m: &str, now: f64) -> Option<f64> {
    use regex_lite::Regex;
    let at = if let Some(c) = Regex::new(r"\|(\d{10})\b").unwrap().captures(m) {
        c[1].parse::<f64>().ok()?
    } else if let Some(c) = Regex::new(r"resets\s+(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)")
        .unwrap()
        .captures(m)
    {
        // Claude Code: "5-hour limit reached · resets 3pm (Asia/Shanghai)": the next 3 pm, in this machine's time
        let h: u32 = c[1].parse().ok()?;
        let min: u32 = c.get(2).map_or(Some(0), |x| x.as_str().parse().ok())?;
        let h = h % 12 + if &c[3] == "pm" { 12 } else { 0 };
        next_local(now, h, min)?
    } else {
        let c = Regex::new(
            r"(?:try again|retry|resets?)\s+(?:in|after)\s+((?:[\d.]+\s*[a-z]+[\s,]*(?:and\s+)?)+)",
        )
        .unwrap()
        .captures(m)?;
        let mut secs = 0.0;
        for p in Regex::new(r"([\d.]+)\s*([a-z]+)")
            .unwrap()
            .captures_iter(&c[1])
        {
            let n: f64 = p[1].parse().ok()?;
            let unit = &p[2];
            secs += n * if unit.starts_with('d') {
                86400.0
            } else if unit.starts_with('h') {
                3600.0
            } else if unit.starts_with("ms") {
                0.001
            } else if unit.starts_with('m') {
                60.0
            } else {
                1.0
            };
        }
        if secs <= 0.0 {
            return None;
        }
        now + secs
    };
    Some(at.clamp(now + 60.0, now + 86400.0))
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
        let primary = cfg.engine_for(&pr.repo, &self.me);
        with_fallback(cfg, &pr.key(), &primary, |name| {
            engine_named(cfg, name)?.ask(cfg, pr, wt, ctx, discussion, self.platform)
        })
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
        let plain = git_env(&Remote::Git {
            url: "https://x/y.git".into(),
            header: None,
        })
        .unwrap();
        assert!(!plain.contains_key("GIT_CONFIG_COUNT"));
        assert_eq!(plain["GIT_TERMINAL_PROMPT"], "0");
    }

    #[test]
    fn engine_errors_are_sorted() {
        let now = 1_000_000.0;
        assert_eq!(
            classify(
                "Claude returned an error: \"Claude AI usage limit reached|1000900\"",
                now
            ),
            Trouble::Quota { until: None } // not ten digits: no time
        );
        assert_eq!(
            classify(
                "Claude returned an error: \"Claude AI usage limit reached|1736280000\"",
                1_736_270_000.0
            ),
            Trouble::Quota {
                until: Some(1_736_280_000.0)
            }
        );
        assert_eq!(
            classify(
                "Codex exited 1: You've hit your usage limit. Try again in 2 hours 5 minutes.",
                now
            ),
            Trouble::Quota {
                until: Some(now + 7500.0)
            }
        );
        assert_eq!(
            classify(
                "Gemini exited 1: 429 RESOURCE_EXHAUSTED. Please retry in 30s.",
                now
            ),
            Trouble::Quota {
                until: Some(now + 60.0)
            } // at least a minute
        );
        assert_eq!(
            classify("HTTP 429 Too Many Requests", now),
            Trouble::Quota { until: None }
        );
        // "resets 3pm": the next 3 pm on this machine, within a day
        let Trouble::Quota { until: Some(t) } =
            classify("You've hit your limit · resets 3pm (Asia/Shanghai)", now)
        else {
            panic!("no reset time")
        };
        use chrono::{Local, TimeZone, Timelike};
        let lt = Local.timestamp_opt(t as i64, 0).unwrap();
        assert!((lt.hour(), lt.minute()) == (15, 0) && t > now && t <= now + 86400.0);
        assert_eq!(
            classify("Your credit balance is too low", now),
            Trouble::Quota { until: None }
        );
        assert_eq!(
            classify("Claude Code (\"claude\") not found on PATH", now),
            Trouble::Unavailable
        );
        assert_eq!(
            classify("Invalid API key · Please run /login", now),
            Trouble::Unavailable
        );
        assert_eq!(
            classify("HTTP 401: unauthorized", now),
            Trouble::Unavailable
        );
        // reqgenie#255, 2026-11-24
        assert_eq!(
            classify(
                "Claude returned an error: \"Failed to authenticate: OAuth session expired and could not be refreshed\"",
                now
            ),
            Trouble::Unavailable
        );
        assert_eq!(
            classify("Claude did not finish within 20 minutes", now),
            Trouble::Other
        );
        assert_eq!(
            classify("Claude gave no structured verdict: line 1429", now),
            Trouble::Other
        );
    }

    #[test]
    fn the_fallback_engine_steps_in_only_when_the_first_cannot_run() {
        use std::cell::RefCell;
        let mut cfg = Config::default();
        let ran = RefCell::new(vec![]);
        let run = |fail: &'static str| {
            let ran = &ran;
            move |name: &str| -> Result<Verdict> {
                ran.borrow_mut().push(name.to_string());
                if name == "claude" {
                    anyhow::bail!("{fail}")
                }
                Ok(crate::review::tests::clean())
            }
        };
        // none set: the error stands
        assert!(with_fallback(&cfg, "k", "claude", run("usage limit reached")).is_err());
        cfg.review.fallback = "codex".into();
        // out of quota, signed out: the fallback reviews
        for msg in [
            "usage limit reached",
            "Claude Code (\"claude\") not found on PATH",
        ] {
            ran.borrow_mut().clear();
            assert!(with_fallback(&cfg, "k", "claude", run(msg)).is_ok());
            assert_eq!(*ran.borrow(), ["claude", "codex"]);
        }
        // a timeout: no fallback
        ran.borrow_mut().clear();
        assert!(
            with_fallback(&cfg, "k", "claude", run("did not finish within 20 minutes")).is_err()
        );
        assert_eq!(*ran.borrow(), ["claude"]);
        // the fallback is the engine that failed: not run twice
        ran.borrow_mut().clear();
        cfg.review.fallback = "claude".into();
        assert!(with_fallback(&cfg, "k", "claude", run("usage limit reached")).is_err());
        assert_eq!(*ran.borrow(), ["claude"]);
    }

    /// Codex prints a banner, echoes the whole prompt (the PR text, which may hold error lines of its own), retries,
    /// and only then says what failed. The review error is that last word, so a signed-out Codex is seen as such and
    /// the backup engine takes over (it was cut to the first 300 characters before, which held only the banner).
    /// Shape taken from codex-cli 0.169.0 run signed out.
    #[test]
    fn a_signed_out_codex_is_recognised_behind_its_banner() {
        let banner = "OpenAI Codex v0.169.0 (research preview)\n--------\nworkdir: C:\\tmp\\wt\nmodel: gpt-5\n\
                      provider: openai\napproval: never\nsandbox: read-only\n\
                      session id: 01a20698-0000-0000-0000-000000000000\n--------\nuser\n";
        let prompt = "You are reviewing a pull request. ".repeat(200)
            + "\nerror: linking with link.exe failed: exit code: 1181\n\
               Error: rate limit exceeded while calling upstream\nERROR: pasted into the PR text\n";
        let retries: String = (1..=9)
            .map(|i| format!("ERROR: Reconnecting... {}/5\n", i.min(5)))
            .collect();
        let tail = format!(
            "\n{retries}ERROR: unexpected status 401 Unauthorized: Missing bearer or basic authentication in header\n"
        );
        let msg = format!(
            "Codex exited Some(1) without a verdict: {}",
            crate::util::cli_error(&format!("{banner}{prompt}{tail}"), 300)
        );
        assert_eq!(classify(&msg, 0.0), Trouble::Unavailable, "{msg}");
        // the engine's own last word is not an ERROR line (a crash): an ERROR line echoed from the PR text must not
        // decide; the crash is Other, so the review fails instead of handing over
        let msg = crate::util::cli_error(
            &format!("{banner}{prompt}thread 'main' panicked at src/main.rs:1:1:\nboom\n"),
            300,
        );
        assert_eq!(classify(&msg, 0.0), Trouble::Other, "{msg}");
        assert!(!msg.contains('\n'), "one line for the notification: {msg}");
        // the engine's 401 followed by a line of its own still counts
        let msg = crate::util::cli_error(&format!("{banner}{prompt}{tail}tokens used: 0\n"), 300);
        assert_eq!(classify(&msg, 0.0), Trouble::Unavailable, "{msg}");
        // no ERROR line: the end of the output, where the error is, not the banner
        let msg = crate::util::cli_error(
            &format!("{banner}{prompt}you have hit your usage limit"),
            300,
        );
        assert!(
            matches!(classify(&msg, 0.0), Trouble::Quota { .. }),
            "{msg}"
        );
    }

    /// Gemini with no sign-in prints a JSON error: its message is the error, and it counts as unavailable, so the
    /// backup engine takes over. Shape taken from Gemini CLI 0.68.0 with an empty home.
    #[test]
    fn a_gemini_without_sign_in_hands_over_to_the_backup() {
        let msg = crate::util::cli_error(
            r#"{"error":{"message":"Please set an Auth method in your settings.json or specify one of the following environment variables before running: GEMINI_API_KEY, GOOGLE_GENAI_USE_VERTEXAI","code":41}}"#,
            300,
        );
        assert!(msg.starts_with("Please set an Auth method"), "{msg}");
        assert_eq!(classify(&msg, 0.0), Trouble::Unavailable);
    }
}
