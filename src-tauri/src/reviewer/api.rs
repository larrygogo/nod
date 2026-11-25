//! Any OpenAI-compatible chat completions API (OpenAI, DeepSeek, Qwen/DashScope, OpenRouter, Ollama, LM Studio, …).
//!
//! The message carries the diff stat, the commit list and as many whole files of the diff as fit. The model also gets
//! read-only tools on the checkout (`api_tools`: get_diff, read_file, list_dir, search) and fetches the rest itself;
//! Nod runs them inside the checkout only, nothing is written or executed. An endpoint that rejects tools gets the
//! diff alone. Either way, changed files whose diff the model never saw are recorded in the verdict
//! (`meta.unseen`), and `review::decide` does not approve a review that did not see the whole change.
//!
//! The whole conversation has a byte budget (`review.api.max_diff_kb`, 0 = by endpoint): when it is spent, or after
//! [`MAX_ROUNDS`] rounds of tool calls, the tools are withdrawn and the model is asked for its verdict.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::api_tools::{is_low_value, pack, split_diff, FileDiff, Tools, RESULT_MAX};
use super::{json_only, verdict_from_text, Engine};
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{build_prompt, Verdict};

pub struct Api;

/// Rounds of tool calls before the tools are withdrawn.
pub const MAX_ROUNDS: usize = 30;
/// Budget for an endpoint on this machine (Ollama, LM Studio) when `max_diff_kb` is 0. Ollama's OpenAI-compatible
/// route cannot set the context size; its default context may be a few thousand tokens, and a longer prompt is cut
/// silently.
pub const LOCAL_KB: u64 = 24;
/// Budget for any other endpoint when `max_diff_kb` is 0.
pub const REMOTE_KB: u64 = 200;
/// The model's names for the files left out of the message are listed up to this many.
const LIST_MAX: usize = 200;
const SYSTEM: &str = "You are a meticulous code reviewer. Your final reply is a single JSON object and nothing else.";

/// The prompt's line about tools when the model has none (also used by external plugins that see only the diff).
pub fn tools(lang: &str) -> &'static str {
    if lang == "en" {
        "You have no tools and cannot open any file. The diff stat, the commits and the diff are included at the end of this message instead of the files listed below."
    } else {
        "你没有任何工具，无法打开文件。下面列出的那些文件的内容（改动文件列表、提交列表和 diff）已经附在这条消息末尾。"
    }
}

/// The prompt's line about tools when the model has the read-only tools.
pub fn tools_on(lang: &str) -> &'static str {
    if lang == "en" {
        "You cannot open the paths below directly. Instead you have read-only tools on this PR's checkout, with paths relative to the repository root: get_diff (one changed file's diff), read_file, list_dir and search. The diff stat, the commits and as much of the diff as fits are at the end of this message; fetch every diff marked as not included with get_diff before you conclude. The tools cannot change or run anything."
    } else {
        "你不能直接打开下面提到的路径，但可以用只读工具访问这个 PR 的检出（路径相对仓库根目录）：get_diff 取某个改动文件的 diff，read_file 读文件，list_dir 列目录，search 搜索。改动文件列表、提交列表和放得下的 diff 已经附在消息末尾；标明没有附上的文件，下结论前要用 get_diff 逐个取来看。工具不能修改或执行任何东西。"
    }
}

/// Whether `url` points at this machine, or at the usual port of a local model server (Ollama 11434, LM Studio 1234).
pub fn is_local(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |x| x.1);
    let hostport = rest.split(['/', '?']).next().unwrap_or("");
    let hostport = hostport.rsplit_once('@').map_or(hostport, |x| x.1);
    let (host, port) = match hostport.strip_prefix('[') {
        Some(h) => {
            let (h, p) = h.split_once(']').unwrap_or((h, ""));
            (h, p.trim_start_matches(':'))
        }
        None => hostport.rsplit_once(':').unwrap_or((hostport, "")),
    };
    let host = host.to_lowercase();
    host == "localhost"
        || host.starts_with("127.")
        || host == "::1"
        || host == "0.0.0.0"
        || host == "host.docker.internal"
        || port == "11434"
        || port == "1234"
}

/// Bytes the whole conversation may send.
pub fn budget(cfg: &Config) -> usize {
    let kb = match cfg.review.api.max_diff_kb {
        0 if is_local(&cfg.review.api.base_url) => LOCAL_KB,
        0 => REMOTE_KB,
        n => n.max(8),
    };
    (kb * 1024) as usize
}

/// The first user message, and the diff split into files.
pub struct Prepared {
    pub user: String,
    pub files: Vec<FileDiff>,
    /// Files whose whole diff is in the message.
    pub included: Vec<String>,
}

/// The user message: the prompt plus the change itself. With tools, the diff gets about 60% of the budget (the rest
/// is for what the tools return); without, all of it.
pub fn message(
    cfg: &Config,
    pr: &Pr,
    ctx: &Path,
    discussion: &str,
    platform: &str,
    with_tools: bool,
) -> Prepared {
    let read = |n: &str| std::fs::read_to_string(ctx.join(n)).unwrap_or_default();
    let en = cfg.language == "en";
    let line = if with_tools {
        tools_on(&cfg.language)
    } else {
        tools(&cfg.language)
    };
    let mut user = format!(
        "{}\n\n{}\n\n## diffstat.txt\n```\n{}\n```\n\n## commits.txt\n```\n{}\n```\n\n",
        build_prompt(cfg, pr, ctx, discussion, platform, line),
        json_only(&cfg.language),
        read("diffstat.txt").trim(),
        read("commits.txt").trim(),
    );
    let b = budget(cfg);
    let room = if with_tools { b * 6 / 10 } else { b }.saturating_sub(user.len() + 2048);
    let files = split_diff(&read("diff.patch"));
    let p = pack(&files, room);
    user.push_str("## diff.patch\n```diff\n");
    user.push_str(&p.text);
    user.push_str("```\n");
    if !p.left_out.is_empty() {
        let head = match (with_tools, en) {
            (true, true) => "\nThe diffs of these changed files did not fit into this message. Fetch each with get_diff before you conclude:",
            (true, false) => "\n下面这些改动文件的 diff 放不下，没有附在这条消息里。下结论前用 get_diff 逐个取来看：",
            (false, true) => "\nThe diffs of these changed files did not fit and were not sent. Your verdict does not cover them and Nod will not approve on it; if what you can see is not enough for a trustworthy verdict, say so in unable_reason:",
            (false, false) => "\n下面这些改动文件的 diff 放不下，没有发给你。你的结论覆盖不到它们，Nod 不会据此 approve；如果看到的内容不足以给出可信的结论，在 unable_reason 里说明：",
        };
        user.push_str(head);
        user.push('\n');
        for (path, size) in p.left_out.iter().take(LIST_MAX) {
            user.push_str(&format!("- {path} ({} KB)\n", size.div_ceil(1024)));
        }
        if p.left_out.len() > LIST_MAX {
            let more = p.left_out.len() - LIST_MAX;
            user.push_str(&if en {
                format!("- … and {more} more (see diffstat.txt)\n")
            } else {
                format!("- ……还有 {more} 个（见 diffstat.txt）\n")
            });
        }
    }
    Prepared {
        user,
        files,
        included: p.included,
    }
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

fn size(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len())
}

/// What one conversation ended with.
struct Outcome {
    verdict: Verdict,
    model: String,
    requests: u64,
    files: Vec<FileDiff>,
    seen: std::collections::BTreeSet<String>,
}

enum Ended {
    Done(Box<Outcome>),
    /// The endpoint rejected the first request with tools: start over without.
    NoTools,
}

fn converse(
    cfg: &Config,
    pr: &Pr,
    wt: &Path,
    ctx: &Path,
    discussion: &str,
    platform: &str,
    with_tools: bool,
) -> Result<Ended> {
    let en = cfg.language == "en";
    let prep = message(cfg, pr, ctx, discussion, platform, with_tools);
    let mut tools = Tools::new(wt, prep.files, &prep.included);
    let mut msgs = vec![
        json!({"role": "system", "content": SYSTEM}),
        json!({"role": "user", "content": prep.user}),
    ];
    let mut used: usize = msgs.iter().map(size).sum();
    let budget = budget(cfg);
    let (mut rounds, mut requests) = (0usize, 0u64);
    let (mut closing, mut json_mode, mut fixed) = (false, true, false);
    loop {
        if with_tools && !closing && (used + RESULT_MAX > budget || rounds >= MAX_ROUNDS) {
            closing = true;
            let m = json!({"role": "user", "content": if en {
                "The tool budget is used up. Reply now with the JSON verdict, based on what you have seen; if files you did not see could change it, say so in unable_reason."
            } else {
                "工具调用额度已经用完。请根据已经看到的内容，现在只回复 JSON 结论；没看到的文件如果可能影响结论，在 unable_reason 里说明。"
            }});
            used += size(&m);
            msgs.push(m);
        }
        let mut body = json!({
            "model": cfg.review.api.model,
            "temperature": 0,
            "messages": msgs,
        });
        if with_tools {
            body["tools"] = Tools::defs();
            body["tool_choice"] = json!(if closing { "none" } else { "auto" });
        } else if json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        requests += 1;
        let res = match post(cfg, &body) {
            Ok(v) => v,
            // tools or JSON mode not supported by this server: once, at the start
            Err(ureq::Error::Status(400, _)) if with_tools && requests == 1 => {
                return Ok(Ended::NoTools)
            }
            Err(ureq::Error::Status(400, _)) if !with_tools && json_mode => {
                json_mode = false;
                continue;
            }
            Err(ureq::Error::Status(code, r)) => {
                bail!(
                    "the API returned HTTP {code}: {}",
                    crate::util::truncate(r.into_string().unwrap_or_default().trim(), 300)
                )
            }
            Err(e) => bail!("cannot reach the API: {e}"),
        };
        let m = &res["choices"][0]["message"];
        if let Some(calls) = m["tool_calls"].as_array().filter(|c| !c.is_empty()) {
            if !with_tools {
                bail!("the API reply has no message content");
            }
            rounds += 1;
            if rounds > MAX_ROUNDS + 2 {
                bail!("the model kept calling tools after they were withdrawn");
            }
            let mut a = m.clone();
            a["role"] = json!("assistant");
            used += size(&a);
            msgs.push(a);
            for c in calls {
                let name = c["function"]["name"].as_str().unwrap_or("");
                let args = match &c["function"]["arguments"] {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    v => v.to_string(),
                };
                let out = if closing {
                    if en {
                        "No more tool calls. Reply now with the JSON verdict.".to_string()
                    } else {
                        "不能再调用工具了。现在只回复 JSON 结论。".to_string()
                    }
                } else if used + RESULT_MAX > budget {
                    if en {
                        "Skipped: the tool budget is used up.".to_string()
                    } else {
                        "未执行：工具调用额度已经用完。".to_string()
                    }
                } else {
                    tools.call(name, &args)
                };
                let t = json!({"role": "tool", "tool_call_id": c["id"], "content": out});
                used += size(&t);
                msgs.push(t);
            }
            continue;
        }
        let text = m["content"].as_str().unwrap_or("");
        if text.trim().is_empty() && fixed {
            bail!("the API reply has no message content");
        }
        match verdict_from_text(text) {
            Ok(verdict) => {
                return Ok(Ended::Done(Box::new(Outcome {
                    verdict,
                    model: res["model"].as_str().unwrap_or("").to_string(),
                    requests,
                    files: tools.files().to_vec(),
                    seen: tools.seen,
                })))
            }
            Err(e) if fixed => return Err(e),
            Err(_) => {
                // once: ask again for the JSON alone
                fixed = true;
                closing = true;
                for m in [
                    json!({"role": "assistant", "content": text}),
                    json!({"role": "user", "content": if en {
                        "That was not the JSON verdict. Reply with exactly one JSON object that follows the schema, and nothing else."
                    } else {
                        "上一条回复不是要求的 JSON 结论。只回复一个符合 Schema 的 JSON 对象，不要其他文字。"
                    }}),
                ] {
                    used += size(&m);
                    msgs.push(m);
                }
            }
        }
    }
}

impl Engine for Api {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        if cfg.review.api.model.is_empty() {
            return Err(anyhow!("review.api.model is not set"));
        }
        let start = Instant::now();
        let o = match converse(cfg, pr, wt, ctx, discussion, platform, true)? {
            Ended::Done(o) => o,
            Ended::NoTools => {
                crate::warn!(
                    "{}: the API rejected the request with tools; reviewing with the diff alone",
                    pr.key()
                );
                match converse(cfg, pr, wt, ctx, discussion, platform, false)? {
                    Ended::Done(o) => o,
                    Ended::NoTools => unreachable!("only a request with tools ends this way"),
                }
            }
        };
        let mut v = o.verdict;
        // the model's own "_meta", if it wrote one, is not trusted
        v.meta = Default::default();
        for f in &o.files {
            if !o.seen.contains(&f.path) {
                if is_low_value(&f.path) {
                    v.meta.unseen_minor.push(f.path.clone());
                } else {
                    v.meta.unseen.push(f.path.clone());
                }
            }
        }
        v.meta.duration_s = start.elapsed().as_secs();
        v.meta.turns = Some(o.requests);
        v.meta.model = if o.model.is_empty() {
            cfg.review.api.model.clone()
        } else {
            o.model
        };
        v.meta.engine = "api".into();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::http::fake;
    use crate::review::tests::pr;
    use std::fs;

    fn verdict(approve: bool) -> String {
        json!({"summary": "fine", "findings": [], "approve_recommended": approve, "unable_reason": ""}).to_string()
    }

    fn reply(content: &str) -> String {
        json!({"model": "deepseek-chat", "choices": [{"message": {"role": "assistant", "content": content}}]})
            .to_string()
    }

    fn call(id: &str, name: &str, args: Value) -> String {
        json!({"model": "deepseek-chat", "choices": [{"message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": id, "type": "function", "function": {"name": name, "arguments": args.to_string()}}]}}]})
        .to_string()
    }

    fn diff_of(path: &str, lines: usize) -> String {
        let body: String = (0..lines)
            .map(|i| format!("+line {i} of {path}\n"))
            .collect();
        format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n{body}")
    }

    /// A checkout with src/main.rs and the context files of a change to `diff`.
    fn setup(diff: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        let ctx = d.path().join("wt-ctx");
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::create_dir_all(&ctx).unwrap();
        fs::write(wt.join("src/main.rs"), "fn main() {\n    run();\n}\n").unwrap();
        fs::write(ctx.join("diff.patch"), diff).unwrap();
        fs::write(ctx.join("diffstat.txt"), "src/main.rs | 3 +").unwrap();
        fs::write(ctx.join("commits.txt"), "abc fix").unwrap();
        (d, wt, ctx)
    }

    fn config(url: &str, kb: u64) -> Config {
        let mut cfg = Config {
            language: "en".into(),
            ..Default::default()
        };
        cfg.review.api.base_url = format!("{url}/v1");
        cfg.review.api.model = "deepseek-chat".into();
        cfg.review.api.key = "sk-test".into();
        cfg.review.api.max_diff_kb = kb;
        cfg
    }

    fn bodies(f: &fake::Fake) -> Vec<Value> {
        f.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(&s.body).unwrap())
            .collect()
    }

    const ROUTE: &str = "POST /v1/chat/completions";

    #[test]
    fn a_small_change_goes_whole_into_the_message_and_tools_are_offered() {
        let f = fake::serve(vec![(ROUTE, 200, reply(&verdict(true)))]);
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let v = Api
            .ask(&config(&f.url, 0), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert!(v.approve_recommended && v.meta.unseen.is_empty());
        assert_eq!(
            (v.meta.engine.as_str(), v.meta.model.as_str()),
            ("api", "deepseek-chat")
        );
        assert_eq!(f.seen.lock().unwrap()[0].auth, "Bearer sk-test");
        let b = &bodies(&f)[0];
        let user = b["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("+line 2 of src/main.rs") && !user.contains("did not fit"));
        let names: Vec<&str> = b["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["get_diff", "read_file", "list_dir", "search"]);
        assert_eq!(b["tool_choice"], "auto");
    }

    #[test]
    fn the_model_reads_code_and_fetches_what_did_not_fit() {
        // src/mid.rs takes most of the room, so src/big.rs (three pages) is left out but can still be fetched
        let diff = diff_of("src/main.rs", 3)
            + &diff_of("src/mid.rs", 1800)
            + &diff_of("src/big.rs", 1100)
            + &diff_of("Cargo.lock", 2000);
        let f = fake::serve(vec![
            (
                ROUTE,
                200,
                call("c1", "read_file", json!({"path": "src/main.rs"})),
            ),
            (
                ROUTE,
                200,
                call("c2", "get_diff", json!({"path": "src/big.rs"})),
            ),
            (
                ROUTE,
                200,
                call("c3", "get_diff", json!({"path": "src/big.rs", "page": 2})),
            ),
            (
                ROUTE,
                200,
                call("c4", "get_diff", json!({"path": "src/big.rs", "page": 3})),
            ),
            (
                ROUTE,
                200,
                call("c5", "read_file", json!({"path": "../wt-ctx/diff.patch"})),
            ),
            (ROUTE, 200, reply(&verdict(true))),
        ]);
        let (_d, wt, ctx) = setup(&diff);
        let v = Api
            .ask(&config(&f.url, 100), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        assert_eq!(b.len(), 6);
        let first = b[0]["messages"][1]["content"].as_str().unwrap();
        // whole files only: src/big.rs is listed as left out, not cut in half
        assert!(
            first.contains("+line 2 of src/main.rs") && first.contains("+line 1799 of src/mid.rs")
        );
        assert!(!first.contains("of src/big.rs\n+line"));
        assert!(first.contains("- src/big.rs (") && first.contains("- Cargo.lock ("));
        // the tool result went back with its call id
        let after1 = b[1]["messages"].as_array().unwrap();
        let t = after1.last().unwrap();
        assert_eq!(
            (t["role"].as_str(), t["tool_call_id"].as_str()),
            (Some("tool"), Some("c1"))
        );
        assert!(t["content"].as_str().unwrap().contains("2\t    run();"));
        assert_eq!(after1[after1.len() - 2]["tool_calls"][0]["id"], "c1");
        // a path out of the checkout was refused
        let t5 = b[5]["messages"].as_array().unwrap().last().unwrap().clone();
        assert!(
            t5["content"].as_str().unwrap().starts_with("error:"),
            "{t5}"
        );
        // every page of src/big.rs was fetched: seen; Cargo.lock never: listed, not blocking
        assert!(v.meta.unseen.is_empty(), "{:?}", v.meta.unseen);
        assert_eq!(v.meta.unseen_minor, ["Cargo.lock"]);
        assert_eq!(v.meta.turns, Some(6));
    }

    #[test]
    fn a_diff_the_model_never_fetched_is_recorded_as_unseen() {
        let diff = diff_of("src/main.rs", 3) + &diff_of("src/big.rs", 2000);
        let f = fake::serve(vec![(ROUTE, 200, reply(&verdict(true)))]);
        let (_d, wt, ctx) = setup(&diff);
        let v = Api
            .ask(&config(&f.url, 30), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert_eq!(v.meta.unseen, ["src/big.rs"]);
    }

    #[test]
    fn an_endpoint_without_tools_gets_the_diff_alone() {
        let diff = diff_of("src/main.rs", 3) + &diff_of("src/big.rs", 2000);
        let f = fake::serve(vec![
            (
                ROUTE,
                400,
                r#"{"error":{"message":"model does not support tools"}}"#.into(),
            ),
            (
                ROUTE,
                400,
                r#"{"error":{"message":"response_format not supported"}}"#.into(),
            ),
            (
                ROUTE,
                200,
                reply(&format!("Sure:\n```json\n{}\n```", verdict(true))),
            ),
        ]);
        let (_d, wt, ctx) = setup(&diff);
        let v = Api
            .ask(&config(&f.url, 30), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        assert!(b[0]["tools"].is_array());
        assert!(b[1]["tools"].is_null() && b[1]["response_format"]["type"] == "json_object");
        assert!(b[2]["tools"].is_null() && b[2]["response_format"].is_null());
        let user = b[2]["messages"][1]["content"].as_str().unwrap();
        assert!(
            user.contains("You have no tools")
                && user.contains("were not sent")
                && user.contains("- src/big.rs (")
        );
        // whole files only, and the conversation stayed within the budget
        assert!(user.len() <= 30 * 1024, "{}", user.len());
        assert_eq!(v.meta.unseen, ["src/big.rs"]);
    }

    #[test]
    fn the_tools_are_withdrawn_when_the_budget_is_spent() {
        // the model reads the same file again and again
        let f = fake::serve(vec![
            (
                ROUTE,
                200,
                call("c", "read_file", json!({"path": "src/main.rs"})),
            ),
            (
                ROUTE,
                200,
                call("c", "read_file", json!({"path": "src/main.rs"})),
            ),
        ]);
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let err = Api
            .ask(&config(&f.url, 200), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap_err();
        assert!(err.to_string().contains("kept calling tools"), "{err}");
        let b = bodies(&f);
        assert_eq!(b.len(), MAX_ROUNDS + 3);
        let closing = b.iter().position(|x| x["tool_choice"] == "none").unwrap();
        assert_eq!(closing, MAX_ROUNDS);
        let last = b.last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert!(last["content"]
            .as_str()
            .unwrap()
            .starts_with("No more tool calls"));
        // every request stays near the budget
        for x in &b {
            assert!(size(&x["messages"]) < 200 * 1024);
        }
    }

    #[test]
    fn the_byte_budget_ends_the_tool_calls() {
        let lines: String = (0..3000).map(|i| format!("let v{i} = {i};\n")).collect();
        let mut seq: Vec<(&str, u16, String)> = (0..10)
            .map(|i| {
                (
                    ROUTE,
                    200,
                    call(
                        &format!("c{i}"),
                        "read_file",
                        json!({"path": "src/long.rs", "start_line": i * 400 + 1}),
                    ),
                )
            })
            .collect();
        seq.push((ROUTE, 200, reply(&verdict(true))));
        let f = fake::serve(seq);
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        fs::write(wt.join("src/long.rs"), lines).unwrap();
        let v = Api
            .ask(&config(&f.url, 40), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert!(v.approve_recommended);
        let b = bodies(&f);
        let closing = b
            .iter()
            .position(|x| x["tool_choice"] == "none")
            .expect("withdrawn");
        assert!(closing < 10, "withdrawn after {closing} rounds");
        for x in &b {
            assert!(
                size(&x["messages"]) <= 40 * 1024 + RESULT_MAX + 2048,
                "{}",
                size(&x["messages"])
            );
        }
    }

    #[test]
    fn a_reply_that_is_not_json_gets_one_more_chance() {
        let f = fake::serve(vec![
            (ROUTE, 200, reply("Looks good to me!")),
            (ROUTE, 200, reply(&verdict(false))),
        ]);
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let v = Api
            .ask(&config(&f.url, 0), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert!(!v.approve_recommended);
        let b = bodies(&f);
        assert_eq!(b[1]["tool_choice"], "none");
        let f = fake::serve(vec![(ROUTE, 200, reply("nope"))]);
        assert!(Api
            .ask(&config(&f.url, 0), &pr("alice"), &wt, &ctx, "", "GitHub")
            .is_err());
    }

    #[test]
    fn the_budget_follows_the_endpoint() {
        let mut cfg = Config::default();
        for (url, kb) in [
            ("http://localhost:11434/v1", LOCAL_KB),
            ("http://127.0.0.1:8080/v1", LOCAL_KB),
            ("http://[::1]:9000/v1", LOCAL_KB),
            ("http://gpu-box:11434/v1", LOCAL_KB),
            ("http://10.0.0.5:1234/v1", LOCAL_KB),
            ("https://api.deepseek.com/v1", REMOTE_KB),
            ("https://user:pw@api.openai.com/v1", REMOTE_KB),
            ("https://localhost.example.com/v1", REMOTE_KB),
        ] {
            cfg.review.api.base_url = url.into();
            cfg.review.api.max_diff_kb = 0;
            assert_eq!(budget(&cfg), (kb * 1024) as usize, "{url}");
        }
        cfg.review.api.max_diff_kb = 64;
        assert_eq!(budget(&cfg), 64 * 1024, "a set value is used as is");
        cfg.review.api.max_diff_kb = 2;
        assert_eq!(budget(&cfg), 8 * 1024);
        assert_eq!(Config::default().review.api.max_diff_kb, 0);
    }
}
