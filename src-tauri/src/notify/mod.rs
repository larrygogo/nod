//! Notification channels: `system` (desktop notifications), `feishu` (interactive cards), `webhook` (JSON events),
//! `dingtalk` / `wecom` / `slack` (chat robots) and external plugins. Several can be on at once.

pub mod feishu;
pub mod feishu_ws;
pub mod robot;
pub mod system;
pub mod webhook;

use std::sync::Arc;

use anyhow::{bail, Result};
use serde_json::Value;

use crate::config::Config;
use crate::engine::Entry;
use crate::forge::PrStatus;

/// Who clicked, and on which message (from the Feishu callback).
#[derive(Debug, Clone, Default)]
pub struct ClickIds {
    pub open_id: String,
    pub union_id: String,
    pub user_id: String,
    pub message_id: String,
    pub chat_id: String,
}

/// Called for a button click; returns a replacement card (or `None`). Errors are shown to the user as a toast.
pub type OnAction<'a> = &'a (dyn Fn(Value, ClickIds) -> Result<Option<Value>> + Sync);

pub trait Notifier: Send + Sync {
    /// Show the entry's current state. Implementations deduplicate unchanged content themselves.
    fn update(&self, _key: &str, _e: &mut Entry, _st: Option<&PrStatus>) -> Result<bool> {
        Ok(true)
    }
    fn alert(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    fn test(&self) -> Result<String> {
        Ok("no notification channel configured".into())
    }
    fn supports_actions(&self) -> bool {
        false
    }
    /// Blocking: deliver button clicks until the connection ends.
    fn listen(&self, _on_action: OnAction) -> Result<()> {
        bail!("this channel has no buttons")
    }
    /// Whether a click may approve on the user's behalf.
    fn may_act(&self, _ids: &ClickIds, _e: &Entry) -> bool {
        false
    }
    fn render_for(&self, _key: &str, _e: &Entry, _st: Option<&PrStatus>) -> Option<Value> {
        None
    }
}

pub struct NoNotify;
impl Notifier for NoNotify {}

/// One channel by name.
pub fn make_one(cfg: &Config, me: &str, channel: &str) -> Result<Arc<dyn Notifier>> {
    Ok(match channel {
        "feishu" => Arc::new(feishu::FeishuNotifier::new(cfg, me)),
        "webhook" => Arc::new(webhook::WebhookNotifier::new(cfg)),
        "system" => Arc::new(system::SystemNotifier::new(cfg)),
        "dingtalk" => Arc::new(robot::RobotNotifier::new(cfg, robot::Kind::DingTalk)),
        "wecom" => Arc::new(robot::RobotNotifier::new(cfg, robot::Kind::WeCom)),
        "slack" => Arc::new(robot::RobotNotifier::new(cfg, robot::Kind::Slack)),
        c if c.starts_with("ext:") => {
            Arc::new(crate::plugins::ExternalNotifier::new(cfg, &c[4..])?)
        }
        "none" => Arc::new(NoNotify),
        c => bail!("unknown notification channel {c:?}"),
    })
}

/// Every enabled channel, as one notifier.
pub fn make(cfg: &Config, me: &str) -> Arc<dyn Notifier> {
    let mut all: Vec<Arc<dyn Notifier>> = vec![];
    for ch in cfg.notify.active() {
        match make_one(cfg, me, &ch) {
            Ok(n) => all.push(n),
            Err(e) => crate::warn!("notification channel {ch}: {e:#}"),
        }
    }
    match all.len() {
        0 => Arc::new(NoNotify),
        1 => all.pop().unwrap(),
        _ => Arc::new(Multi(all)),
    }
}

/// Several channels at once. Buttons ("Approve now") come from the first channel that has them.
pub struct Multi(pub Vec<Arc<dyn Notifier>>);

impl Multi {
    fn with_actions(&self) -> Option<&Arc<dyn Notifier>> {
        self.0.iter().find(|n| n.supports_actions())
    }
}

impl Notifier for Multi {
    fn update(&self, key: &str, e: &mut Entry, st: Option<&PrStatus>) -> Result<bool> {
        // every channel gets its turn even if an earlier one fails; each remembers what it already sent
        let mut first_err = None;
        let mut delivered = true;
        for n in &self.0 {
            match n.update(key, e, st) {
                Ok(d) => delivered &= d,
                Err(err) => {
                    first_err.get_or_insert(err);
                }
            }
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(delivered),
        }
    }
    fn alert(&self, text: &str) -> Result<()> {
        let errs: Vec<String> = self
            .0
            .iter()
            .filter_map(|n| n.alert(text).err().map(|e| e.to_string()))
            .collect();
        if errs.is_empty() {
            Ok(())
        } else {
            bail!("{}", errs.join("; "))
        }
    }
    fn test(&self) -> Result<String> {
        let mut out = vec![];
        for n in &self.0 {
            out.push(n.test()?);
        }
        Ok(out.join("; "))
    }
    fn supports_actions(&self) -> bool {
        self.with_actions().is_some()
    }
    fn listen(&self, on_action: OnAction) -> Result<()> {
        match self.with_actions() {
            Some(n) => n.listen(on_action),
            None => bail!("no channel with buttons"),
        }
    }
    fn may_act(&self, ids: &ClickIds, e: &Entry) -> bool {
        self.with_actions().is_some_and(|n| n.may_act(ids, e))
    }
    fn render_for(&self, key: &str, e: &Entry, st: Option<&PrStatus>) -> Option<Value> {
        self.with_actions().and_then(|n| n.render_for(key, e, st))
    }
}
