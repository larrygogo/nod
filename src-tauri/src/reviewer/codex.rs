//! OpenAI Codex CLI: `codex exec --sandbox read-only` — the model may read files and run read-only commands; the
//! operating-system sandbox blocks writes and network access. The verdict is constrained by `--output-schema`.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use super::{json_only, safe_env, verdict_from_text, Engine};
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{build_prompt, Verdict, SCHEMA};
use crate::util::{run_raw, which, RunOpts};

pub struct Codex;

pub fn tools(lang: &str) -> &'static str {
    if lang == "en" {
        "You run in a read-only sandbox: you can read files and run commands that only read, but cannot change files or reach the network."
    } else {
        "你运行在只读沙箱里：可以读文件、运行只读的命令，但不能修改文件，也不能联网。"
    }
}

/// The command line (without the prompt, which goes to stdin).
pub fn args(cfg: &Config, exe: &str, wt: &Path, schema: &Path, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec![
        exe.into(),
        "exec".into(),
        "--sandbox".into(),
        "read-only".into(),
        "--skip-git-repo-check".into(),
        "--ephemeral".into(),
        "--color".into(),
        "never".into(),
        "--cd".into(),
        wt.to_string_lossy().into(),
        "--output-schema".into(),
        schema.to_string_lossy().into(),
        "--output-last-message".into(),
        out.to_string_lossy().into(),
    ];
    if !cfg.review.codex.model.is_empty() {
        a.push("--model".into());
        a.push(cfg.review.codex.model.clone());
    }
    a.push("-".into());
    a
}

impl Engine for Codex {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        let exe = which(&cfg.review.codex.exe)
            .ok_or_else(|| anyhow!("Codex CLI ({:?}) not found on PATH", cfg.review.codex.exe))?;
        let exe = exe.to_string_lossy().to_string();
        // schema and answer live next to the context files, outside the checkout
        let schema = ctx.join("verdict.schema.json");
        let out = ctx.join("verdict.json");
        fs::write(&schema, SCHEMA)?;
        let _ = fs::remove_file(&out);
        let prompt = format!(
            "{}\n\n{}",
            build_prompt(cfg, pr, ctx, discussion, platform, tools(&cfg.language)),
            json_only(&cfg.language)
        );
        let a = args(cfg, &exe, wt, &schema, &out);
        let argv: Vec<&str> = a.iter().map(String::as_str).collect();
        let env = safe_env(&[
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_HOME",
            "OPENAI_BASE_URL",
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
                "Codex did not finish within {} minutes",
                cfg.review.timeout_min
            );
        }
        let text = fs::read_to_string(&out).unwrap_or_default();
        if text.trim().is_empty() {
            bail!(
                "Codex exited {:?} without a verdict: {}",
                o.status,
                crate::util::truncate(format!("{}{}", o.stderr, o.stdout).trim(), 300)
            );
        }
        let mut v = verdict_from_text(&text)?;
        v.meta.duration_s = start.elapsed().as_secs();
        v.meta.model = cfg.review.codex.model.clone();
        v.meta.engine = "codex".into();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_read_only_with_the_schema() {
        let mut cfg = Config::default();
        cfg.review.codex.model = "gpt-5".into();
        let a = args(
            &cfg,
            "codex",
            Path::new("/w"),
            Path::new("/c/s.json"),
            Path::new("/c/o.json"),
        );
        let s = a.join(" ");
        assert!(s.contains("exec --sandbox read-only"));
        assert!(
            s.contains("--output-schema /c/s.json")
                && s.contains("--output-last-message /c/o.json")
        );
        assert!(s.ends_with("--model gpt-5 -"));
        assert!(
            !s.contains("danger") && !s.contains("workspace-write") && !s.contains("--add-dir")
        );
    }
}
