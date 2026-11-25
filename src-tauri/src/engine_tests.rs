//! Review lifecycle and the daemon's concurrency, with GitHub, Claude and notifications replaced by fakes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::config::{AlwaysRule, Config};
use crate::daemon::Daemon;
use crate::engine::{Engine, Entry, Store};
use crate::forge::{DiffLines, Forge, Pr, PrStatus};
use crate::notify::{ClickIds, Notifier};
use crate::review::tests::{clean, finding};
use crate::review::Verdict;
use crate::reviewer::Reviewer;
use crate::util;

const HEAD: &str = "aaaaaaa1";

/// Tests that approve directly stop the running review of "o/r#1", and every fake PR is "o/r#1": the registry of
/// running engine processes is shared by the whole test binary, so one test's approval would kill another test's
/// review. These tests take turns.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct World {
    posts: Vec<String>,
    /// The reviews as posted, in order.
    bodies: Vec<Value>,
    post_delay_ms: u64,
    post_fail: bool,
    checkout_delay_ms: u64,
    claude_proc: bool,
    verdict: Option<Verdict>,
    pr_head: String,
    pr_author: String,
    pr_draft: bool,
    pr_state: String,
    ci: String,
    requested: Vec<(String, u64)>,
    reviews: Vec<(u64, String)>,
    dismissed: Vec<u64>,
    updates: Vec<(String, String)>,
    claude_fail: bool,
    /// The engine's error when `claude_fail` (default "boom").
    fail_msg: String,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<World>>);

impl Fake {
    fn w(&self) -> std::sync::MutexGuard<'_, World> {
        self.0.lock().unwrap()
    }
    fn pr(&self) -> Pr {
        let w = self.w();
        Pr {
            repo: "o/r".into(),
            number: 1,
            title: "t".into(),
            author: w.pr_author.clone(),
            url: "u".into(),
            head: w.pr_head.clone(),
            base: "main".into(),
            draft: w.pr_draft,
            state: if w.pr_state.is_empty() {
                "open".into()
            } else {
                w.pr_state.clone()
            },
            body: String::new(),
        }
    }
}

impl Forge for Fake {
    fn remote(&self, _repo: &str) -> Result<crate::forge::Remote> {
        Ok(crate::forge::Remote::Git {
            url: "https://example.invalid/o/r.git".into(),
            header: None,
        })
    }

    fn current_user(&self) -> Result<String> {
        Ok("me".into())
    }
    fn fetch_pr(&self, _: &str, _: u64) -> Result<Pr> {
        Ok(self.pr())
    }
    fn requested_prs(&self, _teams: bool) -> Result<Vec<(String, u64)>> {
        Ok(self.w().requested.clone())
    }
    fn ci_state(&self, _: &Pr) -> Result<String> {
        Ok(self.w().ci.clone())
    }
    fn pr_status(&self, _: &str, _: u64) -> Result<PrStatus> {
        let p = self.pr();
        Ok(PrStatus {
            state: "open".into(),
            head: p.head,
            author: p.author,
            title: "t".into(),
            url: "u".into(),
            ..Default::default()
        })
    }
    fn discussion(&self, _: &Pr) -> Result<String> {
        Ok(String::new())
    }
    fn diff_lines(&self, _: &Pr) -> Result<DiffLines> {
        Ok(DiffLines::from([(
            "a.py".to_string(),
            std::collections::HashSet::from([1]),
        )]))
    }
    fn post_review(&self, _: &Pr, review: &Value) -> Result<(String, u64)> {
        let (delay, fail) = {
            let w = self.w();
            (w.post_delay_ms, w.post_fail)
        };
        thread::sleep(Duration::from_millis(delay));
        if fail {
            bail!("GitHub 502");
        }
        let mut w = self.w();
        w.posts.push(review["event"].as_str().unwrap().to_string());
        w.bodies.push(review.clone());
        Ok((
            "https://github.com/o/r/pull/1#r".into(),
            w.posts.len() as u64,
        ))
    }
    fn list_reviews(&self, _: &Pr) -> Result<Vec<(u64, String)>> {
        Ok(self.w().reviews.clone())
    }
    fn dismiss_review(&self, _: &Pr, id: u64, _: &str) -> Result<()> {
        self.w().dismissed.push(id);
        Ok(())
    }
}

impl Reviewer for Fake {
    fn checkout(&self, _: &Pr) -> Result<(PathBuf, PathBuf)> {
        let d = self.w().checkout_delay_ms;
        thread::sleep(Duration::from_millis(d));
        Ok(("wt".into(), "ctx".into()))
    }
    fn ask(&self, _: &Config, pr: &Pr, _: &Path, _: &Path, _: &str) -> Result<Verdict> {
        let (proc_, fail, msg) = {
            let w = self.w();
            (w.claude_proc, w.claude_fail, w.fail_msg.clone())
        };
        if fail {
            bail!("{}", if msg.is_empty() { "boom" } else { &msg });
        }
        if proc_ {
            // a real child process, so "Approve now" has something to kill
            let sleeper: Vec<&str> = if cfg!(windows) {
                vec!["powershell", "-NoProfile", "-Command", "Start-Sleep 20"]
            } else {
                vec!["sleep", "20"]
            };
            let key = pr.key();
            util::run(
                &sleeper,
                util::RunOpts {
                    track: Some(&key),
                    timeout: Duration::from_secs(60),
                    ..Default::default()
                },
            )?;
        } else {
            thread::sleep(Duration::from_millis(30));
        }
        Ok(self.w().verdict.clone().unwrap())
    }
    fn cleanup(&self, _: &Pr, _: &Path) {}
}

impl Notifier for Fake {
    fn update(&self, _: &str, e: &mut Entry, _: Option<&PrStatus>) -> Result<bool> {
        let kind = e.note.as_ref().map(|n| n.kind.clone()).unwrap_or_default();
        self.w().updates.push((e.status.clone(), kind));
        e.message_id = "om_1".into();
        Ok(true)
    }
    fn supports_actions(&self) -> bool {
        true
    }
    fn may_act(&self, ids: &ClickIds, _: &Entry) -> bool {
        ids.open_id == "me-id"
    }
    fn render_for(&self, _: &str, e: &Entry, _: Option<&PrStatus>) -> Option<Value> {
        Some(json!({"busy": e.direct_busy}))
    }
}

fn bad() -> Verdict {
    let mut v = clean();
    v.summary = "bad".into();
    v.approve_recommended = false;
    v.findings = vec![finding("high", true, 1)];
    v
}

/// Nothing blocks approval, but there is something to say: a low-severity finding.
fn minor() -> Verdict {
    let mut v = clean();
    v.findings = vec![finding("low", true, 1)];
    v
}

fn setup() -> (Fake, Arc<Engine>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let f = Fake(Arc::new(Mutex::new(World {
        post_delay_ms: 50,
        pr_head: HEAD.into(),
        pr_author: "alice".into(),
        ci: "success".into(),
        verdict: Some(bad()),
        ..Default::default()
    })));
    let engine = Engine {
        cfg: Config::default(),
        store: Arc::new(Store::open(Some(dir.path().join("state.json"))).unwrap()),
        me: "me".into(),
        forge: Arc::new(f.clone()),
        reviewer: Arc::new(f.clone()),
        notifier: Arc::new(f.clone()),
    };
    (f, Arc::new(engine), dir)
}

fn e(engine: &Engine) -> Entry {
    engine.store.get("o/r#1").unwrap_or_default()
}

fn wait_for(cond: impl Fn() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) && !cond() {
        thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn issues_are_commented_not_approved() {
    let (f, en, _d) = setup();
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["COMMENT"]);
    assert_eq!(e(&en).status, "posted");
    assert_eq!(e(&en).reasons, vec!["high", "model_not_recommended"]);
    assert_eq!(f.w().updates.last().unwrap().1, "comment");
}

/// [scope] authors decides whose requests start a review; a PR handed over by hand is reviewed anyway.
#[test]
fn only_requests_from_chosen_authors_are_reviewed() {
    let (f, en, dir) = setup();
    f.w().requested = vec![("o/r".into(), 1)];
    let mut cfg = en.cfg.clone();
    cfg.scope.authors = vec!["bob".into()];
    let en2 = Engine {
        cfg,
        store: en.store.clone(),
        me: en.me.clone(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    };
    assert_eq!(
        en2.collect_todo().unwrap().len(),
        0,
        "alice is not on the list"
    );
    let track = dir.path().join("track.txt");
    std::fs::write(
        &track, "o/r#1!
",
    )
    .unwrap();
    let d = Daemon::with_track("github", en2, track);
    d.poll_once().unwrap();
    assert_eq!(e(&en).status, "queued", "handed over by hand: reviewed");
}

fn with_cfg(en: &Engine, f: impl FnOnce(&mut Config)) -> Engine {
    let mut cfg = en.cfg.clone();
    f(&mut cfg);
    Engine {
        cfg,
        store: en.store.clone(),
        me: en.me.clone(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    }
}

/// [approve] always_authors: approved whatever the review finds and without waiting for CI; the findings still go up.
#[test]
fn always_approve_list_approves_whatever_the_review_says() {
    let (f, en, _d) = setup();
    f.w().ci = "pending".into(); // bad() has a high-severity finding
    let en = with_cfg(&en, |c| {
        c.approve.always = vec![AlwaysRule {
            author: "alice".into(),
            ..Default::default()
        }];
        c.approve.mode = "never".into(); // independent of the auto-approve switch
    });
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    let b = f.w().bodies[0].clone();
    assert!(
        b["body"].as_str().unwrap().contains("无条件 approve 名单"),
        "{b}"
    );
    assert!(
        !b["comments"].as_array().unwrap().is_empty(),
        "the findings are posted with it"
    );
}

/// On the list, a review that fails for good still ends in an approval.
#[test]
fn always_approve_list_approves_when_the_review_fails() {
    let (f, en, _d) = setup();
    f.w().claude_fail = true;
    let en = with_cfg(&en, |c| {
        c.approve.always = vec![AlwaysRule {
            repo: "o/*".into(),
            ..Default::default()
        }]
    });
    en.review_one(&f.pr());
    assert!(f.w().posts.is_empty(), "the first failure is retried");
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    assert_eq!(
        (e(&en).status.as_str(), e(&en).event.as_deref()),
        ("posted", Some("APPROVE"))
    );
}

/// Your own PR cannot be approved, list or not.
#[test]
fn always_approve_list_still_skips_your_own_pr() {
    let (f, en, _d) = setup();
    f.w().pr_author = "me".into();
    let en = with_cfg(&en, |c| {
        c.approve.always = vec![AlwaysRule {
            author: "*".into(),
            ..Default::default()
        }]
    });
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["COMMENT"]);
}

/// "Approve" in the window: the worker approves exactly the commit the window showed, once.
#[test]
fn approve_from_the_window() {
    let _one = one_at_a_time();
    let (f, en, dir) = setup();
    en.review_one(&f.pr()); // bad(): commented, not approved
    assert_eq!(f.w().posts, vec!["COMMENT"]);
    let track = dir.path().join("track.txt");
    let d = Daemon::with_track("github", with_cfg(&en, |_| {}), track.clone());
    std::fs::write(
        crate::daemon::approve_file(&track),
        format!(
            "o/r#1 {HEAD}
"
        ),
    )
    .unwrap();
    d.take_approvals();
    assert_eq!(f.w().posts, vec!["COMMENT", "APPROVE"]);
    assert_eq!(e(&en).status, "direct_approved");
    d.take_approvals(); // handled once
    assert_eq!(f.w().posts.len(), 2);

    // "Withdraw approval": the approval is dismissed and the record says so
    f.w().reviews = vec![(2, "APPROVED".into())];
    std::fs::write(
        crate::daemon::approve_file(&track),
        "withdraw o/r#1
",
    )
    .unwrap();
    d.take_approvals();
    assert_eq!(f.w().dismissed, vec![2]);
    let x = e(&en);
    assert_eq!(
        (x.status.as_str(), x.direct_approve_id, x.approve_ids.len()),
        ("withdrawn", None, 0)
    );
    assert!(
        en.collect_todo().unwrap().is_empty(),
        "the same commit is not reviewed again"
    );

    // a commit the window showed before a new push: refused, and the record says why
    f.w().pr_head = "bbbbbbb2".into();
    std::fs::write(
        crate::daemon::approve_file(&track),
        format!(
            "o/r#1 {HEAD}
"
        ),
    )
    .unwrap();
    d.take_approvals();
    assert_eq!(f.w().posts.len(), 2);
    assert!(!e(&en).action_msg.is_empty() && !e(&en).direct_busy);
}

/// "Review again": a commit that was already reviewed and posted is reviewed and posted once more, from scratch.
#[test]
fn review_again_starts_over_whatever_happened() {
    let (f, en, dir) = setup();
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["COMMENT"]);
    let track = dir.path().join("track.txt");
    let d = Daemon::with_track("github", with_cfg(&en, |_| {}), track.clone());
    std::fs::write(
        &track, "o/r#1!
",
    )
    .unwrap();
    d.poll_once().unwrap();
    assert_eq!(
        e(&en).status,
        "posted",
        "the Review dialog alone does not redo a posted review"
    );
    std::fs::write(
        &track, "o/r#1!!
",
    )
    .unwrap();
    d.poll_once().unwrap();
    let x = e(&en);
    assert_eq!(
        (x.status.as_str(), x.verdict.is_none(), x.event.is_none()),
        ("queued", true, true)
    );
    f.w().verdict = Some(clean());
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["COMMENT", "APPROVE"]);
}

/// Each review is kept as a round of its own, with what went up for it.
#[test]
fn every_review_is_a_round_of_its_own() {
    let (f, en, _d) = setup();
    f.w().ci = "pending".into();
    f.w().verdict = Some(minor());
    en.review_one(&f.pr()); // comments now, approval after CI
    f.w().ci = "success".into();
    en.finish_safe("o/r#1");
    // a new commit with issues
    f.w().pr_head = "bbbbbbb2".into();
    f.w().verdict = Some(bad());
    en.review_one(&f.pr());
    let r = e(&en).rounds;
    assert_eq!(r.len(), 2);
    assert_eq!(
        (
            r[0].sha.as_str(),
            r[0].event.as_deref(),
            r[0].comment_url.is_some()
        ),
        (HEAD, Some("APPROVE"), true)
    );
    assert_eq!(
        (r[1].sha.as_str(), r[1].event.as_deref()),
        ("bbbbbbb2", Some("COMMENT"))
    );
    assert!(!r[1].findings.is_empty());
}

#[test]
fn clean_pr_is_approved_and_remembered() {
    let (f, en, _d) = setup();
    f.w().verdict = Some(clean());
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    assert_eq!(e(&en).approve_ids, vec![1]);
    assert!(
        f.w().bodies[0]["body"]
            .as_str()
            .unwrap()
            .contains("规则全部通过"),
        "says why it approved"
    );
}

#[test]
fn failure_retries_then_notifies() {
    let (f, en, _d) = setup();
    f.w().claude_fail = true;
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "retry");
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "failed");
    assert_eq!(f.w().updates.last().unwrap().1, "manual");
    assert!(f.w().posts.is_empty());
}

#[test]
fn an_ordinary_failure_pauses_before_the_retry() {
    let (f, en, _d) = setup();
    f.w().claude_fail = true;
    en.review_one(&f.pr());
    let x = e(&en);
    assert!(
        x.waiting() && x.retry_at > util::now() + 30.0,
        "retries after a pause"
    );
    assert_eq!(
        en.collect_todo().unwrap().len(),
        0,
        "not picked up while it waits"
    );
}

#[test]
fn out_of_quota_waits_without_using_up_attempts() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.claude_fail = true;
        w.fail_msg =
            "Claude returned an error: \"Claude AI usage limit reached|9999999999\"".into();
        w.requested = vec![("o/r".into(), 1)];
    }
    for _ in 0..4 {
        en.review_one(&f.pr());
    }
    let x = e(&en);
    assert_eq!(
        (x.status.as_str(), x.attempts, x.waits),
        ("retry", 0, 4),
        "never gives up on a quota"
    );
    assert!(
        x.waiting() && x.retry_at <= util::now() + 86400.0 + 1.0,
        "waits, at most a day"
    );
    assert_eq!(x.note.as_ref().unwrap().kind, "wait");
    assert!(f.w().posts.is_empty(), "nothing approved while it waits");
    assert_eq!(en.collect_todo().unwrap().len(), 0);

    // the time comes: picked up again, and a good review clears the wait
    en.store.with("o/r#1", |e| e.retry_at = util::now() - 1.0);
    assert_eq!(en.collect_todo().unwrap().len(), 1);
    f.w().claude_fail = false;
    en.review_one(&f.pr());
    let x = e(&en);
    assert_eq!(
        (x.status.as_str(), x.waits, x.note.unwrap().kind.as_str()),
        ("posted", 0, "comment")
    );
}

/// reqgenie#255: Claude Code's sign-in expired. Not this PR's fault: no attempts used up, no "needs you" card with the
/// raw English error, but a wait that says how to fix it, retried at least every half hour.
#[test]
fn a_signed_out_engine_waits_instead_of_failing_the_pr() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.claude_fail = true;
        w.fail_msg = "Claude returned an error: \"Failed to authenticate: OAuth session expired and could not be refreshed\"".into();
        w.requested = vec![("o/r".into(), 1)];
    }
    for _ in 0..4 {
        en.store.with("o/r#1", |e| e.retry_at = 0.0);
        en.review_one(&f.pr());
    }
    let x = e(&en);
    assert_eq!((x.status.as_str(), x.attempts, x.waits), ("retry", 0, 4));
    assert!(x.retry_at <= util::now() + crate::engine::SIGNIN_RETRY_MAX_S + 1.0);
    let note = x.note.unwrap();
    assert_eq!(note.kind, "wait");
    assert!(note.text.contains("/login"), "{}", note.text);
    assert!(f.w().posts.is_empty());

    // signed in again: the next try reviews it
    en.store.with("o/r#1", |e| e.retry_at = util::now() - 1.0);
    f.w().claude_fail = false;
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "posted");
}

#[test]
fn quota_backoff_doubles_up_to_six_hours() {
    use crate::engine::quota_backoff;
    assert_eq!(quota_backoff(1), 900.0);
    assert_eq!(quota_backoff(2), 1800.0);
    assert_eq!(quota_backoff(10), 6.0 * 3600.0);
    assert_eq!(quota_backoff(u32::MAX), 6.0 * 3600.0);
}

/// "Review" in the window on a PR whose review of this commit failed starts it over, once.
#[test]
fn reviewing_a_failed_pr_again_on_request() {
    let (f, en, dir) = setup();
    f.w().claude_fail = true;
    en.review_one(&f.pr());
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "failed");
    let track = dir.path().join("track.txt");
    let en2 = Engine {
        cfg: en.cfg.clone(),
        store: en.store.clone(),
        me: en.me.clone(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    };
    let d = Daemon::with_track("github", en2, track.clone());
    std::fs::write(
        &track, "o/r#1
",
    )
    .unwrap();
    d.poll_once().unwrap();
    assert_eq!(
        e(&en).status,
        "failed",
        "only following it does not review the same commit again"
    );
    std::fs::write(
        &track, "o/r#1!
",
    )
    .unwrap();
    d.poll_once().unwrap();
    let x = e(&en);
    assert_eq!(
        (x.status.as_str(), x.attempts),
        ("queued", 0),
        "asked again: starts over"
    );
    let text = std::fs::read_to_string(&track).unwrap();
    assert!(
        text.contains("o/r#1") && !text.contains('!'),
        "once: {text}"
    );
}

#[test]
fn post_failure_keeps_verdict_then_gives_up() {
    let (f, en, _d) = setup();
    f.w().post_fail = true;
    en.review_one(&f.pr());
    assert_eq!(
        (e(&en).status.as_str(), e(&en).post_attempts),
        ("reviewed", 1)
    );
    en.finish_safe("o/r#1");
    assert_eq!(e(&en).status, "failed");
}

#[test]
fn clean_waits_for_ci() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.verdict = Some(minor());
        w.ci = "pending".into();
    }
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "awaiting_ci");
    // the comments go up at once; the approval waits for CI
    assert_eq!(f.w().posts, vec!["COMMENT"]);
    let first = f.w().bodies[0]["body"].as_str().unwrap().to_string();
    assert!(
        first.contains("没有阻塞问题，CI 通过后 approve") && !first.contains("未 approve"),
        "{first}"
    );
    en.finish_safe("o/r#1"); // still pending: nothing new
    assert_eq!(f.w().posts, vec!["COMMENT"]);
    f.w().ci = "success".into();
    en.finish_safe("o/r#1");
    assert_eq!(f.w().posts, vec!["COMMENT", "APPROVE"]);
    let approve = f.w().bodies[1].clone();
    assert!(
        approve.get("comments").is_none(),
        "the comments are not posted twice"
    );
    assert_eq!(
        (e(&en).status.as_str(), e(&en).approve_ids.as_slice()),
        ("posted", &[2u64][..])
    );
}

/// Nothing blocks and nothing to say: no "not approved" comment while CI runs, just one approval after it.
#[test]
fn clean_without_findings_posts_only_the_approval_after_ci() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.verdict = Some(clean());
        w.ci = "pending".into();
    }
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "awaiting_ci");
    assert!(f.w().posts.is_empty());
    let updates = f.w().updates.len();
    en.finish_safe("o/r#1"); // still pending: no repeat notification either
    assert!(f.w().posts.is_empty());
    assert_eq!(f.w().updates.len(), updates);
    f.w().ci = "success".into();
    en.finish_safe("o/r#1");
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    let body = f.w().bodies[0]["body"].as_str().unwrap().to_string();
    assert!(body.contains("结论：Approve"), "{body}");
}

/// Only issues raised before (a busy PR, commit after commit): no review of their own while CI runs; they go once,
/// folded, with the approval.
#[test]
fn issues_raised_before_wait_for_the_approval_instead_of_a_review_of_their_own() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        let mut v = minor();
        for x in &mut v.findings {
            x.title = format!("{}（已有评论提过）", x.title);
        }
        w.verdict = Some(v);
        w.ci = "pending".into();
    }
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "awaiting_ci");
    assert!(f.w().posts.is_empty());
    f.w().ci = "success".into();
    en.finish_safe("o/r#1");
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    let b = f.w().bodies[0].clone();
    let body = b["body"].as_str().unwrap();
    assert!(body.contains("<details>"), "{body}");
    assert!(b["comments"].as_array().is_none_or(|c| c.is_empty()), "{b}");
}

#[test]
fn ci_failing_after_the_comments_says_why_without_repeating_them() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.verdict = Some(minor());
        w.ci = "pending".into();
    }
    en.review_one(&f.pr());
    f.w().ci = "failure".into();
    en.finish_safe("o/r#1");
    assert_eq!(f.w().posts, vec!["COMMENT", "COMMENT"]);
    let last = f.w().bodies[1]["body"].as_str().unwrap().to_string();
    assert!(
        last.contains("未 approve") && f.w().bodies[1].get("comments").is_none(),
        "{last}"
    );
}

#[test]
fn no_ci_waits_from_when_the_commit_was_seen_and_not_in_a_repo_without_ci() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.verdict = Some(clean());
        w.ci = "none".into();
    }
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "awaiting_ci");
    assert!(e(&en).ci_deadline > util::now());
    // seen more than the grace ago (the review itself took most of it): approve now
    en.store
        .with("o/r#1", |e| e.queued_at = util::now() - 11.0 * 60.0);
    en.finish_safe("o/r#1");
    assert_eq!(f.w().posts, vec!["APPROVE"]);

    // a second PR in the same repository: it had no CI before, so no wait
    let (f2, en2, _d2) = setup();
    {
        let mut w = f2.w();
        w.verdict = Some(clean());
        w.ci = "none".into();
    }
    en2.store.with("o/r#9", |e| {
        e.status = "posted".into();
        e.last_pr_status = Some(PrStatus {
            ci: "none".into(),
            ..Default::default()
        });
    });
    en2.review_one(&f2.pr());
    assert_eq!(f2.w().posts, vec!["APPROVE"]);
}

#[test]
fn approving_directly_keeps_the_findings() {
    let _one = one_at_a_time();
    let (f, en, _d) = setup();
    f.w().ci = "pending".into();
    f.w().verdict = Some(Verdict {
        approve_recommended: false,
        ..bad()
    });
    // reviewed, comments not yet posted (e.g. a rule still blocks): the approval carries them
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.verdict_sha = HEAD.into();
        e.verdict = Some(bad());
        e.status = "reviewed".into();
    });
    en.direct_approve("o/r#1", HEAD);
    let b = f.w().bodies[0].clone();
    assert_eq!(b["event"], "APPROVE");
    assert!(b["body"].as_str().unwrap().contains("由本人直接通过"));
    assert!(!b["comments"].as_array().unwrap().is_empty(), "{b}");

    // comments already up: the approval alone
    let (f2, en2, _d2) = setup();
    en2.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.verdict_sha = HEAD.into();
        e.comment_sha = HEAD.into();
        e.verdict = Some(bad());
        e.status = "awaiting_ci".into();
    });
    en2.direct_approve("o/r#1", HEAD);
    assert!(f2.w().bodies[0].get("comments").is_none());
}

#[test]
fn merged_while_waiting_for_ci_does_not_claim_ci_blocked_it() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.verdict = Some(minor());
        w.ci = "pending".into();
    }
    en.review_one(&f.pr());
    assert_eq!(e(&en).reasons, vec!["ci_pending"]);
    f.w().pr_state = "merged".into();
    en.finish_safe("o/r#1");
    assert_eq!(
        f.w().posts,
        vec!["COMMENT"],
        "only the comments posted while waiting"
    );
    assert_eq!(e(&en).status, "closed");
    assert_eq!(e(&en).reasons, vec!["closed_while_waiting"]);
}

/// Merged while the new commit was being reviewed: the record says the verdict never went up, instead of nothing.
#[test]
fn merged_during_the_review_says_the_verdict_was_not_posted() {
    let (f, en, _d) = setup();
    f.w().verdict = Some(clean());
    f.w().pr_state = "merged".into();
    en.review_one(&f.pr());
    assert!(f.w().posts.is_empty());
    assert_eq!(
        (e(&en).status.as_str(), e(&en).reasons.as_slice()),
        ("closed", &["closed_before_posting".to_string()][..])
    );
}

#[test]
fn closing_keeps_reasons_of_a_posted_review() {
    let mut x = Entry {
        status: "posted".into(),
        event: Some("COMMENT".into()),
        reasons: vec!["high".into()],
        ..Default::default()
    };
    x.mark_closed();
    assert_eq!(
        (x.status.as_str(), x.reasons.clone()),
        ("closed", vec!["high".to_string()])
    );
    let mut y = Entry {
        status: "reviewed".into(),
        reasons: vec!["ci_failed".into()],
        ..Default::default()
    };
    y.mark_closed();
    assert!(y.reasons.is_empty());
}

#[test]
fn new_commit_during_review_discards_verdict() {
    let (f, en, _d) = setup();
    let pr = f.pr();
    f.w().pr_head = "bbbbbbb2".into();
    en.review_one(&pr);
    assert_eq!(e(&en).status, "stale");
    assert!(f.w().posts.is_empty());
}

#[test]
fn secret_in_review_is_blocked() {
    let (f, en, _d) = setup();
    let mut v = bad();
    v.summary = format!("token gho_{}", "a".repeat(36));
    f.w().verdict = Some(v);
    en.review_one(&f.pr());
    assert_eq!(e(&en).status, "failed");
    assert!(f.w().posts.is_empty());
}

#[test]
fn withdraws_only_own_earlier_approval() {
    let (f, en, _d) = setup();
    en.store.with("o/r#1", |e| e.approve_ids = vec![7]);
    f.w().reviews = vec![(7, "APPROVED".into()), (8, "APPROVED".into())];
    en.review_one(&f.pr());
    assert_eq!(f.w().dismissed, vec![7]);
    assert!(e(&en).approve_ids.is_empty());
}

#[test]
fn collect_todo_and_rereview() {
    let (f, en, _d) = setup();
    f.w().requested = vec![("o/r".into(), 1)];
    assert_eq!(en.collect_todo().unwrap().len(), 1);
    en.review_one(&f.pr());
    assert!(en.collect_todo().unwrap().is_empty()); // same commit: not again
    {
        let mut w = f.w();
        w.pr_head = "ccccccc3".into();
        w.requested.clear();
    }
    assert_eq!(en.collect_todo().unwrap()[0].head, "ccccccc3"); // new commit: again
}

/// Comments posted while waiting for CI, then a new commit: GitHub has dropped the review request and nothing was
/// "posted" yet, but the new commit is still picked up on the next poll (kaptra-worker-template#5).
#[test]
fn stale_after_comments_is_rereviewed() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.verdict = Some(minor());
        w.ci = "pending".into();
        w.requested.clear();
    }
    en.review_one(&f.pr());
    assert_eq!(f.w().posts, vec!["COMMENT"]);
    f.w().pr_head = "ccccccc3".into();
    en.finish_safe("o/r#1");
    assert_eq!(e(&en).status, "stale");
    assert_eq!(e(&en).posted_at, 0.0);
    let todo = en.collect_todo().unwrap();
    assert_eq!(todo.len(), 1);
    assert_eq!(todo[0].head, "ccccccc3");
    en.review_one(&todo[0]);
    assert!(en.collect_todo().unwrap().is_empty()); // same commit: not again
}

#[test]
fn draft_is_skipped_without_losing_state() {
    let (f, en, _d) = setup();
    en.review_one(&f.pr());
    {
        let mut w = f.w();
        w.pr_head = "d2".into();
        w.pr_draft = true;
    }
    assert!(en.collect_todo().unwrap().is_empty());
    assert_eq!(e(&en).status, "posted");
    f.w().pr_draft = false;
    assert_eq!(en.collect_todo().unwrap()[0].head, "d2");
}

#[test]
fn deny_list() {
    let (f, en, _d) = setup();
    f.w().requested = vec![("o/r".into(), 1)];
    let mut cfg = en.cfg.clone();
    cfg.scope.exclude_repos = vec!["O/*".into()];
    let en2 = Engine {
        cfg,
        store: en.store.clone(),
        me: "me".into(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    };
    assert!(en2.collect_todo().unwrap().is_empty());
}

// ---------------------------------------------------------------- daemon: "Approve now"

fn daemon(en: &Arc<Engine>) -> Arc<Daemon> {
    let en2 = Engine {
        cfg: en.cfg.clone(),
        store: en.store.clone(),
        me: en.me.clone(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    };
    let d = Daemon::for_account("github", en2);
    d.start_worker();
    d
}

fn click(d: &Arc<Daemon>, who: &str) -> Result<Option<Value>> {
    d.on_action(
        json!({"action": "approve", "key": "o/r#1", "sha": HEAD}),
        ClickIds {
            open_id: who.into(),
            ..Default::default()
        },
    )
}

#[test]
fn click_during_checkout_stops_review() {
    let _one = one_at_a_time();
    let (f, en, _d) = setup();
    f.w().checkout_delay_ms = 1000;
    let d = daemon(&en);
    d.enqueue(f.pr());
    wait_for(|| e(&en).status == "reviewing");
    click(&d, "me-id").unwrap();
    thread::sleep(Duration::from_millis(2000));
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    assert_eq!(e(&en).status, "direct_approved");
}

#[test]
fn click_while_claude_runs_kills_it() {
    let _one = one_at_a_time();
    let (f, en, _d) = setup();
    f.w().claude_proc = true;
    let d = daemon(&en);
    d.enqueue(f.pr());
    wait_for(|| util::abort_target_exists("o/r#1"));
    assert!(util::abort_target_exists("o/r#1"));
    let start = Instant::now();
    click(&d, "me-id").unwrap();
    wait_for(|| e(&en).status == "direct_approved" && !util::abort_target_exists("o/r#1"));
    thread::sleep(Duration::from_millis(300));
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the 20 s child was not killed"
    );
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    assert!(e(&en).aborted_review);
}

#[test]
fn double_click_approves_once() {
    let _one = one_at_a_time();
    let (f, en, _d) = setup();
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "posted".into();
    });
    let d = daemon(&en);
    assert_eq!(click(&d, "me-id").unwrap(), Some(json!({"busy": true})));
    click(&d, "me-id").unwrap();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(f.w().posts, vec!["APPROVE"]);
}

#[test]
fn failed_approve_lets_review_continue() {
    let _one = one_at_a_time();
    let (f, en, _d) = setup();
    f.w().post_fail = true;
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "queued".into();
    });
    let d = daemon(&en);
    click(&d, "me-id").unwrap();
    thread::sleep(Duration::from_millis(400));
    assert!(!e(&en).aborted);
    assert!(e(&en).action_msg.contains("失败"));
    f.w().post_fail = false;
    d.work_one(&f.pr());
    assert_eq!(f.w().posts, vec!["COMMENT"]);
}

#[test]
fn head_moved_or_own_pr_or_stranger_is_refused() {
    let _one = one_at_a_time();
    let (f, en, _d) = setup();
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "posted".into();
    });
    let d = daemon(&en);
    assert!(click(&d, "someone-else").is_err());
    f.w().pr_head = "zzzzzzz9".into();
    click(&d, "me-id").unwrap();
    thread::sleep(Duration::from_millis(400));
    assert!(e(&en).action_msg.contains("zzzzzzz"));
    f.w().pr_head = HEAD.into();
    f.w().pr_author = "me".into();
    click(&d, "me-id").unwrap();
    thread::sleep(Duration::from_millis(400));
    assert!(f.w().posts.is_empty());
}

#[test]
fn awaiting_ci_and_click_do_not_interleave() {
    let _one = one_at_a_time();
    let (f, en, _d) = setup();
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "awaiting_ci".into();
        e.verdict = Some(bad());
        e.reviewed_at = util::now() - 30.0;
    });
    f.w().post_delay_ms = 300;
    let d = daemon(&en);
    let en2 = en.clone();
    let t = thread::spawn(move || en2.finish_safe("o/r#1"));
    thread::sleep(Duration::from_millis(50));
    click(&d, "me-id").unwrap();
    t.join().unwrap();
    thread::sleep(Duration::from_millis(800));
    assert_eq!(f.w().posts, vec!["COMMENT", "APPROVE"]);
    assert_eq!(e(&en).status, "direct_approved");
}

#[test]
fn restart_requeues_unfinished() {
    let (f, en, _d) = setup();
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "reviewing".into();
    });
    let d = daemon(&en);
    d.requeue_unfinished();
    wait_for(|| e(&en).status == "posted");
    assert_eq!(f.w().posts, vec!["COMMENT"]);
}

/// A review cut off by a restart (quitting Nod, saving settings) does not use up an attempt: an engine error after the
/// restart still gets its retry.
#[test]
fn a_restart_mid_review_does_not_use_up_an_attempt() {
    let (f, en, _d) = setup();
    f.w().claude_fail = true;
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "reviewing".into();
        e.attempts = 1; // the run the restart cut off
    });
    let d = daemon(&en);
    d.requeue_unfinished();
    wait_for(|| e(&en).status == "retry");
    assert_eq!(
        (e(&en).status.as_str(), e(&en).attempts),
        ("retry", 1),
        "retries instead of giving up"
    );
}

#[test]
fn state_with_nulls_and_unknown_fields_loads() {
    let s = crate::engine::parse_state(r#"{"prs":{"a/b#1":{"status":"posted","reasons":null,"event":null,"future_field":1,
        "verdict":{"summary":"s","findings":[],"approve_recommended":true,"unable_reason":"","_meta":{"duration_s":3,"cost_usd":null}}}}}"#)
        .unwrap();
    let e = &s.prs["a/b#1"];
    assert_eq!(e.status, "posted");
    assert!(e.reasons.is_empty() && e.extra.contains_key("future_field"));
}

/// Regression (found in review): a commit pushed while an older one is being reviewed must not receive the old
/// verdict. The old review's result is dropped and the new commit stays queued for its own review.
#[test]
fn new_commit_enqueued_during_review_is_not_approved_with_old_verdict() {
    let (f, en, _d) = setup();
    {
        let mut w = f.w();
        w.verdict = Some(clean());
        w.checkout_delay_ms = 600;
    }
    let d = Daemon::for_account(
        "github",
        Engine {
            cfg: en.cfg.clone(),
            store: en.store.clone(),
            me: en.me.clone(),
            forge: en.forge.clone(),
            reviewer: en.reviewer.clone(),
            notifier: en.notifier.clone(),
        },
    );
    let old = f.pr();
    let en2 = en.clone();
    let t = thread::spawn(move || en2.review_one(&old));
    thread::sleep(Duration::from_millis(200));
    f.w().pr_head = "bbbbbbb2".into(); // the poll thread sees a new commit while the old review runs
    d.enqueue(f.pr());
    t.join().unwrap();
    assert!(
        f.w().posts.is_empty(),
        "old verdict was posted: {:?}",
        f.w().posts
    );
    let st = e(&en);
    assert_eq!(
        (st.status.as_str(), st.sha.as_str()),
        ("queued", "bbbbbbb2")
    );
    assert!(st.verdict.is_none());
    // and the new commit, once reviewed, gets its own verdict
    d.work_one(&f.pr());
    assert_eq!(f.w().posts, vec!["APPROVE"]);
    assert_eq!(e(&en).verdict_sha, "bbbbbbb2");
}

#[test]
fn out_of_scope_verdict_is_not_posted() {
    let (f, en, _d) = setup();
    f.w().verdict = Some(clean());
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.verdict_sha = HEAD.into();
        e.status = "awaiting_ci".into();
        e.verdict = Some(clean());
    });
    let mut cfg = en.cfg.clone();
    cfg.scope.exclude_repos = vec!["o/r".into()];
    let en2 = Engine {
        cfg,
        store: en.store.clone(),
        me: "me".into(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    };
    en2.finish_safe("o/r#1");
    assert!(f.w().posts.is_empty());
    assert_eq!(e(&en).status, "out_of_scope");
}

/// Regression: channels that keep their "already sent" mark in `extra` (desktop, robots, external plugins) must
/// not announce the same outcome again on every refresh.
#[test]
fn robot_channels_announce_an_outcome_once_across_refreshes() {
    use crate::forge::http::fake;
    let srv = fake::serve(vec![("POST /hook", 200, "ok".into())]);
    let (f, _en, dir) = setup();
    let mut cfg = Config::default();
    cfg.notify.slack.url = format!("{}/hook", srv.url);
    let engine = Engine {
        cfg: cfg.clone(),
        store: Arc::new(Store::open(Some(dir.path().join("state2.json"))).unwrap()),
        me: "me".into(),
        forge: Arc::new(f.clone()),
        reviewer: Arc::new(f.clone()),
        notifier: Arc::new(crate::notify::robot::RobotNotifier::new(
            &cfg,
            crate::notify::robot::Kind::Slack,
        )),
    };
    engine.store.with("o/r#1", |e| {
        e.status = "posted".into();
        e.event = Some("COMMENT".into());
        e.sha = HEAD.into();
        e.title = "t".into();
    });
    for _ in 0..3 {
        engine.refresh("o/r#1", None, false);
    }
    assert_eq!(
        srv.seen.lock().unwrap().len(),
        1,
        "sent once, not once per refresh"
    );
}

/// Regression (code review): after a restart, an entry that could not be re-queued (draft, or the code host was
/// unreachable) stayed "queued", which counts as handled for its commit, so the PR was never reviewed until a new push.
#[test]
fn restart_does_not_strand_entries_that_cannot_be_requeued() {
    let (f, en, _d) = setup();
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "queued".into();
    });
    f.w().pr_draft = true;
    f.w().requested = vec![("o/r".into(), 1)];
    let d = daemon(&en);
    d.requeue_unfinished();
    assert_ne!(e(&en).status, "queued");
    // ready for review again: the next poll picks it up
    f.w().pr_draft = false;
    assert_eq!(en.collect_todo().unwrap().len(), 1);
}

/// Regression (code review): a dry run from the window checked out into the worker's worktree and deleted it.
#[test]
fn dry_runs_use_their_own_workspace() {
    let (f, _en, _d) = setup();
    let mut cfg = Config::default();
    let real = crate::daemon::workspace(&cfg, "github", Arc::new(f.clone()));
    cfg.dry_run = true;
    let dry = crate::daemon::workspace(&cfg, "github", Arc::new(f.clone()));
    assert_ne!(real.root, dry.root);
}

/// Regression (review of the fix): a PR reviewed before and waiting for its new commit, or one only tracked by hand,
/// must also come back after a restart that could not re-queue it (code host unreachable at sign-in, or a draft).
#[test]
fn restart_keeps_reviewed_and_tracked_prs_coming_back() {
    let (f, en, dir) = setup();
    en.store.with("o/r#1", |e| {
        e.sha = HEAD.into();
        e.status = "queued".into();
        e.posted_at = util::now(); // reviewed before: no longer "requested", found again as a recent review
    });
    f.w().pr_draft = true;
    f.w().requested = vec![];
    let d = daemon(&en);
    d.requeue_unfinished();
    f.w().pr_draft = false;
    assert_eq!(en.collect_todo().unwrap().len(), 1, "recalled");

    // tracked by hand only
    en.store.with("o/r#1", |e| {
        e.status = "queued".into();
        e.posted_at = 0.0;
    });
    f.w().pr_draft = true;
    let track = dir.path().join("track.txt");
    std::fs::write(&track, "o/r#1\n").unwrap();
    let en2 = Engine {
        cfg: en.cfg.clone(),
        store: en.store.clone(),
        me: en.me.clone(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    };
    let d2 = Daemon::with_track("github", en2, track);
    d2.requeue_unfinished();
    f.w().pr_draft = false;
    d2.poll_once().unwrap();
    assert_eq!(e(&en).status, "queued", "tracked PR is queued again");
}

/// Regression (code review): the poll rewrote track.txt with the lines it had read, so a PR the window appended while
/// the lookups ran was lost.
#[test]
fn tracking_a_pr_during_a_poll_is_not_lost() {
    let (f, en, dir) = setup();
    f.w().pr_head = "bbbbbbb2".into();
    let track = dir.path().join("track.txt");
    let en2 = Engine {
        cfg: en.cfg.clone(),
        store: en.store.clone(),
        me: en.me.clone(),
        forge: en.forge.clone(),
        reviewer: en.reviewer.clone(),
        notifier: en.notifier.clone(),
    };
    let d = Daemon::with_track("github", en2, track.clone());
    // a poll was interrupted after taking the list aside; meanwhile the window tracked another PR
    std::fs::write(
        track.with_extension("polling"),
        "o/r#1
",
    )
    .unwrap();
    std::fs::write(
        &track, "o/r#2
",
    )
    .unwrap();
    d.poll_once().unwrap();
    let text = std::fs::read_to_string(&track).unwrap();
    assert!(text.contains("o/r#1") && text.contains("o/r#2"), "{text}");
    assert!(!track.with_extension("polling").exists());
}
