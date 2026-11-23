//! System notifications. The worker process cannot show them itself (the desktop app owns the notification
//! identity), so it writes one JSON line per notification to stdout and the app shows it.
//!
//! Only outcomes are announced: a review posted (or failed), a direct approval, and whole-round failures. The
//! intermediate stages (queued, reviewing) are visible in the window and tray instead.

use std::io::Write;
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{json, Value};

use super::Notifier;
use crate::config::Config;
use crate::engine::Entry;
use crate::forge::PrStatus;
use crate::i18n::t;

const SIG: &str = "system_sig";

pub struct SystemNotifier {
    lang: String,
    dry_run: bool,
    out: Mutex<()>,
}

impl SystemNotifier {
    pub fn new(cfg: &Config) -> Self {
        SystemNotifier {
            lang: cfg.language.clone(),
            dry_run: cfg.dry_run,
            out: Mutex::new(()),
        }
    }

    fn emit(&self, title: &str, body: &str, url: &str) -> Result<()> {
        let line = json!({"type": "notify", "title": title, "body": body, "url": url}).to_string();
        let _g = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let mut o = std::io::stdout().lock();
        writeln!(o, "{line}")?;
        o.flush()?;
        Ok(())
    }

    /// What to announce for this entry right now, if anything: (signature, title, body, url).
    pub fn message(&self, key: &str, e: &Entry) -> Option<(String, String, String, String)> {
        let l = &self.lang;
        let (kind, title) = match (e.status.as_str(), e.event.as_deref()) {
            ("direct_approved", _) => ("direct", t(l, "sys.direct")),
            (_, Some("APPROVE")) => ("approve", t(l, "sys.approved")),
            (_, Some(_)) => ("comment", t(l, "sys.commented")),
            ("failed", _) => ("failed", t(l, "sys.failed")),
            // once per commit: the engine is out of quota and Nod waits for it
            ("retry", _) if e.waits > 0 => ("wait", t(l, "sys.waiting")),
            _ => return None,
        };
        let sig = format!("{kind}:{}", e.sha);
        let body = match (&e.verdict, e.error.as_deref()) {
            (_, Some(err)) if kind == "failed" => crate::util::truncate(err, 160),
            _ if kind == "wait" => e.note.as_ref().map(|n| n.text.clone()).unwrap_or_default(),
            // not approved: why, which says whether it needs you, rather than the summary
            _ if kind == "comment" && !e.reasons.is_empty() => e
                .reasons
                .iter()
                .map(|r| crate::i18n::reason_text(l, r))
                .collect::<Vec<_>>()
                .join(if l == "en" { "; " } else { "；" }),
            (Some(v), _) => crate::util::truncate(&v.summary, 160),
            _ => String::new(),
        };
        let url = e.review_url.clone().unwrap_or_else(|| e.url.clone());
        Some((
            sig,
            format!("{title} · {key}"),
            format!("{}\n{body}", e.title).trim().to_string(),
            url,
        ))
    }
}

impl Notifier for SystemNotifier {
    fn update(&self, key: &str, e: &mut Entry, _st: Option<&PrStatus>) -> Result<bool> {
        let Some((sig, title, body, url)) = self.message(key, e) else {
            return Ok(true);
        };
        if e.extra.get(SIG).and_then(Value::as_str) == Some(sig.as_str()) {
            return Ok(true);
        }
        if self.dry_run {
            crate::info!("[dry-run] system notification {key}: {title}");
        } else {
            self.emit(&title, &body, &url)?;
        }
        e.extra.insert(SIG.into(), Value::String(sig));
        Ok(true)
    }

    fn alert(&self, text: &str) -> Result<()> {
        if self.dry_run {
            crate::info!(
                "[dry-run] system alert: {}",
                crate::util::truncate(text, 200)
            );
            return Ok(());
        }
        self.emit(&t(&self.lang, "card.alert_title"), text, "")
    }

    fn test(&self) -> Result<String> {
        self.emit(
            &t(&self.lang, "card.test_title"),
            &t(&self.lang, "sys.test_body"),
            "",
        )?;
        Ok("sent".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announces_outcomes_once() {
        let n = SystemNotifier::new(&Config {
            language: "en".into(),
            dry_run: true,
            ..Default::default()
        });
        let mut e = Entry {
            status: "reviewing".into(),
            sha: "abc".into(),
            title: "Fix".into(),
            url: "https://github.com/a/b/pull/1".into(),
            ..Default::default()
        };
        assert!(n.message("a/b#1", &e).is_none());
        e.status = "posted".into();
        e.event = Some("APPROVE".into());
        let (sig, title, _, url) = n.message("a/b#1", &e).unwrap();
        assert_eq!(sig, "approve:abc");
        assert!(title.starts_with("Approved"));
        assert_eq!(url, "https://github.com/a/b/pull/1");
        n.update("a/b#1", &mut e, None).unwrap();
        assert_eq!(e.extra[SIG], "approve:abc");
        // not approved: says so, with the reasons instead of the summary
        let mut c = e.clone();
        c.event = Some("COMMENT".into());
        c.reasons = vec!["high".into(), "ci_failed".into()];
        let (_, title, body, _) = n.message("a/b#1", &c).unwrap();
        assert!(title.starts_with("Not approved"), "{title}");
        assert_eq!(body, "Fix\nhigh-severity issue found; CI failed");
        // a new commit announces again
        e.sha = "def".into();
        assert_eq!(n.message("a/b#1", &e).unwrap().0, "approve:def");
    }
}
