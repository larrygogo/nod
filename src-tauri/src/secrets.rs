//! Tokens, app secrets and robot webhook URLs live in the system keychain (Windows Credential Manager, macOS
//! Keychain, Secret Service on Linux), not in config.toml.
//!
//! `stash` moves every secret field of a configuration into the keychain and blanks it in the copy that is written to
//! disk; `hydrate` fills blank fields back from the keychain after loading. If the keychain cannot be used (e.g. a
//! Linux box without a Secret Service), the value stays in config.toml, as before, and `stash` reports it.

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;

use crate::config::{Config, ForgeAccount};

/// Keychain service name. Side-by-side development instances (NOD_CONFIG_DIR set) get their own namespace so
/// they never read or overwrite the real installation's secrets.
fn service() -> String {
    match std::env::var("NOD_CONFIG_DIR") {
        Ok(d) if !d.is_empty() => {
            use sha2::{Digest, Sha256};
            let h = Sha256::digest(d.as_bytes());
            format!(
                "nod-dev-{}",
                h.iter()
                    .take(4)
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        }
        _ => "nod".into(),
    }
}

thread_local! {
    /// Tests use an in-memory keychain instead of the real one.
    static FAKE: RefCell<Option<HashMap<String, String>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub fn fake_put(name: &str, value: &str) {
    FAKE.with(|f| {
        if let Some(m) = f.borrow_mut().as_mut() {
            m.insert(name.to_string(), value.to_string());
        }
    });
}

#[cfg(test)]
pub fn use_fake() {
    FAKE.with(|f| *f.borrow_mut() = Some(HashMap::new()));
}

/// Tests use the in-memory keychain on this thread: nothing may touch real files or the real keychain.
pub fn is_fake() -> bool {
    FAKE.with(|f| f.borrow().is_some())
}

fn get(name: &str) -> Option<String> {
    if let Some(v) = FAKE.with(|f| f.borrow().as_ref().map(|m| m.get(name).cloned())) {
        return v;
    }
    keyring::Entry::new(&service(), name)
        .ok()?
        .get_password()
        .ok()
        .filter(|s| !s.is_empty())
}

fn set(name: &str, value: &str) -> Result<()> {
    let faked = FAKE.with(|f| {
        f.borrow_mut().as_mut().map(|m| {
            m.insert(name.to_string(), value.to_string());
        })
    });
    if faked.is_some() {
        return Ok(());
    }
    keyring::Entry::new(&service(), name)?.set_password(value)?;
    Ok(())
}

fn forget(name: &str) {
    let faked = FAKE.with(|f| f.borrow_mut().as_mut().map(|m| m.remove(name)));
    if faked.is_some() {
        return;
    }
    if let Ok(e) = keyring::Entry::new(&service(), name) {
        let _ = e.delete_credential();
    }
}

/// A value kept only in the keychain (not mirrored from a config field), e.g. GitHub sign-in tokens.
pub fn get_value(name: &str) -> Option<String> {
    get(name)
}

pub fn set_value(name: &str, value: &str) -> Result<()> {
    set(name, value)
}

pub fn forget_value(name: &str) {
    forget(name)
}

/// Copy every secret of `c` from another keychain service (the app's previous name) into this one, without
/// overwriting a secret already stored here. Returns how many were copied.
pub fn copy_from(old_service: &str, c: &Config) -> usize {
    let mut probe = c.clone();
    let names: Vec<String> = fields(&mut probe).into_iter().map(|(n, _)| n).collect();
    let mut n = 0;
    for name in names {
        if get(&name).is_some() {
            continue;
        }
        let old = FAKE
            .with(|f| {
                f.borrow()
                    .as_ref()
                    .map(|m| m.get(&format!("{old_service}/{name}")).cloned())
            })
            .unwrap_or_else(|| {
                keyring::Entry::new(old_service, &name)
                    .ok()
                    .and_then(|e| e.get_password().ok())
            })
            .filter(|s| !s.is_empty());
        if let Some(v) = old {
            if set(&name, &v).is_ok() {
                n += 1;
            }
        }
    }
    n
}

fn forge_key(a: &ForgeAccount) -> String {
    format!("forge.{}.token", a.id)
}

/// The token of a code-host account (keychain, or the file as a fallback).
pub fn forge_token(a: &ForgeAccount) -> String {
    if !a.token.is_empty() {
        return a.token.clone();
    }
    get(&forge_key(a)).unwrap_or_default()
}

/// Settings that third-party plugins declare as passwords: (plugin name, setting key, plugin kind).
fn plugin_secret_keys() -> Vec<(String, String, String)> {
    crate::plugins::external::discover()
        .0
        .into_iter()
        .flat_map(|m| {
            m.settings
                .iter()
                .filter(|s| s.r#type == "password")
                .map(|s| (m.name.clone(), s.key.clone(), m.kind.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Make sure every declared plugin secret has a string slot in the config, so it can be filled or blanked.
fn plugin_slots(c: &mut Config, keys: &[(String, String, String)]) {
    for (name, key, kind) in keys {
        if kind == "forge" {
            for a in c
                .forges
                .iter_mut()
                .filter(|a| a.kind == format!("ext:{name}"))
            {
                a.settings
                    .entry(key.clone())
                    .or_insert_with(|| toml::Value::String(String::new()));
            }
        } else {
            let t = c
                .plugins
                .entry(name.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(t) = t.as_table_mut() {
                t.entry(key.clone())
                    .or_insert_with(|| toml::Value::String(String::new()));
            }
        }
    }
}

/// Every secret field: (keychain name, field).
fn fields(c: &mut Config) -> Vec<(String, &mut String)> {
    fields_with(c, &plugin_secret_keys())
}

fn fields_with<'a>(
    c: &'a mut Config,
    keys: &[(String, String, String)],
) -> Vec<(String, &'a mut String)> {
    plugin_slots(c, keys);
    let n = &mut c.notify;
    let mut v: Vec<(String, &mut String)> = vec![
        ("notify.feishu.app_secret".into(), &mut n.feishu.app_secret),
        ("notify.webhook.secret".into(), &mut n.webhook.secret),
        ("notify.feishu_bot.url".into(), &mut n.feishu_bot.url),
        ("notify.feishu_bot.secret".into(), &mut n.feishu_bot.secret),
        ("notify.dingtalk.url".into(), &mut n.dingtalk.url),
        ("notify.dingtalk.secret".into(), &mut n.dingtalk.secret),
        ("notify.wecom.url".into(), &mut n.wecom.url),
        ("notify.slack.url".into(), &mut n.slack.url),
        ("review.api.key".into(), &mut c.review.api.key),
    ];
    for a in c.forges.iter_mut() {
        let k = forge_key(a);
        let id = a.id.clone();
        let kind = a.kind.clone();
        v.push((k, &mut a.token));
        for (key, val) in a.settings.iter_mut() {
            let declared = keys
                .iter()
                .any(|(n, k, t)| t == "forge" && kind == format!("ext:{n}") && k == key);
            if let (true, toml::Value::String(s)) = (declared, val) {
                v.push((format!("forge.{id}.settings.{key}"), s));
            }
        }
    }
    for (name, t) in c.plugins.iter_mut() {
        let Some(t) = t.as_table_mut() else { continue };
        for (key, val) in t.iter_mut() {
            let declared = keys
                .iter()
                .any(|(n, k, t)| t != "forge" && n == name && k == key);
            if let (true, toml::Value::String(s)) = (declared, val) {
                v.push((format!("plugin.{name}.{key}"), s));
            }
        }
    }
    v
}

/// What a secret belongs to. A stored secret is only reused for the same thing: if the API address, the webhook
/// address, the Feishu app or a code-host account's kind or address changed, the old secret is not sent there.
fn binding(c: &Config, name: &str) -> String {
    match name {
        "review.api.key" => c.review.api.base_url.trim_end_matches('/').to_string(),
        "notify.webhook.secret" => c.notify.webhook.url.clone(),
        "notify.feishu.app_secret" => {
            format!("{}|{}", c.notify.feishu.app_id, c.notify.feishu.domain)
        }
        "notify.dingtalk.secret" => c.notify.dingtalk.url.clone(),
        n if n.starts_with("forge.") => {
            let id = n
                .trim_start_matches("forge.")
                .split('.')
                .next()
                .unwrap_or("");
            c.forges
                .iter()
                .find(|a| a.id == id)
                .map(|a| format!("{}|{}", a.kind, a.url.trim_end_matches('/')))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Fill blank secret fields from the keychain.
pub fn hydrate(c: &mut Config) {
    for (name, field) in fields(c) {
        if field.is_empty() {
            if let Some(v) = get(&name) {
                *field = v;
            }
        }
    }
}

/// Move secrets into the keychain; returns the config to write to disk and the fields that had to stay in the file.
/// Secrets that are now blank (cleared by the user, or of a removed account) are deleted from the keychain.
pub fn stash(c: &Config, previous: &Config) -> (Config, Vec<String>) {
    let keys = plugin_secret_keys();
    let mut out = c.clone();
    let mut kept = vec![];
    let mut now = std::collections::HashSet::new();
    for (name, field) in fields_with(&mut out, &keys) {
        now.insert(name.clone());
        if field.is_empty() {
            forget(&name);
            continue;
        }
        match set(&name, field) {
            Ok(()) => field.clear(),
            Err(e) => {
                crate::warn!("keychain unavailable for {name}: {e}; keeping it in config.toml");
                kept.push(name);
            }
        }
    }
    // secrets of accounts or plugin settings that no longer exist
    let mut prev = previous.clone();
    for (name, _) in fields_with(&mut prev, &keys) {
        if !now.contains(&name) {
            forget(&name);
        }
    }
    (out, kept)
}

/// Which secret fields have a value (for the window), keyed by keychain name.
pub fn present(c: &Config) -> HashMap<String, bool> {
    let mut c = c.clone();
    hydrate(&mut c);
    fields(&mut c)
        .into_iter()
        .map(|(k, v)| (k, !v.is_empty()))
        .collect()
}

/// Copy of the config with every secret blanked (what the window may see).
pub fn redacted(c: &Config) -> Config {
    let mut c = c.clone();
    for (_, field) in fields(&mut c) {
        field.clear();
    }
    c
}

/// Fill blank secrets in `new` (from the window, which never sees them) with the current values, so saving a form
/// does not erase a secret the user did not retype — unless the user cleared it (`cleared`, keychain names) or what
/// it belongs to changed (see `binding`).
pub fn keep_unchanged(new: &mut Config, current: &Config, cleared: &[String]) {
    let mut cur = current.clone();
    hydrate(&mut cur);
    let keys = plugin_secret_keys();
    let cur_map: HashMap<String, String> = fields_with(&mut cur, &keys)
        .into_iter()
        .map(|(k, v)| (k, v.clone()))
        .collect();
    // Two passes: first the secrets that are not tied to anything (some of them, like the DingTalk URL, are what
    // another secret is tied to), then the tied ones, compared on the new config with the first pass filled in.
    for tied in [false, true] {
        let probe = new.clone();
        for (name, field) in fields_with(new, &keys) {
            if !field.is_empty() || cleared.contains(&name) {
                continue;
            }
            let b = binding(&probe, &name);
            if (b.is_empty() && binding(&cur, &name).is_empty()) == tied {
                continue;
            }
            if tied && b != binding(&cur, &name) {
                continue;
            }
            if let Some(v) = cur_map.get(&name) {
                *field = v.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_leave_the_file_and_come_back() {
        use_fake();
        let mut c = Config::default();
        c.notify.feishu.app_secret = "s3cret".into();
        c.notify.slack.url = "https://hooks.slack.com/services/T/B/X".into();
        c.forges.push(ForgeAccount {
            id: "gl".into(),
            kind: "gitee".into(),
            enabled: true,
            token: "gitee-tok-1".into(),
            ..Default::default()
        });
        let (disk, kept) = stash(&c, &Config::default());
        assert!(kept.is_empty());
        let text = crate::config::dumps(&disk);
        assert!(
            !text.contains("s3cret")
                && !text.contains("gitee-tok-1")
                && !text.contains("hooks.slack.com")
        );
        let mut back = crate::config::parse(&text).unwrap();
        hydrate(&mut back);
        assert_eq!(back.notify.feishu.app_secret, "s3cret");
        assert_eq!(forge_token(&back.forges[0]), "gitee-tok-1");
        // the window sends blanks for secrets it was not shown: they are kept
        let mut from_window = redacted(&back);
        from_window.language = "en".into();
        keep_unchanged(&mut from_window, &back, &[]);
        assert_eq!(
            from_window.notify.slack.url,
            "https://hooks.slack.com/services/T/B/X"
        );
        assert!(present(&disk)["notify.feishu.app_secret"]);
    }

    #[test]
    fn cleared_secrets_and_changed_targets_are_not_reused() {
        use_fake();
        let mut c = Config::default();
        c.review.api.base_url = "https://api.openai.com/v1".into();
        c.review.api.key = "sk-OLD".into();
        c.forges.push(ForgeAccount {
            id: "ge".into(),
            kind: "gitee".into(),
            enabled: true,
            token: "PAT-OLD".into(),
            ..Default::default()
        });
        let (disk, _) = stash(&c, &Config::default());
        // the API address changes and the key field is left blank: the old key must not follow
        let mut w = redacted(&disk);
        w.review.api.base_url = "http://127.0.0.1:1/v1".into();
        keep_unchanged(&mut w, &disk, &[]);
        assert_eq!(w.review.api.key, "");
        // the account is removed and another one with the same id but another kind is added
        let mut w = redacted(&disk);
        w.forges[0].kind = "gitea".into();
        w.forges[0].url = "http://127.0.0.1:2".into();
        keep_unchanged(&mut w, &disk, &[]);
        assert_eq!(w.forges[0].token, "");
        // an explicit clear removes the key from the keychain
        let mut w = redacted(&disk);
        keep_unchanged(&mut w, &disk, &["review.api.key".to_string()]);
        assert_eq!(w.review.api.key, "");
        assert_eq!(w.forges[0].token, "PAT-OLD", "untouched secrets are kept");
        let (disk2, _) = stash(&w, &disk);
        let mut back = disk2.clone();
        hydrate(&mut back);
        assert_eq!(back.review.api.key, "");
        // removing the account forgets its token
        let mut w = redacted(&disk2);
        w.forges.clear();
        keep_unchanged(&mut w, &disk2, &[]);
        let _ = stash(&w, &disk2);
        assert_eq!(get("forge.ge.token"), None);
    }

    /// The DingTalk signing secret is tied to the robot URL, which is itself a secret the window never sees:
    /// saving an unrelated setting must keep both; changing or clearing the URL drops the signing secret.
    #[test]
    fn dingtalk_signing_secret_survives_unrelated_saves() {
        use_fake();
        let mut cur = Config::default();
        cur.notify.dingtalk.url = "https://oapi.dingtalk.com/robot/send?access_token=T".into();
        cur.notify.dingtalk.secret = "SEC".into();
        let (disk, _) = stash(&cur, &cur);
        // what the window sends back: both secrets blank, another field changed
        let mut win = redacted(&disk);
        win.daemon.poll_seconds = 33.0;
        keep_unchanged(&mut win, &disk, &[]);
        assert_eq!(win.notify.dingtalk.url, cur.notify.dingtalk.url);
        assert_eq!(win.notify.dingtalk.secret, "SEC");

        let mut moved = redacted(&disk);
        moved.notify.dingtalk.url =
            "https://oapi.dingtalk.com/robot/send?access_token=OTHER".into();
        keep_unchanged(&mut moved, &disk, &[]);
        assert_eq!(
            moved.notify.dingtalk.secret, "",
            "a new robot does not get the old signing secret"
        );

        let mut gone = redacted(&disk);
        keep_unchanged(&mut gone, &disk, &["notify.dingtalk.url".into()]);
        assert_eq!(
            (
                gone.notify.dingtalk.url.as_str(),
                gone.notify.dingtalk.secret.as_str()
            ),
            ("", "")
        );
    }

    /// An installation from before plugins: one implicit GitHub account, a single `notify.channel`, Claude settings at
    /// the top of [review], the Feishu secret in plain text. It must load and behave the same, and saving moves the
    /// secret into the keychain without changing anything else.
    #[test]
    fn old_configurations_keep_working() {
        use_fake();
        let old = r#"
language = "zh-CN"
[review]
claude = "claude"
model = "claude-sonnet-5-5"
[approve]
mode = "auto"
[notify]
channel = "feishu"
[notify.feishu]
app_id = "cli_a"
app_secret = "PLAIN-SECRET"
recipient = "email:me@example.com"
"#;
        let c = crate::config::parse(old).unwrap();
        let c = c.validate().unwrap();
        let acc = c.accounts();
        assert_eq!(
            (acc.len(), acc[0].id.as_str(), acc[0].kind.as_str()),
            (1, "github", "github")
        );
        assert_eq!(c.notify.active(), vec!["feishu".to_string()]);
        assert_eq!(c.review.engine, "claude");
        assert_eq!(c.engine_for("acme/app", "me"), "claude");
        assert_eq!(
            crate::daemon::state_file("github").file_name().unwrap(),
            "state.json"
        );
        let (disk, kept) = stash(&c, &c);
        assert!(kept.is_empty());
        let text = crate::config::dumps(&disk);
        assert!(!text.contains("PLAIN-SECRET"));
        let mut back = crate::config::parse(&text).unwrap();
        hydrate(&mut back);
        assert_eq!(back.notify.feishu.app_secret, "PLAIN-SECRET");
        assert_eq!(back.notify.channel, "feishu");
        assert!(back.forges.is_empty(), "no account list is invented");
    }
}
