//! Feishu (Lark) bot: one interactive card per PR, updated in place; "Approve now" via a long connection.
//!
//! Each user needs their own Feishu app: a long connection delivers each click to only one connected client, so
//! several people sharing one app would receive each other's clicks at random.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::{ClickIds, Notifier, OnAction};
use crate::config::Config;
use crate::engine::Entry;
use crate::forge::PrStatus;
use crate::i18n::{reason_text, t, tf};
use crate::util::{hhmm, now, truncate};

pub struct FeishuClient {
    pub app_id: String,
    pub app_secret: String,
    pub domain: String,
    token: Mutex<(String, f64)>,
}

impl FeishuClient {
    pub fn new(app_id: &str, app_secret: &str, domain: &str) -> Self {
        FeishuClient {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            domain: domain.trim_end_matches('/').into(),
            token: Mutex::new((String::new(), 0.0)),
        }
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build()
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>, auth: bool) -> Result<Value> {
        let url = format!("{}/open-apis{path}", self.domain);
        let mut req = Self::agent()
            .request(method, &url)
            .set("Content-Type", "application/json; charset=utf-8");
        if auth {
            req = req.set("Authorization", &format!("Bearer {}", self.token()?));
        }
        let resp = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        let v: Value = match resp {
            Ok(r) => r.into_json()?,
            Err(ureq::Error::Status(_, r)) => r
                .into_json()
                .unwrap_or(json!({"code": -1, "msg": "http error"})),
            Err(e) => bail!("{method} {}: {e}", path.split('?').next().unwrap_or(path)),
        };
        if v["code"].as_i64().unwrap_or(0) != 0 {
            bail!(
                "{method} {}: code={} {}",
                path.split('?').next().unwrap_or(path),
                v["code"],
                v["msg"].as_str().unwrap_or("")
            );
        }
        Ok(v)
    }

    pub fn token(&self) -> Result<String> {
        let mut g = self.token.lock().unwrap();
        if !g.0.is_empty() && now() < g.1 - 120.0 {
            return Ok(g.0.clone());
        }
        let d = self.call(
            "POST",
            "/auth/v3/tenant_access_token/internal",
            Some(json!({"app_id": self.app_id, "app_secret": self.app_secret})),
            false,
        )?;
        let tok = d["tenant_access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("no tenant_access_token"))?
            .to_string();
        *g = (tok.clone(), now() + d["expire"].as_f64().unwrap_or(3600.0));
        Ok(tok)
    }

    /// Returns (message_id, chat_id).
    pub fn send_card(&self, recipient: &str, card: &Value) -> Result<(String, String)> {
        let (rtype, rid) = recipient
            .split_once(':')
            .ok_or_else(|| anyhow!("bad recipient {recipient:?}"))?;
        let d = self.call(
            "POST",
            &format!("/im/v1/messages?receive_id_type={rtype}"),
            Some(
                json!({"receive_id": rid, "msg_type": "interactive", "content": card.to_string()}),
            ),
            true,
        )?;
        Ok((
            d["data"]["message_id"].as_str().unwrap_or_default().into(),
            d["data"]["chat_id"].as_str().unwrap_or_default().into(),
        ))
    }

    pub fn update_card(&self, message_id: &str, card: &Value) -> Result<()> {
        self.call(
            "PATCH",
            &format!("/im/v1/messages/{message_id}"),
            Some(json!({"content": card.to_string()})),
            true,
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------- card rendering (card JSON 2.0)

fn tag(text: &str, color: &str) -> String {
    format!("<text_tag color='{color}'>{text}</text_tag>")
}
fn grey(text: &str) -> String {
    format!("<font color='grey'>{text}</font>")
}
fn md(content: &str, size: &str) -> Value {
    json!({"tag": "markdown", "content": content, "text_size": size})
}
fn sev_color(s: &str) -> &'static str {
    match s {
        "high" => "red",
        "medium" => "orange",
        "low" => "yellow",
        _ => "neutral",
    }
}

pub fn headline(lang: &str, e: &Entry, st: Option<&PrStatus>) -> (String, &'static str) {
    let kind = e.note.as_ref().map(|n| n.kind.as_str()).unwrap_or("");
    // The header says what Nod did; the PR's own state (merged, closed) has its own coloured tag below. Only a PR
    // that went away before Nod said anything takes its state as the headline.
    let posted = matches!(kind, "approve" | "ready" | "comment") || e.status == "direct_approved";
    match (st.map(|s| s.state.as_str()), e.status.as_str()) {
        (Some("merged"), _) if !posted => (t(lang, "status.merged"), "violet"),
        (Some("closed"), _) if !posted => (t(lang, "status.closed"), "grey"),
        (_, "direct_approved") => (t(lang, "status.direct"), "green"),
        (_, "queued" | "") => (t(lang, "status.queued"), "blue"),
        (_, "retry") if e.waits > 0 => (t(lang, "status.waiting"), "orange"),
        (_, "reviewing" | "reviewed" | "retry") => (t(lang, "status.reviewing"), "blue"),
        (_, "awaiting_ci") => (t(lang, "status.awaiting_ci"), "blue"),
        (_, "out_of_scope") => (t(lang, "status.out_of_scope"), "grey"),
        (_, s) => match kind {
            "approve" => (t(lang, "status.approved"), "green"),
            "ready" => (t(lang, "status.ready"), "turquoise"),
            "comment" => (t(lang, "status.issues"), "orange"),
            _ if kind == "manual" || s == "failed" => (t(lang, "status.manual"), "red"),
            _ => (s.to_string(), "grey"),
        },
    }
}

fn stat(label: &str, value: &str) -> Value {
    json!({"tag": "column", "width": "weighted", "weight": 1, "vertical_spacing": "4px",
           "elements": [md(&grey(label), "notation"), md(value, "normal")]})
}

fn short(s: &str) -> &str {
    &s[..7.min(s.len())]
}

pub fn render(
    lang: &str,
    key: &str,
    e: &Entry,
    st: Option<&PrStatus>,
    me: &str,
    direct_on_own: bool,
) -> Value {
    let (head_text, color) = headline(lang, e, st);
    let empty = PrStatus::default();
    let s = st.unwrap_or(&empty);
    let title = if s.title.is_empty() {
        &e.title
    } else {
        &s.title
    };
    let url = if s.url.is_empty() { &e.url } else { &s.url };
    let author = if s.author.is_empty() {
        &e.author
    } else {
        &s.author
    };
    let sha = e.sha.as_str();
    let mut els: Vec<Value> = vec![];

    if st.is_some() {
        let pr_state = match s.state.as_str() {
            // GitHub's colours: open green, merged purple, closed red, draft grey
            "merged" => tag(&t(lang, "status.merged"), "purple"),
            "closed" => tag(&t(lang, "status.closed"), "red"),
            _ if s.draft => tag(&t(lang, "card.draft"), "neutral"),
            _ => tag(&t(lang, "card.open"), "green"),
        };
        let dec_color = match s.decision.as_str() {
            "APPROVED" => "green",
            "CHANGES_REQUESTED" => "red",
            _ => "neutral",
        };
        let decision = tag(
            &if s.decision.is_empty() {
                "—".into()
            } else {
                t(lang, &format!("decision.{}", s.decision))
            },
            dec_color,
        );
        let ci_color = match s.ci.as_str() {
            "success" => "green",
            "failure" => "red",
            "pending" => "blue",
            _ => "neutral",
        };
        let ci = tag(
            &if s.ci.is_empty() {
                "—".into()
            } else {
                t(lang, &format!("ci.{}", s.ci))
            },
            ci_color,
        );
        let size = match (s.additions, s.deletions) {
            (Some(a), Some(d)) => format!(
                "<font color='green'>+{a}</font> <font color='red'>−{d}</font>{}",
                grey(&format!(
                    " · {}",
                    tf(
                        lang,
                        "card.files",
                        &[("n", &s.changed_files.unwrap_or(0).to_string())]
                    )
                ))
            ),
            _ => "—".into(),
        };
        els.push(json!({"tag": "column_set", "flex_mode": "bisect", "horizontal_spacing": "8px", "columns": [
            stat(&t(lang, "card.pr"), &pr_state), stat(&t(lang, "card.decision"), &decision),
            stat(&t(lang, "card.ci"), &ci), stat(&t(lang, "card.size"), &size)]}));
        let branch = if s.head_ref.is_empty() {
            String::new()
        } else {
            format!("{} → {} · ", s.head_ref, s.base_ref)
        };
        els.push(md(
            &grey(&format!(
                "{branch}{}",
                tf(lang, "card.commit", &[("sha", short(sha))])
            )),
            "notation",
        ));
    } else {
        els.push(md(
            &grey(&tf(lang, "card.commit", &[("sha", short(sha))])),
            "notation",
        ));
    }
    if !s.head.is_empty()
        && !sha.is_empty()
        && s.head != sha
        && s.state == "open"
        && e.status != "queued"
    {
        let txt = tf(
            lang,
            "card.moved",
            &[("new", short(&s.head)), ("old", short(sha))],
        );
        els.push(md(&format!("<font color='orange'>{txt}</font>"), "normal"));
    }
    els.push(json!({"tag": "hr"}));

    let v = e.verdict.as_ref();
    let since = |ts: f64| hhmm(if ts > 0.0 { ts } else { now() });
    match e.status.as_str() {
        "queued" | "" => els.push(md(
            &tf(lang, "card.queued", &[("since", &since(e.queued_at))]),
            "normal",
        )),
        "retry" if e.waits > 0 => {} // the note below says what it waits for
        "reviewing" | "reviewed" | "retry" => els.push(md(
            &tf(
                lang,
                "card.reviewing",
                &[(
                    "since",
                    &since(if e.started_at > 0.0 {
                        e.started_at
                    } else {
                        e.queued_at
                    }),
                )],
            ),
            "normal",
        )),
        "direct_approved" => {
            let mut txt = tf(
                lang,
                "card.direct",
                &[("at", &since(e.direct_at)), ("sha", short(sha))],
            );
            if e.aborted_review {
                txt += &grey(&t(lang, "card.aborted"));
            }
            els.push(md(&txt, "normal"));
        }
        "awaiting_ci" => els.push(md(
            &tf(
                lang,
                "card.awaiting_ci",
                &[("summary", v.map(|v| v.summary.as_str()).unwrap_or(""))],
            ),
            "normal",
        )),
        _ => {
            if let Some(v) = v {
                els.push(md(
                    &tf(lang, "card.verdict", &[("summary", &v.summary)]),
                    "normal",
                ));
                if !e.reasons.is_empty() && e.event.as_deref() != Some("APPROVE") {
                    let tags: Vec<String> = e
                        .reasons
                        .iter()
                        .map(|r| tag(&reason_text(lang, r), "neutral"))
                        .collect();
                    els.push(md(
                        &format!("{}{}", grey(&t(lang, "card.not_approved")), tags.join(" ")),
                        "notation",
                    ));
                }
            }
        }
    }
    let note = e.note.clone().unwrap_or_default();
    if note.kind == "manual" || e.status == "failed" {
        let text = if note.text.is_empty() {
            e.error.clone().unwrap_or_default()
        } else {
            note.text.clone()
        };
        els.push(md(
            &format!("<font color='red'>{}</font>", truncate(&text, 400)),
            "normal",
        ));
    } else if note.kind == "wait" && e.status == "retry" {
        els.push(md(
            &format!("<font color='orange'>{}</font>", note.text),
            "normal",
        ));
    } else if let Some((_, extra)) = note.text.split_once('\n') {
        // appended notes such as withdrawn approvals
        els.push(md(
            &format!("<font color='orange'>{extra}</font>"),
            "normal",
        ));
    }

    if let Some(v) = v
        .filter(|v| !v.findings.is_empty() && !["queued", "reviewing"].contains(&e.status.as_str()))
    {
        let mut tags = vec![];
        for sev in ["high", "medium", "low", "nit"] {
            let n = v.findings.iter().filter(|f| f.severity == sev).count();
            if n > 0 {
                tags.push(tag(
                    &format!("{} {n}", t(lang, &format!("sev.{sev}"))),
                    sev_color(sev),
                ));
            }
        }
        let mut rows: Vec<Value> = v
            .findings
            .iter()
            .take(10)
            .map(|f| {
                let old = if f.introduced_by_pr {
                    String::new()
                } else {
                    grey(&t(lang, "review.old_issue"))
                };
                md(
                    &format!(
                        "{} {}{old}\n{}",
                        tag(
                            &t(lang, &format!("sev.{}", f.severity)),
                            sev_color(&f.severity)
                        ),
                        f.title,
                        grey(&format!("{}:{}", f.path, f.line))
                    ),
                    "normal",
                )
            })
            .collect();
        if v.findings.len() > 10 {
            rows.push(md(
                &grey(&tf(
                    lang,
                    "card.more",
                    &[("n", &(v.findings.len() - 10).to_string())],
                )),
                "normal",
            ));
        }
        let serious = v
            .findings
            .iter()
            .any(|f| f.severity == "high" || f.severity == "medium");
        els.push(json!({"tag": "collapsible_panel", "expanded": serious && v.findings.len() <= 4,
            "header": {"title": md(&format!("{} {}", tf(lang, "card.n_issues", &[("n", &v.findings.len().to_string())]), tags.join(" ")), "normal"),
                       "icon": {"tag": "standard_icon", "token": "down-small-ccm_outlined", "size": "16px 16px"},
                       "icon_position": "right", "icon_expanded_angle": -180},
            "border": {"color": "grey", "corner_radius": "6px"}, "vertical_spacing": "6px",
            "padding": "8px 8px 8px 8px", "elements": rows}));
    }
    if !e.action_msg.is_empty() {
        els.push(md(
            &format!("<font color='orange'>{}</font>", e.action_msg),
            "normal",
        ));
    }

    let own = author.eq_ignore_ascii_case(me) && !direct_on_own;
    let open = s.state.is_empty() || s.state == "open";
    let can_direct =
        e.status != "direct_approved" && !own && open && s.decision != "APPROVED" && !e.direct_busy;
    let mut buttons = vec![];
    if can_direct {
        let target = if s.head.is_empty() {
            sha
        } else {
            s.head.as_str()
        };
        buttons.push(json!({"tag": "button", "type": "primary_filled", "width": "fill",
            "text": {"tag": "plain_text", "content": t(lang, "card.btn_direct")},
            "behaviors": [{"type": "callback", "value": {"action": "approve", "key": key, "sha": target}}],
            "confirm": {"title": {"tag": "plain_text", "content": t(lang, "card.confirm_title")},
                        "text": {"tag": "plain_text", "content": tf(lang, "card.confirm_text", &[("sha", short(target))])}}}));
    }
    if !url.is_empty() {
        buttons.push(json!({"tag": "button", "type": "default", "width": "fill",
            "text": {"tag": "plain_text", "content": t(lang, "card.btn_github")},
            "behaviors": [{"type": "open_url", "default_url": url}]}));
    }
    if let Some(r) = &e.review_url {
        buttons.push(json!({"tag": "button", "type": "default", "width": "fill",
            "text": {"tag": "plain_text", "content": t(lang, "card.btn_review")},
            "behaviors": [{"type": "open_url", "default_url": r}]}));
    }
    if !buttons.is_empty() {
        let cols: Vec<Value> = buttons
            .into_iter()
            .map(|b| json!({"tag": "column", "width": "weighted", "weight": 1, "elements": [b]}))
            .collect();
        els.push(json!({"tag": "column_set", "flex_mode": "bisect", "horizontal_spacing": "8px", "columns": cols}));
    }
    els.push(md(
        &grey(&tf(
            lang,
            "card.footer",
            &[(
                "at",
                &chrono::Local::now().format("%m-%d %H:%M").to_string(),
            )],
        )),
        "notation",
    ));

    let tag_color = if color == "grey" { "neutral" } else { color };
    json!({
        "schema": "2.0",
        "config": {"update_multi": true, "width_mode": "fill", "summary": {"content": format!("[{head_text}] {title}")}},
        "header": {"title": {"tag": "plain_text", "content": truncate(title, 80)},
                   "subtitle": {"tag": "plain_text", "content": format!("{key} · {author}")},
                   "text_tag_list": [{"tag": "text_tag", "text": {"tag": "plain_text", "content": head_text}, "color": tag_color}],
                   "template": color, "padding": "12px 12px 12px 12px"},
        "body": {"direction": "vertical", "padding": "12px 12px 12px 12px", "vertical_spacing": "8px", "elements": els}
    })
}

pub fn simple_card(title: &str, body: &str, color: &str) -> Value {
    json!({"schema": "2.0", "config": {"width_mode": "fill"},
           "header": {"title": {"tag": "plain_text", "content": title}, "template": color},
           "body": {"elements": [md(body, "normal")]}})
}

/// The card without its timestamp, to detect "nothing changed".
fn signature(card: &Value) -> String {
    let s = card.to_string();
    let re = regex_lite::Regex::new(r"(updated|更新于) [0-9: -]+").unwrap();
    re.replace_all(&s, "").into_owned()
}

pub struct FeishuNotifier {
    pub client: FeishuClient,
    lang: String,
    recipient: String,
    me: String,
    direct_on_own: bool,
    dry_run: bool,
}

impl FeishuNotifier {
    pub fn new(cfg: &Config, me: &str) -> Self {
        let f = &cfg.notify.feishu;
        FeishuNotifier {
            client: FeishuClient::new(&f.app_id, &cfg.feishu_secret(), &f.domain),
            lang: cfg.language.clone(),
            recipient: f.recipient.clone(),
            me: me.into(),
            direct_on_own: cfg.test_direct_button_on_own_pr,
            dry_run: cfg.dry_run,
        }
    }
}

impl Notifier for FeishuNotifier {
    fn update(&self, key: &str, e: &mut Entry, st: Option<&PrStatus>) -> Result<bool> {
        let card = render(&self.lang, key, e, st, &self.me, self.direct_on_own);
        let sig = signature(&card);
        if sig == e.card_sig {
            return Ok(true);
        }
        if self.dry_run {
            crate::info!(
                "[dry-run] card {key}: {}",
                card["header"]["text_tag_list"][0]["text"]["content"]
            );
        } else if e.message_id.is_empty() {
            let (m, c) = self.client.send_card(&self.recipient, &card)?;
            e.message_id = m;
            e.chat_id = c;
            e.card_sent_at = now();
        } else {
            self.client.update_card(&e.message_id, &card)?;
        }
        e.card_sig = sig;
        Ok(true)
    }

    fn alert(&self, text: &str) -> Result<()> {
        self.client.send_card(
            &self.recipient,
            &simple_card(
                &t(&self.lang, "card.alert_title"),
                &truncate(text, 1500),
                "red",
            ),
        )?;
        Ok(())
    }

    fn test(&self) -> Result<String> {
        self.client.send_card(
            &self.recipient,
            &simple_card(
                &t(&self.lang, "card.test_title"),
                &t(&self.lang, "card.test_body"),
                "blue",
            ),
        )?;
        Ok(format!("sent a test card to {}", self.recipient))
    }

    fn supports_actions(&self) -> bool {
        true
    }

    fn listen(&self, on_action: OnAction) -> Result<()> {
        super::feishu_ws::run(&self.client, on_action)
    }

    /// Only the configured user may approve, and only from the card that was sent to them: the click must come
    /// from that exact message in the one-to-one chat it was delivered to, so a forwarded card is useless to anyone
    /// else. When the recipient is configured by id, the operator id must match as well.
    fn may_act(&self, ids: &ClickIds, e: &Entry) -> bool {
        if e.message_id.is_empty() || ids.message_id != e.message_id {
            return false;
        }
        if !e.chat_id.is_empty() && ids.chat_id != e.chat_id {
            return false;
        }
        match self.recipient.split_once(':') {
            Some(("open_id", id)) => ids.open_id == id,
            Some(("union_id", id)) => ids.union_id == id,
            Some(("user_id", id)) => ids.user_id == id,
            _ => !e.chat_id.is_empty(),
        }
    }

    fn render_for(&self, key: &str, e: &Entry, st: Option<&PrStatus>) -> Option<Value> {
        Some(render(&self.lang, key, e, st, &self.me, self.direct_on_own))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Note;
    use crate::review::tests::{clean, finding};

    fn st() -> PrStatus {
        PrStatus {
            state: "open".into(),
            head: "abc1234".into(),
            ci: "success".into(),
            title: "t".into(),
            url: "u".into(),
            author: "alice".into(),
            additions: Some(3),
            deletions: Some(1),
            changed_files: Some(1),
            head_ref: "f".into(),
            base_ref: "main".into(),
            ..Default::default()
        }
    }
    fn buttons(c: &Value) -> Vec<String> {
        let mut out = vec![];
        for el in c["body"]["elements"].as_array().unwrap() {
            if el["tag"] == "column_set" {
                for col in el["columns"].as_array().unwrap() {
                    for b in col["elements"].as_array().unwrap() {
                        if b["tag"] == "button" {
                            out.push(b["text"]["content"].as_str().unwrap().to_string());
                        }
                    }
                }
            }
        }
        out
    }
    fn entry(status: &str) -> Entry {
        Entry {
            status: status.into(),
            sha: "abc1234".into(),
            author: "alice".into(),
            ..Default::default()
        }
    }

    #[test]
    fn direct_button_rules() {
        for (lang, label) in [("zh-CN", "直接通过"), ("en", "Approve now")] {
            let c = render(lang, "o/r#1", &entry("reviewing"), Some(&st()), "me", false);
            assert_eq!(c["schema"], "2.0");
            assert!(buttons(&c).contains(&label.to_string()));
        }
        let own = PrStatus {
            author: "me".into(),
            ..st()
        };
        assert!(!buttons(&render(
            "en",
            "o/r#1",
            &entry("posted"),
            Some(&own),
            "me",
            false
        ))
        .contains(&"Approve now".into()));
        let mut busy = entry("posted");
        busy.direct_busy = true;
        assert!(
            !buttons(&render("en", "o/r#1", &busy, Some(&st()), "me", false))
                .contains(&"Approve now".into())
        );
        let merged = PrStatus {
            state: "merged".into(),
            ..st()
        };
        let c = render("en", "o/r#1", &entry("posted"), Some(&merged), "me", false);
        assert!(!buttons(&c).contains(&"Approve now".into()));
        assert_eq!(c["header"]["text_tag_list"][0]["text"]["content"], "Merged");
    }

    #[test]
    fn header_keeps_the_review_result_after_merge() {
        let mut e = entry("posted");
        e.event = Some("APPROVE".into());
        e.note = Some(Note {
            kind: "approve".into(),
            text: "s".into(),
        });
        let merged = PrStatus {
            state: "merged".into(),
            ..st()
        };
        assert_eq!(headline("en", &e, Some(&merged)).1, "green");
        // a PR merged before Nod said anything takes its state as the headline
        let quiet = entry("closed");
        assert_eq!(headline("en", &quiet, Some(&merged)).1, "violet");
    }

    #[test]
    fn findings_panel_and_reasons() {
        let mut e = entry("posted");
        e.event = Some("COMMENT".into());
        e.note = Some(Note {
            kind: "comment".into(),
            text: "s".into(),
        });
        e.reasons = vec!["high".into()];
        let mut v = clean();
        v.findings = vec![finding("high", true, 1), finding("low", true, 1)];
        e.verdict = Some(v);
        let c = render("en", "o/r#1", &e, Some(&st()), "me", false);
        let panels: Vec<&Value> = c["body"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["tag"] == "collapsible_panel")
            .collect();
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0]["expanded"], true);
        assert_eq!(c["header"]["template"], "orange");
        assert!(c.to_string().contains("high-severity issue found"));
    }

    #[test]
    fn signature_ignores_timestamp() {
        let a = render("en", "o/r#1", &entry("posted"), Some(&st()), "me", false);
        let mut b = a.clone();
        let s = b.to_string().replace(
            &chrono::Local::now().format("%m-%d %H:%M").to_string(),
            "01-01 00:00",
        );
        b = serde_json::from_str(&s).unwrap();
        assert_eq!(signature(&a), signature(&b));
    }

    #[test]
    fn click_permissions() {
        let mut cfg = Config::default();
        cfg.notify.feishu.app_id = "a".into();
        cfg.notify.feishu.app_secret = "b".into();
        cfg.notify.feishu.recipient = "email:me@example.com".into();
        let n = FeishuNotifier::new(&cfg, "me");
        let e = Entry {
            message_id: "om_1".into(),
            chat_id: "oc_1".into(),
            ..Default::default()
        };
        let ids = |m: &str, c: &str, o: &str| ClickIds {
            message_id: m.into(),
            chat_id: c.into(),
            open_id: o.into(),
            ..Default::default()
        };
        assert!(n.may_act(&ids("om_1", "oc_1", "x"), &e));
        assert!(!n.may_act(&ids("om_1", "oc_forwarded", "x"), &e));
        assert!(!n.may_act(&ids("om_2", "oc_1", "x"), &e));
        assert!(!n.may_act(&ids("om_1", "oc_1", "x"), &Entry::default()));
        cfg.notify.feishu.recipient = "open_id:ou_me".into();
        let n = FeishuNotifier::new(&cfg, "me");
        assert!(n.may_act(&ids("om_1", "oc_1", "ou_me"), &e));
        assert!(!n.may_act(&ids("om_1", "oc_1", "ou_other"), &e));
    }
}
