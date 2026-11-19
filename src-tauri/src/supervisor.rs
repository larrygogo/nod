//! Runs the worker child process: start, stop, restart after settings change, pause, and automatic restart with
//! back-off when it dies. Also computes the status shown in the tray and window.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::engine::{parse_state, Entry};
use crate::util::now;
use crate::{config, paths, worker};

/// Called with (title, body, url) for each notification line the worker prints.
pub type OnNotify = Arc<dyn Fn(String, String, String) + Send + Sync>;

struct Running {
    child: Child,
    /// Closing it tells the worker to stop (it exits when stdin ends).
    stdin: Option<ChildStdin>,
    started: Instant,
    generation: u64,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    generation: u64,
    /// Why the last worker stopped on its own (shown until the next one runs fine).
    last_exit: Option<String>,
    failures: u32,
    retry_at: Option<Instant>,
}

pub struct Supervisor {
    inner: Mutex<Inner>,
    on_notify: OnNotify,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    /// setup | paused | starting | idle | reviewing | error
    pub state: &'static str,
    /// For "error": a plain explanation, the technical detail, and what the fix button does (see doctor::FIX_*).
    pub title: Option<String>,
    pub error: Option<String>,
    pub action: Option<&'static str>,
    pub last_poll: f64,
    pub active: usize,
}

/// Whether an error is a network hiccup that the next poll may well not have.
pub fn is_network(raw: &str) -> bool {
    let r = raw.to_lowercase();
    [
        "timeout",
        "timed out",
        "eof",
        "connection",
        "dial tcp",
        "tls",
        "network",
        "resolve",
        "no such host",
        "cannot reach github",
    ]
    .iter()
    .any(|k| r.contains(k))
}

/// A plain explanation of a worker error, and the fix the window can offer.
#[cfg(test)]
pub fn explain(lang: &str, raw: &str) -> (String, Option<&'static str>) {
    explain_for(lang, raw, None)
}

/// Like [`explain`], knowing the kind of code host that failed ("github", "gitee", …) when the heartbeat says which
/// account it was. Without it, a sign-in error is attributed by its wording.
pub fn explain_for(lang: &str, raw: &str, kind: Option<&str>) -> (String, Option<&'static str>) {
    use crate::doctor::FIX_SETTINGS;
    let l = |zh: &str, en: &str| if lang == "en" { en } else { zh }.to_string();
    let r = raw.to_lowercase();
    if r.contains("another nod process") {
        (l("另一个 Nod 正在审查（可能是旧版命令行程序），这里先不重复审查", "Another Nod is already reviewing (perhaps the old command-line edition); this one waits"), None)
    } else if r.contains("configuration") || r.contains("must be") || r.contains("toml") {
        (
            l(
                "配置有误，请在设置里改正",
                "The settings are invalid; fix them in Settings",
            ),
            Some(FIX_SETTINGS),
        )
    } else if r.contains(crate::forge::github_auth::SIGNIN_REQUIRED)
        || (kind == Some("github")
            && (r.contains("401") || r.contains("unauthorized") || r.contains("bad credentials")))
    {
        (
            l(
                "GitHub 还没登录或登录已失效，请在「插件」里登录 GitHub",
                "Not signed in to GitHub, or the sign-in expired; sign in under Plugins",
            ),
            Some("plugins"),
        )
    } else if r.contains("401") || r.contains("unauthorized") || r.contains("bad credentials") {
        // a token-based code host (Gitee, Gitea, a third-party plugin): the fix is a new token
        (
            l(
                "代码平台的令牌无效或已过期，请在「插件」里重新填写",
                "A code host's token is invalid or expired; enter a new one under Plugins",
            ),
            Some("plugins"),
        )
    } else if is_network(raw) {
        (
            l(
                "连不上 GitHub（网络或代理问题），会自动重试",
                "Cannot reach GitHub (network or proxy); retrying automatically",
            ),
            None,
        )
    } else if r.starts_with("no poll for") || r.starts_with("no successful poll") {
        (
            l(
                "有一阵子没能检查 GitHub 了",
                "GitHub has not been checked for a while",
            ),
            None,
        )
    } else {
        (l("后台审查出错了", "Reviewing ran into a problem"), None)
    }
}

fn app_file() -> std::path::PathBuf {
    paths::data_dir().join("app.json")
}

pub fn paused() -> bool {
    fs::read_to_string(app_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["paused"].as_bool())
        .unwrap_or(false)
}

fn set_paused_file(p: bool) {
    let _ = fs::create_dir_all(paths::data_dir());
    let _ = fs::write(app_file(), json!({ "paused": p }).to_string());
}

pub fn configured() -> bool {
    paths::config_file().exists()
}

/// The worker's command line: this same program with the internal worker argument.
pub fn worker_command(args: &[&str]) -> Command {
    let exe = std::env::current_exe().unwrap_or_else(|_| "nod".into());
    let mut c = Command::new(exe);
    c.args(args);
    crate::util::hide(&mut c);
    c
}

impl Supervisor {
    pub fn new(on_notify: OnNotify) -> Arc<Supervisor> {
        let s = Arc::new(Supervisor {
            inner: Mutex::new(Inner::default()),
            on_notify,
        });
        let me = s.clone();
        thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || loop {
                me.tick();
                thread::sleep(Duration::from_secs(1));
            })
            .unwrap();
        s
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start the worker if it should run and is not running.
    pub fn start(&self) {
        // Checked under the lock: `set_paused` writes the flag before it stops the worker (which takes the lock), so a
        // start racing with a pause either sees the flag or is stopped right after.
        let mut g = self.lock();
        if !configured() || paused() || g.running.is_some() {
            return;
        }
        g.generation += 1;
        let generation = g.generation;
        let mut cmd = worker_command(&[worker::ARG_WORKER]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(mut child) => {
                crate::info!("worker started pid={}", child.id());
                let stdin = child.stdin.take();
                if let Some(out) = child.stdout.take() {
                    let cb = self.on_notify.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                                if v["type"] == "notify" {
                                    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
                                    cb(s("title"), s("body"), s("url"));
                                }
                            }
                        }
                    });
                }
                if let Some(err) = child.stderr.take() {
                    let me_tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
                    let tail = me_tail.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(err).lines().map_while(Result::ok) {
                            let mut t = tail.lock().unwrap();
                            t.push_back(line);
                            if t.len() > 5 {
                                t.pop_front();
                            }
                        }
                        let text: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
                        STDERR_TAIL
                            .lock()
                            .unwrap()
                            .replace((generation, text.join("\n")));
                    });
                }
                g.running = Some(Running {
                    child,
                    stdin,
                    started: Instant::now(),
                    generation,
                });
                g.retry_at = None;
            }
            Err(e) => {
                g.last_exit = Some(format!("cannot start the worker: {e}"));
                g.failures += 1;
                g.retry_at = Some(Instant::now() + backoff(g.failures));
            }
        }
    }

    /// Stop the worker: close its stdin (it stops its reviews and exits), then kill it if it lingers.
    pub fn stop(&self) {
        let r = self.lock().running.take();
        if let Some(mut r) = r {
            drop(r.stdin.take());
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(5) {
                if matches!(r.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            if matches!(r.child.try_wait(), Ok(None)) {
                crate::util::kill_tree(r.child.id());
                let _ = r.child.kill();
                let _ = r.child.wait();
            }
            crate::info!("worker stopped");
        }
        let mut g = self.lock();
        g.retry_at = None;
        g.failures = 0;
        g.last_exit = None;
    }

    /// Apply new settings.
    pub fn restart(&self) {
        self.stop();
        self.start();
    }

    pub fn set_paused(&self, p: bool) {
        set_paused_file(p);
        if p {
            self.stop();
        } else {
            self.start();
        }
    }

    /// Notice a worker that died, and restart it after a back-off.
    fn tick(&self) {
        let mut g = self.lock();
        if let Some(r) = g.running.as_mut() {
            if let Ok(Some(st)) = r.child.try_wait() {
                let ran = r.started.elapsed();
                let gen = r.generation;
                let child_pid = r.child.id();
                g.running = None;
                // give the stderr reader a moment to finish
                drop(g);
                thread::sleep(Duration::from_millis(200));
                let tail = STDERR_TAIL
                    .lock()
                    .unwrap()
                    .take()
                    .filter(|(x, _)| *x == gen)
                    .map(|(_, t)| t)
                    .unwrap_or_default();
                let mut g = self.lock();
                // The worker records its fatal error in heartbeat.json in full; stderr only has its last lines.
                let recorded = read_heartbeat()
                    .filter(|v| v["pid"].as_u64() == Some(child_pid as u64) && v["ok"] == false)
                    .and_then(|v| v["error"].as_str().map(str::to_string));
                let why = match recorded {
                    Some(e) => e,
                    None if tail.trim().is_empty() => format!("the worker exited ({st})"),
                    None => tail.trim().to_string(),
                };
                crate::warn!("worker exited after {}s: {why}", ran.as_secs());
                g.failures = if ran > Duration::from_secs(600) {
                    1
                } else {
                    g.failures + 1
                };
                g.last_exit = Some(why);
                g.retry_at = Some(Instant::now() + backoff(g.failures));
                return;
            }
            if r.started.elapsed() > Duration::from_secs(120) {
                g.failures = 0;
            }
            return;
        }
        if g.retry_at.is_some_and(|t| Instant::now() >= t) {
            g.retry_at = None;
            drop(g);
            self.start();
        }
    }

    pub fn status(&self) -> Status {
        let entries = load_entries();
        let active = entries
            .iter()
            .filter(|(_, e)| matches!(e.status.as_str(), "queued" | "reviewing"))
            .count();
        let hb = read_heartbeat();
        let last_poll = hb.as_ref().and_then(|v| v["at"].as_f64()).unwrap_or(0.0);
        let g = self.lock();
        // read once: this runs every two seconds while the window is open
        let cfg = config::load_from(&paths::config_file(), false).ok();
        let lang = cfg.as_ref().map(|c| c.language.clone()).unwrap_or_default();
        // the account whose error is shown, and so the kind of code host to blame
        let kind = hb
            .as_ref()
            .and_then(|v| v["account"].as_str())
            .and_then(|id| {
                cfg.as_ref()?
                    .accounts()
                    .into_iter()
                    .find(|a| a.id == id)
                    .map(|a| a.kind)
            });
        // `from_heartbeat`: the error is the one the heartbeat records for that account, so its kind applies
        let base_with = |state, error: Option<String>, from_heartbeat: bool| {
            let (title, action) = match &error {
                Some(e) => {
                    let (t, a) = explain_for(&lang, e, kind.as_deref().filter(|_| from_heartbeat));
                    (Some(t), a)
                }
                None => (None, None),
            };
            Status {
                state,
                title,
                error,
                action,
                last_poll,
                active,
            }
        };
        let base = |state, error: Option<String>| base_with(state, error, false);
        if !configured() {
            return base("setup", None);
        }
        if paused() {
            return base("paused", None);
        }
        let Some(r) = g.running.as_ref() else {
            return base(
                "error",
                Some(
                    g.last_exit
                        .clone()
                        .unwrap_or_else(|| "the worker is not running".into()),
                ),
            );
        };
        let fresh = last_poll > now() - r.started.elapsed().as_secs_f64();
        if let Some(v) = hb.as_ref().filter(|_| fresh) {
            let err = v["error"].as_str().unwrap_or_default();
            let fails = v["fails"].as_u64().unwrap_or(1);
            // one network hiccup is not worth a red icon; anything else, or a second failure, is
            if v["ok"] == false && (fails >= 2 || !is_network(err)) {
                return base_with("error", Some(err.to_string()), true);
            }
        }
        let poll = cfg.as_ref().map(|c| c.daemon.poll_seconds).unwrap_or(30.0);
        if !fresh {
            // no poll since this worker started
            return if r.started.elapsed().as_secs_f64() > poll * 3.0 + 60.0 {
                base("error", Some("no successful poll yet".into()))
            } else {
                base("starting", None)
            };
        }
        if now() - last_poll > poll * 3.0 + 120.0 {
            return base(
                "error",
                Some(format!(
                    "no poll for {} minutes",
                    ((now() - last_poll) / 60.0) as u64
                )),
            );
        }
        base(if active > 0 { "reviewing" } else { "idle" }, None)
    }
}

static STDERR_TAIL: Mutex<Option<(u64, String)>> = Mutex::new(None);

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.min(6)).min(300))
}

fn read_heartbeat() -> Option<Value> {
    fs::read_to_string(paths::data_dir().join("heartbeat.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn read_state(path: &std::path::Path) -> Vec<(String, Entry)> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| parse_state(&s).ok())
        .map(|d| d.prs.into_iter().collect())
        .unwrap_or_default()
}

/// Every account's PRs: ((account id, kind), key, entry).
pub fn load_all() -> Vec<((String, String), String, Entry)> {
    let cfg = config::load_from(&paths::config_file(), false).unwrap_or_default();
    let mut out = vec![];
    for acc in cfg.accounts() {
        for (k, e) in read_state(&crate::daemon::state_file(&acc.id)) {
            out.push(((acc.id.clone(), acc.kind.clone()), k, e));
        }
    }
    out
}

pub fn load_entries() -> Vec<(String, Entry)> {
    load_all().into_iter().map(|(_, k, e)| (k, e)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_errors() {
        let (t, a) = explain(
            "en",
            "GET /repos/o/r/pulls/1: Connection Failed: tls connection init failed: TLS handshake timeout",
        );
        assert!(t.starts_with("Cannot reach GitHub"));
        assert_eq!(a, None);
        let (t, a) = explain(
            "zh-CN",
            "github_signin_required: not signed in to GitHub; sign in on the GitHub page in Nod",
        );
        assert!(t.contains("登录 GitHub"), "{t}");
        assert_eq!(a, Some("plugins"));
        assert_eq!(
            explain(
                "zh-CN",
                "gitee-work: GET /user: HTTP 401: {\"message\":\"401 Unauthorized\"}"
            )
            .1,
            Some("plugins"),
            "a Gitee token problem is not a GitHub sign-in"
        );
        assert_eq!(
            explain(
                "en",
                "gitea: GET /user: HTTP 401: token is not valid for this high-security instance"
            )
            .1,
            Some("plugins")
        );
        // the account kind decides, whatever the message says
        assert_eq!(
            explain_for(
                "en",
                "gitee-work: HTTP 401 for owner/github-sync",
                Some("gitee")
            )
            .1,
            Some("plugins")
        );
        assert!(explain_for("en", "HTTP 401: Unauthorized", Some("github"))
            .0
            .contains("sign in under Plugins"));
        assert!(
            explain("en", "another Nod process is already reviewing (pid 3)")
                .0
                .starts_with("Another Nod")
        );
        assert_eq!(
            explain("en", "notify.webhook.url must be an http(s) URL").1,
            Some(crate::doctor::FIX_SETTINGS)
        );
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(10));
        assert_eq!(backoff(3), Duration::from_secs(40));
        assert_eq!(backoff(50), Duration::from_secs(300));
    }
}
