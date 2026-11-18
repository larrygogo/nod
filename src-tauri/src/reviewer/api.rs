//! Any OpenAI-compatible chat completions API (OpenAI, DeepSeek, Qwen/DashScope, OpenRouter, Ollama, LM Studio, …).
//!
//! The model gets no tools and no access to the checkout: only the diff stat, commit list and the (size-limited) diff
//! are sent. That is weaker review context than the agent CLIs, but nothing can be read or run on this machine.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::{json_only, verdict_from_text, Engine};
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{build_prompt, Verdict};

pub struct Api;

pub fn tools(lang: &str) -> &'static str {
    if lang == "en" {
        "You have no tools and cannot open any file. The diff stat, the commits and the diff are included at the end of this message instead of the files listed below."
    } else {
        "你没有任何工具，无法打开文件。下面列出的那些文件的内容（改动文件列表、提交列表和 diff）已经附在这条消息末尾。"
    }
}

/// The user message: the prompt plus the change itself, the diff cut to `max_kb`.
pub fn message(cfg: &Config, pr: &Pr, ctx: &Path, discussion: &str, platform: &str) -> String {
    let read = |n: &str| fs::read_to_string(ctx.join(n)).unwrap_or_default();
    let max = (cfg.review.api.max_diff_kb.max(8) * 1024) as usize;
    let mut diff = read("diff.patch");
    let cut = diff.len() > max;
    if cut {
        let mut end = max;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
    }
    let note = if cut {
        if cfg.language == "en" {
            "\n[The diff was cut here because it is too long. If what you can see is not enough for a trustworthy verdict, say so in unable_reason.]"
        } else {
            "\n[diff 太长，在这里截断了。如果看到的内容不足以给出可信的结论，请在 unable_reason 里说明。]"
        }
    } else {
        ""
    };
    format!(
        "{}\n\n{}\n\n## diffstat.txt\n```\n{}\n```\n\n## commits.txt\n```\n{}\n```\n\n## diff.patch\n```diff\n{diff}\n```{note}",
        build_prompt(cfg, pr, ctx, discussion, platform, tools(&cfg.language)),
        json_only(&cfg.language),
        read("diffstat.txt").trim(),
        read("commits.txt").trim(),
    )
}

pub fn request(cfg: &Config, user: &str, json_mode: bool) -> Value {
    let mut body = json!({
        "model": cfg.review.api.model,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": "You are a meticulous code reviewer. Reply with a single JSON object and nothing else."},
            {"role": "user", "content": user},
        ],
    });
    if json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }
    body
}

#[allow(clippy::result_large_err)]
fn post(cfg: &Config, body: &Value) -> std::result::Result<Value, ureq::Error> {
    let url = format!(
        "{}/chat/completions",
        cfg.review.api.base_url.trim_end_matches('/')
    );
    let mut req = ureq::post(&url)
        .timeout(Duration::from_secs_f64(cfg.review.timeout_min * 60.0))
        .set("Content-Type", "application/json");
    if !cfg.review.api.key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", cfg.review.api.key));
    }
    req.send_json(body.clone())?
        .into_json()
        .map_err(|e| e.into())
}

impl Engine for Api {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        _wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        if cfg.review.api.model.is_empty() {
            return Err(anyhow!("review.api.model is not set"));
        }
        let user = message(cfg, pr, ctx, discussion, platform);
        let start = Instant::now();
        // JSON mode first; some compatible servers reject response_format, so retry once without it
        let res = match post(cfg, &request(cfg, &user, true)) {
            Err(ureq::Error::Status(400, _)) => post(cfg, &request(cfg, &user, false)),
            r => r,
        };
        let res = match res {
            Ok(v) => v,
            Err(ureq::Error::Status(code, r)) => {
                return Err(anyhow!(
                    "the API returned HTTP {code}: {}",
                    crate::util::truncate(r.into_string().unwrap_or_default().trim(), 300)
                ))
            }
            Err(e) => return Err(anyhow!("cannot reach the API: {e}")),
        };
        let text = res["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("the API reply has no message content"))?;
        let mut v = verdict_from_text(text)?;
        v.meta.duration_s = start.elapsed().as_secs();
        v.meta.model = res["model"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| cfg.review.api.model.clone());
        v.meta.engine = "api".into();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::http::fake;
    use crate::review::tests::pr;

    #[test]
    fn sends_only_the_diff_and_reads_the_verdict() {
        let verdict = json!({"summary": "fine", "findings": [], "approve_recommended": true, "unable_reason": ""});
        let f = fake::serve(vec![(
            "POST /v1/chat/completions",
            200,
            json!({"model": "deepseek-chat", "choices": [{"message": {"content": verdict.to_string()}}]}).to_string(),
        )]);
        let ctx = tempfile::tempdir().unwrap();
        fs::write(ctx.path().join("diff.patch"), "+let x = 1;\n".repeat(5000)).unwrap();
        fs::write(ctx.path().join("diffstat.txt"), "a.rs | 5000 +").unwrap();
        fs::write(ctx.path().join("commits.txt"), "abc fix").unwrap();
        let mut cfg = Config {
            language: "en".into(),
            ..Default::default()
        };
        cfg.review.api.base_url = format!("{}/v1", f.url);
        cfg.review.api.model = "deepseek-chat".into();
        cfg.review.api.key = "sk-test".into();
        cfg.review.api.max_diff_kb = 8;
        let v = Api
            .ask(
                &cfg,
                &pr("alice"),
                Path::new("/nowhere"),
                ctx.path(),
                "",
                "GitHub",
            )
            .unwrap();
        assert!(v.approve_recommended);
        assert_eq!(
            (v.meta.engine.as_str(), v.meta.model.as_str()),
            ("api", "deepseek-chat")
        );
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen[0].auth, "Bearer sk-test");
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("## diff.patch") && user.contains("cut here"));
        assert!(
            user.len() < 20 * 1024,
            "the diff was limited to max_diff_kb"
        );
        assert_eq!(body["response_format"]["type"], "json_object");
    }
}
