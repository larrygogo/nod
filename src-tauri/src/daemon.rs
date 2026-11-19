//! The long-running process: for every code-host account, poll for review requests, notify first, review in a worker
//! thread and refresh notifications; one listener handles "Approve now" clicks for all of them.
//!
//! Threads per account: poll loop, review worker, refresher. Shared: the long-connection listener (Feishu only).

use std::fs::{self, File, OpenOptions};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::config::{Config, ForgeAccount};
use crate::engine::{Engine, Store};
use crate::forge::{split_key as parse_key, Forge, Pr};
use crate::i18n::t;
use crate::notify::{self, ClickIds};
use crate::reviewer::{platform, Router, Workspace};
use crate::util::now;
use crate::{error, info, paths, warn};

// ---------------------------------------------------------------- single instance

fn lock_path() -> std::path::PathBuf {
    paths::data_dir().join("daemon.lock")
}

/// Exclusive lock on daemon.lock; keep the returned file open. The OS releases it when the process exits, so a
/// crashed daemon never blocks a new one.
pub fn try_lock() -> Option<File> {
    let _ = fs::create_dir_all(paths::data_dir());
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path())
        .ok()?;
    match f.try_lock() {
        Ok(()) => Some(f),
        Err(_) => None,
    }
}

/// PID of the running daemon, 0 if none, -1 if one runs but its PID file is unreadable.
pub fn running_pid() -> i64 {
    if let Some(f) = try_lock() {
        let _ = f.unlock();
        return 0;
    }
    fs::read_to_string(paths::data_dir().join("daemon.pid"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(-1)
}

// ---------------------------------------------------------------- daemon

pub struct Daemon {
    /// Code-host account id.
    pub account: String,
    pub engine: Arc<Engine>,
    /// PRs added by hand ("Track" in the window), one key per line.
    track: std::path::PathBuf,
    tx: Sender<Pr>,
    rx: Mutex<Option<Receiver<Pr>>>,
}

impl Daemon {
    pub fn for_account(account: &str, engine: Engine) -> Arc<Daemon> {
        Self::with_track(account, engine, track_file(account))
    }

    pub fn with_track(account: &str, engine: Engine, track: std::path::PathBuf) -> Arc<Daemon> {
        let (tx, rx) = channel();
        Arc::new(Daemon {
            account: account.to_string(),
            engine: Arc::new(engine),
            track,
            tx,
            rx: Mutex::new(Some(rx)),
        })
    }

    fn store(&self) -> &Store {
        &self.engine.store
    }

    pub fn enqueue(&self, pr: Pr) {
        let key = pr.key();
        self.store().with(&key, |e| {
            if e.sha != pr.head {
                e.verdict = None;
                e.verdict_sha.clear();
                e.note = None;
                e.aborted = false;
                e.aborted_review = false;
                e.action_msg.clear();
                e.attempts = 0;
                e.post_attempts = 0;
                e.waits = 0;
                e.error = None;
                e.reviewed_at = 0.0;
                e.event = None;
                e.reasons.clear();
                e.direct_busy = false;
            }
            e.sha = pr.head.clone();
            e.status = "queued".into();
            e.retry_at = 0.0;
            e.queued_at = now();
            e.title = pr.title.clone();
            e.url = pr.url.clone();
            e.author = pr.author.clone();
        });
        self.store().save();
        self.engine.refresh(&key, None, true); // notify first, then review
        let _ = self.tx.send(pr);
    }

    pub fn work_one(&self, pr: &Pr) {
        let key = pr.key();
        let go = self.store().with(&key, |e| {
            if e.status != "queued" || e.sha != pr.head {
                return false; // approved directly or superseded while waiting
            }
            e.status = "reviewing".into();
            e.started_at = now();
            true
        });
        if !go {
            return;
        }
        self.engine.refresh(&key, None, false);
        self.engine.review_one(pr);
        // A retry that has to wait (out of quota, or a pause after an error) is left as "retry"; the poll picks it
        // up once its time comes.
        let retry = self.store().with(&key, |e| {
            if e.status == "retry" && !e.waiting() {
                e.status = "queued".into();
                true
            } else {
                false
            }
        });
        if retry {
            let _ = self.tx.send(pr.clone());
        }
        self.engine.refresh(&key, None, true);
    }

    pub fn start_worker(self: &Arc<Self>) {
        let rx = self
            .rx
            .lock()
            .unwrap()
            .take()
            .expect("worker started twice");
        let me = self.clone();
        thread::Builder::new()
            .name("worker".into())
            .spawn(move || {
                for pr in rx {
                    me.work_one(&pr);
                }
            })
            .unwrap();
    }

    /// "Approve" in the window: `repo#n sha` lines the window drops in `approve_file`. Each is approved like the
    /// Feishu card's button, for exactly that commit (a newer push, your own PR or a closed one is refused, and the
    /// reason shows on the record).
    pub fn take_approvals(&self) {
        let path = approve_file(&self.track);
        let taken = path.with_extension("taking");
        if !(taken.exists() || fs::rename(&path, &taken).is_ok()) {
            return;
        }
        let text = fs::read_to_string(&taken).unwrap_or_default();
        let _ = fs::remove_file(&taken);
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(key), Some(sha)) = (it.next(), it.next()) else {
                continue;
            };
            if self.store().get(key).is_none() {
                warn!("{key}: approve from the window for an unknown PR; ignored");
                continue;
            }
            info!(
                "{key}: approve from the window @{}",
                &sha[..8.min(sha.len())]
            );
            self.store().with(key, |e| {
                e.direct_busy = true;
                e.action_msg = t(&self.engine.cfg.language, "direct.working");
            });
            self.engine.direct_approve(key, sha);
        }
    }

    pub fn poll_once(&self) -> Result<()> {
        self.take_approvals();
        for key in self.store().keys() {
            let Some(e) = self.store().get(&key) else {
                continue;
            };
            // "reviewed" is finished by the worker itself; only pick up ones stuck for 5+ minutes.
            let stuck = e.status == "reviewed" && now() - e.reviewed_at > 300.0;
            if (e.status == "awaiting_ci" || stuck) && e.verdict.is_some() && !e.aborted {
                self.engine.finish_safe(&key);
            }
        }
        let mut todo = self.engine.collect_todo()?;
        let track = self.track.clone();
        // Move the list aside before reading it: "Track" in the window appends to track.txt at any time, and writing
        // the list back would drop a PR added during the seconds the lookups take. The kept lines are appended to
        // whatever track.txt holds by then.
        let taken = track.with_extension("polling");
        let text = if taken.exists() || fs::rename(&track, &taken).is_ok() {
            fs::read_to_string(&taken).ok()
        } else {
            None
        };
        if let Some(text) = text {
            let mut keep = vec![];
            for line in text.split_whitespace() {
                // "o/r#1!" = the user asked for a review just now (the "Review" dialog): a failed or waiting review of
                // the same commit starts over. The line is kept without the mark, so it happens once.
                let (line, again) = match line.strip_suffix('!') {
                    Some(l) => (l, true),
                    None => (line, false),
                };
                let Ok((r, n)) = parse_key(line) else {
                    continue;
                };
                if !self.engine.allowed(&r) {
                    warn!("{line}: tracked but outside [scope]; ignoring");
                    continue;
                }
                let pr = match self.engine.forge.fetch_pr(&r, n) {
                    Ok(p) => p,
                    Err(err) => {
                        warn!("{line}: could not read tracked PR: {err:#}");
                        keep.push(line.to_string());
                        continue;
                    }
                };
                if pr.state != "open" {
                    continue; // merged or closed: stop tracking
                }
                keep.push(line.to_string());
                let mut e = self.store().get(&pr.key()).unwrap_or_default();
                if again && e.sha == pr.head && ["failed", "retry"].contains(&e.status.as_str()) {
                    info!("{line}: reviewing again on request");
                    self.store().with(&pr.key(), |e| e.reset_for_retry());
                    e.reset_for_retry();
                }
                if !pr.draft && !(e.sha == pr.head && (e.status != "retry" || e.waiting())) {
                    todo.push(pr);
                }
            }
            // The move succeeds even while the window has track.txt open, so a line it was writing at that very
            // moment lands in the moved file: read it once more and keep what arrived meanwhile.
            if let Ok(again) = fs::read_to_string(&taken) {
                let seen: std::collections::HashSet<&str> = text.split_whitespace().collect();
                keep.extend(
                    again
                        .split_whitespace()
                        .filter(|l| !seen.contains(l))
                        .map(str::to_string),
                );
            }
            if keep.is_empty() || append_lines(&track, &keep).is_ok() {
                let _ = fs::remove_file(&taken);
            }
        }
        let mut seen = std::collections::HashSet::new();
        for pr in todo {
            if seen.insert(pr.key()) {
                info!(
                    "{}: new review task @{}",
                    pr.key(),
                    &pr.head[..8.min(pr.head.len())]
                );
                self.enqueue(pr);
            }
        }
        Ok(())
    }

    pub fn refresh_all(&self) {
        let horizon = now() - self.engine.cfg.review.rereview_days * 86400.0;
        for key in self.store().keys() {
            let Some(e) = self.store().get(&key) else {
                continue;
            };
            if e.card_final || e.card_sent_at.max(e.queued_at) < horizon {
                continue;
            }
            let st = match parse_key(&key).and_then(|(r, n)| self.engine.forge.pr_status(&r, n)) {
                Ok(s) => s,
                Err(err) => {
                    warn!("{key}: refresh failed: {err:#}");
                    continue;
                }
            };
            let fin = st.state == "merged" || st.state == "closed";
            if self.engine.refresh(&key, Some(st), false) && fin {
                self.store().with(&key, |e| e.card_final = true); // one last update after merge/close, then stop
                self.store().save();
            }
        }
    }

    pub fn requeue_unfinished(&self) {
        // A direct approval cannot survive a restart; never leave its "busy" flag (which hides the button) behind.
        for key in self.store().keys() {
            self.store().with(&key, |e| e.direct_busy = false);
        }
        for key in self.store().keys() {
            let Some(e) = self.store().get(&key) else {
                continue;
            };
            if !["queued", "reviewing", "retry"].contains(&e.status.as_str()) || e.waiting() {
                continue; // a waiting retry keeps its time; the poll picks it up then
            }
            if e.status == "reviewing" {
                // The review was cut off by the restart (quitting Nod, saving settings), not by the engine: it does
                // not use up an attempt, or a later real error would give up without a retry.
                self.store()
                    .with(&key, |e| e.attempts = e.attempts.saturating_sub(1));
            }
            if parse_key(&key).is_ok_and(|(r, _)| !self.engine.allowed(&r)) {
                self.store()
                    .with(&key, |e| e.status = "out_of_scope".into());
                continue;
            }
            match parse_key(&key).and_then(|(r, n)| self.engine.forge.fetch_pr(&r, n)) {
                Ok(pr) if pr.state == "open" && !pr.draft => {
                    info!("{key}: re-queued after restart");
                    self.enqueue(pr);
                }
                Ok(pr) if pr.state != "open" => self.store().with(&key, |e| e.mark_closed()),
                // A draft, or the code host could not be reached (often the case right after sign-in): leave it to a
                // later poll. "queued" would count as handled for this commit, and the PR would wait for a new push.
                // "retry" is picked up again by every path: still requested, reviewed before (recalled), tracked.
                Ok(_) => self.store().with(&key, |e| e.status = "retry".into()),
                Err(err) => {
                    warn!("{key}: could not read PR on restart: {err:#}");
                    self.store().with(&key, |e| e.status = "retry".into());
                }
            }
        }
    }

    // ------------------------------------------------------------ "Approve now"

    pub fn on_action(self: &Arc<Self>, value: Value, ids: ClickIds) -> Result<Option<Value>> {
        let lang = self.engine.cfg.language.clone();
        let key = value["key"].as_str().unwrap_or_default().to_string();
        info!("button {} on {key}", value["action"]);
        if value["action"] != "approve" {
            return Ok(None);
        }
        let e = self
            .store()
            .get(&key)
            .ok_or_else(|| anyhow!(t(&lang, "direct.unknown_pr")))?;
        if !self.engine.notifier.may_act(&ids, &e) {
            warn!(
                "{key}: rejected click (message {}, chat {})",
                ids.message_id, ids.chat_id
            );
            return Err(anyhow!(t(&lang, "direct.not_you")));
        }
        let sha = value["sha"].as_str().unwrap_or_default().to_string();
        let start = self.store().with(&key, |e| {
            if e.direct_busy {
                return false; // double click: ignore
            }
            e.direct_busy = true;
            e.action_msg = t(&lang, "direct.working");
            true
        });
        if start {
            let me = self.clone();
            let k = key.clone();
            thread::spawn(move || me.engine.direct_approve(&k, &sha));
        }
        let e = self.store().get(&key).unwrap_or_default();
        Ok(self
            .engine
            .notifier
            .render_for(&key, &e, e.last_pr_status.as_ref()))
    }

    /// Whether a click on message `message_id` belongs to this account's PR `key`.
    fn owns(&self, key: &str, message_id: &str) -> bool {
        self.store()
            .get(key)
            .is_some_and(|e| message_id.is_empty() || e.message_id == message_id)
    }

    pub fn start_refresher(self: &Arc<Self>) {
        let me = self.clone();
        let every = Duration::from_secs_f64(self.engine.cfg.daemon.refresh_seconds);
        thread::Builder::new()
            .name("refresher".into())
            .spawn(move || loop {
                me.refresh_all();
                thread::sleep(every);
            })
            .unwrap();
    }

    pub fn run_forever(self: &Arc<Self>) -> ! {
        self.start_worker();
        self.start_refresher();
        self.requeue_unfinished();
        let every = Duration::from_secs_f64(self.engine.cfg.daemon.poll_seconds);
        loop {
            match self.poll_once() {
                Ok(()) => write_health(&self.account, None),
                Err(e) => {
                    error!("{}: poll failed: {e:#}", self.account);
                    write_health(&self.account, Some(&format!("{e:#}")));
                    self.engine.alert(&format!("{e:#}"));
                }
            }
            wait_for_work(&self.track, every);
        }
    }
}

/// The flag the window drops next to the track file when it hands over a PR. The track file itself keeps every
/// PR still open, so it cannot be the signal.
pub fn wake_file(track: &std::path::Path) -> std::path::PathBuf {
    track.with_extension("wake")
}

/// Sleep until the next poll, but start at once when the window hands over a PR to review: someone who pastes a
/// link should not wait for the next round.
pub fn wait_for_work(track: &std::path::Path, every: Duration) {
    let wake = wake_file(track);
    let until = std::time::Instant::now() + every;
    let tick = Duration::from_millis(500).min(every);
    while std::time::Instant::now() < until {
        if fs::remove_file(&wake).is_ok() {
            return;
        }
        thread::sleep(tick);
    }
}

/// Where the window asks for approvals ("repo#n sha" per line), next to the track file.
pub fn approve_file(track: &std::path::Path) -> std::path::PathBuf {
    track.with_extension("approve")
}

/// The state file of an account: the original GitHub account keeps `state.json`.
pub fn state_file(account: &str) -> std::path::PathBuf {
    if account == "github" {
        paths::data_dir().join("state.json")
    } else {
        paths::data_dir().join(format!("state-{account}.json"))
    }
}

/// PRs handed over by "Track" in the window, one `repo#n` per line.
fn append_lines(path: &std::path::Path, lines: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for l in lines {
        writeln!(f, "{l}")?;
    }
    Ok(())
}

pub fn track_file(account: &str) -> std::path::PathBuf {
    if account == "github" {
        paths::data_dir().join("track.txt")
    } else {
        paths::data_dir().join(format!("track-{account}.txt"))
    }
}

/// heartbeat.json: when the last poll ended and whether it worked, overall and per account. The desktop app shows it
/// in the tray and window.
///
/// `fails` counts consecutive failures, so the app can ride out one flaky request without turning red.
pub fn write_health(account: &str, error: Option<&str>) {
    static STATE: Mutex<Option<serde_json::Map<String, Value>>> = Mutex::new(None);
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let accounts = g.get_or_insert_with(serde_json::Map::new);
    let prev = accounts
        .get(account)
        .and_then(|v| v["fails"].as_u64())
        .unwrap_or(0);
    let fails = if error.is_some() { prev + 1 } else { 0 };
    accounts.insert(
        account.to_string(),
        json!({"at": now(), "ok": error.is_none(), "error": error, "fails": fails}),
    );
    // overall: the worst account (most consecutive failures)
    let worst = accounts
        .iter()
        .filter(|(_, v)| v["ok"] == false)
        .max_by_key(|(_, v)| v["fails"].as_u64().unwrap_or(0));
    let many = accounts.len() > 1;
    let worst_id = worst.map(|(id, _)| id.clone());
    let (ok, err, worst_fails) = match worst {
        Some((id, v)) => (
            false,
            v["error"].as_str().map(|e| {
                if many {
                    format!("{id}: {e}")
                } else {
                    e.to_string()
                }
            }),
            v["fails"].as_u64().unwrap_or(1),
        ),
        None => (true, None, 0),
    };
    let v = json!({"at": now(), "pid": std::process::id(), "ok": ok, "error": err, "fails": worst_fails,
                   "account": worst_id, "accounts": accounts});
    let _ = fs::write(paths::data_dir().join("heartbeat.json"), v.to_string());
}

/// Where this account's checkouts go: a dry run from the window keeps apart from the worker's.
pub fn workspace(cfg: &Config, account: &str, forge: Arc<dyn Forge>) -> Workspace {
    Workspace {
        forge,
        account: account.to_string(),
        root: crate::reviewer::workspace_root(cfg.dry_run),
    }
}

/// Connect to one code-host account and put its engine together. Retries until the host answers.
pub fn build_engine(cfg: Config, acc: &ForgeAccount, store: Store) -> Result<Engine> {
    let forge: Arc<dyn Forge> = crate::forge::make(acc, &cfg)?;
    // The network may be down when the app starts (boot, resume, flaky link): keep trying instead of exiting.
    let mut delay = 10;
    let me = loop {
        match forge.current_user() {
            Ok(me) if !me.is_empty() => break me,
            Ok(_) => {
                warn!(
                    "{}: the code host returned an empty login; retrying in {delay}s",
                    acc.id
                );
                write_health(&acc.id, Some("the code host returned an empty login"));
            }
            Err(e) => {
                warn!(
                    "{}: cannot reach the code host yet ({e:#}); retrying in {delay}s",
                    acc.id
                );
                write_health(&acc.id, Some(&format!("{e:#}")));
            }
        }
        std::thread::sleep(Duration::from_secs(delay));
        delay = (delay * 2).min(300);
    };
    let notifier = notify::make(&cfg, &me);
    let reviewer = Arc::new(Router {
        ws: Arc::new(workspace(&cfg, &acc.id, forge.clone())),
        platform: platform(&acc.kind),
        me: me.clone(),
    });
    Ok(Engine {
        cfg,
        store: Arc::new(store),
        me,
        forge,
        reviewer,
        notifier,
    })
}

/// One listener for "Approve now" clicks, handing each click to the account whose card was clicked.
fn start_shared_listener(
    daemons: Arc<Mutex<Vec<Arc<Daemon>>>>,
    notifier: Arc<dyn notify::Notifier>,
    lang: String,
) {
    thread::Builder::new()
        .name("listener".into())
        .spawn(move || loop {
            let cb = |v: Value, ids: ClickIds| {
                let key = v["key"].as_str().unwrap_or_default().to_string();
                let owner = daemons
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|d| d.owns(&key, &ids.message_id))
                    .cloned();
                match owner {
                    Some(d) => d.on_action(v, ids),
                    None => Err(anyhow!(t(&lang, "direct.unknown_pr"))),
                }
            };
            match notifier.listen(&cb) {
                Ok(()) => warn!("notification listener returned; reconnecting in 15s"),
                Err(e) => error!("notification listener: {e:#}; reconnecting in 15s"),
            }
            thread::sleep(Duration::from_secs(15));
        })
        .unwrap();
}

pub fn run(cfg: Config) -> Result<()> {
    let Some(_lock) = try_lock() else {
        return Err(anyhow!(
            "another Nod process is already reviewing (pid {})",
            running_pid()
        ));
    };
    fs::write(
        paths::data_dir().join("daemon.pid"),
        std::process::id().to_string(),
    )?;
    let accounts: Vec<ForgeAccount> = cfg.accounts().into_iter().filter(|a| a.enabled).collect();
    let daemons: Arc<Mutex<Vec<Arc<Daemon>>>> = Arc::new(Mutex::new(vec![]));
    let listener = notify::make(&cfg, "");
    if listener.supports_actions() {
        start_shared_listener(daemons.clone(), listener, cfg.language.clone());
    }
    for acc in accounts {
        let cfg = cfg.clone();
        let daemons = daemons.clone();
        thread::Builder::new()
            .name(format!("account-{}", acc.id))
            .spawn(move || {
                let started = (|| -> Result<Arc<Daemon>> {
                    let engine = build_engine(cfg, &acc, Store::open(Some(state_file(&acc.id)))?)?;
                    info!(
                        "daemon started pid={} account={} ({}) user={} channels={:?}",
                        std::process::id(),
                        acc.id,
                        acc.kind,
                        engine.me,
                        engine.cfg.notify.active()
                    );
                    Ok(Daemon::for_account(&acc.id, engine))
                })();
                match started {
                    Ok(d) => {
                        daemons.lock().unwrap().push(d.clone());
                        d.run_forever()
                    }
                    // a bad account (unknown plugin, unreadable state) is reported; the others keep going
                    Err(e) => {
                        error!("{}: {e:#}", acc.id);
                        write_health(&acc.id, Some(&format!("{e:#}")));
                    }
                }
            })?;
    }
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handed_over_pr_wakes_the_poll_once() {
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("track.txt");
        fs::write(&track, "o/r#1\n").unwrap(); // a PR that is still tracked must not keep waking the loop
        let t = std::time::Instant::now();
        wait_for_work(&track, Duration::from_millis(300));
        assert!(t.elapsed() >= Duration::from_millis(300));
        fs::write(wake_file(&track), "").unwrap();
        let t = std::time::Instant::now();
        wait_for_work(&track, Duration::from_secs(30));
        assert!(t.elapsed() < Duration::from_secs(2));
        assert!(!wake_file(&track).exists());
    }
}
