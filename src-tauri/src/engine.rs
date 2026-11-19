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
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Entry {
    /// The PR was merged or closed. Reasons computed for a review that was never posted (e.g. while waiting for
    /// CI) no longer explain anything, and would read as if Nod blocked an approval; replace or drop them.
    pub fn mark_closed(&mut self) {
        if self.event.is_none() {
            self.reasons = if self.status == "awaiting_ci" {
                vec!["closed_while_waiting".into()]
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

const RECALL: [&str; 5] = ["posted", "stale", "direct_approved", "aborted", "retry"];
const DONE_FOR_SHA: [&str; 7] = [
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

    /// A whole round failed (gh logged out, network…). At most once every 6 hours.
    pub fn alert(&self, text: &str) {
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
        if let Err(e) = self.notifier.alert(&tf(
            self.lang(),
            "note.system_failed",
            &[("err", &truncate(text, 400))],
        )) {
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
                let recent = e.posted_at > horizon || !e.approve_ids.is_empty();
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
            } else if same && DONE_FOR_SHA.contains(&e.status.as_str()) {
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
                    e.verdict = Some(v);
                    e.verdict_sha = pr.head.clone();
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
                let (failed, note) = self.store.with(&key, |e| {
                    e.error = Some(truncate(&format!("{err:#}"), 800));
                    if e.attempts >= self.cfg.review.max_attempts {
                        e.status = "failed".into();
                        (true, self.stale_approve_note(e))
                    } else {
                        e.status = "retry".into();
                        (false, String::new())
                    }
                });
                self.store.save();
                if failed {
                    let msg = tf(
                        self.lang(),
                        "note.review_failed",
                        &[
                            ("n", &attempts.to_string()),
                            ("err", &truncate(&format!("{err:#}"), 400)),
                        ],
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
        if ci == "none" && waited < self.cfg.review.ci_none_grace_min {
            ci = "none_wait".into(); // checks may not exist yet right after a push
        }
        let (event, reasons) = review::decide(&self.cfg, &pr, &verdict, &ci, &self.me);
        self.store.with(key, |e| e.reasons = reasons.clone());
        let only_ci = reasons == ["ci_pending"] || reasons == ["ci_none_wait"];
        if (ci == "pending" || ci == "none_wait")
            && only_ci
            && waited < self.cfg.review.ci_wait_max_min
        {
            self.store.with(key, |e| e.status = "awaiting_ci".into());
            info!("{key}: clean, waiting for CI ({waited:.0} min so far)");
            return Ok(());
        }
        let body = review::build_review(
            self.lang(),
            &pr,
            &verdict,
            event,
            &reasons,
            &self.forge.diff_lines(&pr)?,
        );
        let leaked = review::find_secrets(&body);
        if !leaked.is_empty() {
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
            if event == "APPROVE" {
                e.approve_ids.push(id);
            }
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
                let body = serde_json::json!({"commit_id": sha, "event": "APPROVE", "body": t(&lang, "review.direct")});
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
