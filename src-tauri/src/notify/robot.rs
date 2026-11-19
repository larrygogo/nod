//! Chat robots reached through an incoming-webhook URL: Feishu / Lark group bots, DingTalk, WeCom (企业微信) and Slack.
//!
//! Like desktop notifications they announce outcomes only (review posted, failed, approved directly) and whole-round
//! failures; each robot remembers per PR what it last sent, so nothing is repeated.

use std::time::Duration;

use anyhow::{bail, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use super::system::SystemNotifier;
use super::Notifier;
use crate::config::{Config, Robot};
use crate::engine::Entry;
use crate::forge::PrStatus;
use crate::i18n::t;
use crate::util::now;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    FeishuBot,
    DingTalk,
    WeCom,
    Slack,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::FeishuBot => "feishu_bot",
            Kind::DingTalk => "dingtalk",
            Kind::WeCom => "wecom",
            Kind::Slack => "slack",
        }
    }
}

pub struct RobotNotifier {
    kind: Kind,
    robot: Robot,
    lang: String,
    dry_run: bool,
    /// Reuses the desktop notifier's choice of what to announce.
    outcome: SystemNotifier,
}

/// Feishu / Lark "签名校验": `sign = base64(hmac_sha256(key = "{timestamp}\n{secret}", message = ""))`, with the
/// timestamp in seconds; both go in the JSON body.
pub fn feishu_sign(secret: &str, ts_s: u64) -> String {
    use base64::Engine;
    let m = Hmac::<Sha256>::new_from_slice(format!("{ts_s}\n{secret}").as_bytes())
        .expect("any key length");
    base64::engine::general_purpose::STANDARD.encode(m.finalize().into_bytes())
}

/// DingTalk "加签": `timestamp` and `sign = base64(hmac_sha256(secret, "{timestamp}\n{secret}"))`, URL-encoded.
pub fn dingtalk_sign(secret: &str, ts_ms: u64) -> String {
    use base64::Engine;
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    m.update(format!("{ts_ms}\n{secret}").as_bytes());
    let b64 = base64::engine::general_purpose::STANDARD.encode(m.finalize().into_bytes());
    crate::forge::http::enc(&b64)
}

impl RobotNotifier {
    pub fn new(cfg: &Config, kind: Kind) -> Self {
        let robot = match kind {
            Kind::FeishuBot => cfg.notify.feishu_bot.clone(),
            Kind::DingTalk => cfg.notify.dingtalk.clone(),
            Kind::WeCom => cfg.notify.wecom.clone(),
            Kind::Slack => cfg.notify.slack.clone(),
        };
        RobotNotifier {
            kind,
            robot,
            lang: cfg.language.clone(),
            dry_run: cfg.dry_run,
            outcome: SystemNotifier::new(cfg),
        }
    }

    /// The JSON body for one message.
    pub fn body(&self, title: &str, text: &str, url: &str) -> Value {
        let open = if self.lang == "en" { "Open" } else { "打开" };
        match self.kind {
            Kind::FeishuBot => {
                // a rich-text ("post") message: the title, the text, and a link line
                let mut lines = vec![json!([{"tag": "text", "text": text}])];
                if !url.is_empty() {
                    lines.push(json!([{"tag": "a", "text": open, "href": url}]));
                }
                let mut b = json!({"msg_type": "post", "content": {"post": {"zh_cn": {"title": title, "content": lines}}}});
                if !self.robot.secret.is_empty() {
                    let ts = now() as u64;
                    b["timestamp"] = json!(ts.to_string());
                    b["sign"] = json!(feishu_sign(&self.robot.secret, ts));
                }
                b
            }
            Kind::DingTalk => {
                let link = if url.is_empty() {
                    String::new()
                } else {
                    format!("\n\n[{open}]({url})")
                };
                json!({"msgtype": "markdown", "markdown": {"title": title, "text": format!("### {title}\n\n{text}{link}")}})
            }
            Kind::WeCom => {
                let link = if url.is_empty() {
                    String::new()
                } else {
                    format!("\n[{open}]({url})")
                };
                json!({"msgtype": "markdown", "markdown": {"content": format!("**{title}**\n{text}{link}")}})
            }
            Kind::Slack => {
                let link = if url.is_empty() {
                    String::new()
                } else {
                    format!("\n<{url}|{open}>")
                };
                json!({"text": format!("*{title}*\n{text}{link}")})
            }
        }
    }

    fn url(&self) -> String {
        if self.kind == Kind::DingTalk && !self.robot.secret.is_empty() {
            let ts = (now() * 1000.0) as u64;
            let sep = if self.robot.url.contains('?') {
                '&'
            } else {
                '?'
            };
            return format!(
                "{}{sep}timestamp={ts}&sign={}",
                self.robot.url,
                dingtalk_sign(&self.robot.secret, ts)
            );
        }
        self.robot.url.clone()
    }

    pub fn send(&self, title: &str, text: &str, link: &str) -> Result<()> {
        if self.dry_run {
            crate::info!("[dry-run] {} {title}", self.kind.name());
            return Ok(());
        }
        let url = self.url();
        let resp = ureq::post(&url)
            .timeout(Duration::from_secs(20))
            .send_json(self.body(title, text, link));
        let text = match resp {
            Ok(r) => r.into_string().unwrap_or_default(),
            Err(ureq::Error::Status(code, r)) => {
                bail!(
                    "{} returned HTTP {code}: {}",
                    self.kind.name(),
                    r.into_string().unwrap_or_default()
                )
            }
            // the URL carries the robot's token: never put it in an error (they reach the log and the window)
            Err(e) => bail!("{}: {}", self.kind.name(), redact_url(&e.to_string(), &url)),
        };
        // DingTalk and WeCom answer 200 with {"errcode": n, "errmsg": …}; Feishu with {"code": n, "msg": …}; Slack "ok"
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if v["errcode"].as_i64().is_some_and(|c| c != 0) {
                bail!("{} refused the message: {}", self.kind.name(), v["errmsg"]);
            }
            if v["code"].as_i64().is_some_and(|c| c != 0) {
                bail!(
                    "{} refused the message: {} {}",
                    self.kind.name(),
                    v["code"],
                    v["msg"]
                );
            }
        }
        Ok(())
    }
}

/// `text` with `url` (and its query string anywhere) replaced by the bare host.
pub fn redact_url(text: &str, url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("robot");
    let mut out = text.replace(url, &format!("https://{host}/…"));
    if let Some((_, q)) = url.split_once('?') {
        out = out.replace(q, "…");
    }
    out
}

impl Notifier for RobotNotifier {
    fn update(&self, key: &str, e: &mut Entry, _st: Option<&PrStatus>) -> Result<bool> {
        let Some((sig, title, text, url)) = self.outcome.message(key, e) else {
            return Ok(true);
        };
        let slot = format!("{}_sig", self.kind.name());
        if e.extra.get(&slot).and_then(Value::as_str) == Some(sig.as_str()) {
            return Ok(true);
        }
        self.send(&title, &text, &url)?;
        e.extra.insert(slot, Value::String(sig));
        Ok(true)
    }

    fn alert(&self, text: &str) -> Result<()> {
        self.send(&t(&self.lang, "card.alert_title"), text, "")
    }

    fn test(&self) -> Result<String> {
        self.send(
            &t(&self.lang, "card.test_title"),
            &t(&self.lang, "robot.test_body"),
            "",
        )?;
        Ok("sent".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::http::fake;

    fn cfg(url: &str) -> Config {
        let mut c = Config {
            language: "en".into(),
            ..Default::default()
        };
        c.notify.dingtalk.url = format!("{url}/robot/send?access_token=t");
        c.notify.dingtalk.secret = "SEC".into();
        c.notify.wecom.url = format!("{url}/cgi-bin/webhook/send?key=k");
        c.notify.slack.url = format!("{url}/services/T/B/X");
        c
    }

    #[test]
    fn each_robot_gets_its_own_format_once_per_outcome() {
        let f = fake::serve(vec![
            ("POST /robot/send", 200, json!({"errcode": 0}).to_string()),
            (
                "POST /cgi-bin/webhook/send",
                200,
                json!({"errcode": 0}).to_string(),
            ),
            ("POST /services/T/B/X", 200, "ok".into()),
        ]);
        let c = cfg(&f.url);
        let mut e = Entry {
            status: "posted".into(),
            event: Some("COMMENT".into()),
            sha: "abc".into(),
            title: "Fix".into(),
            url: "https://github.com/a/b/pull/1".into(),
            ..Default::default()
        };
        for k in [Kind::DingTalk, Kind::WeCom, Kind::Slack] {
            let n = RobotNotifier::new(&c, k);
            n.update("a/b#1", &mut e, None).unwrap();
            n.update("a/b#1", &mut e, None).unwrap(); // unchanged: not sent again
        }
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].path.contains("timestamp=") && seen[0].path.contains("&sign="));
        assert!(
            seen[0].body.contains("\"msgtype\":\"markdown\"") && seen[0].body.contains("pull/1")
        );
        assert!(seen[1].body.contains("\"content\""));
        assert!(seen[2]
            .body
            .contains("<https://github.com/a/b/pull/1|Open>"));
    }

    #[test]
    fn refused_messages_are_errors() {
        let f = fake::serve(vec![(
            "POST /robot/send",
            200,
            json!({"errcode": 310000, "errmsg": "sign not match"}).to_string(),
        )]);
        let n = RobotNotifier::new(&cfg(&f.url), Kind::DingTalk);
        let err = n.test().unwrap_err().to_string();
        assert!(err.contains("sign not match"));
    }

    #[test]
    fn transport_errors_do_not_reveal_the_robot_token() {
        let mut c = cfg("http://127.0.0.1:9");
        c.notify.wecom.url = "http://127.0.0.1:9/cgi-bin/webhook/send?key=SECRET-KEY".into();
        let err = RobotNotifier::new(&c, Kind::WeCom)
            .test()
            .unwrap_err()
            .to_string();
        assert!(!err.contains("SECRET-KEY"), "{err}");
        assert!(err.starts_with("wecom:"));
    }

    #[test]
    fn feishu_group_bot_posts_rich_text_and_signs_in_the_body() {
        let f = fake::serve(vec![(
            "POST /open-apis/bot/v2/hook/abc",
            200,
            json!({"code": 0, "msg": "success"}).to_string(),
        )]);
        let mut c = cfg(&f.url);
        c.notify.feishu_bot.url = format!("{}/open-apis/bot/v2/hook/abc", f.url);
        c.notify.feishu_bot.secret = "S3".into();
        let n = RobotNotifier::new(&c, Kind::FeishuBot);
        n.send("Reviewed", "looks fine", "https://github.com/a/b/pull/1")
            .unwrap();
        let body: Value = serde_json::from_str(&f.seen.lock().unwrap()[0].body).unwrap();
        assert_eq!(body["msg_type"], "post");
        let post = &body["content"]["post"]["zh_cn"];
        assert_eq!(post["title"], "Reviewed");
        assert_eq!(
            post["content"][1][0]["href"],
            "https://github.com/a/b/pull/1"
        );
        let ts: u64 = body["timestamp"].as_str().unwrap().parse().unwrap();
        assert_eq!(body["sign"], feishu_sign("S3", ts));
    }

    #[test]
    fn feishu_refusals_are_errors() {
        let f = fake::serve(vec![("POST /hook", 200, json!({"code": 19021, "msg": "sign match fail or timestamp is not within one hour from current time"}).to_string())]);
        let mut c = cfg(&f.url);
        c.notify.feishu_bot.url = format!("{}/hook", f.url);
        let err = RobotNotifier::new(&c, Kind::FeishuBot)
            .test()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("19021") && err.contains("sign match fail"),
            "{err}"
        );
    }

    #[test]
    fn feishu_signature_matches_the_documented_algorithm() {
        // HmacSHA256 keyed with "timestamp\nsecret" over an empty message, base64
        use base64::Engine;
        let mut m = Hmac::<Sha256>::new_from_slice(b"1599360473\ndemo").unwrap();
        m.update(b"");
        let want = base64::engine::general_purpose::STANDARD.encode(m.finalize().into_bytes());
        assert_eq!(feishu_sign("demo", 1599360473), want);
    }

    #[test]
    fn dingtalk_signature_is_url_encoded_base64() {
        let s = dingtalk_sign("SEC", 1_700_000_000_000);
        assert!(!s.contains('+') && !s.contains('/') && !s.contains('='));
        assert!(s.ends_with("%3D"));
    }
}
