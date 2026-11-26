//! Any OpenAI-compatible chat completions API (OpenAI, DeepSeek, Qwen/DashScope, OpenRouter, Ollama, LM Studio, …).
//!
//! The message carries the diff stat, the commit list and as many whole files of the diff as fit. The model also gets
//! read-only tools on the checkout (`api_tools`: get_diff, read_file, list_dir, search) and fetches the rest itself;
//! Nod runs them inside the checkout only, nothing is written or executed. An endpoint that rejects tools gets the
//! diff alone. Either way, changed files whose diff the model did not see are recorded in the verdict
//! (`meta.unseen`), and `review::decide` does not approve a review that did not see the whole change.
//!
//! There is no size setting. A review assumes the model's context has room for everything ([`OPEN_WINDOW`]; for a
//! model loaded in a local Ollama, the context it was loaded with) and learns otherwise from the endpoint: a request
//! refused as too long, or a reply that counted far fewer prompt tokens than were sent (Ollama cuts a long prompt
//! silently), narrows the window and the request is sent again. To stay inside the window, the oldest tool results
//! are taken out of the conversation first (code read before diffs), then the diffs in the first message. What was
//! learned is kept for the next review of the same endpoint and model. The only limit on a review is its time
//! (`review.timeout_min`): past it the tools are withdrawn and the model is asked for its verdict.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::api_tools::{
    covered, is_low_value, opaque_section, pack, result_max, split_diff, FileDiff, Tools,
};
use super::{json_only, verdict_from_text, Engine};
use crate::config::Config;
use crate::forge::Pr;
use crate::review::{build_prompt, Verdict};

pub struct Api;

/// The size of the model's context, in bytes of the request, until the endpoint shows otherwise: about 256k tokens,
/// more than most models take, so in practice no limit.
pub const OPEN_WINDOW: usize = 1024 * 1024;
/// Bytes per token when a token count is turned into bytes (real text and code run 3–4).
const BYTES_PER_TOKEN: usize = 3;
/// A reply that counted fewer prompt tokens than one per this many bytes sent got a cut prompt.
const CUT_RATIO: usize = 6;
/// Below this many bytes sent a cut prompt is not looked for.
const CUT_MIN: usize = 12 * 1024;
/// A window smaller than this cannot hold a review.
const MIN_WINDOW: usize = 2 * 1024;
/// The window narrows at most this many times in one review.
const MAX_NARROW: usize = 12;
/// Turns in a row in which the model only asked again for what it already got, before it is asked for its verdict.
const STALE_TURNS: usize = 3;
/// What a short answer to a call that was not run takes (the message around the text).
const SHORT_ANSWER: usize = 320;
/// Room kept beside a tool result for the messages around it (the "not run" answers of the same turn).
const ROOM_SLACK: usize = 1024;
/// The model's names for the files left out of the message are listed up to this many.
const LIST_MAX: usize = 200;
const SYSTEM: &str = "You are a meticulous code reviewer. Your final reply is a single JSON object and nothing else.";

#[cfg(test)]
thread_local! {
    /// Tests: the window a review starts with (0 = as in production).
    pub static START_WINDOW: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
        "You cannot open the paths below directly. Instead you have read-only tools on this PR's checkout, with paths relative to the repository root: get_diff (one changed file's diff), read_file, list_dir and search. The diff stat, the commits and as much of the diff as fits are at the end of this message; fetch every diff marked as not included with get_diff before you conclude. In a long conversation older tool results may be taken out to fit your context, so note what you find as you go. The tools cannot change or run anything."
    } else {
        "你不能直接打开下面提到的路径，但可以用只读工具访问这个 PR 的检出（路径相对仓库根目录）：get_diff 取某个改动文件的 diff，read_file 读文件，list_dir 列目录，search 搜索。改动文件列表、提交列表和放得下的 diff 已经附在消息末尾；标明没有附上的文件，下结论前要用 get_diff 逐个取来看。对话长了，较早的工具结果可能会被移出对话以放进你的上下文，所以边看边记下发现。工具不能修改或执行任何东西。"
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

/// Windows learned from earlier reviews, per (base URL, model).
fn learned() -> &'static Mutex<HashMap<(String, String), usize>> {
    static L: OnceLock<Mutex<HashMap<(String, String), usize>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

fn endpoint(cfg: &Config) -> (String, String) {
    (
        cfg.review.api.base_url.trim_end_matches('/').to_string(),
        cfg.review.api.model.clone(),
    )
}

/// The key of [`learned`]. Tests run in parallel and their fake servers can get a port another test's server had:
/// what one test learned must not reach another.
fn learned_key(cfg: &Config) -> (String, String) {
    #[allow(unused_mut)]
    let mut k = endpoint(cfg);
    #[cfg(test)]
    k.0.push_str(&format!("#{:?}", std::thread::current().id()));
    k
}

/// The window a review starts with: what an earlier review learned, else the context a local Ollama loaded the
/// model with, else [`OPEN_WINDOW`].
fn start_window(cfg: &Config) -> usize {
    #[cfg(test)]
    {
        let w = START_WINDOW.with(|w| w.get());
        if w > 0 {
            return w;
        }
    }
    if let Some(&w) = learned().lock().unwrap().get(&learned_key(cfg)) {
        return w;
    }
    if is_local(&cfg.review.api.base_url) {
        if let Some(t) = ollama_context(cfg) {
            return (t * BYTES_PER_TOKEN).min(OPEN_WINDOW);
        }
    }
    OPEN_WINDOW
}

/// The context length (tokens) a local Ollama has the model loaded with (`/api/ps`), when it is loaded there.
fn ollama_context(cfg: &Config) -> Option<usize> {
    let (base, model) = endpoint(cfg);
    let root = base.strip_suffix("/v1").unwrap_or(&base);
    let v: Value = ureq::get(&format!("{root}/api/ps"))
        .timeout(Duration::from_secs(3))
        .call()
        .ok()?
        .into_json()
        .ok()?;
    let m = v["models"].as_array()?.iter().find(|m| {
        let n = m["name"].as_str().unwrap_or("");
        n == model || n.strip_suffix(":latest") == Some(model.as_str())
    })?;
    m["context_length"]
        .as_u64()
        .map(|t| t as usize)
        .filter(|&t| t > 0)
}

/// Room kept free in the window for the model's reply.
fn reserve(window: usize) -> usize {
    (window / 8).clamp(1024, 32 * 1024)
}

/// Whether an error answer says the request was too long for the model.
fn too_long(code: u16, body: &str) -> bool {
    let b = body.to_lowercase();
    code == 413
        || matches!(code, 400 | 422)
            && [
                "context length",
                "context_length",
                "context window",
                "maximum context",
                "too many tokens",
                "too long",
                "token limit",
                "reduce the length",
                "上下文",
                "过长",
                "太长",
            ]
            .iter()
            .any(|k| b.contains(k))
}

/// The window after the endpoint refused `sent` bytes as too long: in proportion when the answer gives both the count
/// and the limit ("maximum context length is 128000 tokens … resulted in 150000 tokens", "200100 tokens > 200000
/// maximum"), else half of what was sent.
fn after_refusal(body: &str, sent: usize) -> usize {
    let num = |re: &str| -> Option<u64> {
        regex_lite::Regex::new(re)
            .ok()?
            .captures(body)?
            .get(1)?
            .as_str()
            .parse()
            .ok()
    };
    let pair = regex_lite::Regex::new(r"(\d+) tokens? > (\d+)")
        .ok()
        .and_then(|r| r.captures(body))
        .and_then(|c| Some((c[1].parse::<u64>().ok()?, c[2].parse::<u64>().ok()?)))
        .or_else(|| {
            Some((
                num(r"(?i)(?:resulted in|requested) (\d+)")?,
                num(r"(?i)context length is (\d+)")?,
            ))
        });
    match pair {
        Some((count, limit)) if limit > 0 && count > limit => {
            (sent as u64 * limit / count * 85 / 100) as usize
        }
        _ => sent / 2,
    }
}

/// The first user message, and the diff split into files.
pub struct Prepared {
    pub user: String,
    pub files: Vec<FileDiff>,
    /// Files whose whole diff is in the message.
    pub included: Vec<String>,
}

/// The user message, at most about `space` bytes: the prompt plus the change itself. With tools, a change that fits
/// with room for two tool results (`rmax` each) goes in whole; a bigger one leaves room for four results (but takes
/// at least half of what the prompt leaves). Without tools, all of the space.
#[allow(clippy::too_many_arguments)]
pub fn message(
    cfg: &Config,
    pr: &Pr,
    ctx: &Path,
    discussion: &str,
    platform: &str,
    with_tools: bool,
    space: usize,
    rmax: usize,
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
    let files = split_diff(&read("diff.patch"));
    let whole: usize = files.iter().map(|f| f.text.len()).sum();
    // JSON escaping makes the text longer on the wire: count it in
    let left = (space * 9 / 10).saturating_sub(user.len() + 64);
    let room = if !with_tools {
        left
    } else if whole + 2 * rmax <= left {
        whole
    } else {
        left.saturating_sub((4 * rmax).min(left / 2))
    };
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
fn post(cfg: &Config, body: &Value, timeout: Duration) -> std::result::Result<Value, ureq::Error> {
    let url = format!(
        "{}/chat/completions",
        cfg.review.api.base_url.trim_end_matches('/')
    );
    let mut req = ureq::post(&url)
        .timeout(timeout)
        .set("Content-Type", "application/json");
    if !cfg.review.api.key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", cfg.review.api.key));
    }
    req.send_json(body.clone())?
        .into_json()
        .map_err(|e| e.into())
}

/// Whether two tool-call argument strings say the same (`{"a": 1}` and `{"a":1}` do).
fn same_args(a: &str, b: &str) -> bool {
    let p = |s: &str| serde_json::from_str::<Value>(s).ok();
    match (p(a), p(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a.trim() == b.trim(),
    }
}

fn size(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len())
}

/// What one conversation ended with.
struct Outcome {
    verdict: Verdict,
    model: String,
    requests: u64,
    unseen: Vec<String>,
    unseen_minor: Vec<String>,
}

/// Sort the changed files the model did not see: low-value and really binary ones are listed only; the rest hold
/// the approval. A file git showed only as "Binary files … differ" was never seen, whatever the message held.
fn unseen(tools: &Tools, seen: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let (mut hold, mut minor) = (vec![], vec![]);
    for f in tools.files() {
        let opaque = opaque_section(&f.text);
        if !opaque && seen.contains(&f.path) {
            continue;
        }
        if is_low_value(&f.path) || (opaque && tools.really_binary(&f.path)) {
            minor.push(f.path.clone());
        } else {
            hold.push(f.path.clone());
        }
    }
    (hold, minor)
}

enum Ended {
    Done(Box<Outcome>),
    /// The endpoint rejected the first request with tools: start over without.
    NoTools,
}

/// One tool call and where its result sits in the conversation.
struct Call {
    idx: usize,
    name: String,
    args: String,
    /// The part of a changed file's diff the result holds (get_diff).
    range: Option<(String, usize, usize)>,
    /// Taken out of the conversation to fit the window.
    removed: bool,
    /// Its result reached the model (a request holding it was answered, and not cut).
    delivered: bool,
}

/// The conversation of one review, kept inside the model's window.
struct Talk<'a> {
    cfg: &'a Config,
    pr: &'a Pr,
    ctx: &'a Path,
    discussion: &'a str,
    platform: &'a str,
    with_tools: bool,
    window: usize,
    narrowed: usize,
    defs: usize,
    msgs: Vec<Value>,
    calls: Vec<Call>,
    /// Files whose whole diff is in the first message.
    inline: Vec<String>,
    /// What of the change reached the model, whether or not it is still in the conversation.
    seen_ranges: Vec<(String, usize, usize)>,
    seen_inline: BTreeSet<String>,
    tools: Tools,
}

impl Talk<'_> {
    fn total(&self) -> usize {
        self.defs + self.msgs.iter().map(size).sum::<usize>()
    }

    fn removed_text(&self) -> &'static str {
        if self.cfg.language == "en" {
            "[Taken out of the conversation to fit the model's context. You saw it earlier; fetch it again only if you need it.]"
        } else {
            "[为了放进模型的上下文，这条结果已从对话中移除。你之前已经看过；确实还需要时再调用一次工具。]"
        }
    }

    /// Make the next request fit the window: take out the oldest tool results holding no diff, then rounds whose
    /// results are all out, then the oldest diffs, then diffs of the first message. Fails when even the prompt does
    /// not fit.
    fn fit(&mut self) -> Result<()> {
        let target = self.window.saturating_sub(reserve(self.window));
        loop {
            let total = self.total();
            if total <= target {
                return Ok(());
            }
            // results the model has not got yet go last: taking them out unread makes it ask again and again
            let round = self.last_answer();
            if self.remove_one(round, false)
                || self.drop_stub_round()
                || self.remove_one(round, true)
                || self.drop_stub_round()
            {
                continue;
            }
            if self.shrink_first(total - target) || self.remove_one(usize::MAX, true) {
                continue;
            }
            bail!(
                "the model's context holds only about {} KB, too little for this review (it needs at least {} KB); give the model a longer context (for Ollama: OLLAMA_CONTEXT_LENGTH)",
                self.window / 1024,
                (total + reserve(self.window)).div_ceil(1024)
            );
        }
    }

    /// Make the first message about `by` bytes shorter by leaving out diffs it holds (listed for get_diff instead).
    /// The page size stays: page numbers the model was given keep their meaning.
    fn shrink_first(&mut self, by: usize) -> bool {
        if self.inline.is_empty() {
            return false;
        }
        let first = size(&self.msgs[1]);
        let p = message(
            self.cfg,
            self.pr,
            self.ctx,
            self.discussion,
            self.platform,
            self.with_tools,
            first.saturating_sub(by),
            result_max(self.window),
        );
        let m = json!({"role": "user", "content": p.user});
        if size(&m) >= first {
            return false;
        }
        self.msgs[1] = m;
        self.inline = p.included;
        true
    }

    /// Fix the get_diff page size so that one page always fits beside the system message, the first message and the
    /// tool list: a page that does not fit is taken out before the model gets it, and asked for again and again.
    fn size_pages(&mut self) {
        let target = self.window.saturating_sub(reserve(self.window));
        let base = self.defs + size(&self.msgs[0]) + size(&self.msgs[1]);
        let room = target.saturating_sub(base + 2 * ROOM_SLACK) * 4 / 5;
        self.tools
            .set_page_max(result_max(self.window).min(room).max(512));
    }

    /// Take the oldest tool result before message `before` out of the conversation: one holding no diff first, then
    /// (when `diffs`) one holding part of a diff.
    fn remove_one(&mut self, before: usize, diffs: bool) -> bool {
        let pick = |diff: bool| {
            self.calls
                .iter()
                .position(|c| !c.removed && c.idx < before && c.range.is_some() == diff)
        };
        let Some(i) = pick(false).or_else(|| if diffs { pick(true) } else { None }) else {
            return false;
        };
        let stub = self.removed_text();
        let c = &mut self.calls[i];
        c.removed = true;
        c.range = None;
        self.msgs[c.idx]["content"] = json!(stub);
        true
    }

    /// Before a tool call of the latest round: make room for a whole result by taking out older
    /// results (a diff makes way only for another diff), and when there is still not enough,
    /// let this result be only as long as the room left.
    ///
    /// Returns false when this round's earlier results already fill the room: the call is then not run, and the model
    /// asks again in its next turn, after it got those results.
    /// `after` = calls of this turn still to come, each of which gets at least a short answer.
    fn make_room(&mut self, diff: bool, after: usize) -> bool {
        let target = self.window.saturating_sub(reserve(self.window));
        // a page is as long as it is (escaped on the wire); other results can be cut to the room left
        let rmax = if diff {
            self.tools.page_max() * 5 / 4
        } else {
            result_max(self.window)
        };
        loop {
            // dropping a round moves the messages after it: find the latest round again each time
            let round = self.last_answer();
            let fits = self.total() + rmax + ROOM_SLACK + after * SHORT_ANSWER <= target;
            // a get_diff page is never shortened: page numbers must keep their meaning
            let need =
                (self.total() + rmax + ROOM_SLACK + after * SHORT_ANSWER).saturating_sub(target);
            // diffs of the first message the model already got can go (they stay seen)
            let got_first = self.inline.iter().all(|f| self.seen_inline.contains(f));
            if fits
                || !(self.remove_one(round, false)
                    || self.drop_stub_round()
                    || diff && self.remove_one(round, true)
                    || got_first && self.shrink_first(need))
            {
                break;
            }
        }
        let fits = self.total() + rmax + ROOM_SLACK + after * SHORT_ANSWER <= target;
        let round = self.last_answer();
        let first_of_round = !self.calls.iter().any(|c| c.idx > round);
        if !fits && !first_of_round {
            return false;
        }
        if !diff {
            // newlines and tabs are escaped on the wire
            let room =
                target.saturating_sub(self.total() + ROOM_SLACK + after * SHORT_ANSWER) * 4 / 5;
            self.tools.set_max(rmax.min(room).max(512));
        }
        true
    }

    /// The index of the model's latest message (the tool calls being answered).
    fn last_answer(&self) -> usize {
        self.msgs
            .iter()
            .rposition(|m| m["role"] == "assistant")
            .unwrap_or(0)
    }

    /// Drop the oldest round of tool calls whose results were all taken out (never the latest round).
    fn drop_stub_round(&mut self) -> bool {
        let mut a = 0;
        while let Some(off) = self.msgs[a..]
            .iter()
            .position(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
        {
            let start = a + off;
            let mut end = start + 1;
            while end < self.msgs.len() && self.msgs[end]["role"] == "tool" {
                end += 1;
            }
            if end >= self.msgs.len() {
                return false;
            }
            let all_removed = self
                .calls
                .iter()
                .filter(|c| c.idx > start && c.idx < end)
                .all(|c| c.removed);
            if all_removed {
                // what the model wrote along with its calls (its notes) stays
                let note = self.msgs[start]["content"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let from = if note.is_empty() {
                    start
                } else {
                    self.msgs[start] = json!({"role": "assistant", "content": note});
                    start + 1
                };
                let n = end - from;
                self.msgs.drain(from..end);
                self.calls.retain(|c| c.idx < start || c.idx >= end);
                for c in &mut self.calls {
                    if c.idx >= end {
                        c.idx -= n;
                    }
                }
                return true;
            }
            a = end;
        }
        false
    }

    /// The model's context turned out smaller: narrow the window to `to` bytes (half when that is no smaller), keep
    /// it for later reviews, and run the calls not yet answered again with the smaller result size.
    fn narrow(&mut self, to: usize, key: &str) -> Result<()> {
        let to = if to >= self.window {
            self.window / 2
        } else {
            to
        };
        self.narrowed += 1;
        if self.narrowed > MAX_NARROW || to < MIN_WINDOW {
            bail!(
                "the model's context is too small for this review (it refused or cut {} KB); give the model a longer context (for Ollama: OLLAMA_CONTEXT_LENGTH)",
                self.window / 1024
            );
        }
        crate::info!(
            "{key}: the model's context holds less than was sent; narrowing to about {} KB",
            to / 1024
        );
        self.window = to;
        learned().lock().unwrap().insert(learned_key(self.cfg), to);
        self.size_pages();
        let last = self
            .msgs
            .iter()
            .rposition(|m| m["role"] == "assistant")
            .unwrap_or(0);
        for c in self.calls.iter_mut().filter(|c| c.idx > last && !c.removed) {
            let out = self.tools.call(&c.name, &c.args);
            c.range = self.tools.last_range.clone();
            self.msgs[c.idx]["content"] = json!(out);
        }
        Ok(())
    }

    /// A request was answered and not cut: what it held reached the model.
    fn delivered(&mut self) {
        self.seen_inline.extend(self.inline.iter().cloned());
        for c in self.calls.iter_mut().filter(|c| !c.removed && !c.delivered) {
            c.delivered = true;
            self.seen_ranges.extend(c.range.clone());
        }
    }

    /// The changed files whose whole diff reached the model: in the first message, or in get_diff results (also
    /// those taken out of the conversation later; the model read them and was told to note what it found).
    fn seen(&self) -> BTreeSet<String> {
        let mut s = covered(self.tools.files(), &self.seen_ranges);
        s.extend(self.seen_inline.iter().cloned());
        s
    }
}

#[allow(clippy::too_many_arguments)]
fn converse(
    cfg: &Config,
    pr: &Pr,
    wt: &Path,
    ctx: &Path,
    discussion: &str,
    platform: &str,
    with_tools: bool,
    start: Instant,
) -> Result<Ended> {
    let en = cfg.language == "en";
    let key = pr.key();
    // the review's time limit: past it the tools are withdrawn and one last request (at most 5 minutes) may answer,
    // which keeps the review well inside the limit after which the worker takes it for dead and queues it again
    let limit = Duration::from_secs_f64(cfg.review.timeout_min * 60.0);
    let mut late_sent = false;
    // a request that broke off (timed out at the limit, or the network) after some reading: withdraw and ask once
    let mut broke_off = false;
    let window = start_window(cfg);
    let defs = if with_tools { size(&Tools::defs()) } else { 0 };
    let rmax = result_max(window);
    let space = window.saturating_sub(reserve(window) + SYSTEM.len() + 64 + defs);
    let prep = message(cfg, pr, ctx, discussion, platform, with_tools, space, rmax);
    let mut t = Talk {
        cfg,
        pr,
        ctx,
        discussion,
        platform,
        with_tools,
        window,
        narrowed: 0,
        defs,
        msgs: vec![
            json!({"role": "system", "content": SYSTEM}),
            json!({"role": "user", "content": prep.user}),
        ],
        calls: vec![],
        inline: prep.included,
        seen_ranges: vec![],
        seen_inline: BTreeSet::new(),
        tools: Tools::new(wt, prep.files, rmax),
    };
    t.size_pages();
    let mut requests = 0u64;
    // whether the endpoint has answered at all yet
    let mut answered = false;
    let (mut closing, mut json_mode, mut fixed) = (false, true, false);
    // after the tools were withdrawn: how often the model still called one
    let mut ignored = 0;
    // calls already run (name and arguments), and how many turns in a row ran none that was new
    let mut ran: std::collections::HashSet<String> = Default::default();
    let mut stale = 0;
    loop {
        if crate::util::stop_requested(&key) {
            bail!("the review was stopped");
        }
        let late = start.elapsed() >= limit;
        if late && late_sent {
            bail!(
                "the API review did not finish within {} minutes",
                cfg.review.timeout_min
            );
        }
        if with_tools && !closing && late {
            closing = true;
            t.msgs.push(json!({"role": "user", "content": if en {
                "Time is up. Reply now with the JSON verdict, based on what you have seen; if files you did not see could change it, say so in unable_reason."
            } else {
                "时间到了。请根据已经看到的内容，现在只回复 JSON 结论；没看到的文件如果可能影响结论，在 unable_reason 里说明。"
            }}));
        }
        t.fit()?;
        let mut body = json!({
            "model": cfg.review.api.model,
            "temperature": 0,
            "messages": t.msgs,
        });
        if with_tools && ignored == 0 {
            body["tools"] = Tools::defs();
            body["tool_choice"] = json!(if closing { "none" } else { "auto" });
        } else if !with_tools && json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        requests += 1;
        let sent = size(&body["messages"]);
        let timeout = if late {
            late_sent = true;
            limit.clamp(Duration::from_secs(60), Duration::from_secs(300))
        } else {
            limit
                .saturating_sub(start.elapsed())
                .max(Duration::from_secs(60))
        };
        let res = match post(cfg, &body, timeout) {
            Ok(v) => v,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                if too_long(code, &text) {
                    t.narrow(after_refusal(&text, sent), &key)?;
                    late_sent = false;
                    continue;
                }
                match code {
                    // tools not supported by this server: once, at the start
                    400 if with_tools && !answered => return Ok(Ended::NoTools),
                    // a server that rejects tool_choice "none": ask once more without the tools
                    400 if with_tools && closing && ignored == 0 => {
                        ignored = 1;
                        continue;
                    }
                    400 if !with_tools && json_mode => {
                        json_mode = false;
                        continue;
                    }
                    _ => bail!(
                        "the API returned HTTP {code}: {}",
                        crate::util::truncate(text.trim(), 300)
                    ),
                }
            }
            Err(ureq::Error::Transport(e))
                if with_tools && answered && !broke_off && !late_sent =>
            {
                crate::warn!("{key}: an API request broke off ({e}); asking for the verdict");
                broke_off = true;
                if !closing {
                    closing = true;
                    t.msgs.push(json!({"role": "user", "content": if en {
                        "Time is up. Reply now with the JSON verdict, based on what you have seen; if files you did not see could change it, say so in unable_reason."
                    } else {
                        "时间到了。请根据已经看到的内容，现在只回复 JSON 结论；没看到的文件如果可能影响结论，在 unable_reason 里说明。"
                    }}));
                }
                // the last request gets its own time even past the limit
                late_sent = false;
                continue;
            }
            Err(e) => bail!("cannot reach the API: {e}"),
        };
        // a server that cuts a long prompt silently (Ollama) still counts what it kept
        if let Some(n) = res["usage"]["prompt_tokens"].as_u64() {
            let n = n as usize;
            if n > 0 && sent > CUT_MIN && sent > n * CUT_RATIO {
                crate::warn!(
                    "{key}: the API counted {n} prompt tokens for {} KB; it cut the prompt",
                    sent / 1024
                );
                t.narrow(n * BYTES_PER_TOKEN * 9 / 10, &key)?;
                late_sent = false;
                continue;
            }
        }
        answered = true;
        t.delivered();
        let m = &res["choices"][0]["message"];
        if let Some(calls) = m["tool_calls"].as_array().filter(|c| !c.is_empty()) {
            if !with_tools {
                bail!("the API reply has no message content");
            }
            if closing {
                // tool_choice "none" ignored: next time without the tools at all, then give up
                ignored += 1;
                if ignored > 1 {
                    bail!("the model kept calling tools after they were withdrawn");
                }
            }
            let mut fresh = false;
            let mut a = m.clone();
            a["role"] = json!("assistant");
            t.msgs.push(a);
            for (i, c) in calls.iter().enumerate() {
                let name = c["function"]["name"].as_str().unwrap_or("").to_string();
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
                } else {
                    crate::info!(
                        "{key}: the model calls {name} {}",
                        crate::util::truncate(&args, 200)
                    );
                    // a call the model already got the result of (still above, or taken out after it got it): not run
                    // again, which in a small context would push out what it got next and go round in circles
                    let same = |x: &Call| x.name == name && same_args(&x.args, &args);
                    if let Some(x) = t
                        .calls
                        .iter()
                        .find(|x| same(x) && (x.delivered || !x.removed))
                    {
                        let text = match (x.removed, en) {
                            (false, true) => "Not run again: its result is already above in this conversation.",
                            (false, false) => "没有再执行：它的结果已经在上面的对话里了。",
                            (true, true) => "Not run again: you got its result earlier; it was taken out to fit your context. Go on with what you have not seen yet and rely on your notes.",
                            (true, false) => "没有再执行：你之前已经拿到过它的结果，为了放进上下文已移出。请继续看还没看过的内容，这部分靠你的笔记。",
                        };
                        crate::info!("{key}: {name} not run again: the model got it earlier");
                        t.msgs.push(
                            json!({"role": "tool", "tool_call_id": c["id"], "content": text}),
                        );
                        continue;
                    }
                    if !t.make_room(name == "get_diff", calls.len() - i - 1) {
                        crate::info!("{key}: {name} not run: no room left in this turn");
                        t.msgs.push(json!({"role": "tool", "tool_call_id": c["id"], "content": if en {
                            "Not run: the results of this turn already fill your context. Ask for it again in your next turn."
                        } else {
                            "未执行：这一轮的结果已经占满你的上下文。下一轮再调用一次。"
                        }}));
                        continue;
                    }
                    if ran.insert(format!("{name} {args}")) {
                        fresh = true;
                    }
                    let out = t.tools.call(&name, &args);
                    t.calls.push(Call {
                        idx: t.msgs.len(),
                        name,
                        args,
                        range: t.tools.last_range.clone(),
                        removed: false,
                        delivered: false,
                    });
                    out
                };
                t.msgs
                    .push(json!({"role": "tool", "tool_call_id": c["id"], "content": out}));
            }
            // asking for the same things over and over (what it got does not stay in a small context): no progress
            stale = if fresh { 0 } else { stale + 1 };
            if stale >= STALE_TURNS && !closing {
                crate::warn!(
                    "{key}: the model keeps asking for what it already got; asking for the verdict"
                );
                closing = true;
                t.msgs.push(json!({"role": "user", "content": if en {
                    "You keep asking for what you already got. Reply now with the JSON verdict, based on what you have seen; if files you did not see could change it, say so in unable_reason."
                } else {
                    "你一直在重复要已经给过的内容。请根据已经看到的内容，现在只回复 JSON 结论；没看到的文件如果可能影响结论，在 unable_reason 里说明。"
                }}));
            }
            continue;
        }
        let text = m["content"].as_str().unwrap_or("");
        if text.trim().is_empty() && fixed {
            bail!("the API reply has no message content");
        }
        match verdict_from_text(text) {
            Ok(verdict) => {
                let (unseen, unseen_minor) = unseen(&t.tools, &t.seen());
                return Ok(Ended::Done(Box::new(Outcome {
                    verdict,
                    model: res["model"].as_str().unwrap_or("").to_string(),
                    requests,
                    unseen,
                    unseen_minor,
                })));
            }
            Err(e) if fixed => return Err(e),
            Err(_) => {
                // once: ask again for the JSON alone
                fixed = true;
                closing = true;
                t.msgs.push(json!({"role": "assistant", "content": text}));
                t.msgs.push(json!({"role": "user", "content": if en {
                    "That was not the JSON verdict. Reply with exactly one JSON object that follows the schema, and nothing else."
                } else {
                    "上一条回复不是要求的 JSON 结论。只回复一个符合 Schema 的 JSON 对象，不要其他文字。"
                }}));
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
        // no child process to kill: a direct approval asks this review to stop between its requests
        let _running = crate::util::track_in_process(&pr.key());
        let o = match converse(cfg, pr, wt, ctx, discussion, platform, true, start)? {
            Ended::Done(o) => o,
            Ended::NoTools => {
                crate::warn!(
                    "{}: the API rejected the request with tools; reviewing with the diff alone",
                    pr.key()
                );
                match converse(cfg, pr, wt, ctx, discussion, platform, false, start)? {
                    Ended::Done(o) => o,
                    Ended::NoTools => unreachable!("only a request with tools ends this way"),
                }
            }
        };
        let mut v = o.verdict;
        // the model's own "_meta", if it wrote one, is not trusted
        v.meta = Default::default();
        v.meta.unseen = o.unseen;
        v.meta.unseen_minor = o.unseen_minor;
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

    /// Settings for the fake at `url`; the review starts with a window of `kb` KiB (0 = as in production).
    fn config(url: &str, kb: usize) -> Config {
        START_WINDOW.with(|w| w.set(kb * 1024));
        let mut cfg = Config {
            language: "en".into(),
            ..Default::default()
        };
        cfg.review.api.base_url = format!("{url}/v1");
        cfg.review.api.model = "deepseek-chat".into();
        cfg.review.api.key = "sk-test".into();
        cfg
    }

    /// The chat requests the fake got (not the probe for Ollama's /api/ps).
    fn bodies(f: &fake::Fake) -> Vec<Value> {
        f.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path.ends_with("/chat/completions"))
            .map(|s| serde_json::from_str(&s.body).unwrap())
            .collect()
    }

    fn tool_results(b: &Value) -> Vec<String> {
        b["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap().to_string())
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
        let auth = f.seen.lock().unwrap().last().unwrap().auth.clone();
        assert_eq!(auth, "Bearer sk-test");
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

    /// With no setting, a change of a few hundred KB goes into the first message whole.
    #[test]
    fn a_big_change_goes_whole_into_the_message_by_default() {
        let diff = diff_of("src/main.rs", 3) + &diff_of("src/big.rs", 12_000); // about 300 KB
        let f = fake::serve(vec![(ROUTE, 200, reply(&verdict(true)))]);
        let (_d, wt, ctx) = setup(&diff);
        let v = Api
            .ask(&config(&f.url, 0), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        let user = b[0]["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("+line 11999 of src/big.rs") && !user.contains("did not fit"));
        assert!(v.meta.unseen.is_empty());
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
            .ask(&config(&f.url, 140), &pr("alice"), &wt, &ctx, "", "GitHub")
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
        // whole files only, and the request stayed within the window
        assert!(size(&b[2]["messages"]) <= 30 * 1024);
        assert_eq!(v.meta.unseen, ["src/big.rs"]);
    }

    /// There is no limit on rounds or bytes sent: the model reads as long as it wants (within the time limit). The
    /// oldest code it read makes room in a small window; the diffs it fetched stay as long as possible.
    #[test]
    fn the_model_reads_as_long_as_it_wants_and_old_reads_make_room() {
        let lines: String = (0..3000).map(|i| format!("let v{i} = {i};\n")).collect();
        let diff = diff_of("src/main.rs", 3) + &diff_of("src/big.rs", 190);
        let mut seq = vec![(
            ROUTE,
            200,
            call("d1", "get_diff", json!({"path": "src/big.rs"})),
        )];
        for i in 0..60 {
            // each read is a new one
            let args = json!({"path": "src/long.rs", "start_line": (i % 7) * 400 + 1, "max_lines": 400 - i});
            let mut c: Value =
                serde_json::from_str(&call(&format!("c{i}"), "read_file", args)).unwrap();
            if i == 0 {
                c["choices"][0]["message"]["content"] = json!("NOTE-1: v0 is never read");
            }
            seq.push((ROUTE, 200, c.to_string()));
        }
        seq.push((ROUTE, 200, reply(&verdict(true))));
        let f = fake::serve(seq);
        let (_d, wt, ctx) = setup(&diff);
        fs::write(wt.join("src/long.rs"), lines).unwrap();
        // with a long description, src/big.rs (one page of about 4 KB) does not go into the first message
        let mut p = pr("alice");
        p.body = "x".repeat(6000);
        let v = Api
            .ask(&config(&f.url, 24), &p, &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        assert_eq!(b.len(), 62);
        assert!(b.iter().all(|x| x["tool_choice"] == "auto"));
        for x in &b {
            let n = size(&x["messages"]) + size(&x["tools"]);
            assert!(n <= 24 * 1024, "{n}");
        }
        let last = tool_results(b.last().unwrap());
        // the old reads went out of the conversation, the model's note with them stayed
        assert!(last.len() < 10, "{} results still in", last.len());
        let msgs = b.last().unwrap()["messages"].as_array().unwrap().clone();
        assert!(msgs.iter().any(|m| m["role"] == "assistant"
            && m["content"] == "NOTE-1: v0 is never read"
            && m["tool_calls"].is_null()));
        // the fetched diff outlived the reads: still in the conversation, so seen
        assert!(last.iter().any(|r| r.contains("+line 0 of src/big.rs")));
        assert!(v.meta.unseen.is_empty(), "{:?}", v.meta.unseen);
        assert!(v.approve_recommended);
    }

    /// A small window holds one diff at a time: each fetch takes the one before out of the conversation. A diff the
    /// model got still counts as seen; one taken out before any answer came does not (see the cut-prompt test).
    #[test]
    fn diffs_taken_out_after_the_model_got_them_still_count_as_seen() {
        let diff = diff_of("src/main.rs", 3)
            + &diff_of("src/a.rs", 160)
            + &diff_of("src/b.rs", 160)
            + &diff_of("src/c.rs", 160);
        let seq = vec![
            (
                ROUTE,
                200,
                call("a", "get_diff", json!({"path": "src/a.rs"})),
            ),
            (
                ROUTE,
                200,
                call("b", "get_diff", json!({"path": "src/b.rs"})),
            ),
            (
                ROUTE,
                200,
                call("c", "get_diff", json!({"path": "src/c.rs"})),
            ),
            (ROUTE, 200, reply(&verdict(true))),
        ];
        let f = fake::serve(seq);
        let (_d, wt, ctx) = setup(&diff);
        // a long description leaves no room for the diffs in the first message; each diff is one page
        let mut p = pr("alice");
        p.body = "x".repeat(4000);
        let v = Api
            .ask(&config(&f.url, 20), &p, &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        // the window holds one of these diffs at a time: each fetch takes the one before out
        let last = tool_results(b.last().unwrap());
        assert!(!last
            .iter()
            .any(|r| r.contains("src/a.rs") || r.contains("src/b.rs")));
        assert!(last.iter().any(|r| r.contains("+line 0 of src/c.rs")));
        assert!(v.meta.unseen.is_empty(), "{:?}", v.meta.unseen);
    }

    /// Live (24 KB window): the model asked for seven diffs at once, their results did not fit together and all were
    /// taken out before it got any, so it asked again for 25 minutes. Calls that do not fit beside the earlier ones of
    /// the same turn are not run; the model asks for them in its next turn.
    #[test]
    fn calls_that_do_not_fit_in_one_turn_wait_for_the_next() {
        let names = ["a", "b", "c", "d", "e"];
        let diff: String = diff_of("src/main.rs", 3)
            + &names
                .iter()
                .map(|n| diff_of(&format!("src/{n}.rs"), 160))
                .collect::<String>();
        let many = |ids: &[&str]| {
            let calls: Vec<Value> = ids
                .iter()
                .map(|n| {
                    json!({"id": n, "type": "function", "function": {"name": "get_diff",
                    "arguments": json!({"path": format!("src/{n}.rs")}).to_string()}})
                })
                .collect();
            json!({"model": "m", "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": calls}}]})
                .to_string()
        };
        let f = fake::serve(vec![
            (ROUTE, 200, many(&names)),
            (ROUTE, 200, many(&names)),
            (ROUTE, 200, many(&names[2..])),
            (ROUTE, 200, many(&names[3..])),
            (ROUTE, 200, many(&names[4..])),
            (ROUTE, 200, reply(&verdict(true))),
        ]);
        let (_d, wt, ctx) = setup(&diff);
        let mut p = pr("alice");
        p.body = "x".repeat(4000);
        let v = Api
            .ask(&config(&f.url, 20), &p, &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        let second = tool_results(&b[1]);
        assert!(second[0].starts_with("[page 1/1.]"), "{}", second[0]);
        assert!(second[4].starts_with("Not run"), "{}", second[4]);
        // the whole list again: a is still above and is not run again, b gets the room
        let third = tool_results(&b[2]);
        assert!(
            third.iter().any(|r| r.starts_with("Not run again")),
            "{third:?}"
        );
        assert!(
            third.iter().any(|r| r.contains("+line 0 of src/b.rs")),
            "{third:?}"
        );
        for x in &b {
            let n = size(&x["messages"]) + size(&x["tools"]);
            assert!(n <= 20 * 1024, "{n}");
        }
        assert!(v.meta.unseen.is_empty(), "{:?}", v.meta.unseen);
    }

    /// Live (24 KB window): the model asked for the same diffs about 160 times each for 25 minutes. After a few turns
    /// that ran nothing new, the tools are withdrawn and the model is asked for its verdict.
    #[test]
    fn a_model_going_round_in_circles_is_asked_for_its_verdict() {
        let again = || (ROUTE, 200, call("c", "list_dir", json!({"path": "src"})));
        let f = fake::serve(vec![
            again(),
            again(),
            again(),
            again(),
            (ROUTE, 200, reply(&verdict(false))),
        ]);
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let v = Api
            .ask(&config(&f.url, 200), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert!(!v.approve_recommended);
        let b = bodies(&f);
        assert_eq!(b.len(), 5);
        assert_eq!(b[3]["tool_choice"], "auto");
        assert_eq!(b[4]["tool_choice"], "none");
    }

    /// Regression (redteam-1, V4): a long line in an ordinary file's diff came back cut, and the file still counted
    /// as seen. Fetching it page by page, as the model is told, now gets all of it; stopping early leaves it unseen.
    #[test]
    fn a_long_line_is_seen_only_when_all_of_it_was_fetched() {
        let long = format!(
            "diff --git a/src/data.rs b/src/data.rs
--- a/src/data.rs
+++ b/src/data.rs
@@ -0,0 +1 @@
+{}END_MARKER
",
            "x".repeat(60_000)
        );
        // src/mid.rs fills what the message may use, so src/data.rs (one 60 KB line) has to be fetched; src/other.rs
        // is never fetched
        let diff = diff_of("src/main.rs", 3)
            + &diff_of("src/mid.rs", 3800)
            + &long
            + &diff_of("src/other.rs", 3000);
        let (_d, wt, ctx) = setup(&diff);
        // more pages than there are: the extra asks get an error, the file is seen once the last page came
        let pages = 60_000 / (result_max(260 * 1024) - 100) + 3;
        let mut seq: Vec<(&str, u16, String)> = (1..=pages)
            .map(|p| {
                let args = json!({"path": "src/data.rs", "page": p});
                (ROUTE, 200, call(&format!("p{p}"), "get_diff", args))
            })
            .collect();
        seq.push((ROUTE, 200, reply(&verdict(true))));
        let f = fake::serve(seq);
        let v = Api
            .ask(&config(&f.url, 260), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        let first = b[0]["messages"][1]["content"].as_str().unwrap();
        assert!(first.contains("+line 3799 of src/mid.rs") && !first.contains("END_MARKER"));
        let results: String = tool_results(b.last().unwrap()).concat();
        assert!(results.contains("END_MARKER") && !results.contains("output cut"));
        assert_eq!(v.meta.unseen, ["src/other.rs"]);
        // only the first page: unseen, so no approval
        let f = fake::serve(vec![
            (
                ROUTE,
                200,
                call("p1", "get_diff", json!({"path": "src/data.rs"})),
            ),
            (ROUTE, 200, reply(&verdict(true))),
        ]);
        let v = Api
            .ask(&config(&f.url, 260), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert_eq!(v.meta.unseen, ["src/data.rs", "src/other.rs"]);
    }

    /// Regression (redteam-1): in a small window, a PR description of a few KB left no room for a single tool call.
    /// Tool results shrink with the window, so a few calls still fit.
    #[test]
    fn a_small_window_can_still_fetch_a_small_diff() {
        let diff = diff_of("src/main.rs", 3) + &diff_of("src/big.rs", 60);
        let (_d, wt, ctx) = setup(&diff);
        let f = fake::serve(vec![
            (
                ROUTE,
                200,
                call("c1", "get_diff", json!({"path": "src/big.rs"})),
            ),
            (
                ROUTE,
                200,
                call("c2", "get_diff", json!({"path": "src/big.rs", "page": 2})),
            ),
            (ROUTE, 200, reply(&verdict(true))),
        ]);
        let mut p = pr("alice");
        p.body = "背景说明。".repeat(200); // 3 KB
        let cfg = config(&f.url, 16);
        let v = Api.ask(&cfg, &p, &wt, &ctx, "", "GitHub").unwrap();
        let b = bodies(&f);
        assert_eq!(b[0]["tool_choice"], "auto");
        assert!(
            v.meta.unseen.is_empty(),
            "{:?} after {} requests",
            v.meta.unseen,
            b.len()
        );
        for x in &b {
            let n = size(&x["messages"]) + size(&x["tools"]);
            assert!(n <= 16 * 1024, "{n}");
        }
    }

    /// A request refused as too long narrows the window in proportion to what the error says and goes again; the next
    /// review of the same endpoint and model starts with the narrower window.
    #[test]
    fn a_request_refused_as_too_long_is_sent_again_smaller() {
        let diff = diff_of("src/main.rs", 3) + &diff_of("src/big.rs", 12_000); // about 300 KB
        let refusal = json!({"error": {"message": "This model's maximum context length is 32768 tokens. However, your messages resulted in 98304 tokens. Please reduce the length of the messages.", "code": "context_length_exceeded"}}).to_string();
        let f = fake::serve(vec![
            (ROUTE, 400, refusal),
            (ROUTE, 200, reply(&verdict(true))),
            (ROUTE, 200, reply(&verdict(true))),
        ]);
        let (_d, wt, ctx) = setup(&diff);
        let cfg = config(&f.url, 0);
        let v = Api
            .ask(&cfg, &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        assert_eq!(b.len(), 2);
        let sent = size(&b[0]["messages"]);
        let second = size(&b[1]["messages"]) + size(&b[1]["tools"]);
        assert!(sent > 250 * 1024);
        assert!(second <= sent / 3, "{second} after {sent}");
        assert!(b[1]["tools"].is_array(), "still with tools");
        let user = b[1]["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("- src/big.rs ("));
        assert_eq!(v.meta.unseen, ["src/big.rs"]);
        // learned
        Api.ask(&cfg, &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        assert_eq!(b.len(), 3);
        assert!(size(&b[2]["messages"]) <= second);
    }

    /// Ollama cuts a long prompt silently; the prompt tokens it counted give it away, and the request goes again
    /// within what it kept. A model already loaded tells its context through /api/ps.
    #[test]
    fn a_prompt_the_server_cut_silently_is_sent_again_smaller() {
        let diff = diff_of("src/main.rs", 3) + &diff_of("src/big.rs", 4000); // about 100 KB
        let cut = json!({"model": "deepseek-chat", "usage": {"prompt_tokens": 4096},
            "choices": [{"message": {"role": "assistant", "content": verdict(true)}}]})
        .to_string();
        let fine = json!({"model": "deepseek-chat", "usage": {"prompt_tokens": 2500},
            "choices": [{"message": {"role": "assistant", "content": verdict(true)}}]})
        .to_string();
        let f = fake::serve(vec![(ROUTE, 200, cut), (ROUTE, 200, fine)]);
        let (_d, wt, ctx) = setup(&diff);
        let v = Api
            .ask(&config(&f.url, 0), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        assert_eq!(b.len(), 2);
        assert!(size(&b[0]["messages"]) > 90 * 1024);
        let n = size(&b[1]["messages"]) + size(&b[1]["tools"]);
        assert!(n <= 4096 * BYTES_PER_TOKEN, "{n}");
        assert_eq!(
            v.meta.unseen,
            ["src/big.rs"],
            "the cut verdict was not used"
        );

        // loaded in Ollama with 8192 tokens: the first request already fits
        let ps = json!({"models": [{"name": "deepseek-chat:latest", "context_length": 8192}]})
            .to_string();
        let f = fake::serve(vec![
            ("GET /api/ps", 200, ps),
            (ROUTE, 200, reply(&verdict(true))),
        ]);
        Api.ask(&config(&f.url, 0), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        let b = bodies(&f);
        assert_eq!(b.len(), 1);
        assert!(size(&b[0]["messages"]) + size(&b[0]["tools"]) <= 8192 * BYTES_PER_TOKEN);
    }

    #[test]
    fn a_window_too_small_for_the_prompt_says_so() {
        let f = fake::serve(vec![(ROUTE, 200, reply(&verdict(true)))]);
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let mut p = pr("alice");
        p.body = "x".repeat(8000);
        let err = Api
            .ask(&config(&f.url, 4), &p, &wt, &ctx, "", "GitHub")
            .unwrap_err();
        assert!(
            err.to_string().contains("too little for this review"),
            "{err}"
        );
        assert!(bodies(&f).is_empty());
    }

    #[test]
    fn what_the_endpoint_says_about_length() {
        assert!(too_long(
            400,
            r#"{"error":{"code":"context_length_exceeded"}}"#
        ));
        assert!(too_long(
            400,
            "prompt is too long: 210000 tokens > 200000 maximum"
        ));
        assert!(too_long(413, "Request Entity Too Large"));
        assert!(too_long(400, "输入内容过长"));
        assert!(!too_long(429, "rate limit: too many tokens per minute"));
        assert!(!too_long(400, "model does not support tools"));
        assert!(!too_long(403, "超出额度"));
        assert_eq!(
            after_refusal("prompt is too long: 200000 tokens > 100000 maximum", 1000),
            425
        );
        assert_eq!(
            after_refusal(
                "maximum context length is 1000 tokens. However, your messages resulted in 4000 tokens",
                8000
            ),
            1700
        );
        assert_eq!(after_refusal("too long", 8000), 4000);
    }

    /// Regression (redteam-2): a server that rejects the closing request (tool_choice "none") failed the review.
    #[test]
    fn a_rejected_closing_request_is_sent_again_without_tools() {
        let f = fake::serve(vec![
            (ROUTE, 200, reply("Looks good to me!")),
            (
                ROUTE,
                400,
                r#"{"error":{"message":"tool_choice none not supported"}}"#.into(),
            ),
            (ROUTE, 200, reply(&verdict(true))),
        ]);
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let v = Api
            .ask(&config(&f.url, 200), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert!(v.approve_recommended);
        let b = bodies(&f);
        assert_eq!(b[1]["tool_choice"], "none");
        assert!(b[2]["tools"].is_null());
    }

    /// Regression (redteam-2): a long review had no overall time limit and could not be stopped, while the worker
    /// queues a review again once it runs past the limit. Past the limit the tools are withdrawn and one last request
    /// may answer; a stop request ends it between requests.
    #[test]
    fn the_review_keeps_to_its_time_limit_and_can_be_stopped() {
        let calls = || -> Vec<(&str, u16, String)> {
            (0..30)
                .map(|i| {
                    (
                        ROUTE,
                        200,
                        call(
                            &format!("c{i}"),
                            "list_dir",
                            json!({"path": format!("src/{i}")}),
                        ),
                    )
                })
                .collect()
        };
        let f = fake::serve(calls());
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let mut cfg = config(&f.url, 200);
        cfg.review.timeout_min = 0.0; // already past it
        let err = Api
            .ask(&cfg, &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap_err();
        assert!(err.to_string().contains("did not finish within"), "{err}");
        let b = bodies(&f);
        assert_eq!(b.len(), 1, "only the last request was sent");
        assert_eq!(b[0]["tool_choice"], "none");

        // stopped from outside (a direct approval) while it runs
        let f = fake::serve(calls());
        let cfg = config(&f.url, 200);
        let mut p = pr("alice");
        p.number = 9701; // a key of its own: tests run side by side and a stop is per PR
        let key = p.key();
        let seen = f.seen.clone();
        let stopper = std::thread::spawn(move || {
            while seen.lock().unwrap().len() < 3 {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                crate::util::abort(&key),
                "a running API review counts as stoppable"
            );
        });
        let err = Api.ask(&cfg, &p, &wt, &ctx, "", "GitHub").unwrap_err();
        stopper.join().unwrap();
        assert!(err.to_string().contains("stopped"), "{err}");
        assert!(f.seen.lock().unwrap().len() < 30);
        assert!(!crate::util::abort_target_exists(&p.key()));
    }

    /// Regression (redteam-3): one `-diff` line in the PR's .gitattributes made git print a source file's change as
    /// "Binary files … differ"; that line counted as seen and the review approved. A file git showed no changes of is
    /// never seen, and only a really binary one (binary extension and content, or deleted) is let through.
    #[test]
    fn a_change_git_did_not_show_holds_the_approval_unless_really_binary() {
        let bin = |p: &str| {
            format!(
                "diff --git a/{p} b/{p}
index 1..2 100644
Binary files a/{p} and b/{p} differ
"
            )
        };
        let diff = diff_of("src/main.rs", 3)
            + &diff_of(".gitattributes", 1)
            + &bin("src/auth.rs")
            + &bin("img/logo.png")
            + &bin("img/fake.png")
            + &bin("img/gone.png");
        let (_d, wt, ctx) = setup(&diff);
        fs::write(
            wt.join("src/auth.rs"),
            "fn check(t: &str) -> bool { t == \"letmein\" }
",
        )
        .unwrap();
        fs::create_dir_all(wt.join("img")).unwrap();
        fs::write(wt.join("img/logo.png"), [0x89u8, b'P', b'N', b'G', 0, 0, 1]).unwrap();
        fs::write(
            wt.join("img/fake.png"),
            "fn main() {}
",
        )
        .unwrap();
        for with_tools in [true, false] {
            let mut routes = vec![];
            if !with_tools {
                routes.push((
                    ROUTE,
                    400,
                    r#"{"error":{"message":"no tools"}}"#.to_string(),
                ));
            }
            routes.push((ROUTE, 200, reply(&verdict(true))));
            let f = fake::serve(routes);
            let v = Api
                .ask(&config(&f.url, 200), &pr("alice"), &wt, &ctx, "", "GitHub")
                .unwrap();
            assert_eq!(
                v.meta.unseen,
                ["src/auth.rs", "img/fake.png"],
                "tools: {with_tools}"
            );
            assert_eq!(v.meta.unseen_minor, ["img/logo.png", "img/gone.png"]);
            let c = Config::default();
            assert_eq!(
                crate::review::decide(&c, &pr("alice"), &v, "success", "me").0,
                "COMMENT"
            );
        }
    }

    /// Regression (redteam-3): a request that broke off near the time limit (or on a network error) failed the whole
    /// review and threw away what was read. Now the tools are withdrawn and one last request asks for the verdict.
    #[test]
    fn a_request_that_breaks_off_is_followed_by_one_for_the_verdict() {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let replies = [
            Some(call("c1", "list_dir", json!({"path": "src"}))),
            None, // the connection drops without an answer
            Some(reply(&verdict(true))),
        ];
        let server = std::thread::spawn(move || {
            let mut bodies = vec![];
            for answer in replies {
                let (mut s, _) = listener.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut b = vec![0; len];
                r.read_exact(&mut b).unwrap();
                bodies.push(String::from_utf8(b).unwrap());
                if let Some(a) = answer {
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: {}
Connection: close

{a}",
                        a.len()
                    );
                }
            }
            bodies
        });
        let (_d, wt, ctx) = setup(&diff_of("src/main.rs", 3));
        let v = Api
            .ask(&config(&url, 200), &pr("alice"), &wt, &ctx, "", "GitHub")
            .unwrap();
        assert!(v.approve_recommended);
        let bodies = server.join().unwrap();
        let last: Value = serde_json::from_str(&bodies[2]).unwrap();
        assert_eq!(last["tool_choice"], "none");
        let m = last["messages"].as_array().unwrap().last().unwrap().clone();
        assert!(
            m["content"].as_str().unwrap().starts_with("Time is up"),
            "{m}"
        );
    }

    /// Live check against the endpoint in your own settings (it costs tokens): reviews one commit of this repository.
    /// `NOD_LIVE_API=1 cargo test live_review -- --ignored --nocapture`; `NOD_LIVE_COMMIT` (default ec6cdaf) picks
    /// the commit, `NOD_LIVE_KB` starts the review with a window of that many KiB (a small one makes the model fetch
    /// diffs with get_diff and take old results out).
    #[test]
    #[ignore]
    fn live_review_against_the_configured_endpoint() {
        if std::env::var("NOD_LIVE_API").as_deref() != Ok("1") {
            return;
        }
        let commit = std::env::var("NOD_LIVE_COMMIT").unwrap_or_else(|_| "ec6cdaf".into());
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let git = |args: &[&str], cwd: &Path| {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        let ctx = d.path().join("wt-ctx");
        git(
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                wt.to_str().unwrap(),
                &commit,
            ],
            &repo,
        );
        fs::create_dir_all(&ctx).unwrap();
        let base = format!("{commit}^");
        fs::write(
            ctx.join("diff.patch"),
            git(&["diff", "-U8", &base, &commit], &wt),
        )
        .unwrap();
        fs::write(
            ctx.join("diffstat.txt"),
            git(&["diff", "--stat=200", &base, &commit], &wt),
        )
        .unwrap();
        fs::write(
            ctx.join("commits.txt"),
            git(
                &["log", "--format=%h %an %s", &format!("{base}..{commit}")],
                &wt,
            ),
        )
        .unwrap();
        let cfg = crate::config::load().unwrap();
        if let Ok(kb) = std::env::var("NOD_LIVE_KB") {
            START_WINDOW.with(|w| w.set(kb.parse::<usize>().unwrap() * 1024));
        }
        let mut p = pr("live");
        p.repo = "larrygogo/nod".into();
        p.title = git(&["log", "-1", "--format=%s", &commit], &wt)
            .trim()
            .to_string();
        p.head = commit.clone();
        // NOD_LIVE_ENGINE picks another engine (antigravity, kimi, …) for the same check
        let engine = std::env::var("NOD_LIVE_ENGINE").unwrap_or_else(|_| "api".into());
        let res = crate::reviewer::engine_named(&cfg, &engine)
            .unwrap()
            .ask(&cfg, &p, &wt, &ctx, "", "GitHub");
        let _ = git(
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
            &repo,
        );
        let v = res.unwrap();
        eprintln!(
            "LIVE model={} requests={:?} {}s unseen={:?} minor={:?}
summary: {}
findings: {}
approve_recommended: {} unable: {:?}",
            v.meta.model,
            v.meta.turns,
            v.meta.duration_s,
            v.meta.unseen,
            v.meta.unseen_minor,
            v.summary,
            v.findings.len(),
            v.approve_recommended,
            v.unable_reason
        );
        for f in &v.findings {
            eprintln!("  [{}] {}:{} {}", f.severity, f.path, f.line, f.title);
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
    fn the_same_arguments_in_other_spacing_are_the_same_call() {
        assert!(same_args(r#"{"path": "a.rs"}"#, r#"{"path":"a.rs"}"#));
        assert!(!same_args(
            r#"{"path": "a.rs"}"#,
            r#"{"path":"a.rs","page":2}"#
        ));
        assert!(same_args("", " "));
    }

    #[test]
    fn local_endpoints() {
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:9000/v1",
            "http://gpu-box:11434/v1",
            "http://10.0.0.5:1234/v1",
        ] {
            assert!(is_local(url), "{url}");
        }
        for url in [
            "https://api.deepseek.com/v1",
            "https://user:pw@api.openai.com/v1",
            "https://localhost.example.com/v1",
        ] {
            assert!(!is_local(url), "{url}");
        }
    }
}
