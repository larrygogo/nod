//! Chat apps reached through an incoming-webhook URL: Feishu / Lark group bots, DingTalk, WeCom (企业微信) and Slack
//! as channels of their own, and these plus Discord, Microsoft Teams, Google Chat, Telegram, Bark, Server酱 and ntfy
//! when their address is given to the generic Webhook channel (see `detect`).
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
    Discord,
    Teams,
    GoogleChat,
    Telegram,
    Bark,
    ServerChan,
    Ntfy,
}

/// The app behind a webhook address, if it is one Nod knows the message format of.
pub fn detect(url: &str) -> Option<Kind> {
    let u = url.trim().to_ascii_lowercase();
    let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(&u);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(':').next().unwrap_or(host); // drop a port
    let path = format!("/{path}");
    let on = |h: &str| host == h || host.ends_with(&format!(".{h}"));
    Some(
        if (on("feishu.cn") || on("larksuite.com") || on("larkoffice.com"))
            && path.starts_with("/open-apis/bot/")
        {
            Kind::FeishuBot
        } else if on("dingtalk.com") && path.starts_with("/robot/send") {
            Kind::DingTalk
        } else if on("qyapi.weixin.qq.com") && path.starts_with("/cgi-bin/webhook/send") {
            Kind::WeCom
        } else if on("hooks.slack.com") {
            Kind::Slack
        } else if (on("discord.com") || on("discordapp.com")) && path.starts_with("/api/webhooks/")
        {
            Kind::Discord
        } else if on("webhook.office.com")
            || ((on("logic.azure.com") || on("powerplatform.com")) && path.contains("/workflows/"))
        {
            Kind::Teams
        } else if on("chat.googleapis.com") {
            Kind::GoogleChat
        } else if on("api.telegram.org") && path.contains("/sendmessage") {
            Kind::Telegram
        } else if on("api.day.app") {
            Kind::Bark
        } else if on("ftqq.com") && path.ends_with(".send") {
            Kind::ServerChan
        } else if host == "ntfy.sh" || host.starts_with("ntfy.") {
            Kind::Ntfy
        } else {
            return None;
        },
    )
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::FeishuBot => "feishu_bot",
            Kind::DingTalk => "dingtalk",
            Kind::WeCom => "wecom",
            Kind::Slack => "slack",
            Kind::Discord => "discord",
            Kind::Teams => "teams",
            Kind::GoogleChat => "google_chat",
            Kind::Telegram => "telegram",
            Kind::Bark => "bark",
            Kind::ServerChan => "serverchan",
            Kind::Ntfy => "ntfy",
        }
    }
}

pub struct RobotNotifier {
    kind: Kind,
    robot: Robot,
    /// Where it remembers per PR what it last sent (its own channel's, or the Webhook channel's).
    slot: String,
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
            // only reached through the Webhook channel (`for_webhook`)
            _ => Robot::default(),
        };
        RobotNotifier {
            kind,
            robot,
            slot: format!("{}_sig", kind.name()),
            lang: cfg.language.clone(),
            dry_run: cfg.dry_run,
            outcome: SystemNotifier::new(cfg),
        }
    }

    /// The Webhook channel's address, sent in the format of the app it belongs to. The Webhook secret signs for
    /// the apps that sign (Feishu, DingTalk).
    pub fn for_webhook(cfg: &Config, kind: Kind) -> Self {
        let mut n = RobotNotifier::new(cfg, kind);
        n.robot = Robot {
            url: cfg.notify.webhook.url.trim().to_string(),
            secret: cfg.webhook_secret(),
        };
        n.slot = "webhook_app_sig".into();
        n
    }

    pub fn kind(&self) -> Kind {
        self.kind
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
            Kind::Discord => {
                let mut embed = json!({"title": title, "description": text, "color": 0x4F46E5});
                if !url.is_empty() {
                    embed["url"] = json!(url);
                }
                json!({"username": "Nod", "embeds": [embed]})
            }
            Kind::Teams => {
                // a Workflows ("When a Teams webhook request is received") or connector URL: an Adaptive Card
                let mut card = json!({"$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "type": "AdaptiveCard",
                    "version": "1.4", "body": [
                        {"type": "TextBlock", "text": title, "weight": "Bolder", "size": "Medium", "wrap": true},
                        {"type": "TextBlock", "text": text, "wrap": true}]});
                if !url.is_empty() {
                    card["actions"] =
                        json!([{"type": "Action.OpenUrl", "title": open, "url": url}]);
                }
                json!({"type": "message", "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}]})
            }
            Kind::GoogleChat => {
                let link = if url.is_empty() {
                    String::new()
                } else {
                    format!("\n<{url}|{open}>")
                };
                json!({"text": format!("*{title}*\n{text}{link}")})
            }
            Kind::Telegram => {
                let link = if url.is_empty() {
                    String::new()
                } else {
                    format!("\n\n{url}")
                };
                json!({"text": format!("{title}\n\n{text}{link}"), "disable_web_page_preview": true})
            }
            Kind::Bark => {
                let mut b = json!({"title": title, "body": text, "group": "Nod"});
                if !url.is_empty() {
                    b["url"] = json!(url);
                }
                b
            }
            Kind::ServerChan => {
                let link = if url.is_empty() {
                    String::new()
                } else {
                    format!("\n\n[{open}]({url})")
                };
                json!({"title": title, "desp": format!("{text}{link}")})
            }
            Kind::Ntfy => {
                // JSON publishing goes to the server root with the topic in the body
                let topic = self
                    .robot
                    .url
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let mut b =
                    json!({"topic": topic, "title": title, "message": text, "tags": ["mag"]});
                if !url.is_empty() {
                    b["click"] = json!(url);
                }
                b
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
        if self.kind == Kind::Ntfy {
            // https://ntfy.sh/topic → https://ntfy.sh/ (JSON publishing)
            let u = self.robot.url.trim_end_matches('/');
            if let Some((base, _topic)) = u.rsplit_once('/') {
                if base.contains("://") && base.split("://").nth(1).is_some_and(|r| !r.is_empty()) {
                    return format!("{base}/");
                }
            }
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
        // Several apps answer HTTP 200 and say in the body whether they took the message.
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(why) = refused(self.kind, &v) {
                bail!("{} refused the message: {why}", self.kind.name());
            }
        }
        Ok(())
    }
}

/// What an app says when it answered HTTP 200 but did not take the message.
fn refused(kind: Kind, v: &Value) -> Option<String> {
    let code = |k: &str| v[k].as_i64();
    match kind {
        // {"errcode": n, "errmsg": …}
        Kind::DingTalk | Kind::WeCom => code("errcode")
            .filter(|c| *c != 0)
            .map(|_| v["errmsg"].to_string()),
        // {"code": n, "msg": …} (older bots: {"StatusCode": n, "StatusMessage": …})
        Kind::FeishuBot => code("code")
            .filter(|c| *c != 0)
            .map(|c| format!("{c} {}", v["msg"]))
            .or_else(|| {
                code("StatusCode")
                    .filter(|c| *c != 0)
                    .map(|c| format!("{c} {}", v["StatusMessage"]))
            }),
        Kind::ServerChan => code("code")
            .filter(|c| *c != 0)
            .map(|c| format!("{c} {}", v["message"])),
        Kind::Bark => code("code")
            .filter(|c| *c != 200)
            .map(|c| format!("{c} {}", v["message"])),
        Kind::Telegram => (v["ok"] == false).then(|| v["description"].to_string()),
        _ => None,
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
        if e.extra.get(&self.slot).and_then(Value::as_str) == Some(sig.as_str()) {
            return Ok(true);
        }
        self.send(&title, &text, &url)?;
        e.extra.insert(self.slot.clone(), Value::String(sig));
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
    fn recognises_common_webhook_addresses() {
        for (url, want) in [
            ("https://open.feishu.cn/open-apis/bot/v2/hook/0455f0b7", Some(Kind::FeishuBot)),
            ("https://open.larksuite.com/open-apis/bot/v2/hook/x", Some(Kind::FeishuBot)),
            ("https://oapi.dingtalk.com/robot/send?access_token=t", Some(Kind::DingTalk)),
            ("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=k", Some(Kind::WeCom)),
            ("https://hooks.slack.com/services/T/B/X", Some(Kind::Slack)),
            ("https://discord.com/api/webhooks/1/abc", Some(Kind::Discord)),
            ("https://discordapp.com/api/webhooks/1/abc", Some(Kind::Discord)),
            ("https://acme.webhook.office.com/webhookb2/x", Some(Kind::Teams)),
            ("https://prod-12.westus.logic.azure.com:443/workflows/abc/triggers/manual/paths/invoke?sig=s", Some(Kind::Teams)),
            ("https://default1.environment.api.powerplatform.com/powerautomate/automations/direct/workflows/abc/triggers/manual/paths/invoke", Some(Kind::Teams)),
            ("https://chat.googleapis.com/v1/spaces/AAA/messages?key=k&token=t", Some(Kind::GoogleChat)),
            ("https://api.telegram.org/bot123:ABC/sendMessage?chat_id=42", Some(Kind::Telegram)),
            ("https://api.day.app/KEY", Some(Kind::Bark)),
            ("https://sctapi.ftqq.com/SCT123.send", Some(Kind::ServerChan)),
            ("https://ntfy.sh/nod-reviews", Some(Kind::Ntfy)),
            ("https://ntfy.example.com/team", Some(Kind::Ntfy)),
            ("https://example.com/hooks/nod", None),
            ("https://evil.example.com/open-apis/bot/v2/hook/x", None),
        ] {
            assert_eq!(detect(url), want, "{url}");
        }
    }

    #[test]
    fn webhook_apps_get_their_own_formats() {
        let mut c = cfg("http://unused");
        let body = |url: &str, c: &mut Config| {
            c.notify.webhook.url = url.into();
            let n = RobotNotifier::for_webhook(c, detect(url).unwrap());
            (n.body("T", "x", "https://github.com/a/b/pull/1"), n.url())
        };
        let (b, _) = body("https://discord.com/api/webhooks/1/a", &mut c);
        assert_eq!(b["embeds"][0]["url"], "https://github.com/a/b/pull/1");
        let (b, _) = body("https://acme.webhook.office.com/webhookb2/x", &mut c);
        assert_eq!(
            b["attachments"][0]["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        assert_eq!(
            b["attachments"][0]["content"]["actions"][0]["url"],
            "https://github.com/a/b/pull/1"
        );
        let (b, _) = body(
            "https://chat.googleapis.com/v1/spaces/A/messages?key=k",
            &mut c,
        );
        assert!(b["text"]
            .as_str()
            .unwrap()
            .contains("<https://github.com/a/b/pull/1|Open>"));
        let (b, _) = body(
            "https://api.telegram.org/bot1:A/sendMessage?chat_id=4",
            &mut c,
        );
        assert!(b["text"].as_str().unwrap().starts_with("T\n\nx"));
        let (b, _) = body("https://api.day.app/KEY", &mut c);
        assert_eq!(
            (b["title"].as_str(), b["url"].as_str()),
            (Some("T"), Some("https://github.com/a/b/pull/1"))
        );
        let (b, _) = body("https://sctapi.ftqq.com/SCT1.send", &mut c);
        assert!(b["desp"]
            .as_str()
            .unwrap()
            .contains("[Open](https://github.com/a/b/pull/1)"));
        let (b, u) = body("https://ntfy.sh/nod-reviews", &mut c);
        assert_eq!(
            (b["topic"].as_str(), u.as_str()),
            (Some("nod-reviews"), "https://ntfy.sh/")
        );
        assert_eq!(b["click"], "https://github.com/a/b/pull/1");
    }

    #[test]
    fn apps_that_answer_200_can_still_refuse() {
        assert!(refused(Kind::Bark, &json!({"code": 200, "message": "success"})).is_none());
        assert!(refused(
            Kind::Bark,
            &json!({"code": 400, "message": "device key not found"})
        )
        .is_some());
        assert!(refused(
            Kind::Telegram,
            &json!({"ok": false, "description": "chat not found"})
        )
        .unwrap()
        .contains("chat not found"));
        assert!(refused(Kind::Telegram, &json!({"ok": true})).is_none());
        assert!(refused(
            Kind::ServerChan,
            &json!({"code": 40001, "message": "bad sendkey"})
        )
        .is_some());
        assert!(refused(
            Kind::FeishuBot,
            &json!({"StatusCode": 0, "StatusMessage": "success"})
        )
        .is_none());
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
