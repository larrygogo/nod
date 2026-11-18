//! The plugin catalog (what the window shows and builds its forms from) and the adapters that make external
//! plugins look like built-in code hosts, review engines and notification channels.

pub mod catalog;
pub mod external;

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::config::{Config, ForgeAccount};
use crate::engine::Entry;
use crate::forge::{DiffLines, Forge, Pr, PrStatus, Remote};
use crate::notify::system::SystemNotifier;
use crate::notify::Notifier;
use crate::review::{build_prompt, Verdict, SCHEMA};
use crate::reviewer::Engine;
use external::Host;

pub use catalog::catalog;

fn host(cfg: &Config, name: &str, kind: &str) -> Result<Host> {
    let m = external::find(name)?;
    if m.kind != kind {
        return Err(anyhow!("plugin {name} is a {} plugin, not {kind}", m.kind));
    }
    Ok(Host::new(
        m,
        external::settings_of(cfg, name),
        &cfg.language,
    ))
}

// ---------------------------------------------------------------- notification channel

pub struct ExternalNotifier {
    host: Host,
    outcome: SystemNotifier,
    slot: String,
    dry_run: bool,
}

impl ExternalNotifier {
    pub fn new(cfg: &Config, name: &str) -> Result<Self> {
        Ok(ExternalNotifier {
            host: host(cfg, name, "notify")?,
            outcome: SystemNotifier::new(cfg),
            slot: format!("ext_{name}_sig"),
            dry_run: cfg.dry_run,
        })
    }

    fn send(&self, kind: &str, key: &str, title: &str, text: &str, url: &str) -> Result<()> {
        if self.dry_run {
            crate::info!("[dry-run] plugin {}: {title}", self.host.manifest.name);
            return Ok(());
        }
        self.host.call(
            "notify.send",
            json!({"kind": kind, "key": key, "title": title, "text": text, "url": url}),
        )?;
        Ok(())
    }
}

impl Notifier for ExternalNotifier {
    fn update(&self, key: &str, e: &mut Entry, _st: Option<&PrStatus>) -> anyhow::Result<bool> {
        let Some((sig, title, text, url)) = self.outcome.message(key, e) else {
            return Ok(true);
        };
        if e.extra.get(&self.slot).and_then(Value::as_str) == Some(sig.as_str()) {
            return Ok(true);
        }
        let kind = sig.split(':').next().unwrap_or("");
        self.send(kind, key, &title, &text, &url)?;
        e.extra.insert(self.slot.clone(), Value::String(sig));
        Ok(true)
    }

    fn alert(&self, text: &str) -> anyhow::Result<()> {
        self.send("alert", "", "nod", text, "")
    }

    fn test(&self) -> anyhow::Result<String> {
        let r = self.host.call("notify.test", json!({}))?;
        Ok(r.as_str().unwrap_or("sent").to_string())
    }
}

// ---------------------------------------------------------------- review engine

pub struct ExternalEngine {
    host: Host,
}

impl ExternalEngine {
    pub fn new(cfg: &Config, name: &str) -> Result<Self> {
        Ok(ExternalEngine {
            host: host(cfg, name, "engine")?,
        })
    }
}

fn pr_json(pr: &Pr) -> Value {
    json!({"repo": pr.repo, "number": pr.number, "title": pr.title, "author": pr.author, "url": pr.url,
           "head": pr.head, "base": pr.base, "draft": pr.draft, "state": pr.state, "body": pr.body})
}

impl Engine for ExternalEngine {
    fn ask(
        &self,
        cfg: &Config,
        pr: &Pr,
        wt: &Path,
        ctx: &Path,
        discussion: &str,
        platform: &str,
    ) -> Result<Verdict> {
        let caps = &self.host.manifest.capabilities;
        let full = caps.repo_access != "diff";
        let tools = if full {
            crate::reviewer::codex::tools(&cfg.language)
        } else {
            crate::reviewer::api::tools(&cfg.language)
        };
        let prompt = format!(
            "{}\n\n{}",
            build_prompt(cfg, pr, ctx, discussion, platform, tools),
            crate::reviewer::json_only(&cfg.language)
        );
        let r = self.host.call_with(
            "review.run",
            json!({"pr": pr_json(pr), "checkout": if full { wt.to_string_lossy().to_string() } else { String::new() },
                   "context_dir": ctx, "prompt": prompt, "schema": serde_json::from_str::<Value>(SCHEMA)?,
                   "language": cfg.language, "platform": platform}),
            Duration::from_secs_f64(cfg.review.timeout_min * 60.0),
        )?;
        let verdict = r.get("verdict").cloned().unwrap_or(r);
        let mut v = crate::review::validate_verdict(&verdict)?;
        v.meta.engine = format!("ext:{}", self.host.manifest.name);
        v.meta.no_approve = caps.isolation != "strong";
        Ok(v)
    }
}

// ---------------------------------------------------------------- code host

pub struct ExternalForge {
    host: Host,
}

impl ExternalForge {
    pub fn new(acc: &ForgeAccount) -> Result<Self> {
        let name = acc.kind.trim_start_matches("ext:");
        let m = external::find(name)?;
        let mut settings = serde_json::to_value(&acc.settings).unwrap_or(json!({}));
        settings["url"] = json!(acc.url);
        settings["token"] = json!(crate::secrets::forge_token(acc));
        Ok(ExternalForge {
            host: Host::new(m, settings, "en"),
        })
    }

    fn call<T: serde::de::DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let v = self.host.call(method, params)?;
        serde_json::from_value(v).map_err(|e| {
            anyhow!(
                "plugin {}: bad answer to {method}: {e}",
                self.host.manifest.name
            )
        })
    }
}

fn pr_from(v: Value) -> Result<Pr> {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    Ok(Pr {
        repo: s("repo"),
        number: v["number"]
            .as_u64()
            .ok_or_else(|| anyhow!("pull request without a number"))?,
        title: s("title"),
        author: s("author"),
        url: s("url"),
        head: s("head"),
        base: s("base"),
        draft: v["draft"].as_bool().unwrap_or(false),
        state: s("state"),
        body: s("body"),
    })
}

impl Forge for ExternalForge {
    fn current_user(&self) -> Result<String> {
        self.call("forge.currentUser", json!({}))
    }
    fn fetch_pr(&self, repo: &str, number: u64) -> Result<Pr> {
        pr_from(
            self.host
                .call("forge.fetchPr", json!({"repo": repo, "number": number}))?,
        )
    }
    fn requested_prs(&self, teams: bool) -> Result<Vec<(String, u64)>> {
        self.call("forge.requestedPrs", json!({"teams": teams}))
    }
    fn ci_state(&self, pr: &Pr) -> Result<String> {
        self.call("forge.ciState", json!({"pr": pr_json(pr)}))
    }
    fn pr_status(&self, repo: &str, number: u64) -> Result<PrStatus> {
        self.call("forge.prStatus", json!({"repo": repo, "number": number}))
    }
    fn discussion(&self, pr: &Pr) -> Result<String> {
        self.call("forge.discussion", json!({"pr": pr_json(pr)}))
    }
    fn diff_lines(&self, pr: &Pr) -> Result<DiffLines> {
        self.call("forge.diffLines", json!({"pr": pr_json(pr)}))
    }
    fn post_review(&self, pr: &Pr, review: &Value) -> Result<(String, u64)> {
        let r: Value = self.call(
            "forge.postReview",
            json!({"pr": pr_json(pr), "review": review}),
        )?;
        Ok((
            r["url"].as_str().unwrap_or_default().to_string(),
            r["id"]
                .as_u64()
                .ok_or_else(|| anyhow!("postReview returned no id"))?,
        ))
    }
    fn list_reviews(&self, pr: &Pr) -> Result<Vec<(u64, String)>> {
        self.call("forge.listReviews", json!({"pr": pr_json(pr)}))
    }
    fn dismiss_review(&self, pr: &Pr, id: u64, message: &str) -> Result<()> {
        self.host.call(
            "forge.dismissReview",
            json!({"pr": pr_json(pr), "id": id, "message": message}),
        )?;
        Ok(())
    }
    fn remote(&self, repo: &str) -> Result<Remote> {
        let r: Value = self.call("forge.remote", json!({"repo": repo}))?;
        Ok(Remote::Git {
            url: r["url"]
                .as_str()
                .ok_or_else(|| anyhow!("remote without url"))?
                .to_string(),
            header: r["header"].as_str().map(str::to_string),
        })
    }
    fn head_ref(&self, number: u64) -> String {
        self.call("forge.headRef", json!({"number": number}))
            .unwrap_or_else(|_| format!("refs/pull/{number}/head"))
    }
}

/// The example plugin shipped with Nod (examples/plugins/notify-log), for tests.
#[cfg(test)]
pub fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/plugins")
}

#[cfg(test)]
mod tests {
    use super::*;
    use external::{discover_in, Manifest};

    /// A Python that really runs (on Windows `python3` may be the Microsoft Store stub).
    fn python() -> Option<String> {
        ["python3", "python", "py"]
            .iter()
            .find(|p| {
                std::process::Command::new(p)
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success())
            })
            .map(|p| p.to_string())
    }

    fn example() -> Manifest {
        let (ok, bad) = discover_in(&example_dir());
        assert!(bad.is_empty(), "{bad:?}");
        ok.into_iter().find(|m| m.name == "notify-log").unwrap()
    }

    fn with_python(mut m: Manifest) -> Option<Manifest> {
        m.command[0] = python()?;
        Some(m)
    }

    #[test]
    fn the_example_plugin_receives_notifications() {
        let Some(m) = with_python(example()) else {
            eprintln!("python not found: skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        let h = Host::new(m, json!({"path": log}), "en");
        assert_eq!(h.call("notify.test", json!({})).unwrap(), json!("written"));
        h.call(
            "notify.send",
            json!({"kind": "approve", "key": "a/b#1", "title": "Approved", "text": "ok", "url": "https://x"}),
        )
        .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"key\": \"a/b#1\"") || text.contains("\"key\":\"a/b#1\""));
    }

    fn script_plugin(dir: &Path, body: &str) -> Option<Manifest> {
        let py = python()?;
        std::fs::write(dir.join("p.py"), body).unwrap();
        Some(Manifest {
            name: "bad".into(),
            kind: "notify".into(),
            display_name: "bad".into(),
            description: String::new(),
            version: String::new(),
            command: vec![py, "p.py".into()],
            settings: vec![],
            capabilities: Default::default(),
            timeout_s: 2,
            dir: dir.to_path_buf(),
        })
    }

    #[test]
    fn misbehaving_plugins_fail_calls_not_the_app() {
        let d = tempfile::tempdir().unwrap();
        // crashes on start
        let Some(mut m) = script_plugin(
            d.path(),
            "import sys\nsys.stderr.write('boom\\n')\nsys.exit(3)\n",
        ) else {
            return;
        };
        // a cold Python start can take longer than the 2 s used below for "never answers" (seen on CI)
        m.timeout_s = 20;
        let err = Host::new(m, json!({}), "en")
            .call("notify.test", json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("exited") && err.contains("boom"), "{err}");
        // prints garbage
        let mut m = script_plugin(
            d.path(),
            "import sys\nfor l in sys.stdin:\n    print('hello')\n    sys.stdout.flush()\n",
        )
        .unwrap();
        m.timeout_s = 20;
        let err = Host::new(m, json!({}), "en")
            .call("notify.test", json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not JSON-RPC"), "{err}");
        // never answers
        let m = script_plugin(
            d.path(),
            "import sys, time\nfor l in sys.stdin:\n    time.sleep(60)\n",
        )
        .unwrap();
        let t = std::time::Instant::now();
        let err = Host::new(m, json!({}), "en")
            .call("notify.test", json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no answer"), "{err}");
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn broken_manifests_are_reported_not_loaded() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("broken");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("plugin.toml"),
            "name = \"x y\"\nkind = \"notify\"\ndisplay_name = \"X\"\ncommand = [\"x\"]\n",
        )
        .unwrap();
        let (ok, bad) = discover_in(d.path());
        assert!(ok.is_empty());
        assert_eq!(bad[0].0, "broken");
    }
}
