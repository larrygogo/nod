//! External plugins: a program plus a `plugin.toml` manifest in `<config dir>/plugins/<name>/`. Nod starts the
//! program and talks JSON-RPC 2.0 to it over stdin/stdout, one JSON message per line (the same framing as MCP's stdio
//! transport). See docs/plugins.md for the protocol.
//!
//! The process is kept running between calls. If it crashes, prints something that is not JSON-RPC, or does not answer
//! in time, the call fails with a readable reason, the process is stopped, and the next call starts it again; nothing
//! of this reaches the rest of Nod.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::paths;

pub const PROTOCOL: u32 = 1;

/// A setting the plugin needs; the window builds its form from these.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub label_en: String,
    /// text | password | number | bool | select | list
    #[serde(default = "text")]
    pub r#type: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub placeholder: String,
}

fn text() -> String {
    "text".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
    /// Review engines: "full" (reads the checkout) or "diff" (sees only the diff).
    pub repo_access: String,
    /// Review engines: "strong" (OS sandbox, read-only) or "weak". Weak engines never lead to an approval.
    pub isolation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    /// forge | engine | notify
    pub kind: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    /// Program and arguments. A first element that exists inside the plugin directory is run from there.
    pub command: Vec<String>,
    #[serde(default)]
    pub settings: Vec<Setting>,
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Seconds a call may take (default 30; review calls use the review timeout instead).
    #[serde(default)]
    pub timeout_s: u64,
    #[serde(skip)]
    pub dir: PathBuf,
}

pub fn plugins_dir() -> PathBuf {
    paths::config_dir().join("plugins")
}

/// Every manifest in the plugins directory: (manifests, problems with the ones that could not be read).
pub fn discover() -> (Vec<Manifest>, Vec<(String, String)>) {
    discover_in(&plugins_dir())
}

pub fn discover_in(dir: &Path) -> (Vec<Manifest>, Vec<(String, String)>) {
    let mut ok = vec![];
    let mut bad = vec![];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (ok, bad);
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let file = e.path().join("plugin.toml");
        if !file.exists() {
            continue;
        }
        let folder = e.file_name().to_string_lossy().to_string();
        match std::fs::read_to_string(&file)
            .map_err(anyhow::Error::from)
            .and_then(|t| {
                toml::from_str::<Manifest>(&t).map_err(|e| anyhow!("{}", e.to_string().trim()))
            })
            .and_then(|mut m| {
                validate(&m)?;
                m.dir = e.path();
                Ok(m)
            }) {
            Ok(m) => ok.push(m),
            Err(err) => bad.push((folder, format!("{err:#}"))),
        }
    }
    (ok, bad)
}

fn validate(m: &Manifest) -> Result<()> {
    if m.name.is_empty()
        || !m
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("name must be letters, digits, - and _");
    }
    if !["forge", "engine", "notify"].contains(&m.kind.as_str()) {
        bail!("kind must be forge, engine or notify");
    }
    if m.command.is_empty() {
        bail!("command is empty");
    }
    Ok(())
}

pub fn find(name: &str) -> Result<Manifest> {
    discover()
        .0
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| {
            anyhow!(
                "plugin {name:?} is not installed in {}",
                plugins_dir().display()
            )
        })
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: std::sync::Arc<Mutex<Vec<String>>>,
}

/// A running plugin, shared by every thread that uses it.
pub struct Host {
    pub manifest: Manifest,
    settings: Value,
    language: String,
    proc: Mutex<Option<Proc>>,
    next_id: Mutex<u64>,
}

impl Host {
    pub fn new(manifest: Manifest, settings: Value, language: &str) -> Host {
        Host {
            manifest,
            settings,
            language: language.to_string(),
            proc: Mutex::new(None),
            next_id: Mutex::new(1),
        }
    }

    fn spawn(&self) -> Result<Proc> {
        let m = &self.manifest;
        let first = m.dir.join(&m.command[0]);
        let program = if first.exists() {
            first.to_string_lossy().to_string()
        } else {
            m.command[0].clone()
        };
        let mut cmd = Command::new(&program);
        for a in &m.command[1..] {
            let local = m.dir.join(a);
            cmd.arg(if local.exists() {
                local.to_string_lossy().to_string()
            } else {
                a.clone()
            });
        }
        cmd.current_dir(&m.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .envs(crate::reviewer::safe_env(&[]));
        crate::util::hide(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start plugin {} ({program})", m.name))?;
        let stdin = child.stdin.take().unwrap();
        let (tx, rx) = channel();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = std::sync::Arc::new(Mutex::new(vec![]));
        let tail = stderr.clone();
        let err = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let mut t = tail.lock().unwrap();
                t.push(line);
                if t.len() > 20 {
                    t.remove(0);
                }
            }
        });
        Ok(Proc {
            child,
            stdin,
            lines: rx,
            stderr,
        })
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(if self.manifest.timeout_s > 0 {
            self.manifest.timeout_s
        } else {
            30
        })
    }

    /// Call `method`; on any failure the process is stopped so the next call starts afresh.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with(method, params, self.timeout())
    }

    pub fn call_with(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let mut guard = self.proc.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let p = self.spawn()?;
            *guard = Some(p);
            let init = json!({"protocol": PROTOCOL, "settings": self.settings, "language": self.language,
                              "data_dir": paths::data_dir().join("plugins").join(&self.manifest.name)});
            if let Err(e) =
                self.exchange(guard.as_mut().unwrap(), "initialize", init, self.timeout())
            {
                Self::stop(&mut guard);
                return Err(e);
            }
        }
        let r = self.exchange(guard.as_mut().unwrap(), method, params, timeout);
        if r.is_err() {
            Self::stop(&mut guard);
        }
        r
    }

    fn stop(guard: &mut Option<Proc>) {
        if let Some(mut p) = guard.take() {
            crate::util::kill_tree(p.child.id());
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }

    fn exchange(
        &self,
        p: &mut Proc,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let name = &self.manifest.name;
        let id = {
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            *n
        };
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(p.stdin, "{req}")
            .and_then(|_| p.stdin.flush())
            .map_err(|_| self.died(p, "stopped accepting input"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match p.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => {
                    bail!(
                        "plugin {name}: no answer to {method} within {}s",
                        timeout.as_secs()
                    )
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.died(p, "exited")),
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = serde_json::from_str(&line).map_err(|_| {
                anyhow!(
                    "plugin {name}: wrote something that is not JSON-RPC: {}",
                    crate::util::truncate(line.trim(), 120)
                )
            })?;
            if msg.get("method").is_some() && msg.get("id").is_none() {
                // notification from the plugin, e.g. {"method":"log","params":{"message":"…"}}
                crate::info!(
                    "plugin {name}: {}",
                    msg["params"]["message"].as_str().unwrap_or_default()
                );
                continue;
            }
            if msg["id"] != json!(id) {
                continue; // a late answer to a call that timed out earlier
            }
            if let Some(err) = msg.get("error").filter(|e| !e.is_null()) {
                bail!(
                    "plugin {name}: {}",
                    err["message"].as_str().unwrap_or("error without a message")
                );
            }
            return msg
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow!("plugin {name}: answer without a result"));
        }
    }

    fn died(&self, p: &mut Proc, what: &str) -> anyhow::Error {
        std::thread::sleep(Duration::from_millis(100));
        let code = p.child.try_wait().ok().flatten().map(|s| s.to_string());
        let tail = p.stderr.lock().unwrap().join("\n");
        anyhow!(
            "plugin {} {what}{}{}",
            self.manifest.name,
            code.map(|c| format!(" ({c})")).unwrap_or_default(),
            if tail.is_empty() {
                String::new()
            } else {
                format!(": {}", crate::util::truncate(tail.trim(), 300))
            }
        )
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let mut g = self.proc.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = g.as_mut() {
            let _ = writeln!(
                p.stdin,
                "{}",
                json!({"jsonrpc": "2.0", "method": "shutdown"})
            );
        }
        Self::stop(&mut g);
    }
}

/// Settings of plugin `name` from the configuration, as JSON (secrets filled from the keychain by `secrets`).
pub fn settings_of(cfg: &crate::config::Config, name: &str) -> Value {
    cfg.plugins
        .get(name)
        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
        .unwrap_or_else(|| json!({}))
}
