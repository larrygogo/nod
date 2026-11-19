//! The desktop app: window, tray, commands the window calls, and start-up.

use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::config::{self, Config};
use crate::engine::Entry;
use crate::i18n::reason_text;
use crate::supervisor::{self, Status, Supervisor};
use crate::{doctor, legacy, migrate, osnotify, paths, secrets, tray, util, worker};

/// Started at login: stay in the tray.
pub const ARG_HIDDEN: &str = "--hidden";

pub struct AppState {
    sup: Arc<Supervisor>,
    /// What the start-up clean-up of the command-line edition did (shown once in the window).
    legacy: Mutex<Vec<String>>,
}

fn lang() -> String {
    config::load_from(&paths::config_file(), false)
        .map(|c| c.language)
        .unwrap_or_else(|_| "zh-CN".into())
}

/// Show the window, creating it if needed. The window is destroyed when closed (not hidden), so the web view's memory
/// (well over 100 MB) is given back while Nod sits in the tray.
pub fn show_main(app: &AppHandle) {
    let w = match app.get_webview_window("main") {
        Some(w) => w,
        None => {
            let Some(conf) = app.config().app.windows.iter().find(|w| w.label == "main") else {
                return;
            };
            match tauri::WebviewWindowBuilder::from_config(app, conf).and_then(|b| b.build()) {
                Ok(w) => w,
                Err(e) => {
                    crate::error!("cannot open the window: {e}");
                    return;
                }
            }
        }
    };
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

fn quit(app: &AppHandle) {
    app.state::<AppState>().sup.stop();
    app.exit(0);
}

pub fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => show_main(app),
        "pause" => {
            let sup = app.state::<AppState>().sup.clone();
            let p = !supervisor::paused();
            std::thread::spawn(move || sup.set_paused(p));
        }
        "quit" => quit(app),
        id if id.starts_with("recent:") => {
            // recent:<index>:<url>
            if let Some(url) = id.splitn(3, ':').nth(2) {
                let _ = tauri_plugin_opener::open_url(url, None::<&str>);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- data for the window

#[derive(Serialize)]
struct Row {
    /// Code-host account id and its kind (github, gitee, …).
    account: String,
    platform: String,
    /// Engine that produced the verdict.
    engine: String,
    key: String,
    title: String,
    url: String,
    author: String,
    status: String,
    event: Option<String>,
    summary: Option<String>,
    findings: Vec<Value>,
    reasons: Vec<String>,
    review_url: Option<String>,
    error: Option<String>,
    at: f64,
}

fn row(lang: &str, acc: &(String, String), key: &str, e: &Entry) -> Row {
    Row {
        account: acc.0.clone(),
        platform: acc.1.clone(),
        engine: e
            .verdict
            .as_ref()
            .map(|v| v.meta.engine.clone())
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| if e.verdict.is_some() { "claude".into() } else { String::new() }),
        key: key.to_string(),
        title: e.title.clone(),
        url: e.url.clone(),
        author: e.author.clone(),
        status: e.status.clone(),
        event: e.event.clone(),
        summary: e.verdict.as_ref().map(|v| v.summary.clone()),
        findings: e
            .verdict
            .as_ref()
            .map(|v| {
                v.findings
                    .iter()
                    .map(|f| {
                        json!({"severity": f.severity, "title": f.title, "path": f.path, "line": f.line,
                               "introduced_by_pr": f.introduced_by_pr})
                    })
                    .collect()
            })
            .unwrap_or_default(),
        reasons: e.reasons.iter().map(|r| reason_text(lang, r)).collect(),
        review_url: e.review_url.clone(),
        error: e.error.clone(),
        at: e.posted_at.max(e.queued_at).max(e.direct_at),
    }
}

#[derive(Serialize)]
struct Overview {
    version: &'static str,
    configured: bool,
    status: Status,
    rows: Vec<Row>,
    legacy: Vec<String>,
    old_install: bool,
    autostart: bool,
}

#[tauri::command]
fn overview(app: AppHandle, state: State<AppState>) -> Overview {
    let lang = lang();
    let mut rows: Vec<Row> = supervisor::load_all()
        .iter()
        .filter(|(_, _, e)| !e.title.is_empty())
        .map(|(acc, k, e)| row(&lang, acc, k, e))
        .collect();
    rows.sort_by(|a, b| b.at.total_cmp(&a.at));
    rows.truncate(100);
    let configured = supervisor::configured();
    Overview {
        version: env!("CARGO_PKG_VERSION"),
        configured,
        status: state.sup.status(),
        rows,
        legacy: state.legacy.lock().unwrap().clone(),
        old_install: !configured && migrate::old_install_found(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
    }
}

/// The saved configuration for the window, without secrets (`secrets` says which ones are set). A file that cannot be
/// read is reported next to the defaults instead of failing, so the window can still open Settings and repair it.
#[tauri::command]
fn get_config() -> Value {
    match config::load_from(&paths::config_file(), false) {
        Ok(c) => {
            json!({"config": secrets::redacted(&c), "secrets": secrets::present(&c), "error": null})
        }
        Err(e) => json!({"config": Config::default(), "secrets": {}, "error": format!("{e:#}")}),
    }
}

/// A configuration from the window, with the secrets it was not shown filled back in from the keychain (except the
/// ones the user cleared, keychain names in `clear`).
fn from_window(v: Value, clear: &[String]) -> Result<Config, String> {
    let mut c: Config = serde_json::from_value(v).map_err(|e| e.to_string())?;
    let current = config::load_from(&paths::config_file(), false).unwrap_or_default();
    secrets::keep_unchanged(&mut c, &current, clear);
    Ok(c)
}

/// The plugin catalog in `lang` (the window's current language, which may not be saved yet).
#[tauri::command]
fn catalog(lang: Option<String>) -> Vec<crate::plugins::catalog::Plugin> {
    crate::plugins::catalog(&lang.unwrap_or_else(self::lang))
}

/// Keep a copy of a configuration file that cannot be read before it is replaced, so nothing is lost silently.
fn back_up_broken_config() {
    let p = paths::config_file();
    if p.exists() && config::load_from(&p, false).is_err() {
        let to = p.with_extension(format!("toml.broken-{}", util::now() as u64));
        if std::fs::rename(&p, &to).is_ok() {
            crate::warn!("unreadable configuration kept as {}", to.display());
        }
    }
}

fn parse_config(v: Value, clear: &[String]) -> Result<Config, String> {
    from_window(v, clear)?
        .validate()
        .map_err(|e| format!("{e:#}"))
}

/// Save: secrets go to the keychain, everything else to config.toml; then reviewing restarts with the new settings.
/// Returns the secret fields that had to stay in the file because the keychain is unavailable.
#[tauri::command]
async fn save_config(
    state: State<'_, AppState>,
    cfg: Value,
    clear: Option<Vec<String>>,
) -> Result<Vec<String>, String> {
    let c = parse_config(cfg, &clear.unwrap_or_default())?;
    let previous = config::load_from(&paths::config_file(), false).unwrap_or_default();
    back_up_broken_config();
    let (disk, kept) = secrets::stash(&c, &previous);
    config::save(&disk).map_err(|e| format!("{e:#}"))?;
    // a removed GitHub account takes its sign-in with it (re-adding the same id must not revive it)
    let ids: Vec<String> = c.accounts().into_iter().map(|a| a.id).collect();
    for a in previous.accounts() {
        if a.kind == "github" && !ids.contains(&a.id) {
            crate::forge::github_auth::forget(&a.id);
        }
    }
    let sup = state.sup.clone();
    tauri::async_runtime::spawn_blocking(move || sup.restart())
        .await
        .map_err(|e| e.to_string())?;
    Ok(kept)
}

/// Check everything the configuration uses (git, each enabled account, each engine).
#[tauri::command]
async fn check_tools(
    cfg: Value,
    live: bool,
    clear: Option<Vec<String>>,
) -> Result<Vec<doctor::Check>, String> {
    let c = from_window(cfg, &clear.unwrap_or_default())?;
    tauri::async_runtime::spawn_blocking(move || doctor::all(&c, live))
        .await
        .map_err(|e| e.to_string())
}

/// The GitHub account the window is editing (possibly not saved yet): id, address and settings are all sign-in needs.
fn github_account(account: Value) -> Result<crate::config::ForgeAccount, String> {
    let a: crate::config::ForgeAccount =
        serde_json::from_value(account).map_err(|e| format!("bad account: {e}"))?;
    if a.kind != "github" {
        return Err(format!("{} is not a GitHub account", a.id));
    }
    Ok(a)
}

/// Who is signed in on a GitHub account, if anyone.
#[derive(Serialize)]
struct GithubSession {
    signed_in: bool,
    login: String,
    /// The account has an OAuth App client ID (its own, or Nod's).
    has_client_id: bool,
}

#[tauri::command]
fn github_session(account: Value) -> Result<GithubSession, String> {
    let a = github_account(account)?;
    let cid = crate::forge::github::client_id(&a);
    let hosts = crate::forge::github_auth::Hosts::new(&a.url);
    // tokens for another address or App, or ones that ran out completely, do not count as signed in
    let t = crate::forge::github_auth::load(&a.id)
        .filter(|t| t.fits(&hosts, &cid) && !t.dead(crate::util::now()));
    Ok(GithubSession {
        signed_in: t.is_some(),
        login: t.map(|t| t.login).unwrap_or_default(),
        has_client_id: !cid.is_empty(),
    })
}

/// Start the device flow: the window shows the code and opens the verification page.
#[tauri::command]
async fn github_login_start(
    account: Value,
) -> Result<crate::forge::github_auth::DeviceCode, String> {
    let a = github_account(account)?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::forge::github_auth::start(
            &crate::forge::github_auth::Hosts::new(&a.url),
            &crate::forge::github::client_id(&a),
        )
        .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One poll of the device flow. `state`: pending | slow_down (wait `interval` from now on) | done | expired | denied
/// | failed (`message` says why).
#[derive(Serialize)]
struct GithubPoll {
    state: String,
    interval: u64,
    login: String,
    message: String,
}

#[tauri::command]
async fn github_login_poll(
    state: State<'_, AppState>,
    account: Value,
    device_code: String,
) -> Result<GithubPoll, String> {
    use crate::forge::github_auth::{self as auth, Poll};
    let a = github_account(account)?;
    let sup = state.sup.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let hosts = auth::Hosts::new(&a.url);
        let cid = crate::forge::github::client_id(&a);
        let p = |state: &str| GithubPoll {
            state: state.into(),
            interval: 0,
            login: String::new(),
            message: String::new(),
        };
        Ok(
            match auth::poll(&hosts, &cid, &device_code).map_err(|e| format!("{e:#}"))? {
                Poll::Pending => p("pending"),
                Poll::SlowDown(i) => GithubPoll {
                    interval: i,
                    ..p("slow_down")
                },
                Poll::Failed(m) if m == "expired" || m == "denied" => p(&m),
                Poll::Failed(m) => GithubPoll {
                    message: m,
                    ..p("failed")
                },
                Poll::Done(t) => {
                    let mut t = t.bind(&hosts, &cid);
                    auth::save(&a.id, &t)
                        .map_err(|e| format!("could not store the sign-in: {e:#}"))?;
                    // who signed in: shown on the plugin page and in the self-check
                    let gh = crate::forge::github::Gh::new(&a);
                    gh.session.invalidate();
                    if let Ok(me) = crate::forge::Forge::current_user(&gh) {
                        t.login = me;
                        let _ = auth::save(&a.id, &t);
                    }
                    crate::info!("github: account {} signed in as {}", a.id, t.login);
                    // start reviewing as this user now: the worker may be backing off after "not signed in", and
                    // after a switch its idea of "me" belongs to the previous account
                    sup.restart();
                    GithubPoll {
                        login: t.login,
                        ..p("done")
                    }
                }
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn github_logout(state: State<'_, AppState>, account: Value) -> Result<(), String> {
    let a = github_account(account)?;
    crate::forge::github_auth::forget(&a.id);
    crate::info!("github: account {} signed out", a.id);
    // the worker must stop acting as this user right away, not when its cached token runs out
    let sup = state.sup.clone();
    tauri::async_runtime::spawn_blocking(move || sup.restart())
        .await
        .map_err(|e| e.to_string())
}

/// Check one plugin: a code-host account (`account` = its id in `cfg.forges`, or the implicit GitHub account) or a
/// review engine.
#[tauri::command]
async fn check_plugin(
    cfg: Value,
    kind: String,
    id: String,
    clear: Option<Vec<String>>,
) -> Result<Vec<doctor::Check>, String> {
    let c = from_window(cfg, &clear.unwrap_or_default())?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut ctx = doctor::Ctx::new(&c.language);
        match kind.as_str() {
            "forge" => {
                let acc = c
                    .accounts()
                    .into_iter()
                    .find(|a| a.id == id)
                    .ok_or_else(|| format!("no account {id}"))?;
                doctor::forge_check(&mut ctx, &acc, &c);
            }
            "engine" => doctor::engine_check(&mut ctx, &c, &id, true),
            _ => return Err(format!("cannot check {kind}")),
        }
        Ok(ctx.done())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Send a test message through one channel.
#[tauri::command]
async fn test_notification(
    cfg: Value,
    channel: String,
    clear: Option<Vec<String>>,
) -> Result<String, String> {
    let c = from_window(cfg, &clear.unwrap_or_default())?;
    if channel == "system" {
        osnotify::show(
            &crate::i18n::t(&c.language, "card.test_title"),
            &crate::i18n::t(&c.language, "sys.test_body"),
            "",
        );
        return Ok("sent".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        crate::notify::make_one(&c, "", &channel)
            .and_then(|n| n.test())
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What a fix button does.
#[derive(Debug, PartialEq)]
pub enum Fix {
    /// Run this command line in a new terminal window, with these extra environment variables.
    Terminal(String, Vec<(String, String)>),
    Url(String),
}

/// Run a command in a new terminal window, so the user can sign in interactively.
fn open_terminal(cmdline: &str, env: &[(String, String)]) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // A console of its own (not `start`, which takes a quoted first argument as the title and an unquoted one as
        // the program to run). raw_arg: cmd parses its command line itself; Rust's quoting would get in the way.
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        std::process::Command::new("cmd")
            .raw_arg(format!("/k {cmdline}"))
            .envs(env.iter().map(|(k, v)| (k, v)))
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        // Terminal.app does not inherit our environment: set the variables in the script itself.
        let prefix: String = env
            .iter()
            .map(|(k, v)| format!("{k}={} ", quote_arg(v)))
            .collect();
        let esc = format!("{prefix}{cmdline}")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        std::process::Command::new("osascript")
            .args([
                "-e",
                &format!("tell application \"Terminal\" to do script \"{esc}\""),
                "-e",
                "tell application \"Terminal\" to activate",
            ])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let script = format!("{cmdline}; exec \"${{SHELL:-sh}}\"");
        let tries: [&[&str]; 5] = [
            &["x-terminal-emulator", "-e", "sh", "-c"],
            &["gnome-terminal", "--", "sh", "-c"],
            &["konsole", "-e", "sh", "-c"],
            &["xfce4-terminal", "-x", "sh", "-c"],
            &["xterm", "-e", "sh", "-c"],
        ];
        for t in tries {
            if util::which(t[0]).is_some() {
                return std::process::Command::new(t[0])
                    .args(&t[1..])
                    .arg(&script)
                    .envs(env.iter().map(|(k, v)| (k, v)))
                    .spawn()
                    .map(|_| ());
            }
        }
        Err(std::io::Error::other("no terminal emulator found"))
    }
}

fn quote_arg(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '"', '\'', '&', '(', ')', ';', '$', '|', '<', '>']) {
        s.to_string()
    } else if cfg!(windows) {
        format!("\"{}\"", s.replace('"', ""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The terminal command (or web page) for a fix action.
pub fn fix_target(action: &str, cfg: &Config) -> Option<Fix> {
    let zh = cfg.language != "en";
    Some(match action {
        doctor::FIX_CLAUDE_LOGIN => {
            let dir = crate::review::expand_path(&cfg.review.claude_config_dir);
            let env = if dir.is_empty() {
                vec![]
            } else {
                vec![("CLAUDE_CONFIG_DIR".to_string(), dir)]
            };
            Fix::Terminal(quote_arg(&cfg.review.claude), env)
        }
        doctor::FIX_CODEX_LOGIN => Fix::Terminal(
            format!("{} login", quote_arg(&cfg.review.codex.exe)),
            vec![],
        ),
        doctor::FIX_GEMINI_LOGIN => Fix::Terminal(quote_arg(&cfg.review.gemini.exe), vec![]),
        doctor::FIX_INSTALL_CODEX => Fix::Url("https://developers.openai.com/codex/cli".into()),
        doctor::FIX_INSTALL_GEMINI => {
            Fix::Url("https://geminicli.com/docs/get-started/installation".into())
        }
        doctor::FIX_INSTALL_GIT => Fix::Url("https://git-scm.com/downloads".into()),
        doctor::FIX_INSTALL_CLAUDE => {
            Fix::Url("https://docs.claude.com/en/docs/claude-code/setup".into())
        }
        doctor::FIX_FEISHU_DOCS => Fix::Url(format!(
            "https://github.com/larrygogo/nod/blob/main/docs/feishu-setup{}.md",
            if zh { ".zh-CN" } else { "" }
        )),
        _ => return None,
    })
}

/// Start a fix: one of a fixed set of commands, completed only from the saved configuration (the claude path).
#[tauri::command]
fn fix(action: String) -> Result<(), String> {
    let c = config::load_from(&paths::config_file(), false).unwrap_or_default();
    match fix_target(&action, &c) {
        Some(Fix::Terminal(cmd, env)) => open_terminal(&cmd, &env).map_err(|e| e.to_string()),
        Some(Fix::Url(url)) => {
            tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())
        }
        None => Err(format!("unknown action {action}")),
    }
}

#[tauri::command]
fn set_paused(state: State<AppState>, paused: bool) {
    let sup = state.sup.clone();
    std::thread::spawn(move || sup.set_paused(paused));
}

#[tauri::command]
async fn restart_worker(state: State<'_, AppState>) -> Result<(), String> {
    let sup = state.sup.clone();
    tauri::async_runtime::spawn_blocking(move || sup.restart())
        .await
        .map_err(|e| e.to_string())
}

/// Whether this copy runs against scratch directories (development); it must not touch the real installation.
pub fn isolated() -> bool {
    ["NOD_HOME", "NOD_CONFIG_DIR"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
}

#[tauri::command]
fn set_autostart(app: AppHandle, on: bool) -> Result<(), String> {
    if isolated() {
        // the login entry is shared with the installed copy: turning it on would start this copy against the real
        // installation, turning it off would remove the installed copy's entry
        return Err(
            "NOD_HOME / NOD_CONFIG_DIR are set: not changing the start-at-sign-in entry".into(),
        );
    }
    let a = app.autolaunch();
    if on { a.enable() } else { a.disable() }.map_err(|e| e.to_string())
}

/// Review a PR nobody asked you to review (a link handed over in chat): checked here so a mistake shows at once,
/// then queued for the account's worker, which wakes up for it and applies the usual approval rules.
#[tauri::command]
async fn track_pr(pr: String) -> Result<String, String> {
    let c = config::load_from(&paths::config_file(), false).map_err(|e| format!("{e:#}"))?;
    let (acc, r, n) = crate::forge::resolve(&c, &pr).map_err(|e| format!("{e:#}"))?;
    let key = format!("{r}#{n}");
    let (cc, acc2, r2) = (c.clone(), acc.clone(), r.clone());
    let zh = c.language != "en";
    let msg = move |z: &str, e: &str| if zh { z.to_string() } else { e.to_string() };
    let problem =
        tauri::async_runtime::spawn_blocking(move || -> Result<Option<String>, String> {
            let forge = crate::forge::make(&acc2, &cc).map_err(|e| format!("{e:#}"))?;
            let p = forge.fetch_pr(&r2, n).map_err(|e| format!("{e:#}"))?;
            let me = forge.current_user().unwrap_or_default();
            let s = &cc.scope;
            let in_scope = crate::config::selected(&s.repos, &s.exclude_repos, |pat| {
                crate::config::repo_matches(pat, &r2, &me)
            });
            Ok(if p.state != "open" {
                Some(if p.state == "merged" {
                    msg("这个 PR 已经合并了", "This PR is already merged")
                } else {
                    msg("这个 PR 已经关闭了", "This PR is closed")
                })
            } else if p.draft {
                Some(msg(
                    "这个 PR 还是草稿，转为正式 PR 后再审查",
                    "This PR is a draft; review it once it is ready for review",
                ))
            } else if !in_scope {
                Some(msg(
                "这个仓库不在设置里的审查范围内，先在「设置 → 范围」里加上它",
                "This repository is outside the review scope; add it under Settings → Scope first",
            ))
            } else {
                None
            })
        })
        .await
        .map_err(|e| e.to_string())??;
    if let Some(p) = problem {
        return Err(p);
    }
    let p = crate::daemon::track_file(&acc.id);
    let _ = std::fs::create_dir_all(paths::data_dir());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{key}").map_err(|e| e.to_string())?;
    // wake the account's worker now instead of at its next poll
    let _ = std::fs::write(crate::daemon::wake_file(&p), "");
    Ok(key)
}

#[tauri::command]
async fn dry_run(pr: String) -> Result<Value, String> {
    let c = config::load_from(&paths::config_file(), false).map_err(|e| format!("{e:#}"))?;
    let (acc, r, n) = crate::forge::resolve(&c, &pr).map_err(|e| format!("{e:#}"))?;
    let key = format!("{r}#{n}");
    let text = pr.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = supervisor::worker_command(&[worker::ARG_DRY_RUN, &text]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        // the child stops when its stdin closes: keep it open until it is done
        let _keep = child.stdin.take();
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let v = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["type"] == "result")
            .ok_or_else(|| {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if err.is_empty() {
                    "the review did not produce a result".to_string()
                } else {
                    err
                }
            })?;
        let e: Entry = serde_json::from_value(v["entry"].clone()).unwrap_or_default();
        serde_json::to_value(row(&lang(), &(acc.id.clone(), acc.kind.clone()), &key, &e))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn import_old(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let sup = state.sup.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let msgs = migrate::run(None).map_err(|e| format!("{e:#}"))?;
        sup.restart();
        Ok(msgs)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn open_location(which: String) -> Result<(), String> {
    let p = match which.as_str() {
        "config" => paths::config_dir(),
        "plugins" => crate::plugins::external::plugins_dir(),
        "log" => paths::data_dir().join("autoreview.log"),
        _ => paths::data_dir(),
    };
    if which == "log" && p.exists() {
        tauri_plugin_opener::reveal_item_in_dir(&p).map_err(|e| e.to_string())
    } else {
        let _ = std::fs::create_dir_all(&p);
        tauri_plugin_opener::open_path(&p, None::<&str>).map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only web links can be opened".into());
    }
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn locations() -> Value {
    json!({
        "config": paths::config_file(),
        "data": paths::data_dir(),
        "log": paths::data_dir().join("autoreview.log"),
    })
}

// ---------------------------------------------------------------- start-up

/// Apps started from Finder, the Dock or at login get a minimal PATH, so `git` and `claude` installed by
/// Homebrew, npm or the Claude installer would not be found. Take PATH from the user's login shell instead.
#[cfg(unix)]
fn adopt_login_shell_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let out = util::run_raw(
        &[
            &shell,
            "-ilc",
            "printf '__RW_PATH__%s__RW_PATH__' \"$PATH\"",
        ],
        util::RunOpts {
            timeout: Duration::from_secs(5),
            ..Default::default()
        },
    );
    if let Ok(o) = out {
        if let Some(p) = o
            .stdout
            .split("__RW_PATH__")
            .nth(1)
            .filter(|p| !p.is_empty())
        {
            std::env::set_var("PATH", p);
        }
    }
}

/// The five latest reviews for the tray menu: a mark and the title, with the review's (or PR's) link.
fn recent_items(lang: &str) -> Vec<(String, String)> {
    let mut rows: Vec<Row> = supervisor::load_all()
        .iter()
        .filter(|(_, _, e)| !e.title.is_empty())
        .map(|(acc, k, e)| row(lang, acc, k, e))
        .collect();
    rows.sort_by(|a, b| b.at.total_cmp(&a.at));
    rows.into_iter()
        .take(5)
        .map(|r| {
            let mark = match (r.status.as_str(), r.event.as_deref()) {
                ("direct_approved", _) | (_, Some("APPROVE")) => "✓",
                ("failed", _) => "✕",
                (_, Some(_)) => "•",
                _ => "…",
            };
            (
                format!("{mark}  {}", r.title),
                r.review_url.unwrap_or(r.url),
            )
        })
        .filter(|(_, url)| url.starts_with("https://") || url.starts_with("http://"))
        .collect()
}

fn status_loop(app: AppHandle, t: tray::Tray) {
    std::thread::Builder::new()
        .name("status".into())
        .spawn(move || {
            let mut last: Option<(Status, String)> = None;
            let mut last_recent: Option<(Vec<(String, String)>, String)> = None;
            loop {
                let s = app.state::<AppState>().sup.status();
                let l = lang();
                let cur = (s.clone(), l.clone());
                if last.as_ref() != Some(&cur) {
                    t.update(&l, &s);
                    let _ = app.emit("status", &s);
                    last = Some(cur);
                }
                let recent = recent_items(&l);
                if last_recent.as_ref() != Some(&(recent.clone(), l.clone())) {
                    t.set_recent(&l, &recent);
                    last_recent = Some((recent, l.clone()));
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .unwrap();
}

pub fn run(hidden: bool) {
    #[cfg(unix)]
    adopt_login_shell_path();
    util::setup_logging(false, true);

    let mut builder = tauri::Builder::default();
    // A side-by-side development instance (scratch directories) must not hand itself over to the installed app.
    // Its reviewing still cannot overlap: each data directory has its own lock.
    if !isolated() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }));
    }
    let app = builder
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![ARG_HIDDEN]),
        ))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            overview,
            get_config,
            save_config,
            check_tools,
            check_plugin,
            github_session,
            github_login_start,
            github_login_poll,
            github_logout,
            catalog,
            test_notification,
            fix,
            set_paused,
            restart_worker,
            set_autostart,
            track_pr,
            dry_run,
            import_old,
            open_location,
            open_url,
            locations,
        ])
        .setup(move |app| {
            // The app used to be called revwarden: carry its settings, records and secrets over before anything
            // reads them (the window decides between setup and overview from the configuration).
            let (renamed, old_autostart) = crate::rename::run();
            for m in &renamed {
                crate::info!("rename: {m}");
            }
            if old_autostart {
                if let Err(e) = app.handle().autolaunch().enable() {
                    crate::warn!("rename: could not turn on start at sign-in: {e}");
                }
            }
            let sup = Supervisor::new(Arc::new(|title, body, url| {
                osnotify::show(&title, &body, &url)
            }));
            app.manage(AppState {
                sup: sup.clone(),
                legacy: Mutex::new(vec![]),
            });
            let handle = app.handle().clone();
            let t = tray::build(&handle, &lang())?;
            status_loop(handle.clone(), t);
            // Remove the command-line edition's service (it holds the lock), then start reviewing.
            std::thread::spawn(move || {
                match legacy::remove_service() {
                    Ok(msgs) => {
                        // The old service started reviewing at sign-in; users who upgrade skip the setup (they are
                        // configured), so keep that behaviour by turning start-at-sign-in on in its place.
                        if !msgs.is_empty() && !isolated() {
                            match handle.autolaunch().enable() {
                                Ok(()) => crate::info!("legacy: start at sign-in turned on in place of the old service"),
                                Err(e) => crate::warn!("legacy: could not turn on start at sign-in: {e}"),
                            }
                        }
                        *handle.state::<AppState>().legacy.lock().unwrap() = msgs;
                    }
                    Err(e) => crate::warn!("legacy clean-up: {e:#}"),
                }
                sup.start();
            });
            if !hidden || !supervisor::configured() {
                show_main(app.handle());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Nod");

    app.run(|app, ev| match ev {
        RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            // the window was closed: keep reviewing in the tray (Quit in the tray menu exits with a code)
            api.prevent_exit();
        }
        RunEvent::Exit => app.state::<AppState>().sup.stop(),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_actions() {
        let mut c = Config::default();
        assert_eq!(
            fix_target(doctor::FIX_CLAUDE_LOGIN, &c),
            Some(Fix::Terminal("claude".into(), vec![]))
        );
        c.review.claude = "/opt/my tools/claude".into();
        c.review.claude_config_dir = "/p/alt".into();
        let Some(Fix::Terminal(cmd, env)) = fix_target(doctor::FIX_CLAUDE_LOGIN, &c) else {
            panic!("terminal expected")
        };
        assert!(cmd.starts_with(['"', '\'']) && cmd.contains("my tools"));
        assert_eq!(
            env,
            vec![("CLAUDE_CONFIG_DIR".to_string(), "/p/alt".to_string())]
        );
        assert!(
            matches!(fix_target(doctor::FIX_FEISHU_DOCS, &c), Some(Fix::Url(u)) if u.ends_with("feishu-setup.zh-CN.md"))
        );
        assert!(fix_target("nope", &c).is_none());
    }

    /// The Windows fix must open a console running exactly the command (regression: `start Nod …` ran a
    /// program called Nod instead).
    #[cfg(windows)]
    #[test]
    fn windows_terminal_runs_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ran.txt");
        let cmd = format!("echo %RW_TEST%> \"{}\" & exit", out.display());
        open_terminal(&cmd, &[("RW_TEST".into(), "hello there".into())]).unwrap();
        for _ in 0..100 {
            if out.exists() && std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        assert_eq!(text.trim(), "hello there");
    }

    #[test]
    fn rows_translate_reasons() {
        let e = Entry {
            title: "t".into(),
            reasons: vec!["ci_failed".into()],
            queued_at: 5.0,
            posted_at: 9.0,
            ..Default::default()
        };
        let r = row("en", &("github".into(), "github".into()), "a/b#1", &e);
        assert_eq!(r.reasons, vec!["CI failed"]);
        assert_eq!(r.at, 9.0);
    }
}
