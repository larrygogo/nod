//! Claude Code: `claude -p --restricted` with only Read/Grep/Glob, hooks and MCP servers off, credentials removed
//! from its environment.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::Engine;
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{
    build_prompt, expand_path, validate_verdict, Meta, Verdict, REVIEW_TOOLS, SCHEMA,
};
use crate::util::{run_raw, which, RunOpts};

pub struct Claude;

pub fn tools(lang: &str) -> &'static str {
    if lang == "en" {
        "You only have three read-only tools — Read, Grep and Glob — and cannot run commands."
    } else {
        "你只有 Read、Grep、Glob 三个只读工具，不能执行命令。"
    }
}

impl Engine for Claude {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        let prompt = build_prompt(cfg, pr, ctx, discussion, platform, tools(&cfg.language));
        let exe = which(&cfg.review.claude)
            .ok_or_else(|| anyhow!("Claude Code ({:?}) not found on PATH", cfg.review.claude))?;
        let exe = exe.to_string_lossy().to_string();
        let args = claude_args(cfg, &exe, ctx);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let env = claude_env(cfg);
        let start = Instant::now();
        let timeout = Duration::from_secs_f64(cfg.review.timeout_min * 60.0);
        let key = pr.key();
        let o = run_raw(
            &argv,
            RunOpts {
                cwd: Some(wt),
                stdin: Some(&prompt),
                timeout,
                env: Some(&env),
                track: Some(&key),
            },
        )?;
        if o.timed_out {
            bail!(
                "Claude did not finish within {} minutes",
                cfg.review.timeout_min
            );
        }
        let res: Value = serde_json::from_str(o.stdout.trim()).map_err(|_| {
            anyhow!(
                "Claude exited {:?}: {}",
                o.status,
                crate::util::truncate(o.stderr.trim(), 300)
            )
        })?;
        if res["is_error"].as_bool() == Some(true) {
            bail!(
                "Claude returned an error: {}",
                crate::util::truncate(&res["result"].to_string(), 500)
            );
        }
        let so = &res["structured_output"];
        if so.is_null() {
            bail!(
                "Claude gave no structured verdict: {}",
                crate::util::truncate(&res["result"].to_string(), 300)
            );
        }
        let mut v = validate_verdict(so)?;
        v.meta = Meta {
            duration_s: start.elapsed().as_secs(),
            cost_usd: res["total_cost_usd"].as_f64(),
            turns: res["num_turns"].as_u64(),
            model: res["modelUsage"]
                .as_object()
                .and_then(|m| m.keys().next().cloned())
                .unwrap_or_default(),
            engine: "claude".into(),
            no_approve: false,
        };
        Ok(v)
    }
}

pub fn claude_args(cfg: &Config, exe: &str, ctx: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        exe,
        "-p",
        "--restricted",
        "--strict-mcp-config",
        "--tools",
        REVIEW_TOOLS,
        "--permission-mode",
        "dontAsk",
        "--disable-slash-commands",
        "--add-dir",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push(ctx.to_string_lossy().into());
    a.extend(
        [
            "--output-format",
            "json",
            "--json-schema",
            SCHEMA,
            "--no-session-persistence",
            "--settings",
        ]
        .map(String::from),
    );
    a.push(json!({"disableAllHooks": true}).to_string());
    if !cfg.review.model.is_empty() {
        a.push("--model".into());
        a.push(cfg.review.model.clone());
    }
    a
}

/// Claude Code's own sign-in, which it needs to run: an API key or an OAuth token from `claude setup-token`. Like the
/// other engines, Claude keeps its own login; it has no shell or network tool, so the review cannot read or send it.
pub const CLAUDE_LOGIN: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];
/// Cloud credentials, kept only when Claude Code is set to run through that cloud (`CLAUDE_CODE_USE_BEDROCK` /
/// `CLAUDE_CODE_USE_VERTEX`); otherwise they are general credentials Claude does not need.
const BEDROCK_LOGIN: [&str; 4] = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
];
const VERTEX_LOGIN: [&str; 1] = ["GOOGLE_APPLICATION_CREDENTIALS"];

/// The environment Claude runs with: everything except variables that look like credentials, apart from its login.
pub fn claude_env(cfg: &Config) -> HashMap<String, String> {
    let on = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty() && v != "0");
    let mut keep: Vec<&str> = CLAUDE_LOGIN.to_vec();
    if on("CLAUDE_CODE_USE_BEDROCK") {
        keep.extend(BEDROCK_LOGIN);
    }
    if on("CLAUDE_CODE_USE_VERTEX") {
        keep.extend(VERTEX_LOGIN);
    }
    let mut env = super::safe_env(&keep);
    if !cfg.review.claude_config_dir.is_empty() {
        env.insert(
            "CLAUDE_CONFIG_DIR".into(),
            expand_path(&cfg.review.claude_config_dir),
        );
    }
    env
}
