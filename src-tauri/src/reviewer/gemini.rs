//! Gemini CLI in headless mode with `--approval-mode plan` (read-only tools). There is no operating-system sandbox
//! here, so its verdicts do not lead to an approval unless `review.gemini.allow_approve` is turned on.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::{json_only, safe_env, verdict_from_text, Engine};
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{build_prompt, Verdict};
use crate::util::{run_raw, which, RunOpts};

pub struct Gemini;

pub fn tools(lang: &str) -> &'static str {
    if lang == "en" {
        "You are in read-only plan mode: read and search files only; do not try to change anything or run commands."
    } else {
        "你处于只读的 plan 模式：只读取和搜索文件，不要尝试修改任何东西或运行命令。"
    }
}

pub fn args(cfg: &Config, exe: &str, ctx: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec![
        exe.into(),
        "--approval-mode".into(),
        "plan".into(),
        "--output-format".into(),
        "json".into(),
        "--include-directories".into(),
        ctx.to_string_lossy().into(),
        "--skip-trust".into(),
    ];
    if !cfg.review.gemini.model.is_empty() {
        a.push("--model".into());
        a.push(cfg.review.gemini.model.clone());
    }
    // the prompt itself goes to stdin; -p switches to headless mode and is appended to it
    a.push("--prompt".into());
    a.push("Follow the instructions above and reply with the JSON verdict only.".into());
    a
}

impl Engine for Gemini {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        let exe = which(&cfg.review.gemini.exe)
            .ok_or_else(|| anyhow!("Gemini CLI ({:?}) not found on PATH", cfg.review.gemini.exe))?;
        let exe = exe.to_string_lossy().to_string();
        let prompt = format!(
            "{}\n\n{}",
            build_prompt(cfg, pr, ctx, discussion, platform, tools(&cfg.language)),
            json_only(&cfg.language)
        );
        let a = args(cfg, &exe, ctx);
        let argv: Vec<&str> = a.iter().map(String::as_str).collect();
        let env = safe_env(&[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_CLOUD_PROJECT",
            "GOOGLE_CLOUD_LOCATION",
            "GOOGLE_GENAI_USE_VERTEXAI",
        ]);
        let start = Instant::now();
        let key = pr.key();
        let o = run_raw(
            &argv,
            RunOpts {
                cwd: Some(wt),
                stdin: Some(&prompt),
                timeout: Duration::from_secs_f64(cfg.review.timeout_min * 60.0),
                env: Some(&env),
                track: Some(&key),
            },
        )?;
        if o.timed_out {
            bail!(
                "Gemini did not finish within {} minutes",
                cfg.review.timeout_min
            );
        }
        let res: Value = serde_json::from_str(o.stdout.trim()).map_err(|_| {
            anyhow!(
                "Gemini exited {:?}: {}",
                o.status,
                crate::util::truncate(o.stderr.trim(), 300)
            )
        })?;
        if let Some(err) = res.get("error").filter(|e| !e.is_null()) {
            bail!(
                "Gemini returned an error: {}",
                crate::util::truncate(&err.to_string(), 300)
            );
        }
        let text = res["response"].as_str().unwrap_or_default();
        let mut v = verdict_from_text(text)?;
        v.meta.duration_s = start.elapsed().as_secs();
        v.meta.model = res["stats"]["models"]
            .as_object()
            .and_then(|m| m.keys().next().cloned())
            .unwrap_or_else(|| cfg.review.gemini.model.clone());
        v.meta.engine = "gemini".into();
        v.meta.no_approve = !cfg.review.gemini.allow_approve;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_mode_never_yolo() {
        let a = args(&Config::default(), "gemini", Path::new("/c")).join(" ");
        assert!(a.contains("--approval-mode plan") && a.contains("--output-format json"));
        assert!(a.contains("--include-directories /c"));
        assert!(!a.contains("yolo") && !a.contains("auto_edit"));
    }
}
