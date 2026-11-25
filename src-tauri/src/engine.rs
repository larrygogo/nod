//! Review lifecycle and persistent state, independent of how notifications are delivered.
//!
//! Entry statuses (per PR, per head commit):
//! queued → reviewing → reviewed → (awaiting_ci →) posted
//!                     ↘ retry (transient failure) → queued     ↘ failed (gave up; user notified)
//! Any of the above → direct_approved when the user approves from a notification.
//! Other: stale (new commit during review), closed, aborted, dry-run.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::forge::{split_key as parse_key, Forge, Pr, PrStatus};
use crate::i18n::{t, tf, NON_QUALITY_REASONS};
use crate::notify::Notifier;
use crate::review::{self, Verdict};
use crate::reviewer::Reviewer;
use crate::util::{self, now, truncate};
use crate::{error, info, paths, warn};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub kind: String,
    pub text: String,
}

/// One review of one commit, as the window lists them: what was found and what went up to the code host.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Round {
    pub sha: String,
    /// When the verdict came (or the approval, for one given without a review).
    pub at: f64,
    pub engine: String,
    pub summary: String,
    pub findings: Vec<review::Finding>,
    /// The comments posted ahead of an approval that waited for CI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_url: Option<String>,
    /// What was posted as the outcome (APPROVE / COMMENT), and where; none = nothing went up for this round.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The approval was "Approve now" (you), not Nod's rules.
    pub direct: bool,
    /// The approval was withdrawn later.
    pub withdrawn: bool,
}

/// Rounds kept per PR (the oldest go first).
const ROUNDS_KEPT: usize = 20;

/// One PR's record. Unknown fields are kept so older/newer versions can share a state file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sha: String,
    pub title: String,
    pub url: String,
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// The commit `verdict` was produced for. A verdict is only ever posted for exactly this commit.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub verdict_sha: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
    pub attempts: u32,
    pub post_attempts: u32,
    /// Times the engine said it is out of quota (or rate limited) for this commit. These do not use up attempts.
    #[serde(skip_serializing_if = "is_zero")]
    pub waits: u32,
    /// The commit whose review comments are already on the code host, posted while the approval waits for CI.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment_sha: String,
    /// While waiting for CI: when Nod stops waiting (unix seconds).
    #[serde(skip_serializing_if = "is_zero_f")]
    pub ci_deadline: f64,
    /// A retry waits until this time (unix seconds); 0 = may run now.
    #[serde(skip_serializing_if = "is_zero_f")]
    pub retry_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub queued_at: f64,
    pub started_at: f64,
    pub reviewed_at: f64,
    pub posted_at: f64,
    pub direct_at: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub approve_ids: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_approve_id: Option<u64>,
    pub aborted: bool,
    pub aborted_review: bool,
    pub direct_busy: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub action_msg: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub draft_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pr_status: Option<PrStatus>,
    // notification bookkeeping
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub chat_id: String,
    pub card_sent_at: f64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub card_sig: String,
    pub card_final: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub webhook_sig: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run_file: Option<String>,
    /// Every review of this PR, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<Round>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_zero_f(n: &f64) -> bool {
    *n == 0.0
}

impl Entry {
    /// This commit was not approved, but an earlier approval of yours (Nod's, or "Approve now") still counts on the
    /// code host, so the PR can be merged on it. The window, the tray and the notification all call this "Approved".
    pub fn earlier_approval_counts(&self) -> bool {
        self.event.as_deref() != Some("APPROVE")
            && self.status != "direct_approved"
            && (!self.approve_ids.is_empty() || self.direct_approve_id.is_some())
            // what the code host says now: your own approval where it tells (GitHub; someone else's approval does not
            // make yours count), else the PR's decision, else (Gitea) Nod's own record
            && self.last_pr_status.as_ref().is_none_or(|s| match s.mine_approved {
                Some(m) => m,
                None => s.decision.is_empty() || s.decision == "APPROVED",
            })
    }

    /// The latest round for `sha`, if there is one.
    pub fn round_mut(&mut self, sha: &str) -> Option<&mut Round> {
        self.rounds.iter_mut().rev().find(|r| r.sha == sha)
    }

    /// Start a round (a new verdict, or an approval without one).
    pub fn push_round(&mut self, r: Round) {
        self.rounds.push(r);
        if self.rounds.len() > ROUNDS_KEPT {
            let extra = self.rounds.len() - ROUNDS_KEPT;
            self.rounds.drain(..extra);
        }
    }

    /// The outcome posted for `sha`: into its round, or a round of its own when there was no verdict.
    pub fn record_outcome(&mut self, sha: &str, event: &str, url: &str, direct: bool) {
        if self.round_mut(sha).is_none() {
            self.push_round(Round {
                sha: sha.into(),
                at: now(),
                ..Default::default()
            });
        }
        let r = self.round_mut(sha).expect("just made");
        r.event = Some(event.into());
        r.url = Some(url.into());
        r.direct = direct;
    }

    /// A retry that is still waiting (out of quota, or a pause after an error).
    pub fn waiting(&self) -> bool {
        self.status == "retry" && self.retry_at > now()
    }

    /// Start over on the same commit: the user asked for another review of a failed or waiting one.
    pub fn reset_for_retry(&mut self) {
        self.attempts = 0;
        self.post_attempts = 0;
        self.waits = 0;
        self.retry_at = 0.0;
        self.error = None;
        self.note = None;
        self.status = "retry".into();
    }

    /// "Review again" in the window: forget this commit's verdict and outcome and review it from scratch, whatever
    /// happened before. Approvals already given stay on record (they can still be withdrawn).
    pub fn reset_for_rereview(&mut self) {
        self.reset_for_retry();
        self.verdict = None;
        self.verdict_sha.clear();
        self.event = None;
        self.reasons.clear();
        self.comment_sha.clear();
        self.ci_deadline = 0.0;
        self.aborted = false;
        self.aborted_review = false;
        self.action_msg.clear();
        self.card_final = false;
    }

    /// The PR was merged or closed. Reasons computed for a review that was never posted (e.g. while waiting for
    /// CI) no longer explain anything, and would read as if Nod blocked an approval; replace or drop them.
    pub fn mark_closed(&mut self) {
        if self.event.is_none() {
            self.reasons = if self.status == "awaiting_ci" {
                vec!["closed_while_waiting".into()]
            } else if self.verdict.is_some() && self.verdict_sha == self.sha {
                // reviewed, but merged or closed before the verdict could go up
                vec!["closed_before_posting".into()]
            } else {
                vec![]
            };
        }
        self.status = "closed".into();
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct StateData {
    #[serde(default)]
    pub prs: HashMap<String, Entry>,
}

/// state.json behind a mutex. Long operations (GitHub, Claude) never hold the lock.
pub struct Store {
    pub path: PathBuf,
    data: Mutex<StateData>,
    pr_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Store {
    pub fn open(path: Option<PathBuf>) -> Result<Store> {
        let path = path.unwrap_or_else(|| paths::data_dir().join("state.json"));
        let data = if path.exists() {
            parse_state(&fs::read_to_string(&path)?)?
        } else {
            StateData::default()
        };
        Ok(Store {
            path,
            data: Mutex::new(data),
            pr_locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, StateData> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read or modify one entry (created if missing).
    pub fn with<R>(&self, key: &str, f: impl FnOnce(&mut Entry) -> R) -> R {
        let mut d = self.lock();
        f(d.prs.entry(key.to_string()).or_default())
    }

    pub fn get(&self, key: &str) -> Option<Entry> {
        self.lock().prs.get(key).cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        self.lock().prs.keys().cloned().collect()
    }

    /// Serialises posting a review and a direct approval for the same PR.
    pub fn pr_lock(&self, key: &str) -> Arc<Mutex<()>> {
        self.pr_locks
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_default()
            .clone()
    }

    pub fn save(&self) {
        // Hold the lock while writing so a slower save can never overwrite a newer snapshot.
        let guard = self.lock();
        let text = serde_json::to_string_pretty(&*guard).unwrap_or_default();
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &self.path);
        }
    }
}

/// Parse state.json, ignoring null fields (written by other versions) instead of failing on them.
pub fn parse_state(text: &str) -> Result<StateData> {
    let mut v: Value = serde_json::from_str(text)?;
    if let Some(prs) = v.get_mut("prs").and_then(Value::as_object_mut) {
        for e in prs.values_mut() {
            if let Some(o) = e.as_object_mut() {
                o.retain(|_, x| !x.is_null());
            }
        }
    }
    Ok(serde_json::from_value(v)?)
}

fn short(s: &str) -> &str {
    &s[..8.min(s.len())]
}

pub fn card_kind(event: &str, reasons: &[String]) -> &'static str {
    if event == "APPROVE" {
        "approve"
    } else if !reasons.is_empty()
        && reasons
            .iter()
            .all(|r| NON_QUALITY_REASONS.contains(&r.as_str()))
    {
        "ready"
    } else {
        "comment"
    }
}

/// Pause before trying again after an ordinary error, so a passing hiccup does not use up both attempts at once.
pub const RETRY_PAUSE_S: f64 = 60.0;

/// Wait after the `n`th "out of quota" without a reset time: 15 minutes, doubling, at most 6 hours.
pub fn quota_backoff(n: u32) -> f64 {
    (900.0 * 2f64.powi(n.saturating_sub(1).min(10) as i32)).min(6.0 * 3600.0)
}

/// An engine that is signed out is tried again at least this often, so fixing the sign-in is noticed soon.
pub const SIGNIN_RETRY_MAX_S: f64 = 1800.0;

/// How to get an engine that cannot run working again, in a few words.
fn signin_fix(lang: &str, engine: &str) -> String {
    let zh = lang != "en";
    match engine {
        "" | "claude" if zh => "在终端运行 claude，输入 /login 重新登录（想长期不过期可改用 claude setup-token）",
        "" | "claude" => "run claude in a terminal and type /login (claude setup-token gives a sign-in that lasts a year)",
        "codex" if zh => "在终端运行 codex login 重新登录",
        "codex" => "run codex login in a terminal",
        _ if zh => "到插件页检查审查引擎",
        _ => "check the review engine on the Plugins page",
    }
    .to_string()
}

const RECALL: [&str; 6] = [
    "posted",
    "stale",
    "direct_approved",
    "aborted",
    "retry",
    "withdrawn",
];
const DONE_FOR_SHA: [&str; 8] = [
    "withdrawn",
    "posted",
    "failed",
    "awaiting_ci",
    "dry-run",
    "direct_approved",
    "aborted",
    "queued",
];

pub struct Engine {
    pub cfg: Config,
    pub store: Arc<Store>,
    pub me: String,
    pub forge: Arc<dyn Forge>,
    pub reviewer: Arc<dyn Reviewer>,
    pub notifier: Arc<dyn Notifier>,
}

impl Engine {
    fn lang(&self) -> &str {
        &self.cfg.language
    }

    // ------------------------------------------------------------ notifications

    /// Push the entry's current state to the notification channel. Returns whether it was delivered.
    pub fn refresh(&self, key: &str, st: Option<PrStatus>, fetch: bool) -> bool {
        let mut st = st;
        if st.is_none() && fetch {
            match parse_key(key).and_then(|(r, n)| self.forge.pr_status(&r, n)) {
                Ok(s) => st = Some(s),
                Err(e) => warn!("{key}: could not read PR status: {e}"),
            }
        }
        let mut e = match self.store.get(key) {
            Some(e) => e,
            None => return false,
        };
        if st.is_some() {
            e.last_pr_status = st.clone();
        }
        let shown = st.or_else(|| e.last_pr_status.clone());
        let ok = match self.notifier.update(key, &mut e, shown.as_ref()) {
            Ok(ok) => ok,
            Err(err) => {
                warn!("{key}: notification failed: {err}");
                false
            }
        };
        // Write back only the fields the notifier and refresh own, so concurrent status changes are not lost.
        self.store.with(key, |cur| {
            cur.last_pr_status = e.last_pr_status.clone();
            cur.message_id = e.message_id.clone();
            cur.chat_id = e.chat_id.clone();
            cur.card_sent_at = e.card_sent_at;
            cur.card_sig = e.card_sig.clone();
            cur.webhook_sig = e.webhook_sig.clone();
            // channels without a dedicated field (desktop, robots, external plugins) keep their "already sent"
            // marks in `extra` under `*_sig`
            for (k, v) in e.extra.iter().filter(|(k, _)| k.ends_with("_sig")) {
                cur.extra.insert(k.clone(), v.clone());
            }
        });
        self.store.save();
        ok
    }

    pub fn note(&self, key: &str, kind: &str, text: &str, link: Option<&str>) {
        self.store.with(key, |e| {
            e.note = Some(Note {
                kind: kind.into(),
                text: text.into(),
            });
            if let Some(l) = link {
                e.review_url = Some(l.into());
            }
        });
        self.refresh(key, None, true);
    }

    /// A whole round failed (signed out of GitHub, network…). At most once every 6 hours.
    pub fn alert(&self, text: &str) {
        self.send_alert(&tf(
            self.lang(),
            "note.system_failed",
            &[("err", &truncate(text, 400))],
        ));
    }

    /// The code host has been unreachable for `min` minutes in a row: say so in words, with the last error.
    pub fn alert_offline(&self, min: u64, last: &str) {
        self.send_alert(&tf(
            self.lang(),
            "note.offline",
            &[("min", &min.to_string()), ("err", &truncate(last, 300))],
        ));
    }

    /// At most one alert every six hours, whatever it says.
    fn send_alert(&self, text: &str) {
        let p = paths::data_dir().join("alert.json");
        let last = fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v["at"].as_f64())
            .unwrap_or(0.0);
        if now() - last < 6.0 * 3600.0 {
            return;
        }
        if self.cfg.dry_run {
            info!("[dry-run] alert: {}", truncate(text, 200));
            return;
        }
        let _ = fs::write(&p, serde_json::json!({"at": now()}).to_string());
        if let Err(e) = self.notifier.alert(text) {
            warn!("alert failed: {e}");
        }
    }

    fn stale_approve_note(&self, e: &Entry) -> String {
        if e.approve_ids.is_empty() {
            String::new()
        } else {
            format!("\n{}", t(self.lang(), "note.stale_approve"))
        }
    }

    // ------------------------------------------------------------ choosing work

    /// Whether `repo` is in the review scope (`[scope] repos` / `exclude_repos`, patterns like `owner/*`).
    pub fn allowed(&self, repo: &str) -> bool {
        let s = &self.cfg.scope;
        crate::config::selected(&s.repos, &s.exclude_repos, |p| {
            crate::config::repo_matches(p, repo, &self.me)
        })
    }

    /// Whether a request from `author` starts a review (`[scope] authors` / `exclude_authors`).
    pub fn author_allowed(&self, author: &str) -> bool {
        let s = &self.cfg.scope;
        crate::config::selected(&s.authors, &s.exclude_authors, |p| {
            crate::config::author_matches(p, author, &self.me)
        })
    }

    /// PRs that need a review: you were asked to review them, or you reviewed them and new commits arrived.
    pub fn collect_todo(&self) -> Result<Vec<Pr>> {
        let mut cands: Vec<(String, (String, u64))> = vec![];
        for (r, n) in self.forge.requested_prs(self.cfg.scope.team_requests)? {
            cands.push((format!("{r}#{n}"), (r, n)));
        }
        let horizon = now() - self.cfg.review.rereview_days * 86400.0;
        {
            let d = self.store.lock();
            for (key, e) in &d.prs {
                // reviewed_at too: a review whose comments went up while it waited for CI and then went stale on a new
                // commit has no posted_at, and GitHub dropped the review request when the comments went up, so without
                // it nothing would bring the new commit back (kaptra-worker-template#5 sat 17 minutes).
                let recent =
                    e.posted_at > horizon || e.reviewed_at > horizon || !e.approve_ids.is_empty();
                if RECALL.contains(&e.status.as_str())
                    && recent
                    && !cands.iter().any(|(k, _)| k == key)
                {
                    if let Ok(rn) = parse_key(key) {
                        cands.push((key.clone(), rn));
                    }
                }
            }
        }
        let mut todo = vec![];
        for (key, (r, n)) in cands {
            if !self.allowed(&r) {
                continue;
            }
            let pr = match self.forge.fetch_pr(&r, n) {
                Ok(p) => p,
                Err(e) => {
                    warn!("{key}: could not read PR: {e}");
                    continue;
                }
            };
            let e = self.store.get(&key).unwrap_or_default();
            if pr.state != "open" {
                if self.store.get(&key).is_some() {
                    self.store.with(&key, |e| e.mark_closed());
                }
                continue;
            }
            if !self.author_allowed(&pr.author) {
                continue; // [scope] authors: not reviewed automatically (a PR handed over by hand still is)
            }
            if pr.draft {
                if e.draft_sha != pr.head {
                    info!("{key} is a draft, skipping");
                }
                self.store.with(&key, |e| {
                    e.draft_sha = pr.head.clone();
                    e.title = pr.title.clone();
                });
                continue;
            }
            let same = e.sha == pr.head;
            if same && e.status == "reviewing" {
                // A previous process died mid-review; retry once it has clearly timed out.
                if now() - e.started_at < (self.cfg.review.timeout_min + 10.0) * 60.0 {
                    continue;
                }
            } else if same && (DONE_FOR_SHA.contains(&e.status.as_str()) || e.waiting()) {
                continue;
            }
            if !same && ["posted", "direct_approved", "aborted"].contains(&e.status.as_str()) {
                info!(
                    "{key} has a new commit {}, reviewing again",
                    &pr.head[..8.min(pr.head.len())]
                );
            }
            todo.push(pr);
        }
        Ok(todo)
    }

    // ------------------------------------------------------------ reviewing

    pub fn review_one(&self, pr: &Pr) {
        let key = pr.key();
        let attempts = self.store.with(&key, |e| {
            if e.sha != pr.head {
                e.sha = pr.head.clone();
                e.attempts = 0;
                e.status = "new".into();
            }
            e.title = pr.title.clone();
            e.url = pr.url.clone();
            e.author = pr.author.clone();
            e.attempts += 1;
            e.status = "reviewing".into();
            e.started_at = now();
            e.attempts
        });
        self.store.save();
        info!(
            "reviewing {key} @{} (attempt {attempts})",
            &pr.head[..8.min(pr.head.len())]
        );
        let aborted = || self.store.get(&key).is_some_and(|e| e.aborted);
        let mut wt = None;
        let result: Result<Verdict> = (|| {
            let (w, ctx) = self.reviewer.checkout(pr)?;
            wt = Some(w.clone());
            if aborted() {
                anyhow::bail!("aborted");
            }
            let disc = self.forge.discussion(pr).unwrap_or_default();
            let v = self.reviewer.ask(&self.cfg, pr, &w, &ctx, &disc)?;
            if aborted() {
                anyhow::bail!("aborted");
            }
            Ok(v)
        })();
        if let Some(w) = &wt {
            self.reviewer.cleanup(pr, w);
        }
        match result {
            Ok(v) => {
                info!(
                    "{key}: {} finding(s), approve_recommended={}, {}s",
                    v.findings.len(),
                    v.approve_recommended,
                    v.meta.duration_s
                );
                let kept = self.store.with(&key, |e| {
                    if e.sha != pr.head {
                        return false; // a newer commit was queued while we reviewed this one
                    }
                    e.push_round(Round {
                        sha: pr.head.clone(),
                        at: now(),
                        engine: v.meta.engine.clone(),
                        summary: v.summary.clone(),
                        findings: v.findings.clone(),
                        ..Default::default()
                    });
                    e.verdict = Some(v);
                    e.verdict_sha = pr.head.clone();
                    e.waits = 0;
                    e.retry_at = 0.0;
                    e.error = None;
                    if e.note.as_ref().is_some_and(|n| n.kind == "wait") {
                        e.note = None;
                    }
                    e.status = "reviewed".into();
                    e.reviewed_at = now();
                    true
                });
                self.store.save();
                if !kept {
                    info!(
                        "{key}: superseded by a newer commit during review; verdict for {} dropped",
                        short(&pr.head)
                    );
                    return;
                }
            }
            Err(err) => {
                if self.store.get(&key).is_some_and(|e| e.sha != pr.head) {
                    info!(
                        "{key}: review of {} ended ({err:#}) after a newer commit was queued",
                        short(&pr.head)
                    );
                    return;
                }
                if aborted() {
                    self.store.with(&key, |e| {
                        if e.status != "direct_approved" {
                            e.status = "aborted".into();
                        }
                    });
                    info!("{key}: review stopped (approved directly)");
                    self.store.save();
                    return;
                }
                error!("{key}: review failed: {err:#}");
                let text = format!("{err:#}");
                let trouble = crate::reviewer::classify(&text, now());
                let (failed, first_wait, note) = self.store.with(&key, |e| {
                    e.error = Some(truncate(&text, 800));
                    if let crate::reviewer::Trouble::Quota { until } = trouble {
                        // The engine will work again by itself: wait for it, without using up an attempt.
                        e.attempts = e.attempts.saturating_sub(1);
                        e.waits += 1;
                        e.retry_at = until.unwrap_or_else(|| now() + quota_backoff(e.waits));
                        e.status = "retry".into();
                        (false, e.waits == 1, String::new())
                    } else if trouble == crate::reviewer::Trouble::Unavailable {
                        // Signed out or not installed: nothing wrong with this PR, and every PR fails the same way
                        // until the user fixes the engine. Wait like for a quota, checking often enough that a fixed
                        // sign-in is picked up within half an hour.
                        e.attempts = e.attempts.saturating_sub(1);
                        e.waits += 1;
                        e.retry_at = now() + quota_backoff(e.waits).min(SIGNIN_RETRY_MAX_S);
                        e.status = "retry".into();
                        (false, e.waits == 1, String::new())
                    } else if e.attempts >= self.cfg.review.max_attempts {
                        e.status = "failed".into();
                        (true, false, self.stale_approve_note(e))
                    } else {
                        e.retry_at = now() + RETRY_PAUSE_S;
                        e.status = "retry".into();
                        (false, false, String::new())
                    }
                });
                self.store.save();
                if first_wait {
                    let id = self.cfg.engine_for(&pr.repo, &self.me);
                    let engine = review::engine_name(&id);
                    let until = self.store.get(&key).map(|e| e.retry_at).unwrap_or_default();
                    let msg = if trouble == crate::reviewer::Trouble::Unavailable {
                        tf(
                            self.lang(),
                            "note.signin_wait",
                            &[("engine", &engine), ("fix", &signin_fix(self.lang(), &id))],
                        )
                    } else {
                        tf(
                            self.lang(),
                            "note.quota_wait",
                            &[("engine", &engine), ("at", &util::clock(until))],
                        )
                    };
                    self.note(&key, "wait", &msg, None);
                }
                if failed && review::always_approve(&self.cfg, pr, &self.me) {
                    // on the always-approve list: approve even though the review did not finish
                    match self.approve_without_review(pr, &text) {
                        Ok(()) => return,
                        Err(e) => {
                            warn!("{key}: approving from the always-approve list failed: {e:#}")
                        }
                    }
                }
                if failed {
                    let msg = tf(
                        self.lang(),
                        "note.review_failed",
                        &[("n", &attempts.to_string()), ("err", &truncate(&text, 400))],
                    );
                    self.note(&key, "manual", &(msg + &note), None);
                }
                return;
            }
        }
        if self
            .store
            .get(&key)
            .is_some_and(|e| e.status == "reviewed" && !e.aborted)
        {
            self.finish_safe(&key);
        }
    }

    /// Post the verdict. Failures keep the verdict for the next round; after max_attempts the user is told.
    pub fn finish_safe(&self, key: &str) {
        let lock = self.store.pr_lock(key);
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        let e = match self.store.get(key) {
            Some(e) => e,
            None => return,
        };
        if e.aborted || !["reviewed", "awaiting_ci"].contains(&e.status.as_str()) {
            return; // approved directly, already posted by another thread, superseded, ...
        }
        match self.finish(key) {
            Ok(()) => self.store.with(key, |e| e.error = None),
            Err(err) => {
                let (n, give_up, note) = self.store.with(key, |e| {
                    e.post_attempts += 1;
                    e.error = Some(truncate(&format!("{err:#}"), 800));
                    let give_up = e.post_attempts >= self.cfg.review.max_attempts;
                    if give_up {
                        e.status = "failed".into();
                    }
                    (e.post_attempts, give_up, self.stale_approve_note(e))
                });
                warn!("{key}: posting failed (attempt {n}): {err:#}");
                if give_up {
                    let msg = tf(
                        self.lang(),
                        "note.post_failed",
                        &[
                            ("n", &n.to_string()),
                            ("err", &truncate(&format!("{err:#}"), 400)),
                        ],
                    );
                    self.note(key, "manual", &(msg + &note), None);
                }
            }
        }
        self.store.save();
    }

    fn finish(&self, key: &str) -> Result<()> {
        let (r, n) = parse_key(key)?;
        let pr = self.forge.fetch_pr(&r, n)?;
        let e = self.store.get(key).unwrap_or_default();
        if pr.state != "open" {
            self.store.with(key, |e| e.mark_closed());
            info!("{key} is {}, not posting", pr.state);
            return Ok(());
        }
        if !self.allowed(&pr.repo) {
            self.store.with(key, |e| e.status = "out_of_scope".into());
            info!("{key} is outside [scope] now; not posting");
            return Ok(());
        }
        let verdict_sha = if e.verdict_sha.is_empty() {
            &e.sha
        } else {
            &e.verdict_sha
        };
        if pr.head != *verdict_sha || e.sha != *verdict_sha {
            self.store.with(key, |e| {
                if e.sha == *verdict_sha {
                    e.status = "stale".into();
                }
            });
            info!(
                "{key} got a new commit since the review, not posting the verdict for {}",
                short(verdict_sha)
            );
            return Ok(());
        }
        let Some(verdict) = e.verdict.clone() else {
            return Ok(());
        };
        let mut ci = self.forge.ci_state(&pr)?;
        let waited = (now()
            - if e.reviewed_at > 0.0 {
                e.reviewed_at
            } else {
                now()
            })
            / 60.0;
        // No checks yet may only mean they have not registered since the push: wait a little, counted from when Nod
        // first saw the commit, unless this repository's earlier PRs never had CI either.
        let seen = if e.queued_at > 0.0 {
            e.queued_at
        } else {
            now()
        };
        let grace_end = seen + self.cfg.review.ci_none_grace_min * 60.0;
        if ci == "none" && now() < grace_end && !self.repo_without_ci(&pr.repo, key) {
            ci = "none_wait".into();
        }
        let (event, reasons) = review::decide(&self.cfg, &pr, &verdict, &ci, &self.me);
        self.store.with(key, |e| e.reasons = reasons.clone());
        let only_ci = reasons == ["ci_pending"] || reasons == ["ci_none_wait"];
        let commented = e.comment_sha == pr.head;
        let footer = review::footer_text(self.lang(), &self.cfg.review, &verdict.meta.engine);
        if (ci == "pending" || ci == "none_wait")
            && only_ci
            && waited < self.cfg.review.ci_wait_max_min
        {
            // Nothing blocks but CI. Findings go up now, so the author can act on them; the approval follows as a
            // review of its own once CI passes. With nothing to say there is no early comment: one approval after CI.
            let deadline = if ci == "none_wait" {
                grace_end
            } else {
                e.reviewed_at + self.cfg.review.ci_wait_max_min * 60.0
            };
            // Only new issues are worth a review of their own now: ones raised before go with the approval.
            let anything_new = verdict
                .findings
                .iter()
                .any(|f| review::raised_before(&f.title).is_none());
            if !commented && !self.cfg.dry_run && anything_new {
                let body = review::build_review(
                    self.lang(),
                    &pr,
                    &verdict,
                    "COMMENT",
                    &reasons,
                    &self.forge.diff_lines(&pr)?,
                    footer.as_deref(),
                );
                if self.holds_secrets(key, &body) {
                    return Ok(());
                }
                let (url, _) = self.forge.post_review(&pr, &body)?;
                info!("{key}: posted the review comments {url}; approving after CI");
                self.store.with(key, |e| {
                    e.comment_sha = pr.head.clone();
                    e.review_url = Some(url.clone());
                    if let Some(r) = e.round_mut(&pr.head) {
                        r.comment_url = Some(url.clone());
                    }
                });
            }
            self.store.with(key, |e| {
                e.status = "awaiting_ci".into();
                e.ci_deadline = deadline;
            });
            info!("{key}: clean, waiting for CI ({waited:.0} min so far)");
            if e.status != "awaiting_ci" {
                self.refresh(key, None, true);
            }
            return Ok(());
        }
        let body = if commented {
            // the comments are already there: only the outcome
            let text = if event == "APPROVE" {
                t(self.lang(), "review.approve_after_ci")
            } else {
                let rs: Vec<String> = reasons
                    .iter()
                    .map(|r| crate::i18n::reason_text(self.lang(), r))
                    .collect();
                tf(
                    self.lang(),
                    "review.no_approve_after_ci",
                    &[("reasons", &rs.join("; "))],
                )
            };
            serde_json::json!({"commit_id": pr.head, "event": event, "body": text})
        } else {
            let mut b = review::build_review(
                self.lang(),
                &pr,
                &verdict,
                event,
                &reasons,
                &self.forge.diff_lines(&pr)?,
                footer.as_deref(),
            );
            if event == "APPROVE" {
                // say why it was approved, right under the verdict
                let why = if review::always_approve(&self.cfg, &pr, &self.me) {
                    t(self.lang(), "review.always")
                } else {
                    t(self.lang(), "review.rules_passed")
                };
                let text = b["body"].as_str().unwrap_or_default().to_string();
                let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
                b["body"] = Value::String(format!("{first}\n{why}\n{rest}").trim_end().to_string());
            }
            b
        };
        if self.holds_secrets(key, &body) {
            return Ok(());
        }
        if self.cfg.dry_run {
            let out = paths::data_dir().join("dry-run").join(format!(
                "{}-{}.json",
                key.replace('/', "__").replace('#', "-"),
                &pr.head[..8.min(pr.head.len())]
            ));
            fs::create_dir_all(out.parent().unwrap())?;
            fs::write(&out, serde_json::to_string_pretty(&body)?)?;
            self.store.with(key, |e| {
                e.status = "dry-run".into();
                e.event = Some(event.into());
                e.dry_run_file = Some(out.to_string_lossy().into());
            });
            info!("[dry-run] {key} would post {event}: {}", out.display());
            return Ok(());
        }
        let (url, id) = self.forge.post_review(&pr, &body)?;
        self.store.with(key, |e| {
            e.status = "posted".into();
            e.event = Some(event.into());
            e.review_url = Some(url.clone());
            e.posted_at = now();
            e.post_attempts = 0;
            e.ci_deadline = 0.0;
            if event == "APPROVE" {
                e.approve_ids.push(id);
            }
            e.record_outcome(&pr.head, event, &url, false);
        });
        info!("{key}: posted {event} {url}");
        let mut extra = String::new();
        if event != "APPROVE" {
            extra = match self.withdraw_old_approvals(&pr) {
                Ok(s) => s,
                Err(err) => {
                    warn!("{key}: checking earlier approvals failed: {err:#}");
                    format!("\n{}", t(self.lang(), "note.withdraw_error"))
                }
            };
        }
        self.note(
            key,
            card_kind(event, &reasons),
            &(verdict.summary + &extra),
            Some(&url),
        );
        Ok(())
    }

    /// A PR on the always-approve list whose review failed: approve it with a note that the review did not finish.
    fn approve_without_review(&self, pr: &Pr, err: &str) -> Result<()> {
        let key = pr.key();
        let body = serde_json::json!({
            "commit_id": pr.head, "event": "APPROVE",
            "body": tf(self.lang(), "review.always_failed", &[("err", &truncate(err, 300))]),
        });
        if self.holds_secrets(&key, &body) {
            anyhow::bail!("the error text looks like it holds credentials");
        }
        let (url, id) = self.forge.post_review(pr, &body)?;
        self.store.with(&key, |e| {
            e.status = "posted".into();
            e.event = Some("APPROVE".into());
            e.reasons.clear();
            e.review_url = Some(url.clone());
            e.posted_at = now();
            e.approve_ids.push(id);
            e.record_outcome(&pr.head, "APPROVE", &url, false);
        });
        self.store.save();
        info!("{key}: approved from the always-approve list without a review {url}");
        self.note(
            &key,
            "approve",
            &t(self.lang(), "review.always"),
            Some(&url),
        );
        Ok(())
    }

    /// The review text looks like it holds credentials: never post it; tell the user instead.
    fn holds_secrets(&self, key: &str, body: &Value) -> bool {
        let leaked = review::find_secrets(body);
        if leaked.is_empty() {
            return false;
        }
        let note = self.store.with(key, |e| {
            e.status = "failed".into();
            self.stale_approve_note(e)
        });
        error!(
            "{key}: review text looks like it contains credentials ({}); not posting",
            leaked.join(", ")
        );
        let msg = tf(
            self.lang(),
            "note.secret_blocked",
            &[("kinds", &leaked.join(", "))],
        );
        self.note(key, "manual", &(msg + &note), None);
        true
    }

    /// Another PR of `repo` finished with no CI at all: the repository has none, so waiting for checks to appear
    /// only delays the approval.
    fn repo_without_ci(&self, repo: &str, key: &str) -> bool {
        let prefix = format!("{repo}#");
        self.store.lock().prs.iter().any(|(k, e)| {
            k != key
                && k.starts_with(&prefix)
                && ["posted", "direct_approved", "closed"].contains(&e.status.as_str())
                && e.last_pr_status.as_ref().is_some_and(|s| s.ci == "none")
        })
    }

    /// This round did not approve, but Nod approved an earlier commit: dismiss those approvals so the new
    /// commits cannot be merged on the strength of them. Approvals you gave by hand are never touched.
    fn withdraw_old_approvals(&self, pr: &Pr) -> Result<String> {
        let key = pr.key();
        let ids = self
            .store
            .get(&key)
            .map(|e| e.approve_ids)
            .unwrap_or_default();
        if ids.is_empty() {
            return Ok(String::new());
        }
        let old: Vec<u64> = self
            .forge
            .list_reviews(pr)?
            .into_iter()
            .filter(|(id, st)| ids.contains(id) && st == "APPROVED")
            .map(|x| x.0)
            .collect();
        if old.is_empty() {
            return Ok(String::new());
        }
        let mut failed = false;
        for id in &old {
            match self
                .forge
                .dismiss_review(pr, *id, &t(self.lang(), "review.dismiss_msg"))
            {
                Ok(()) => info!("{key}: dismissed earlier approval {id}"),
                Err(e) => {
                    failed = true;
                    warn!("{key}: dismissing approval {id} failed: {e:#}");
                }
            }
        }
        if failed {
            return Ok(format!("\n{}", t(self.lang(), "note.withdraw_failed")));
        }
        self.store
            .with(&key, |e| e.approve_ids.retain(|i| !old.contains(i)));
        Ok(format!(
            "\n{}",
            tf(
                self.lang(),
                "note.withdrawn",
                &[("n", &old.len().to_string())]
            )
        ))
    }

    // ------------------------------------------------------------ direct approval from a notification

    /// "Withdraw approval" in the window: dismiss every approval Nod gave on this PR (its own and "Approve now"),
    /// so the PR can no longer be merged on them. Approvals given elsewhere (on GitHub itself) are not Nod's to undo.
    pub fn withdraw_approvals(&self, key: &str) {
        let lock = self.store.pr_lock(key);
        let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
        let lang = self.lang().to_string();
        let res: Result<usize> = (|| {
            let e = self.store.get(key).unwrap_or_default();
            let mine: Vec<u64> = e
                .approve_ids
                .iter()
                .copied()
                .chain(e.direct_approve_id)
                .collect();
            let (r, n) = parse_key(key)?;
            let pr = self.forge.fetch_pr(&r, n)?;
            let live: Vec<u64> = self
                .forge
                .list_reviews(&pr)?
                .into_iter()
                .filter(|(id, st)| mine.contains(id) && st == "APPROVED")
                .map(|x| x.0)
                .collect();
            for id in &live {
                self.forge
                    .dismiss_review(&pr, *id, &t(&lang, "review.withdrawn_msg"))?;
                info!("{key}: withdrew approval {id} on request");
            }
            Ok(live.len())
        })();
        let msg = match res {
            Ok(_) => {
                self.store.with(key, |e| {
                    e.approve_ids.clear();
                    e.direct_approve_id = None;
                    for r in e
                        .rounds
                        .iter_mut()
                        .filter(|r| r.event.as_deref() == Some("APPROVE"))
                    {
                        r.withdrawn = true;
                    }
                    if e.event.as_deref() == Some("APPROVE") || e.status == "direct_approved" {
                        e.event = None;
                    }
                    if e.status != "closed" {
                        e.status = "withdrawn".into();
                    }
                    if let Some(st) = e.last_pr_status.as_mut() {
                        st.decision = String::new(); // until the next refresh says what the code host thinks now
                    }
                });
                String::new()
            }
            Err(err) => {
                error!("{key}: withdrawing approval failed: {err:#}");
                tf(
                    &lang,
                    "withdraw.failed",
                    &[("err", &truncate(&format!("{err:#}"), 200))],
                )
            }
        };
        self.store.with(key, |e| {
            e.action_msg = msg;
            e.direct_busy = false;
        });
        self.store.save();
        drop(_g);
        self.refresh(key, None, true);
    }

    /// Approve exactly `sha` as the user. Only after GitHub accepted it is the running review stopped.
    pub fn direct_approve(&self, key: &str, sha: &str) {
        {
            let lock = self.store.pr_lock(key);
            let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
            let lang = self.lang().to_string();
            let res: Result<Option<String>> = (|| {
                let (r, n) = parse_key(key)?;
                let pr = self.forge.fetch_pr(&r, n)?;
                if pr.state != "open" {
                    return Ok(Some(t(&lang, "direct.closed")));
                }
                if pr.author.eq_ignore_ascii_case(&self.me) {
                    return Ok(Some(t(&lang, "direct.own_pr")));
                }
                if pr.head != sha {
                    return Ok(Some(tf(
                        &lang,
                        "direct.moved",
                        &[("sha", &pr.head[..7.min(pr.head.len())])],
                    )));
                }
                let was_running = self
                    .store
                    .get(key)
                    .is_some_and(|e| ["queued", "reviewing", "retry"].contains(&e.status.as_str()));
                // A verdict for this commit whose comments are not on GitHub yet goes up with the approval, so
                // approving early does not throw the findings away.
                let e0 = self.store.get(key).unwrap_or_default();
                let full = e0
                    .verdict
                    .as_ref()
                    .filter(|_| e0.verdict_sha == sha && e0.comment_sha != sha)
                    .and_then(|v| self.forge.diff_lines(&pr).ok().map(|d| (v, d)))
                    .map(|(v, d)| {
                        let footer = review::footer_text(&lang, &self.cfg.review, &v.meta.engine);
                        let mut b = review::build_review(
                            &lang,
                            &pr,
                            v,
                            "APPROVE",
                            &[],
                            &d,
                            footer.as_deref(),
                        );
                        b["body"] = Value::String(format!(
                            "{}\n\n{}",
                            t(&lang, "review.direct_note"),
                            b["body"].as_str().unwrap_or_default()
                        ));
                        b
                    })
                    .filter(|b| review::find_secrets(b).is_empty());
                let body = full.unwrap_or_else(
                    || serde_json::json!({"commit_id": sha, "event": "APPROVE", "body": t(&lang, "review.direct")}),
                );
                let (url, id) = self.forge.post_review(&pr, &body)?;
                self.store.with(key, |e| {
                    e.aborted = true;
                    e.status = "direct_approved".into();
                    e.sha = sha.into();
                    e.review_url = Some(url.clone());
                    e.direct_at = now();
                    e.posted_at = now();
                    e.event = Some("APPROVE".into());
                    e.direct_approve_id = Some(id);
                    e.action_msg.clear();
                    e.record_outcome(sha, "APPROVE", &url, true);
                });
                let killed = util::abort(key);
                self.store
                    .with(key, |e| e.aborted_review = killed || was_running);
                info!("{key}: approved directly {url}");
                Ok(None)
            })();
            let msg = match res {
                Ok(m) => m,
                Err(err) => {
                    error!("{key}: direct approval failed: {err:#}");
                    Some(tf(
                        &lang,
                        "direct.failed",
                        &[("err", &truncate(&format!("{err:#}"), 200))],
                    ))
                }
            };
            self.store.with(key, |e| {
                if let Some(m) = msg {
                    e.action_msg = m;
                }
                e.direct_busy = false;
            });
            self.store.save();
        }
        self.refresh(key, None, true);
    }
}
