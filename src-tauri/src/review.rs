//! One review: check out the PR, ask Claude for a structured verdict, decide approve/comment with fixed rules.

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{self, Config};
use crate::forge::{DiffLines, Pr};
use crate::i18n::{reason_text, t, tf};
use crate::paths;

pub const SEVERITIES: [&str; 4] = ["high", "medium", "low", "nit"];
/// Only read-only file tools. `--restricted` removes every tool that can run commands, ignores user/project
/// settings (including hooks) and confines file tools to the working directories (checkout + `--add-dir`).
pub const REVIEW_TOOLS: &str = "Read,Grep,Glob";
pub const SCHEMA: &str = include_str!("../assets/schema.json");
const PROMPT_ZH: &str = include_str!("../assets/review.zh-CN.md");
const PROMPT_EN: &str = include_str!("../assets/review.en.md");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: String,
    pub path: String,
    pub line: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub introduced_by_pr: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Meta {
    pub duration_s: u64,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub turns: Option<u64>,
    #[serde(default)]
    pub model: String,
    /// Which engine produced the verdict.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub engine: String,
    /// The engine is not isolated well enough for its verdict to lead to an approval.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_approve: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Verdict {
    pub summary: String,
    pub findings: Vec<Finding>,
    pub approve_recommended: bool,
    pub unable_reason: String,
    #[serde(rename = "_meta", default)]
    pub meta: Meta,
}

/// Model output is untrusted: anything with the wrong shape is a failure, never guessed.
pub fn validate_verdict(v: &Value) -> Result<Verdict> {
    let verdict: Verdict =
        serde_json::from_value(v.clone()).map_err(|e| anyhow!("malformed verdict: {e}"))?;
    for f in &verdict.findings {
        if !SEVERITIES.contains(&f.severity.as_str()) {
            bail!("malformed finding: severity {:?}", f.severity);
        }
    }
    Ok(verdict)
}

/// `~/x`, `%VAR%\\x` and `$VAR/x` / `${VAR}/x`.
pub fn expand_path(s: &str) -> String {
    let mut out = if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        paths::home_dir().join(rest).to_string_lossy().to_string()
    } else {
        s.to_string()
    };
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    // %VAR%
    while let Some(a) = out.find('%') {
        let Some(len) = out[a + 1..].find('%') else {
            break;
        };
        let name = out[a + 1..a + 1 + len].to_string();
        out.replace_range(a..a + len + 2, &var(&name));
    }
    // ${VAR} and $VAR
    let mut res = String::new();
    let mut it = out.chars().peekable();
    while let Some(c) = it.next() {
        if c != '$' {
            res.push(c);
            continue;
        }
        let mut name = String::new();
        if it.peek() == Some(&'{') {
            it.next();
            for ch in it.by_ref() {
                if ch == '}' {
                    break;
                }
                name.push(ch);
            }
        } else {
            while let Some(&ch) = it.peek() {
                if ch.is_alphanumeric() || ch == '_' {
                    name.push(ch);
                    it.next();
                } else {
                    break;
                }
            }
        }
        if name.is_empty() {
            res.push('$');
        } else {
            res.push_str(&var(&name));
        }
    }
    res
}

/// The review prompt. `platform` names the code host ("GitHub", "Gitee", …); `tools` says what the engine can do
/// (each engine knows its own sandbox).
pub fn build_prompt(
    cfg: &Config,
    pr: &Pr,
    ctx: &Path,
    discussion: &str,
    platform: &str,
    tools: &str,
) -> String {
    let lang = cfg.language.as_str();
    let tpl = if lang == "en" { PROMPT_EN } else { PROMPT_ZH };
    let none = t(lang, "review.no_body");
    let body = crate::util::truncate(&pr.body, 8000);
    let number = pr.number.to_string();
    let ctx = ctx.to_string_lossy();
    let values: [(&str, &str); 11] = [
        ("platform", platform),
        ("tools", tools),
        ("repo", &pr.repo),
        ("number", &number),
        ("title", &pr.title),
        ("author", &pr.author),
        ("base", &pr.base),
        ("head", &pr.head),
        ("ctx", &ctx),
        ("body", if body.is_empty() { &none } else { &body }),
        (
            "discussion",
            if discussion.is_empty() {
                &none
            } else {
                discussion
            },
        ),
    ];
    fill(tpl, &values)
}

/// Replace `{name}` placeholders of the template in one pass. Text that comes from the PR (title, body, comments)
/// is inserted as is and never scanned again, so a PR that contains "{discussion}" or "{ctx}" cannot pull other
/// parts of the prompt into a place of its choosing. Unknown `{…}` in the template are left alone.
fn fill(tpl: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(tpl.len() + 4096);
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}').and_then(|j| {
            values
                .iter()
                .find(|(k, _)| *k == &after[..j])
                .map(|(_, v)| (j, v))
        }) {
            Some((j, v)) => {
                out.push_str(v);
                rest = &after[j + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------- the gate

/// On the "always approve" list (`[approve] always_authors` / `always_repos`): approved whenever asked, whatever the
/// review says. Only what GitHub itself forbids still stops it (your own PR, a closed PR or a draft).
pub fn always_approve(cfg: &Config, pr: &Pr, me: &str) -> bool {
    let listed = cfg
        .approve
        .always
        .iter()
        .any(|r| r.matches(&pr.repo, &pr.author, me));
    listed && !pr.author.eq_ignore_ascii_case(me) && !pr.draft && pr.state == "open"
}

/// Fixed rules, not the model, decide whether to approve. Returns (APPROVE|COMMENT, reason codes).
pub fn decide(
    cfg: &Config,
    pr: &Pr,
    v: &Verdict,
    ci: &str,
    me: &str,
) -> (&'static str, Vec<String>) {
    if always_approve(cfg, pr, me) {
        return ("APPROVE", vec![]);
    }
    let mut r: Vec<String> = vec![];
    if !v.unable_reason.is_empty() {
        r.push(format!("unable:{}", v.unable_reason));
    }
    if v.findings.iter().any(|f| f.severity == "high") {
        r.push("high".into());
    }
    if v.findings
        .iter()
        .any(|f| f.severity == "medium" && f.introduced_by_pr)
    {
        r.push("introduced_medium".into());
    }
    if !v.approve_recommended {
        r.push("model_not_recommended".into());
    }
    if v.meta.no_approve {
        r.push("engine_no_approve".into());
    }
    match ci {
        "failure" => r.push("ci_failed".into()),
        "pending" => r.push("ci_pending".into()),
        "none_wait" => r.push("ci_none_wait".into()),
        _ => {}
    }
    if pr.author.eq_ignore_ascii_case(me) {
        r.push("own_pr".into());
    }
    if pr.draft || pr.state != "open" {
        r.push("not_open".into());
    }
    // Approval allowlists (on top of the quality rules above).
    let a = &cfg.approve;
    if a.mode == "never" {
        r.push("approve_disabled".into());
    } else {
        if !config::selected(&a.repos, &a.exclude_repos, |p| {
            config::repo_matches(p, &pr.repo, me)
        }) {
            r.push("approve_repo".into());
        }
        if !config::selected(&a.authors, &a.exclude_authors, |p| {
            config::author_matches(p, &pr.author, me)
        }) {
            r.push("approve_author".into());
        }
    }
    (if r.is_empty() { "APPROVE" } else { "COMMENT" }, r)
}

pub fn build_review(
    lang: &str,
    pr: &Pr,
    v: &Verdict,
    event: &str,
    reasons: &[String],
    valid: &DiffLines,
    footer: Option<&str>,
) -> Value {
    let mut findings = v.findings.clone();
    findings.sort_by_key(|f| {
        SEVERITIES
            .iter()
            .position(|s| *s == f.severity)
            .unwrap_or(9)
    });
    let mut comments = vec![];
    let mut outside = vec![];
    for f in &findings {
        let old = if f.introduced_by_pr {
            String::new()
        } else {
            t(lang, "review.old_issue")
        };
        let head = format!(
            "{}{old}: {}",
            t(lang, &format!("sev_icon.{}", f.severity)),
            f.title
        );
        if valid.get(&f.path).is_some_and(|s| s.contains(&f.line)) {
            comments.push(json!({"path": f.path, "line": f.line, "side": "RIGHT", "body": format!("**{head}**\n\n{}", f.body)}));
        } else {
            let loc = tf(
                lang,
                "review.outside_diff",
                &[("loc", &format!("{}:{}", f.path, f.line))],
            );
            outside.push(format!("**{head}**{loc}\n\n{}", f.body));
        }
    }
    let key = if event == "APPROVE" {
        "review.approve"
    } else {
        "review.comment"
    };
    let mut lines = vec![tf(lang, key, &[("summary", &v.summary)])];
    if event != "APPROVE" && (reasons == ["ci_pending"] || reasons == ["ci_none_wait"]) {
        // posted ahead of the approval, which waits only for CI
        lines.push(t(lang, "review.after_ci"));
    } else if event != "APPROVE" && !reasons.is_empty() {
        let rs: Vec<String> = reasons.iter().map(|r| reason_text(lang, r)).collect();
        lines.push(tf(
            lang,
            "review.blocked_by",
            &[("reasons", &rs.join("; "))],
        ));
    }
    if !findings.is_empty() {
        lines.push(String::new());
        lines.push(t(lang, "review.legend"));
        for f in &findings {
            lines.push(format!(
                "- {} `{}:{}` {}",
                t(lang, &format!("sev_icon.{}", f.severity)),
                f.path,
                f.line,
                f.title
            ));
        }
    }
    for o in outside {
        lines.push(String::new());
        lines.push(o);
    }
    if let Some(f) = footer.filter(|f| !f.trim().is_empty()) {
        lines.push(String::new());
        lines.push(f.to_string());
    }
    json!({"commit_id": pr.head, "event": event, "body": lines.join("\n"), "comments": comments})
}

/// The engine's name as people know it, for the review's signature.
pub fn engine_name(id: &str) -> String {
    match id {
        "" | "claude" => "Claude Code".into(),
        "codex" => "OpenAI Codex".into(),
        "gemini" => "Gemini CLI".into(),
        "api" => "AI".into(),
        other => other.trim_start_matches("ext:").to_string(),
    }
}

/// The signature at the end of a review ([review] footer_enabled / footer): None when turned off.
pub fn footer_text(lang: &str, cfg: &crate::config::Review, engine: &str) -> Option<String> {
    if !cfg.footer_enabled {
        return None;
    }
    let name = engine_name(engine);
    let text = if cfg.footer.trim().is_empty() {
        tf(lang, "review.footer", &[("engine", &name)])
    } else {
        cfg.footer.trim().replace("{engine}", &name)
    };
    Some(text)
}

/// Credential-looking strings in the review text (blocks posting).
pub fn find_secrets(review: &Value) -> Vec<&'static str> {
    let mut text = review["body"].as_str().unwrap_or("").to_string();
    for c in review["comments"].as_array().into_iter().flatten() {
        text.push('\n');
        text.push_str(c["body"].as_str().unwrap_or(""));
    }
    let pats: [(&str, &str); 5] = [
        (
            "GitHub token",
            r"\b(gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})",
        ),
        ("API key", r"\bsk-[A-Za-z0-9_-]{20,}"),
        ("Slack token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
        ("AWS key", r"\bAKIA[0-9A-Z]{16}\b"),
        ("private key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ];
    pats.iter()
        .filter(|(_, p)| {
            regex_lite::Regex::new(p)
                .map(|r| r.is_match(&text))
                .unwrap_or(false)
        })
        .map(|(n, _)| *n)
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::reviewer::claude::{claude_args, claude_env};
    use std::collections::HashSet;

    pub fn pr(author: &str) -> Pr {
        Pr {
            repo: "acme/app".into(),
            number: 1,
            title: "t".into(),
            author: author.into(),
            url: "u".into(),
            head: "abc".into(),
            base: "main".into(),
            draft: false,
            state: "open".into(),
            body: String::new(),
        }
    }
    pub fn finding(sev: &str, introduced: bool, line: u64) -> Finding {
        Finding {
            severity: sev.into(),
            path: "a.py".into(),
            line,
            title: "x".into(),
            body: "y".into(),
            introduced_by_pr: introduced,
        }
    }
    pub fn clean() -> Verdict {
        Verdict {
            summary: "ok".into(),
            findings: vec![],
            approve_recommended: true,
            unable_reason: String::new(),
            meta: Meta::default(),
        }
    }
    fn reasons(
        v: &Verdict,
        ci: &str,
        me: &str,
        cfg: &Config,
        p: &Pr,
    ) -> (&'static str, Vec<String>) {
        decide(cfg, p, v, ci, me)
    }

    #[test]
    fn gate() {
        let c = Config::default();
        let p = pr("alice");
        assert_eq!(
            reasons(&clean(), "success", "me", &c, &p),
            ("APPROVE", vec![])
        );
        assert_eq!(reasons(&clean(), "none", "me", &c, &p).0, "APPROVE");
        for (ci, r) in [
            ("failure", "ci_failed"),
            ("pending", "ci_pending"),
            ("none_wait", "ci_none_wait"),
        ] {
            assert_eq!(
                reasons(&clean(), ci, "me", &c, &p),
                ("COMMENT", vec![r.to_string()])
            );
        }
        assert_eq!(
            reasons(&clean(), "success", "ALICE", &c, &p).1,
            vec!["own_pr"]
        );
        let mut v = clean();
        v.findings = vec![finding("high", true, 1)];
        assert_eq!(reasons(&v, "success", "me", &c, &p).1, vec!["high"]);
        v.findings = vec![finding("medium", true, 1)];
        assert_eq!(reasons(&v, "success", "me", &c, &p).0, "COMMENT");
        v.findings = vec![
            finding("medium", false, 1),
            finding("low", true, 1),
            finding("nit", true, 1),
        ];
        assert_eq!(reasons(&v, "success", "me", &c, &p).0, "APPROVE");
        let mut v = clean();
        v.approve_recommended = false;
        assert_eq!(
            reasons(&v, "success", "me", &c, &p).1,
            vec!["model_not_recommended"]
        );
        let mut v = clean();
        v.unable_reason = "too big".into();
        assert_eq!(
            reasons(&v, "success", "me", &c, &p).1,
            vec!["unable:too big"]
        );
        let mut d = pr("alice");
        d.draft = true;
        assert_eq!(
            reasons(&clean(), "success", "me", &c, &d).1,
            vec!["not_open"]
        );
    }

    #[test]
    fn approve_allowlists() {
        let d = |c: &Config, p: &Pr, v: &Verdict| decide(c, p, v, "success", "me");
        let mut c = Config::default();
        c.approve.mode = "never".into();
        assert_eq!(d(&c, &pr("a"), &clean()).1, vec!["approve_disabled"]);

        let mut c = Config::default();
        c.approve.repos = vec!["@me/*".into()];
        assert_eq!(d(&c, &pr("a"), &clean()).1, vec!["approve_repo"]);
        let mut mine = pr("a");
        mine.repo = "me/x".into();
        assert_eq!(d(&c, &mine, &clean()).0, "APPROVE");

        let mut c = Config::default();
        c.approve.repos = vec!["acme/*".into()];
        c.approve.exclude_repos = vec!["acme/app".into()];
        assert_eq!(d(&c, &pr("a"), &clean()).1, vec!["approve_repo"]);

        let mut c = Config::default();
        c.approve.authors = vec!["dependabot[bot]".into(), "alice".into()];
        assert_eq!(d(&c, &pr("Alice"), &clean()).0, "APPROVE");
        assert_eq!(d(&c, &pr("bob"), &clean()).1, vec!["approve_author"]);
        c.approve.authors.clear();
        c.approve.exclude_authors = vec!["bob".into()];
        assert_eq!(d(&c, &pr("bob"), &clean()).1, vec!["approve_author"]);
        assert_eq!(d(&c, &pr("carol"), &clean()).0, "APPROVE");

        // the quality rules still apply inside the allowlist
        let mut v = clean();
        v.findings = vec![finding("high", true, 1)];
        let mut c = Config::default();
        c.approve.authors = vec!["alice".into()];
        assert_eq!(d(&c, &pr("alice"), &v).0, "COMMENT");
    }

    #[test]
    fn review_body() {
        for (lang, word) in [("zh-CN", "只评论"), ("en", "comments only")] {
            let mut v = clean();
            v.findings = vec![finding("low", true, 3), finding("nit", true, 99)];
            let valid = DiffLines::from([("a.py".to_string(), HashSet::from([3]))]);
            let r = build_review(
                lang,
                &pr("a"),
                &v,
                "COMMENT",
                &["high".into()],
                &valid,
                Some("<sub>sig</sub>"),
            );
            assert_eq!(r["comments"].as_array().unwrap().len(), 1);
            assert_eq!(r["comments"][0]["line"], 3);
            let body = r["body"].as_str().unwrap();
            assert!(body.contains("a.py:99") && body.contains(word), "{body}");
            assert_eq!(r["commit_id"], "abc");
        }
        let r = build_review(
            "en",
            &pr("a"),
            &clean(),
            "COMMENT",
            &["ci_failed".into(), "unable:huge".into()],
            &DiffLines::new(),
            None,
        );
        let body = r["body"].as_str().unwrap();
        assert!(body.contains("CI failed") && body.contains("review could not finish: huge"));
        assert!(
            !body.contains("<sub>"),
            "no signature when it is turned off: {body}"
        );
    }

    #[test]
    fn signature() {
        let mut c = crate::config::Review::default();
        // the default names the engine that actually reviewed
        let f = footer_text("en", &c, "codex").unwrap();
        assert!(f.contains("OpenAI Codex") && !f.contains("Claude"), "{f}");
        assert!(footer_text("zh-CN", &c, "claude")
            .unwrap()
            .contains("Claude Code"));
        c.footer = "  — reviewed with {engine} by our bot  ".into();
        assert_eq!(
            footer_text("en", &c, "gemini").unwrap(),
            "— reviewed with Gemini CLI by our bot"
        );
        c.footer_enabled = false;
        assert_eq!(footer_text("en", &c, "claude"), None);
        let body = build_review(
            "en",
            &pr("a"),
            &clean(),
            "APPROVE",
            &[],
            &DiffLines::new(),
            Some("TAIL"),
        );
        assert!(body["body"].as_str().unwrap().ends_with("\n\nTAIL"));
    }

    #[test]
    fn validation() {
        let ok = json!({"summary": "s", "findings": [{"severity": "low", "path": "a", "line": 1, "title": "t", "body": "b", "introduced_by_pr": true}], "approve_recommended": true, "unable_reason": ""});
        assert!(validate_verdict(&ok).is_ok());
        let mut bad = ok.clone();
        bad["approve_recommended"] = json!("false");
        assert!(validate_verdict(&bad).is_err());
        let mut bad = ok.clone();
        bad["findings"][0]["severity"] = json!("critical");
        assert!(validate_verdict(&bad).is_err());
        let mut bad = ok.clone();
        bad["findings"][0]
            .as_object_mut()
            .unwrap()
            .remove("introduced_by_pr");
        assert!(validate_verdict(&bad).is_err());
        let mut bad = ok;
        bad["findings"][0]["line"] = json!("12");
        assert!(validate_verdict(&bad).is_err());
    }

    #[test]
    fn secrets() {
        let r = json!({"body": format!("x gho_{}", "a".repeat(36)), "comments": [{"body": "-----BEGIN RSA PRIVATE KEY-----"}]});
        assert_eq!(find_secrets(&r), vec!["GitHub token", "private key"]);
        assert!(
            find_secrets(&json!({"body": "ok", "comments": [{"body": "sk is a variable"}]}))
                .is_empty()
        );
    }

    #[test]
    fn sandbox_args_and_env() {
        let mut c = Config::default();
        let a = claude_args(&c, "claude", Path::new("ctx"));
        assert!(
            a.contains(&"--restricted".to_string())
                && a.contains(&"--strict-mcp-config".to_string())
        );
        let i = a.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(a[i + 1], "Read,Grep,Glob");
        assert!(!a
            .iter()
            .any(|x| x.contains("bypass") || x.contains("dangerously")));
        c.review.model = "claude-sonnet-5-5".into();
        let a = claude_args(&c, "claude", Path::new("ctx"));
        assert_eq!(
            a[a.iter().position(|x| x == "--model").unwrap() + 1],
            "claude-sonnet-5-5"
        );

        std::env::set_var("GH_TOKEN", "x");
        std::env::set_var("MY_API_KEY", "x");
        std::env::set_var("NOD_FEISHU_APP_SECRET", "x");
        let env = claude_env(&c);
        assert!(!env.contains_key("GH_TOKEN") && !env.contains_key("MY_API_KEY"));
        assert!(!env.contains_key("NOD_FEISHU_APP_SECRET"));
        assert!(env.keys().any(|k| k.eq_ignore_ascii_case("PATH")));
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("MY_API_KEY");
        std::env::remove_var("NOD_FEISHU_APP_SECRET");
        // Regression (code review): Claude's own sign-in was removed too, so headless or Bedrock logins failed.
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "x");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "x");
        let env = claude_env(&c);
        assert!(env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(
            !env.contains_key("AWS_SECRET_ACCESS_KEY"),
            "cloud credentials only for Bedrock users"
        );
        std::env::set_var("CLAUDE_CODE_USE_BEDROCK", "1");
        assert!(claude_env(&c).contains_key("AWS_SECRET_ACCESS_KEY"));
        std::env::remove_var("CLAUDE_CODE_USE_BEDROCK");
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        std::env::set_var("RW_TEST_DIR", "/base");
        assert_eq!(expand_path("$RW_TEST_DIR/a"), "/base/a");
        assert_eq!(expand_path("${RW_TEST_DIR}/a"), "/base/a");
        assert_eq!(expand_path("%RW_TEST_DIR%/a"), "/base/a");
        std::env::remove_var("RW_TEST_DIR");
        c.review.claude_config_dir = "/profiles/work".into();
        assert_eq!(claude_env(&c)["CLAUDE_CONFIG_DIR"], "/profiles/work");
    }

    #[test]
    fn prompts_render() {
        for lang in ["zh-CN", "en"] {
            let c = Config {
                language: lang.into(),
                ..Default::default()
            };
            let p = build_prompt(
                &c,
                &pr("a"),
                Path::new("/tmp/ctx"),
                "",
                "Gitee",
                "TOOLS-LINE",
            );
            assert!(p.contains("acme/app#1") && p.contains("/tmp/ctx"));
            assert!(p.contains("Gitee") && p.contains("TOOLS-LINE"));
            assert!(!p.contains("{repo}") && !p.contains("{discussion}") && !p.contains("{tools}"));
        }
    }

    /// Regression (code review): placeholders were replaced one after another over the growing text, so a PR title
    /// or body containing "{discussion}" or "{ctx}" had the discussion or the local path pasted into it.
    #[test]
    fn text_from_the_pr_is_not_expanded() {
        let mut p = pr("a");
        p.title = "Fix {discussion} {ctx}".into();
        p.body = "see {discussion}".into();
        let out = build_prompt(
            &Config::default(),
            &p,
            Path::new("/secret/ctx"),
            "SECRET-DISCUSSION",
            "GitHub",
            "",
        );
        assert!(out.contains("Fix {discussion} {ctx}") && out.contains("see {discussion}"));
        assert_eq!(
            out.matches("SECRET-DISCUSSION").count(),
            1,
            "the discussion appears once, where the template puts it"
        );
        assert_eq!(
            out.matches("/secret/ctx").count(),
            PROMPT_ZH.matches("{ctx}").count()
        );
    }
}
