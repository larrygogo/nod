//! Signing in to GitHub through Nod's GitHub App with the device flow, and keeping the user access token fresh.
//!
//! The device flow needs only the App's client ID (public, no client secret), and GitHub lets a token obtained this
//! way be refreshed without a secret as well, so nothing confidential ships in the binary. The token acts as the user:
//! reviews are posted in their name and "review requested from me" keeps its meaning. It reaches only repositories
//! where the App is installed.
//!
//! Tokens are kept in the system keychain as one JSON value per account, so the window (which signs in) and the
//! background worker (which uses the token) share them.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::now;

/// Client ID of the Nod GitHub App on github.com. GitHub Enterprise accounts register their own App and set the
/// account's `client_id` setting.
pub const DEFAULT_CLIENT_ID: &str = "";

/// URL name (slug) of the Nod GitHub App on github.com, for the "install it on more accounts" link.
pub const DEFAULT_APP_SLUG: &str = "";

/// Refresh this long before the access token expires.
const REFRESH_MARGIN: f64 = 300.0;

/// Marker in errors that mean "sign in again"; the supervisor and the self-check recognise it.
pub const SIGNIN_REQUIRED: &str = "github_signin_required";

/// Web and API addresses of a GitHub host.
#[derive(Debug, Clone, PartialEq)]
pub struct Hosts {
    /// `https://github.com` or `https://github.example.com`: device flow and token endpoints live here.
    pub web: String,
    /// `https://api.github.com` or `https://github.example.com/api/v3`
    pub api: String,
    /// `https://api.github.com/graphql` or `https://github.example.com/api/graphql`
    pub graphql: String,
}

impl Hosts {
    /// `url` is the account's address: empty (or github.com) for github.com, otherwise the Enterprise web address.
    pub fn new(url: &str) -> Hosts {
        let host = url
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        if host.is_empty() || host == "github.com" {
            Hosts {
                web: "https://github.com".into(),
                api: "https://api.github.com".into(),
                graphql: "https://api.github.com/graphql".into(),
            }
        } else if host.ends_with(".ghe.com") {
            // GitHub Enterprise Cloud with data residency: the API has its own subdomain
            Hosts {
                web: format!("https://{host}"),
                api: format!("https://api.{host}"),
                graphql: format!("https://api.{host}/graphql"),
            }
        } else {
            Hosts {
                web: format!("https://{host}"),
                api: format!("https://{host}/api/v3"),
                graphql: format!("https://{host}/api/graphql"),
            }
        }
    }

    /// All three on one test server.
    #[cfg(test)]
    pub fn at(base: &str) -> Hosts {
        Hosts {
            web: base.into(),
            api: base.into(),
            graphql: format!("{base}/graphql"),
        }
    }

    /// The page where the App is installed on more accounts or organizations (without a known App: the list of
    /// installed Apps).
    pub fn install_url(&self, app_slug: &str) -> String {
        if app_slug.is_empty() {
            format!("{}/settings/installations", self.web)
        } else {
            format!("{}/apps/{app_slug}/installations/new", self.web)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// Unix seconds; 0 = the token does not expire (the App opted out of expiring tokens).
    pub expires_at: f64,
    pub refresh_expires_at: f64,
    /// Who signed in, for display.
    pub login: String,
    /// The host (`Hosts::web`) and App the tokens were issued for. Changing the account's address or client ID
    /// must not send them to another server: a mismatch counts as signed out.
    pub host: String,
    pub client_id: String,
}

impl Tokens {
    fn from_response(v: &Value, at: f64) -> Result<Tokens> {
        let access = v["access_token"].as_str().unwrap_or_default();
        if access.is_empty() {
            bail!("GitHub returned no access token");
        }
        let secs = |k: &str| {
            v[k].as_f64()
                .filter(|s| *s > 0.0)
                .map(|s| at + s)
                .unwrap_or(0.0)
        };
        Ok(Tokens {
            access: access.into(),
            refresh: v["refresh_token"].as_str().unwrap_or_default().into(),
            expires_at: secs("expires_in"),
            refresh_expires_at: secs("refresh_token_expires_in"),
            ..Default::default()
        })
    }

    /// Record which host and App the tokens belong to.
    pub fn bind(mut self, hosts: &Hosts, client_id: &str) -> Tokens {
        self.host = hosts.web.clone();
        self.client_id = client_id.trim().to_string();
        self
    }

    /// Issued for this host and App (tokens without a recorded binding are accepted).
    pub fn fits(&self, hosts: &Hosts, client_id: &str) -> bool {
        (self.host.is_empty() || self.host == hosts.web)
            && (self.client_id.is_empty() || self.client_id == client_id.trim())
    }

    fn needs_refresh(&self, at: f64) -> bool {
        self.expires_at > 0.0 && self.expires_at - at < REFRESH_MARGIN
    }

    /// Both the access token and the refresh token have run out: only signing in again helps.
    pub fn dead(&self, at: f64) -> bool {
        self.expires_at > 0.0
            && self.expires_at <= at
            && (self.refresh.is_empty()
                || (self.refresh_expires_at > 0.0 && self.refresh_expires_at <= at))
    }
}

/// The keychain entry for an account's tokens.
fn key(account: &str) -> String {
    format!("forge.{account}.github")
}

/// Where the tokens go when the system keychain cannot be used (e.g. Linux without a Secret Service), like the
/// other secrets fall back to config.toml.
fn fallback_file(account: &str) -> std::path::PathBuf {
    crate::paths::data_dir().join(format!("github-{account}.json"))
}

pub fn load(account: &str) -> Option<Tokens> {
    let parse = |s: String| serde_json::from_str::<Tokens>(&s).ok();
    let kc = crate::secrets::get_value(&key(account)).and_then(parse);
    let file = if crate::secrets::is_fake() {
        None
    } else {
        std::fs::read_to_string(fallback_file(account))
            .ok()
            .and_then(parse)
    };
    // Both exist when a keychain write failed once after a refresh: the file then holds the newer pair, and the
    // keychain's refresh token has already been spent. Take whichever expires later.
    newest(kc, file)
}

fn newest(a: Option<Tokens>, b: Option<Tokens>) -> Option<Tokens> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.expires_at > a.expires_at { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// Write the fallback file readable by this user only: it holds a live token.
fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    std::io::Write::write_all(&mut o.open(path)?, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        // an older file kept its mode
    }
    Ok(())
}

pub fn save(account: &str, t: &Tokens) -> Result<()> {
    let text = serde_json::to_string(t)?;
    match crate::secrets::set_value(&key(account), &text) {
        Ok(()) => {
            let _ = std::fs::remove_file(fallback_file(account));
            Ok(())
        }
        Err(e) => {
            crate::warn!(
                "github: keychain unavailable ({e:#}); keeping the sign-in in the data folder"
            );
            write_private(&fallback_file(account), &text)
        }
    }
}

pub fn forget(account: &str) {
    crate::secrets::forget_value(&key(account));
    if !crate::secrets::is_fake() {
        let _ = std::fs::remove_file(fallback_file(account));
    }
}
fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Value> {
    let r = ureq::post(url)
        .timeout(Duration::from_secs(30))
        .set("Accept", "application/json")
        .set("User-Agent", concat!("nod/", env!("CARGO_PKG_VERSION")))
        .send_form(form);
    match r {
        Ok(resp) => Ok(resp.into_json()?),
        // the OAuth endpoints answer errors as JSON with 200 or 4xx; keep the body for the caller
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            serde_json::from_str(&body).map_err(|_| {
                anyhow!(
                    "POST {url}: HTTP {code}: {}",
                    crate::util::truncate(&body, 200)
                )
            })
        }
        Err(e) => Err(anyhow!("POST {url}: {e}")),
    }
}

fn client_id_or_err(client_id: &str) -> Result<&str> {
    if client_id.trim().is_empty() {
        bail!("{SIGNIN_REQUIRED}: no GitHub App client ID is configured for this account");
    }
    Ok(client_id.trim())
}

/// A started device-flow sign-in: show `user_code` and send the user to `verification_uri`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds to wait between polls.
    pub interval: u64,
    pub expires_in: u64,
}

pub fn start(hosts: &Hosts, client_id: &str) -> Result<DeviceCode> {
    let cid = client_id_or_err(client_id)?;
    let v = post_form(
        &format!("{}/login/device/code", hosts.web),
        &[("client_id", cid)],
    )?;
    if let Some(err) = v["error"].as_str() {
        bail!("{}", describe_error(err, &v));
    }
    Ok(DeviceCode {
        device_code: v["device_code"].as_str().unwrap_or_default().into(),
        user_code: v["user_code"].as_str().unwrap_or_default().into(),
        verification_uri: v["verification_uri"]
            .as_str()
            .unwrap_or("https://github.com/login/device")
            .into(),
        interval: v["interval"].as_u64().unwrap_or(5),
        expires_in: v["expires_in"].as_u64().unwrap_or(900),
    })
}

/// One answer to a device-flow poll.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    /// The user has not finished yet; poll again after the interval.
    Pending,
    /// Polled too fast: wait this many seconds between polls from now on.
    SlowDown(u64),
    Done(Tokens),
    /// The code expired or the user said no; start over. Carries a readable reason.
    Failed(String),
}

fn describe_error(err: &str, v: &Value) -> String {
    match err {
        "device_flow_disabled" => {
            "the GitHub App does not have Device Flow enabled (App settings → Enable Device Flow)"
                .into()
        }
        "incorrect_client_credentials" => "the GitHub App client ID is wrong".into(),
        _ => v["error_description"].as_str().unwrap_or(err).to_string(),
    }
}

pub fn poll(hosts: &Hosts, client_id: &str, device_code: &str) -> Result<Poll> {
    let cid = client_id_or_err(client_id)?;
    let at = now();
    let v = post_form(
        &format!("{}/login/oauth/access_token", hosts.web),
        &[
            ("client_id", cid),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    )?;
    Ok(match v["error"].as_str() {
        None => Poll::Done(Tokens::from_response(&v, at)?),
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown(v["interval"].as_u64().unwrap_or(10)),
        Some("expired_token") => Poll::Failed("expired".into()),
        Some("access_denied") => Poll::Failed("denied".into()),
        Some(e) => Poll::Failed(describe_error(e, &v)),
    })
}

/// Exchange a refresh token for a new pair (refresh tokens are single use).
pub fn refresh(hosts: &Hosts, client_id: &str, t: &Tokens) -> Result<Tokens> {
    let cid = client_id_or_err(client_id)?;
    if t.refresh.is_empty() {
        bail!("{SIGNIN_REQUIRED}: the GitHub sign-in expired; sign in again in Nod");
    }
    let at = now();
    let v = post_form(
        &format!("{}/login/oauth/access_token", hosts.web),
        &[
            ("client_id", cid),
            ("grant_type", "refresh_token"),
            ("refresh_token", &t.refresh),
        ],
    )?;
    if let Some(e) = v["error"].as_str() {
        bail!(
            "{SIGNIN_REQUIRED}: the GitHub sign-in could not be renewed ({}); sign in again in Nod",
            describe_error(e, &v)
        );
    }
    let mut n = Tokens::from_response(&v, at)?;
    n.login = t.login.clone();
    Ok(n)
}

/// How often a cached token is checked against the keychain, so signing out or switching accounts in the window
/// reaches the worker even while its token is still valid.
const RECHECK_SECS: f64 = 30.0;

/// The current access token of one account, refreshed when it is about to expire. Refresh tokens are single use, so
/// a refresh happens under the session's lock (callers in this process; `shared` gives one session per account)
/// and a lock file (the window and the worker are separate processes), and the keychain is read again inside both.
pub struct Session {
    pub account: String,
    pub hosts: Hosts,
    pub client_id: String,
    /// (tokens, when they were last read from the keychain)
    cached: Mutex<Option<(Tokens, f64)>>,
}

/// One session per account, host and App in this process.
pub fn shared(account: &str, hosts: Hosts, client_id: &str) -> std::sync::Arc<Session> {
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};
    type Key = (String, String, String); // account, host, client ID
    static ALL: OnceLock<Mutex<HashMap<Key, Arc<Session>>>> = OnceLock::new();
    let k = (
        account.to_string(),
        hosts.web.clone(),
        client_id.trim().to_string(),
    );
    ALL.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(k)
        .or_insert_with(|| Arc::new(Session::new(account, hosts, client_id)))
        .clone()
}

/// A lock file held while refreshing, shared by every Nod process of this user. A lock left behind by a crash is
/// taken over after a minute.
struct RefreshLock(std::path::PathBuf);

impl RefreshLock {
    fn acquire(account: &str, web: &str) -> Result<RefreshLock> {
        use sha2::{Digest, Sha256};
        // per OS user: a shared /tmp must not make one user's crash block another
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let h = Sha256::digest(format!("{user}|{account}|{web}").as_bytes());
        let name: String = h.iter().take(6).map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("nod-github-refresh-{name}.lock"));
        let until = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(RefreshLock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(60));
                    // a lock left by a crash is taken over; if it cannot be removed, keep waiting like any other
                    if stale && std::fs::remove_file(&path).is_ok() {
                        continue;
                    }
                    if std::time::Instant::now() > until {
                        // (no "timed out" here: the error classifier would call it a network problem)
                        bail!("another Nod process is still renewing the GitHub sign-in; it will be tried again shortly");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Session {
    pub fn new(account: &str, hosts: Hosts, client_id: &str) -> Session {
        Session {
            account: account.into(),
            hosts,
            client_id: client_id.trim().into(),
            cached: Mutex::new(None),
        }
    }

    /// The stored tokens if they belong to this host and App.
    fn stored(&self) -> Result<Tokens> {
        load(&self.account)
            .filter(|t| !t.access.is_empty() && t.fits(&self.hosts, &self.client_id))
            .ok_or_else(|| {
                anyhow!(
                    "{SIGNIN_REQUIRED}: not signed in to GitHub; sign in on the GitHub page in Nod"
                )
            })
    }

    pub fn token(&self) -> Result<String> {
        let mut c = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        let at = now();
        if let Some((t, read_at)) = c.as_ref() {
            if !t.needs_refresh(at) && at - read_at < RECHECK_SECS {
                return Ok(t.access.clone());
            }
        }
        // signed out, switched, or refreshed by another process since: the keychain decides
        let stored = match self.stored() {
            Ok(t) => t,
            Err(e) => {
                *c = None;
                return Err(e);
            }
        };
        if !stored.needs_refresh(at) {
            *c = Some((stored.clone(), at));
            return Ok(stored.access);
        }
        let _lock = RefreshLock::acquire(&self.account, &self.hosts.web)?;
        // another process may have renewed it while we waited for the lock
        let stored = self.stored()?;
        if !stored.needs_refresh(now()) {
            *c = Some((stored.clone(), now()));
            return Ok(stored.access);
        }
        let fresh = match refresh(&self.hosts, &self.client_id, &stored) {
            Ok(t) => t.bind(&self.hosts, &self.client_id),
            Err(e) => {
                *c = None;
                return Err(e);
            }
        };
        save(&self.account, &fresh)?;
        *c = Some((fresh.clone(), now()));
        Ok(fresh.access)
    }

    /// Drop the cached token (after a 401), so the next call re-reads the keychain.
    pub fn invalidate(&self) {
        *self.cached.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
#[cfg(test)]
mod tests {
    use super::super::http::fake;
    use super::*;
    use serde_json::json;

    #[test]
    fn hosts() {
        let h = Hosts::new("");
        assert_eq!(
            (h.web.as_str(), h.api.as_str()),
            ("https://github.com", "https://api.github.com")
        );
        assert_eq!(Hosts::new("https://github.com/"), h);
        let e = Hosts::new("https://git.corp.example/");
        assert_eq!(e.api, "https://git.corp.example/api/v3");
        assert_eq!(e.graphql, "https://git.corp.example/api/graphql");
        let d = Hosts::new("octo.ghe.com");
        assert_eq!(
            (d.api.as_str(), d.graphql.as_str()),
            (
                "https://api.octo.ghe.com",
                "https://api.octo.ghe.com/graphql"
            )
        );
        assert_eq!(
            e.install_url("nod"),
            "https://git.corp.example/apps/nod/installations/new"
        );
    }

    #[test]
    fn device_flow() {
        let f = fake::serve(vec![(
            "POST /login/device/code",
            200,
            json!({"device_code": "dc", "user_code": "ABCD-1234", "verification_uri": "https://github.com/login/device", "interval": 5, "expires_in": 899}).to_string(),
        )]);
        let h = Hosts::at(&f.url);
        let d = start(&h, "Iv1.x").unwrap();
        assert_eq!((d.user_code.as_str(), d.interval), ("ABCD-1234", 5));
        assert!(f.seen.lock().unwrap()[0].body.contains("client_id=Iv1.x"));
        assert!(start(&h, " ")
            .unwrap_err()
            .to_string()
            .contains(SIGNIN_REQUIRED));
    }

    fn poll_with(body: Value) -> Poll {
        let f = fake::serve(vec![(
            "POST /login/oauth/access_token",
            200,
            body.to_string(),
        )]);
        let p = poll(&Hosts::at(&f.url), "cid", "dc").unwrap();
        let sent = f.seen.lock().unwrap()[0].body.clone();
        assert!(sent.contains("device_code=dc") && sent.contains("grant_type=urn"));
        p
    }

    #[test]
    fn poll_answers() {
        assert_eq!(
            poll_with(json!({"error": "authorization_pending"})),
            Poll::Pending
        );
        assert_eq!(
            poll_with(json!({"error": "slow_down", "interval": 10})),
            Poll::SlowDown(10)
        );
        assert_eq!(
            poll_with(json!({"error": "expired_token"})),
            Poll::Failed("expired".into())
        );
        assert_eq!(
            poll_with(json!({"error": "access_denied"})),
            Poll::Failed("denied".into())
        );
        match poll_with(json!({"error": "device_flow_disabled"})) {
            Poll::Failed(m) => assert!(m.contains("Device Flow")),
            p => panic!("{p:?}"),
        }
        match poll_with(
            json!({"access_token": "ghu_a", "expires_in": 28800, "refresh_token": "ghr_r", "refresh_token_expires_in": 15897600}),
        ) {
            Poll::Done(t) => {
                assert_eq!((t.access.as_str(), t.refresh.as_str()), ("ghu_a", "ghr_r"));
                assert!(t.expires_at > now() + 28000.0);
            }
            p => panic!("{p:?}"),
        }
        // an App that opted out of expiring tokens
        match poll_with(json!({"access_token": "ghu_b"})) {
            Poll::Done(t) => assert_eq!(t.expires_at, 0.0),
            p => panic!("{p:?}"),
        }
    }

    #[test]
    fn session_refreshes_once_and_reports_sign_in() {
        crate::secrets::use_fake();
        let f = fake::serve(vec![(
            "POST /login/oauth/access_token",
            200,
            json!({"access_token": "ghu_new", "expires_in": 28800, "refresh_token": "ghr_new"})
                .to_string(),
        )]);
        let s = Session::new("gh1", Hosts::at(&f.url), "cid");
        assert!(s.token().unwrap_err().to_string().contains(SIGNIN_REQUIRED));

        // still valid: used as is, no request
        save(
            "gh1",
            &Tokens {
                access: "ghu_ok".into(),
                refresh: "ghr_1".into(),
                expires_at: now() + 3600.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.token().unwrap(), "ghu_ok");
        assert!(f.seen.lock().unwrap().is_empty());

        // about to expire: refreshed once, saved, then served from cache
        s.invalidate();
        save(
            "gh1",
            &Tokens {
                access: "ghu_old".into(),
                refresh: "ghr_1".into(),
                expires_at: now() + 60.0,
                login: "me".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.token().unwrap(), "ghu_new");
        assert_eq!(s.token().unwrap(), "ghu_new");
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].body.contains("grant_type=refresh_token")
                && seen[0].body.contains("refresh_token=ghr_1")
        );
        let stored = load("gh1").unwrap();
        assert_eq!(
            (
                stored.access.as_str(),
                stored.refresh.as_str(),
                stored.login.as_str()
            ),
            ("ghu_new", "ghr_new", "me")
        );
    }

    #[test]
    fn concurrent_callers_share_one_refresh() {
        let f = fake::serve(vec![(
            "POST /login/oauth/access_token",
            200,
            json!({"access_token": "ghu_new", "expires_in": 28800, "refresh_token": "ghr_new"})
                .to_string(),
        )]);
        let s = std::sync::Arc::new(Session::new("gh3", Hosts::at(&f.url), "cid"));
        let gate = std::sync::Arc::new(std::sync::Barrier::new(8));
        let hs: Vec<_> = (0..8)
            .map(|_| {
                let (s, gate) = (s.clone(), gate.clone());
                std::thread::spawn(move || {
                    // the fake keychain is per thread: give each the same stale tokens
                    crate::secrets::use_fake();
                    save(
                        "gh3",
                        &Tokens {
                            access: "ghu_old".into(),
                            refresh: "ghr_1".into(),
                            expires_at: now() + 10.0,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    gate.wait();
                    s.token().unwrap()
                })
            })
            .collect();
        for h in hs {
            assert_eq!(h.join().unwrap(), "ghu_new");
        }
        assert_eq!(
            f.seen.lock().unwrap().len(),
            1,
            "the single-use refresh token was spent more than once"
        );
    }

    #[test]
    fn signing_out_reaches_a_running_session() {
        crate::secrets::use_fake();
        let s = Session::new("gh4", Hosts::at("http://127.0.0.1:9"), "cid");
        save(
            "gh4",
            &Tokens {
                access: "ghu_a".into(),
                expires_at: now() + 3600.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.token().unwrap(), "ghu_a");
        forget("gh4");
        // within the recheck window the cached token is still used ...
        assert_eq!(s.token().unwrap(), "ghu_a");
        // ... after it, the keychain wins: signed out
        if let Some((_, read_at)) = s.cached.lock().unwrap().as_mut() {
            *read_at -= RECHECK_SECS + 1.0;
        }
        assert!(s.token().unwrap_err().to_string().contains(SIGNIN_REQUIRED));
        // and a switch to another user is picked up the same way
        save(
            "gh4",
            &Tokens {
                access: "ghu_b".into(),
                expires_at: now() + 3600.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.token().unwrap(), "ghu_b");
    }

    #[test]
    fn tokens_stay_with_their_host_and_app() {
        crate::secrets::use_fake();
        let gh = Hosts::new("");
        let t = Tokens {
            access: "ghu_a".into(),
            ..Default::default()
        }
        .bind(&gh, "Iv1.a");
        save("gh5", &t).unwrap();
        assert_eq!(
            Session::new("gh5", gh.clone(), "Iv1.a").token().unwrap(),
            "ghu_a"
        );
        // the address was changed to an Enterprise server, or another App configured: never send it there
        for s in [
            Session::new("gh5", Hosts::new("https://git.corp.example"), "Iv1.a"),
            Session::new("gh5", gh, "Iv1.other"),
        ] {
            assert!(s.token().unwrap_err().to_string().contains(SIGNIN_REQUIRED));
        }
    }

    #[test]
    fn dead_tokens_and_shared_sessions() {
        let at = now();
        let t = |exp: f64, rexp: f64, r: &str| Tokens {
            access: "a".into(),
            refresh: r.into(),
            expires_at: exp,
            refresh_expires_at: rexp,
            ..Default::default()
        };
        assert!(!t(0.0, 0.0, "").dead(at), "non-expiring tokens never die");
        assert!(
            !t(at - 1.0, at + 100.0, "r").dead(at),
            "can still be refreshed"
        );
        assert!(t(at - 1.0, at - 1.0, "r").dead(at));
        assert!(t(at - 1.0, 0.0, "").dead(at));
        let a = shared("gh6", Hosts::new(""), "cid");
        assert!(std::sync::Arc::ptr_eq(
            &a,
            &shared("gh6", Hosts::new(""), " cid ")
        ));
        assert!(!std::sync::Arc::ptr_eq(
            &a,
            &shared("gh6", Hosts::new("https://x.example"), "cid")
        ));
    }

    #[test]
    fn refresh_lock_is_exclusive_across_holders() {
        let first = RefreshLock::acquire("gh7", "https://lock.test").unwrap();
        let t = std::time::Instant::now();
        let h = std::thread::spawn(|| RefreshLock::acquire("gh7", "https://lock.test").map(|_| ()));
        std::thread::sleep(Duration::from_millis(300));
        drop(first);
        h.join().unwrap().unwrap();
        assert!(t.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn fallback_storage_is_private_and_newest_wins() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("sub").join("github-x.json");
        write_private(&f, "{}").unwrap();
        write_private(&f, "{\"access\":\"a\"}").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"access\":\"a\"}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let t = |a: &str, exp: f64| {
            Some(Tokens {
                access: a.into(),
                expires_at: exp,
                ..Default::default()
            })
        };
        assert_eq!(
            newest(t("kc", 10.0), t("file", 20.0)).unwrap().access,
            "file"
        );
        assert_eq!(newest(t("kc", 30.0), t("file", 20.0)).unwrap().access, "kc");
        assert_eq!(newest(None, t("file", 1.0)).unwrap().access, "file");
    }

    #[test]
    fn lock_wait_message_is_not_a_network_error() {
        // the waiting message must not read as a network problem
        let e = "another Nod process is still renewing the GitHub sign-in; it will be tried again shortly";
        assert!(!crate::supervisor::is_network(e));
    }

    #[test]
    fn failed_refresh_asks_to_sign_in_again() {
        crate::secrets::use_fake();
        let f = fake::serve(vec![(
            "POST /login/oauth/access_token",
            200,
            json!({"error": "bad_refresh_token", "error_description": "The refresh token passed is incorrect or expired."}).to_string(),
        )]);
        save(
            "gh2",
            &Tokens {
                access: "ghu_old".into(),
                refresh: "ghr_x".into(),
                expires_at: now() - 1.0,
                ..Default::default()
            },
        )
        .unwrap();
        let s = Session::new("gh2", Hosts::at(&f.url), "cid");
        let e = s.token().unwrap_err().to_string();
        assert!(
            e.contains(SIGNIN_REQUIRED) && e.contains("incorrect or expired"),
            "{e}"
        );
    }
}
