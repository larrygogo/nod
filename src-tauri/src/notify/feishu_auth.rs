//! Who gets the Feishu cards, found by signing in instead of typing an ID: the OAuth device flow with the user's own
//! Feishu app (the flow `lark-cli auth login` uses). Nod shows a link, the user approves in the browser, and the
//! sign-in yields their open_id and name. Only the open_id is kept; the token is used once and dropped.
//!
//! 1. `POST {accounts}/oauth/v1/device_authorization` (form: client_id, client_secret) → device_code, user_code,
//!    verification_uri_complete, interval, expires_in;
//! 2. `POST {open}/open-apis/authen/v2/oauth/token` (grant_type device_code) until the user approved;
//! 3. `GET {open}/open-apis/authen/v1/user_info` with that token → open_id, name.

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

/// The sign-in page host for an open platform address: Feishu or Lark.
pub fn accounts_host(domain: &str) -> &'static str {
    if domain.contains("larksuite") {
        "https://accounts.larksuite.com"
    } else {
        "https://accounts.feishu.cn"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    /// The page to open: the code is already filled in.
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

fn body(r: std::result::Result<ureq::Response, ureq::Error>) -> Result<Value> {
    let text = match r {
        Ok(x) => x.into_string()?,
        Err(ureq::Error::Status(_, x)) => x.into_string().unwrap_or_default(),
        Err(e) => return Err(anyhow!("cannot reach Feishu: {e}")),
    };
    serde_json::from_str(&text).map_err(|_| {
        anyhow!(
            "Feishu answered with something else than JSON: {}",
            crate::util::truncate(text.trim(), 200)
        )
    })
}

pub fn start(domain: &str, app_id: &str, secret: &str) -> Result<DeviceCode> {
    if app_id.trim().is_empty() || secret.trim().is_empty() {
        return Err(anyhow!("fill in the app ID and app secret first"));
    }
    let url = format!("{}/oauth/v1/device_authorization", accounts_host(domain));
    let v = body(
        ureq::post(&url)
            .timeout(std::time::Duration::from_secs(20))
            .send_form(&[
                ("client_id", app_id.trim()),
                ("client_secret", secret.trim()),
            ]),
    )?;
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    if s("device_code").is_empty() {
        return Err(anyhow!(
            "Feishu did not start the sign-in: {} {}",
            s("error"),
            s("error_description")
        ));
    }
    let uri = match s("verification_uri_complete") {
        u if u.is_empty() => s("verification_uri"),
        u => u,
    };
    Ok(DeviceCode {
        device_code: s("device_code"),
        user_code: s("user_code"),
        verification_uri: uri,
        interval: v["interval"].as_u64().unwrap_or(5).max(1),
        expires_in: v["expires_in"].as_u64().unwrap_or(600),
    })
}

#[derive(Debug, PartialEq)]
pub enum Poll {
    Pending,
    SlowDown,
    /// open_id, the user's name
    Done(String, String),
    /// "expired", "denied", or what went wrong
    Failed(String),
}

/// What the token endpoint's answer means.
pub fn read_token(v: &Value) -> std::result::Result<String, Poll> {
    if let Some(t) = v["access_token"]
        .as_str()
        .or(v["data"]["access_token"].as_str())
        .filter(|t| !t.is_empty())
    {
        return Ok(t.to_string());
    }
    let err = v["error"].as_str().unwrap_or_default();
    Err(match err {
        "authorization_pending" => Poll::Pending,
        "slow_down" => Poll::SlowDown,
        "expired_token" => Poll::Failed("expired".into()),
        "access_denied" => Poll::Failed("denied".into()),
        _ => Poll::Failed(format!(
            "{} {}",
            err,
            v["error_description"]
                .as_str()
                .or(v["msg"].as_str())
                .unwrap_or_default()
        )),
    })
}

pub fn poll(domain: &str, app_id: &str, secret: &str, device_code: &str) -> Result<Poll> {
    let open = domain.trim_end_matches('/');
    let v = body(
        ureq::post(&format!("{open}/open-apis/authen/v2/oauth/token"))
            .timeout(std::time::Duration::from_secs(20))
            .send_json(json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                "client_id": app_id.trim(),
                "client_secret": secret.trim(),
                "device_code": device_code,
            })),
    )?;
    let token = match read_token(&v) {
        Ok(t) => t,
        Err(p) => return Ok(p),
    };
    let u = body(
        ureq::get(&format!("{open}/open-apis/authen/v1/user_info"))
            .timeout(std::time::Duration::from_secs(20))
            .set("Authorization", &format!("Bearer {token}"))
            .call(),
    )?;
    let open_id = u["data"]["open_id"].as_str().unwrap_or_default();
    if open_id.is_empty() {
        return Ok(Poll::Failed(format!(
            "signed in, but Feishu did not say who: {}",
            u["msg"].as_str().unwrap_or_default()
        )));
    }
    Ok(Poll::Done(
        open_id.to_string(),
        u["data"]["name"].as_str().unwrap_or_default().to_string(),
    ))
}

// ------------------------------------------------------------------ connecting an app (no ID or secret to type)

/// Connect a Feishu app without typing its ID and secret: the flow of `lark-cli config init --new`. The page lets the
/// user pick one of their apps or create one; Nod gets its ID and secret and the user's open_id.
/// `POST https://accounts.feishu.cn/oauth/v1/app/registration` (form), `action=begin` then `action=poll`; the begin
/// request always goes to Feishu, the poll moves to Lark when the user's tenant is there.
const REGISTRATION: &str = "/oauth/v1/app/registration";

pub fn connect_start() -> Result<DeviceCode> {
    let v = body(
        ureq::post(&format!("{}{REGISTRATION}", accounts_host("")))
            .timeout(std::time::Duration::from_secs(20))
            .send_form(&[
                ("action", "begin"),
                ("archetype", "PersonalAgent"),
                ("auth_method", "client_secret"),
                ("request_user_info", "open_id tenant_brand"),
            ]),
    )?;
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    if s("device_code").is_empty() {
        return Err(anyhow!(
            "Feishu did not start the connection: {} {}",
            s("error"),
            s("error_description")
        ));
    }
    let uri = match s("verification_uri_complete") {
        u if u.is_empty() => format!(
            "https://open.feishu.cn/page/cli?user_code={}",
            s("user_code")
        ),
        u => u,
    };
    Ok(DeviceCode {
        device_code: s("device_code"),
        user_code: s("user_code"),
        verification_uri: uri,
        interval: v["interval"].as_u64().unwrap_or(5).max(1),
        expires_in: v["expires_in"]
            .as_u64()
            .or(v["expire_in"].as_u64())
            .unwrap_or(600),
    })
}

/// The app the user connected.
#[derive(Debug, PartialEq)]
pub struct Connected {
    pub app_id: String,
    pub secret: String,
    pub open_id: String,
    /// the open platform address for the user's tenant
    pub domain: String,
}

#[derive(Debug, PartialEq)]
pub enum ConnectPoll {
    Pending,
    SlowDown,
    Done(Connected),
    /// "expired", "denied", or what went wrong
    Failed(String),
}

/// What one answer of the registration poll means.
pub fn read_connect(v: &Value) -> ConnectPoll {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    if !s("client_id").is_empty() && !s("client_secret").is_empty() {
        let lark = v["user_info"]["tenant_brand"].as_str() == Some("lark");
        return ConnectPoll::Done(Connected {
            app_id: s("client_id"),
            secret: s("client_secret"),
            open_id: v["user_info"]["open_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            domain: if lark {
                "https://open.larksuite.com"
            } else {
                "https://open.feishu.cn"
            }
            .into(),
        });
    }
    match s("error").as_str() {
        "" | "authorization_pending" => ConnectPoll::Pending,
        "slow_down" => ConnectPoll::SlowDown,
        "access_denied" => ConnectPoll::Failed("denied".into()),
        "expired_token" | "invalid_grant" => ConnectPoll::Failed("expired".into()),
        e => ConnectPoll::Failed(format!("{e} {}", s("error_description"))),
    }
}

pub fn connect_poll(device_code: &str) -> Result<ConnectPoll> {
    let host = accounts_host("");
    let v = body(
        ureq::post(&format!("{host}{REGISTRATION}"))
            .timeout(std::time::Duration::from_secs(20))
            .send_form(&[("action", "poll"), ("device_code", device_code)]),
    )?;
    Ok(read_connect(&v))
}

fn get_json(url: &str, token: &str) -> Option<Value> {
    ureq::get(url)
        .timeout(std::time::Duration::from_secs(10))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .ok()?
        .into_json::<Value>()
        .ok()
}

/// What the window shows for a connected app and its recipient: names instead of IDs.
#[derive(Debug, Default, serde::Serialize)]
pub struct Who {
    /// the app's name; empty when it cannot be read
    pub app_name: String,
    /// the recipient's name; empty when the app may not read it
    pub recipient_name: String,
    /// why the app cannot be used, when its token is refused (a wrong secret, a deleted app)
    pub problem: String,
}

/// The app's name and the recipient's, read with the app's own token. The app name comes from its bot (every app
/// that can send cards has one), the recipient's from the contact directory when the app may read it.
pub fn who(domain: &str, app_id: &str, secret: &str, recipient: &str) -> Who {
    let open = domain.trim_end_matches('/');
    let open = if open.is_empty() {
        "https://open.feishu.cn"
    } else {
        open
    };
    let c = super::feishu::FeishuClient::new(app_id, secret, open);
    let token = match c.token() {
        Ok(t) => t,
        Err(e) => {
            return Who {
                problem: format!("{e:#}"),
                ..Who::default()
            }
        }
    };
    let app_name = get_json(&format!("{open}/open-apis/bot/v3/info"), &token)
        .and_then(|v| v["bot"]["app_name"].as_str().map(str::to_string))
        .unwrap_or_default();
    let recipient = crate::config::feishu_recipient(recipient);
    let recipient_name = match recipient.split_once(':') {
        Some((t @ ("open_id" | "union_id" | "user_id"), id)) if !id.is_empty() => get_json(
            &format!("{open}/open-apis/contact/v3/users/{id}?user_id_type={t}"),
            &token,
        )
        .and_then(|v| v["data"]["user"]["name"].as_str().map(str::to_string))
        .unwrap_or_default(),
        _ => String::new(),
    };
    Who {
        app_name,
        recipient_name,
        problem: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes taken from a real connection (2026-11-26) and from lark-cli's client.
    #[test]
    fn connect_answers() {
        let done = json!({"client_id": "cli_a", "client_secret": "s",
            "user_info": {"open_id": "ou_1", "tenant_brand": "feishu"}});
        assert_eq!(
            read_connect(&done),
            ConnectPoll::Done(Connected {
                app_id: "cli_a".into(),
                secret: "s".into(),
                open_id: "ou_1".into(),
                domain: "https://open.feishu.cn".into()
            })
        );
        let lark = json!({"client_id": "cli_a", "client_secret": "s",
            "user_info": {"open_id": "ou_1", "tenant_brand": "lark"}});
        assert!(
            matches!(read_connect(&lark), ConnectPoll::Done(c) if c.domain == "https://open.larksuite.com")
        );
        assert_eq!(
            read_connect(&json!({"error": "authorization_pending"})),
            ConnectPoll::Pending
        );
        assert_eq!(read_connect(&json!({})), ConnectPoll::Pending);
        assert_eq!(
            read_connect(&json!({"error": "slow_down"})),
            ConnectPoll::SlowDown
        );
        assert_eq!(
            read_connect(&json!({"error": "access_denied"})),
            ConnectPoll::Failed("denied".into())
        );
        assert_eq!(
            read_connect(&json!({"error": "invalid_grant"})),
            ConnectPoll::Failed("expired".into())
        );
    }

    #[test]
    fn token_answers() {
        assert_eq!(
            read_token(&json!({"access_token": "u-1"})),
            Ok("u-1".into())
        );
        assert_eq!(
            read_token(&json!({"error": "authorization_pending"})),
            Err(Poll::Pending)
        );
        assert_eq!(
            read_token(&json!({"error": "slow_down"})),
            Err(Poll::SlowDown)
        );
        assert_eq!(
            read_token(&json!({"error": "access_denied"})),
            Err(Poll::Failed("denied".into()))
        );
        assert_eq!(
            read_token(&json!({"error": "expired_token"})),
            Err(Poll::Failed("expired".into()))
        );
        assert!(
            matches!(read_token(&json!({"error": "invalid_client", "error_description": "bad secret"})),
            Err(Poll::Failed(m)) if m.contains("bad secret"))
        );
        assert_eq!(
            accounts_host("https://open.larksuite.com"),
            "https://accounts.larksuite.com"
        );
        assert_eq!(
            accounts_host("https://open.feishu.cn"),
            "https://accounts.feishu.cn"
        );
    }

    /// `NOD_LIVE_FEISHU=1 cargo test live_feishu_who -- --ignored --nocapture`: the names for the configured app.
    #[test]
    #[ignore]
    fn live_feishu_who() {
        if std::env::var("NOD_LIVE_FEISHU").as_deref() != Ok("1") {
            return;
        }
        let cfg = crate::config::load().unwrap();
        let f = &cfg.notify.feishu;
        eprintln!(
            "WHO {:?}",
            who(&f.domain, &f.app_id, &cfg.feishu_secret(), &f.recipient)
        );
        let me = "open_id:ou_fb6cdf9afcbe227b9fe755b97f907529";
        eprintln!(
            "ME {:?}",
            who(&f.domain, &f.app_id, &cfg.feishu_secret(), me)
        );
    }

    /// `NOD_LIVE_FEISHU=1 cargo test live_feishu_sign_in -- --ignored --nocapture`: prints a link to approve, then
    /// waits up to ten minutes and prints who signed in.
    #[test]
    #[ignore]
    fn live_feishu_sign_in() {
        if std::env::var("NOD_LIVE_FEISHU").as_deref() != Ok("1") {
            return;
        }
        let cfg = crate::config::load().unwrap();
        let (d, id, secret) = (
            &cfg.notify.feishu.domain,
            &cfg.notify.feishu.app_id,
            cfg.feishu_secret(),
        );
        let c = start(d, id, &secret).unwrap();
        eprintln!("OPEN {} (code {})", c.verification_uri, c.user_code);
        for _ in 0..120 {
            std::thread::sleep(std::time::Duration::from_secs(c.interval));
            let p = poll(d, id, &secret, &c.device_code).unwrap();
            eprintln!("POLL {p:?}");
            if !matches!(p, Poll::Pending | Poll::SlowDown) {
                break;
            }
        }
    }
}
