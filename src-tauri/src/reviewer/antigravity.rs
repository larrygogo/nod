//! Google Antigravity CLI (`agy`) in print mode with `--mode plan`: it reads and searches files, while edits and
//! commands would wait for an approval that print mode never gives. There is no operating-system sandbox here, so its
//! verdicts do not lead to an approval unless `review.antigravity.allow_approve` is turned on.
//!
//! `agy` may end its turn early (it starts a subagent and says it is waiting for it, or says what it will look at
//! next): a reply without a verdict is followed up once in the same conversation, asking for the verdict.
//!
//! `--disable-slash-commands` is not passed: with it, agy ignores `--mode plan` (it says so on stderr). The request on
//! the command line is Nod's own short text, so nothing from the PR could be expanded as a slash command.
//!
//! `agy` takes its prompt only on the command line, too short for a PR's text on Windows: the prompt goes into a file
//! of the context directory, which `--add-dir` makes readable, and the model is asked to read it.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::{json_only, prompt_in_file, safe_env, verdict_from_text, Engine};
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{build_prompt, Verdict};
use crate::util::{run_raw, which, RunOpts};

pub struct Antigravity;

pub fn tools(lang: &str) -> &'static str {
    if lang == "en" {
        "You are in read-only plan mode: read and search files only; do not try to change anything or run commands. \
         Do the review yourself in this one turn: do not start subagents, and end the turn only with the verdict."
    } else {
        "你处于只读的 plan 模式：只读取和搜索文件，不要尝试修改任何东西或运行命令。\
         在这一轮里自己完成审查：不要启动子代理（subagent），只在给出结论时结束本轮。"
    }
}

/// The command line; `ask` is the short request that points at the prompt file. `conversation` continues an earlier
/// one (the follow-up asking for the verdict).
pub fn args(
    cfg: &Config,
    exe: &str,
    ctx: &Path,
    ask: &str,
    conversation: Option<&str>,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        exe.into(),
        "--print".into(),
        ask.into(),
        "--mode".into(),
        "plan".into(),
        "--output-format".into(),
        "json".into(),
        "--add-dir".into(),
        ctx.to_string_lossy().into(),
    ];
    if let Some(id) = conversation {
        a.push("--conversation".into());
        a.push(id.into());
    }
    if !cfg.review.antigravity.model.is_empty() {
        a.push("--model".into());
        a.push(cfg.review.antigravity.model.clone());
    }
    a
}

/// What a reply that is not a verdict began with, for the error.
fn reply_head(text: &str) -> String {
    crate::util::truncate(text.trim(), 200)
}

/// The request that follows up a reply without a verdict.
fn follow_up(lang: &str) -> &'static str {
    if lang == "en" {
        "Your last reply had no verdict. Do not start subagents or wait for one: finish the review with what you have \
         read, then reply with only the JSON verdict described in review-prompt.md."
    } else {
        "你上一条回复里没有结论。不要启动或等待子代理，用已经读到的内容完成审查，然后只回复 review-prompt.md 里要求的 JSON 结论。"
    }
}

/// The answer of `agy --output-format json`: the response text and the conversation (to follow it up), or why there
/// is none.
pub fn response(stdout: &str, stderr: &str, status: Option<i32>) -> Result<(String, String)> {
    let res: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
        let said = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        anyhow!(
            "Antigravity exited {status:?}: {}",
            crate::util::cli_error(said, 300)
        )
    })?;
    let st = res["status"].as_str().unwrap_or("");
    if st != "SUCCESS" {
        let err = match &res["error"] {
            Value::String(s) => s.clone(),
            Value::Null => crate::util::cli_error(stderr, 300),
            v => v["message"]
                .as_str()
                .map_or_else(|| v.to_string(), str::to_string),
        };
        bail!(
            "Antigravity returned {st}: {}",
            crate::util::truncate(err.trim(), 300)
        );
    }
    Ok((
        res["response"].as_str().unwrap_or_default().to_string(),
        res["conversation_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    ))
}

impl Engine for Antigravity {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        let exe = which(&cfg.review.antigravity.exe).ok_or_else(|| {
            anyhow!(
                "Antigravity CLI ({:?}) not found on PATH",
                cfg.review.antigravity.exe
            )
        })?;
        let exe = exe.to_string_lossy().to_string();
        let prompt = format!(
            "{}\n\n{}",
            build_prompt(cfg, pr, ctx, discussion, platform, tools(&cfg.language)),
            json_only(&cfg.language)
        );
        let ask = prompt_in_file(ctx, &prompt)?;
        let env = safe_env(&[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GEMINI_BASE_URL",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_CLOUD_PROJECT",
            "GOOGLE_CLOUD_LOCATION",
            "GOOGLE_GENAI_USE_VERTEXAI",
        ]);
        let start = Instant::now();
        let limit = Duration::from_secs_f64(cfg.review.timeout_min * 60.0);
        let key = pr.key();
        let turn = |ask: &str, conversation: Option<&str>| -> Result<(String, String)> {
            let a = args(cfg, &exe, ctx, ask, conversation);
            let argv: Vec<&str> = a.iter().map(String::as_str).collect();
            // stdin is closed right away: agy waits on an open one
            let o = run_raw(
                &argv,
                RunOpts {
                    cwd: Some(wt),
                    stdin: None,
                    timeout: limit.saturating_sub(start.elapsed()),
                    env: Some(&env),
                    track: Some(&key),
                },
            )?;
            if o.timed_out {
                bail!(
                    "Antigravity did not finish within {} minutes",
                    cfg.review.timeout_min
                );
            }
            response(&o.stdout, &o.stderr, o.status)
        };
        let (mut text, conversation) = turn(&ask, None)?;
        if verdict_from_text(&text).is_err() && !conversation.is_empty() {
            crate::info!(
                "{key}: Antigravity ended without a verdict ({}); asking for it",
                reply_head(&text)
            );
            if crate::util::was_aborted(&key) {
                bail!("the review was stopped");
            }
            text = turn(follow_up(&cfg.language), Some(&conversation))?.0;
        }
        let mut v = verdict_from_text(&text)
            .map_err(|e| anyhow!("{e}; Antigravity replied: {}", reply_head(&text)))?;
        v.meta.duration_s = start.elapsed().as_secs();
        v.meta.model = cfg.review.antigravity.model.clone();
        v.meta.engine = "antigravity".into();
        v.meta.no_approve = !cfg.review.antigravity.allow_approve;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_mode_never_skips_permissions() {
        let a = args(&Config::default(), "agy", Path::new("/c"), "read it", None).join(" ");
        assert!(a.starts_with("agy --print read it --mode plan --output-format json"));
        assert!(a.contains("--add-dir /c"));
        assert!(!a.contains("dangerously") && !a.contains("auto") && !a.contains("accept-edits"));
        // with it agy ignores --mode plan
        assert!(!a.contains("--disable-slash-commands"));
        let a = args(
            &Config::default(),
            "agy",
            Path::new("/c"),
            "go on",
            Some("c1"),
        )
        .join(" ");
        assert!(a.ends_with("--conversation c1"), "{a}");
    }

    /// Shapes taken from agy 1.5.17.
    #[test]
    fn its_answer_and_its_errors() {
        let ok = r#"{"conversation_id":"x","status":"SUCCESS","response":"{\"summary\":\"ok\"}","num_turns":1}"#;
        assert_eq!(
            response(ok, "", Some(0)).unwrap(),
            (r#"{"summary":"ok"}"#.to_string(), "x".to_string())
        );
        let err =
            r#"{"status":"ERROR","error":"authentication required: run agy once to sign in"}"#;
        let e = response(err, "", Some(1)).unwrap_err().to_string();
        assert!(
            e.contains("ERROR") && e.contains("authentication required"),
            "{e}"
        );
        assert_eq!(
            crate::reviewer::classify(&e, 0.0),
            crate::reviewer::Trouble::Unavailable
        );
        let e = response("", "Error: unknown model \"x\"\n", Some(1))
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown model"), "{e}");
    }
}
