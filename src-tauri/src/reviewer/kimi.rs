//! Kimi Code CLI (`kimi -p`) with an agent definition that leaves it only Read, Grep and Glob: Kimi Code does not show
//! the other tools to the model and refuses them again before they run (`tools` in the agent file). Print mode cannot
//! be combined with `--plan`, so the tool list is what keeps it read-only.
//!
//! `kimi` takes its prompt only on the command line, too short for a PR's text on Windows: the prompt goes into a
//! file of the context directory, which `--add-dir` makes readable, and the model is asked to read it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::{json_only, prompt_in_file, safe_env, verdict_from_text, Engine};
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{build_prompt, Verdict};
use crate::util::{run_raw, which, RunOpts};

pub struct Kimi;

/// The agent definition: Kimi Code's own system prompt, and only the three read-only tools.
pub const AGENT: &str = "---
name: nod-reviewer
description: Nod's read-only code reviewer
tools: [Read, Grep, Glob]
disallowedTools: [Bash, Write, Edit, Agent, AgentSwarm]
subagents: []
---
${base_prompt}
";

pub fn tools(lang: &str) -> &'static str {
    if lang == "en" {
        "You only have three read-only tools — Read, Grep and Glob — and cannot run commands."
    } else {
        "你只有 Read、Grep、Glob 三个只读工具，不能执行命令。"
    }
}

/// The command line; `ask` is the short request that points at the prompt file.
pub fn args(cfg: &Config, exe: &str, ctx: &Path, agent: &Path, ask: &str) -> Vec<String> {
    let mut a: Vec<String> = vec![
        exe.into(),
        "--prompt".into(),
        ask.into(),
        "--agent-file".into(),
        agent.to_string_lossy().into(),
        "--output-format".into(),
        "stream-json".into(),
        "--add-dir".into(),
        ctx.to_string_lossy().into(),
    ];
    if !cfg.review.kimi.model.is_empty() {
        a.push("--model".into());
        a.push(cfg.review.kimi.model.clone());
    }
    a
}

/// The last thing Kimi Code said: the last assistant message of `--output-format stream-json` with text and no tool
/// calls.
pub fn final_text(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v["role"] == "assistant" && v["tool_calls"].is_null())
        .filter_map(|v| v["content"].as_str().map(str::to_string))
        .find(|t| !t.trim().is_empty())
}

/// Kimi Code's own error line ("error: failed to run prompt: No model configured …"), without colour codes.
pub fn error_line(stderr: &str) -> String {
    let plain = regex_lite::Regex::new(r"\x1b\[[0-9;]*m")
        .unwrap()
        .replace_all(stderr, "");
    plain
        .lines()
        .map(str::trim)
        .rfind(|l| l.to_lowercase().starts_with("error"))
        .map(|l| crate::util::truncate(l, 300))
        .unwrap_or_else(|| crate::util::cli_error(&plain, 300))
}

fn agent_file(ctx: &Path) -> Result<PathBuf> {
    let p = ctx.join("nod-reviewer.md");
    std::fs::write(&p, AGENT)?;
    Ok(p)
}

impl Engine for Kimi {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        let exe = which(&cfg.review.kimi.exe)
            .ok_or_else(|| anyhow!("Kimi Code ({:?}) not found on PATH", cfg.review.kimi.exe))?;
        let exe = exe.to_string_lossy().to_string();
        let prompt = format!(
            "{}\n\n{}",
            build_prompt(cfg, pr, ctx, discussion, platform, tools(&cfg.language)),
            json_only(&cfg.language)
        );
        let ask = prompt_in_file(ctx, &prompt)?;
        let agent = agent_file(ctx)?;
        let a = args(cfg, &exe, ctx, &agent, &ask);
        let argv: Vec<&str> = a.iter().map(String::as_str).collect();
        let mut env = safe_env(&["KIMI_MODEL_API_KEY"]);
        // no trust prompt for the checkout, and no waiting on background tasks once the answer is in
        env.insert("KIMI_CODE_TRUST_WORKSPACE".into(), "1".into());
        env.insert(
            "KIMI_CODE_BACKGROUND_PRINT_BACKGROUND_MODE".into(),
            "exit".into(),
        );
        let start = Instant::now();
        let key = pr.key();
        let o = run_raw(
            &argv,
            RunOpts {
                cwd: Some(wt),
                stdin: None,
                timeout: Duration::from_secs_f64(cfg.review.timeout_min * 60.0),
                env: Some(&env),
                track: Some(&key),
            },
        )?;
        if o.timed_out {
            bail!(
                "Kimi Code did not finish within {} minutes",
                cfg.review.timeout_min
            );
        }
        if o.status != Some(0) {
            bail!("Kimi Code exited {:?}: {}", o.status, error_line(&o.stderr));
        }
        let text = final_text(&o.stdout).ok_or_else(|| {
            anyhow!(
                "Kimi Code gave no answer: {}",
                error_line(&format!("{}\n{}", o.stdout, o.stderr))
            )
        })?;
        let mut v = verdict_from_text(&text)?;
        v.meta.duration_s = start.elapsed().as_secs();
        v.meta.model = cfg.review.kimi.model.clone();
        v.meta.engine = "kimi".into();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_only_tools() {
        assert!(AGENT.contains("tools: [Read, Grep, Glob]\n"));
        assert!(AGENT.contains("subagents: []"));
        let a = args(
            &Config::default(),
            "kimi",
            Path::new("/c"),
            Path::new("/c/nod-reviewer.md"),
            "read it",
        )
        .join(" ");
        assert!(a.starts_with("kimi --prompt read it --agent-file /c/nod-reviewer.md"));
        assert!(a.contains("--add-dir /c") && a.contains("stream-json"));
        assert!(!a.contains("yolo") && !a.contains("--auto"));
    }

    /// Shapes taken from Kimi Code 2.8.0.
    #[test]
    fn its_answer_and_its_errors() {
        let out = r#"{"role":"meta","type":"system.version","version":"2.8.0"}
{"role":"assistant","tool_calls":[{"type":"function","id":"c1","function":{"name":"Read","arguments":"{\"path\":\"a.txt\"}"}}]}
{"role":"tool","tool_call_id":"c1","content":"1\tsecret"}
{"role":"assistant","content":"{\"summary\":\"ok\"}"}
{"role":"meta","type":"session.resume_hint","session_id":"s","content":"To resume this session: kimi -r s"}
"#;
        assert_eq!(final_text(out).unwrap(), r#"{"summary":"ok"}"#);
        assert!(final_text("{\"role\":\"meta\"}\n").is_none());
        let err = "\x1b[38;2;232;84;84merror: failed to run prompt: No model configured. Run `kimi` and use /login to sign in, then retry; or set default_model in config.toml.\x1b[39m\nSee log: C:\\x\\kimi-code.log\n";
        let e = error_line(err);
        assert!(
            e.starts_with("error: failed to run prompt: No model configured"),
            "{e}"
        );
        assert_eq!(
            crate::reviewer::classify(&e, 0.0),
            crate::reviewer::Trouble::Unavailable
        );
    }
}
