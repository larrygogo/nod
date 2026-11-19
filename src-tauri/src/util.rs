//! Logging and child processes (with timeouts, tracking and whole-tree kill).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::paths;

// ---------------------------------------------------------------- logging

struct Logger {
    file: Option<File>,
    path: Option<std::path::PathBuf>,
    /// When the file was (re)opened. The app and its worker share the log, and either may move it aside; reopening
    /// by name every half minute makes the other one follow within that time instead of writing into the old copy.
    opened: std::time::Instant,
    stderr: bool,
    verbose: bool,
}

const LOG_MAX: u64 = 5_000_000;

/// Move a full log aside (one old copy is kept). Checked at start-up and again while running, since the app and its
/// worker can run for weeks.
fn rotate(path: &std::path::Path) {
    if fs::metadata(path)
        .map(|m| m.len() > LOG_MAX)
        .unwrap_or(false)
    {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

pub fn setup_logging(verbose: bool, to_file: bool) {
    let file = if to_file {
        let dir = paths::data_dir();
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("autoreview.log");
        rotate(&path);
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(|f| (f, path))
    } else {
        None
    };
    let (file, path) = match file {
        Some((f, p)) => (Some(f), Some(p)),
        None => (None, None),
    };
    // A detached daemon's stderr goes to daemon.out; don't duplicate the log file there.
    let stderr = !to_file || std::io::IsTerminal::is_terminal(&std::io::stderr());
    let _ = LOGGER.set(Mutex::new(Logger {
        file,
        path,
        opened: std::time::Instant::now(),
        stderr,
        verbose,
    }));
}

pub fn log(level: &str, msg: &str) {
    let Some(l) = LOGGER.get() else {
        if level != "DEBUG" {
            eprintln!("{level} {msg}");
        }
        return;
    };
    let mut l = l.lock().unwrap_or_else(|e| e.into_inner());
    if level == "DEBUG" && !l.verbose {
        return;
    }
    let line = format!(
        "{} {level} {msg}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    if l.opened.elapsed() > Duration::from_secs(30) {
        if let Some(p) = l.path.clone() {
            l.file = None;
            rotate(&p);
            l.file = OpenOptions::new().create(true).append(true).open(&p).ok();
        }
        l.opened = std::time::Instant::now();
    }
    if let Some(f) = l.file.as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
    if l.stderr {
        // A detached daemon may have no usable stderr; never fail because of logging.
        let _ = std::io::stderr().write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! info { ($($t:tt)*) => { $crate::util::log("INFO", &format!($($t)*)) } }
#[macro_export]
macro_rules! warn { ($($t:tt)*) => { $crate::util::log("WARN", &format!($($t)*)) } }
#[macro_export]
macro_rules! error { ($($t:tt)*) => { $crate::util::log("ERROR", &format!($($t)*)) } }
#[macro_export]
macro_rules! debug { ($($t:tt)*) => { $crate::util::log("DEBUG", &format!($($t)*)) } }

// ---------------------------------------------------------------- processes

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Review subprocesses that are currently running, keyed by PR, so a "direct approve" can stop them.
fn active() -> &'static Mutex<HashMap<String, u32>> {
    static A: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether a review process for `key` is currently running.
#[cfg(test)]
pub fn abort_target_exists(key: &str) -> bool {
    active().lock().unwrap().contains_key(key)
}

/// Stop the tracked review process for `key` (and everything it started). Returns whether one was running.
pub fn abort(key: &str) -> bool {
    let pid = active().lock().unwrap().get(key).copied();
    match pid {
        Some(pid) => {
            kill_tree(pid);
            true
        }
        None => false,
    }
}

/// Stop every tracked review process (the worker is exiting).
pub fn abort_all() {
    let pids: Vec<u32> = active().lock().unwrap().values().copied().collect();
    for pid in pids {
        kill_tree(pid);
    }
}

pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(unix)]
    {
        // Children are started in their own process group (see `hide`), so kill the whole group.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status();
    }
}

/// No console window on Windows; own process group on Unix (so the whole tree can be killed).
pub fn hide(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

#[derive(Debug)]
pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub struct RunOpts<'a> {
    pub cwd: Option<&'a Path>,
    pub stdin: Option<&'a str>,
    pub timeout: Duration,
    pub env: Option<&'a HashMap<String, String>>,
    pub track: Option<&'a str>,
}

impl Default for RunOpts<'_> {
    fn default() -> Self {
        RunOpts {
            cwd: None,
            stdin: None,
            timeout: Duration::from_secs(300),
            env: None,
            track: None,
        }
    }
}

fn reader<R: Read + Send + 'static>(mut r: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

pub fn run_raw(args: &[&str], opts: RunOpts) -> Result<Output> {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = opts.cwd {
        cmd.current_dir(d);
    }
    if let Some(env) = opts.env {
        cmd.env_clear().envs(env);
    }
    hide(&mut cmd);
    let mut child: Child = cmd
        .spawn()
        .with_context(|| format!("cannot start `{}`", args[0]))?;
    if let Some(key) = opts.track {
        active().lock().unwrap().insert(key.to_string(), child.id());
    }
    let input = opts.stdin.unwrap_or("").to_string();
    let mut stdin = child.stdin.take();
    let writer = thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(input.as_bytes());
        }
        drop(stdin);
    });
    let out = reader(child.stdout.take().unwrap());
    let err = reader(child.stderr.take().unwrap());
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) => {}
            Err(e) => {
                // stop the process and forget it, or "direct approve" would later try to stop a stale pid
                kill_tree(child.id());
                let _ = child.kill();
                if let Some(key) = opts.track {
                    active().lock().unwrap().remove(key);
                }
                return Err(e.into());
            }
        }
        if start.elapsed() > opts.timeout {
            kill_tree(child.id());
            let _ = child.kill();
            timed_out = true;
            break child.wait().ok().and_then(|s| s.code());
        }
        thread::sleep(Duration::from_millis(50));
    };
    if let Some(key) = opts.track {
        active().lock().unwrap().remove(key);
    }
    let _ = writer.join();
    Ok(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        timed_out,
    })
}

/// Run and return stdout; non-zero exit or timeout is an error.
pub fn run(args: &[&str], opts: RunOpts) -> Result<String> {
    let timeout = opts.timeout;
    let o = run_raw(args, opts)?;
    if o.timed_out {
        bail!(
            "`{}` did not finish within {}s",
            args.join(" "),
            timeout.as_secs()
        );
    }
    if o.status != Some(0) {
        let msg = if o.stderr.trim().is_empty() {
            o.stdout.trim()
        } else {
            o.stderr.trim()
        };
        let head: String = args.iter().take(4).copied().collect::<Vec<_>>().join(" ");
        bail!("`{head}` exited {:?}: {}", o.status, truncate(msg, 800));
    }
    Ok(o.stdout)
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// The last `n` characters of `s`: a PR discussion is cut from the start, since its newest comments matter most.
pub fn keep_last(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn hhmm(ts: f64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// A local time for a message: "15:30" today, "11-20 15:30" on another day.
pub fn clock(ts: f64) -> String {
    use chrono::TimeZone;
    let Some(t) = chrono::Local.timestamp_opt(ts as i64, 0).single() else {
        return String::new();
    };
    if t.date_naive() == chrono::Local::now().date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%m-%d %H:%M").to_string()
    }
}

pub fn which(name: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.exists().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or(".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}
