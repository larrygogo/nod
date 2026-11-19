//! Generic webhook: POST a JSON event whenever a PR's review state changes. See docs/webhook.md.

use std::time::Duration;

use anyhow::{bail, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::Notifier;
use crate::config::Config;
use crate::engine::Entry;
use crate::forge::PrStatus;
use crate::i18n::reason_text;
use crate::util::now;

pub struct WebhookNotifier {
    lang: String,
    url: String,
    secret: String,
    dry_run: bool,
    /// The address belongs to a chat or push app Nod knows (Feishu, Slack, Discord, …): send in its format, and only
    /// outcomes, like that app's own channel. None: Nod's JSON events.
    app: Option<super::robot::RobotNotifier>,
}

impl WebhookNotifier {
    pub fn new(cfg: &Config) -> Self {
        WebhookNotifier {
            lang: cfg.language.clone(),
            url: cfg.notify.webhook.url.clone(),
            secret: cfg.webhook_secret(),
            dry_run: cfg.dry_run,
            app: super::robot::detect(&cfg.notify.webhook.url)
                .map(|k| super::robot::RobotNotifier::for_webhook(cfg, k)),
        }
    }

    pub fn sign(secret: &str, body: &[u8]) -> String {
        let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
        m.update(body);
        format!("sha256={}", hex(&m.finalize().into_bytes()))
    }

    fn post(&self, payload: &Value) -> Result<()> {
        let body = payload.to_string();
        let mut req = ureq::post(&self.url)
            .timeout(Duration::from_secs(20))
            .set("Content-Type", "application/json; charset=utf-8")
            .set("User-Agent", concat!("nod/", env!("CARGO_PKG_VERSION")));
        if !self.secret.is_empty() {
            req = req.set(
                "X-Nod-Signature",
                &Self::sign(&self.secret, body.as_bytes()),
            );
        }
        match req.send_string(&body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => bail!("webhook returned HTTP {code}"),
            Err(e) => bail!("webhook: {e}"),
        }
    }

    pub fn payload(&self, key: &str, e: &Entry, st: Option<&PrStatus>) -> Value {
        let verdict = e.verdict.as_ref().map(|v| {
            json!({"summary": v.summary, "approve_recommended": v.approve_recommended,
                   "findings": v.findings.iter().map(|f| json!({"severity": f.severity, "path": f.path, "line": f.line,
                        "title": f.title, "introduced_by_pr": f.introduced_by_pr})).collect::<Vec<_>>()})
        });
        json!({
            "type": "pr.update", "key": key, "status": e.status,
            "headline": super::feishu::headline(&self.lang, e, st).0,
            "pr": st.map(|s| serde_json::to_value(s).unwrap_or_default()).unwrap_or(json!({})),
            "verdict": verdict, "event": e.event,
            "reasons": e.reasons.iter().map(|r| reason_text(&self.lang, r)).collect::<Vec<_>>(),
            "review_url": e.review_url, "note": e.note.as_ref().map(|n| n.text.clone()),
        })
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Notifier for WebhookNotifier {
    fn update(&self, key: &str, e: &mut Entry, st: Option<&PrStatus>) -> Result<bool> {
        if let Some(app) = &self.app {
            return app.update(key, e, st);
        }
        let mut payload = self.payload(key, e, st);
        let sig = hex(&Sha256::digest(payload.to_string().as_bytes()));
        if sig == e.webhook_sig {
            return Ok(true);
        }
        if self.dry_run {
            crate::info!("[dry-run] webhook {key}: {}", payload["headline"]);
        } else {
            payload["sent_at"] = json!(now() as u64);
            self.post(&payload)?;
        }
        e.webhook_sig = sig;
        Ok(true)
    }

    fn alert(&self, text: &str) -> Result<()> {
        if let Some(app) = &self.app {
            return app.alert(text);
        }
        self.post(&json!({"type": "alert", "text": text, "sent_at": now() as u64}))
    }

    fn test(&self) -> Result<String> {
        if let Some(app) = &self.app {
            app.test()?;
            return Ok(format!(
                "sent a test message in {} format",
                app.kind().name()
            ));
        }
        self.post(&json!({"type": "test", "text": "Nod webhook test", "sent_at": now() as u64}))?;
        Ok(format!("POSTed a test event to {}", self.url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::review::tests::{clean, finding};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Minimal HTTP server: records (body, signature header) of each request.
    fn server() -> (String, mpsc::Receiver<(String, Option<String>)>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/hook", l.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let (mut len, mut sig) = (0usize, None);
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let l = line.trim_end();
                    if l.is_empty() {
                        break;
                    }
                    let (k, v) = l.split_once(':').unwrap_or((l, ""));
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap();
                    }
                    if k.eq_ignore_ascii_case("x-nod-signature") {
                        sig = Some(v.trim().to_string());
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let _ = (&s).write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
                tx.send((String::from_utf8(body).unwrap(), sig)).unwrap();
            }
        });
        (url, rx)
    }

    #[test]
    fn signed_once_per_change() {
        let (url, rx) = server();
        let mut cfg = Config {
            language: "en".into(),
            ..Default::default()
        };
        cfg.notify.webhook.url = url;
        cfg.notify.webhook.secret = "k3y".into();
        let n = WebhookNotifier::new(&cfg);
        let mut v = clean();
        v.findings = vec![finding("high", true, 1)];
        v.approve_recommended = false;
        let mut e = Entry {
            status: "posted".into(),
            event: Some("COMMENT".into()),
            reasons: vec!["high".into()],
            verdict: Some(v),
            ..Default::default()
        };
        n.update("o/r#1", &mut e, None).unwrap();
        n.update("o/r#1", &mut e, None).unwrap(); // unchanged: not sent again
        e.status = "direct_approved".into();
        n.update("o/r#1", &mut e, None).unwrap();
        let (body, sig) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(sig.unwrap(), WebhookNotifier::sign("k3y", body.as_bytes()));
        let p: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(p["type"], "pr.update");
        assert_eq!(p["reasons"][0], "high-severity issue found");
        assert_eq!(p["verdict"]["findings"][0]["severity"], "high");
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
