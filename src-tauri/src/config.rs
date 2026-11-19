//! Configuration: a TOML file in the user's config directory, validated on load.
//!
//! Secrets may come from the environment instead of the file:
//! `NOD_FEISHU_APP_SECRET`, `NOD_WEBHOOK_SECRET`.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

pub const LANGUAGES: [&str; 2] = ["zh-CN", "en"];
pub const APPROVE_MODES: [&str; 2] = ["auto", "never"];
/// Built-in notification channels ("none" is the legacy single-channel value for "nothing").
pub const CHANNELS: [&str; 8] = [
    "system",
    "feishu",
    "feishu_bot",
    "webhook",
    "dingtalk",
    "wecom",
    "slack",
    "none",
];
/// Built-in review engines.
pub const ENGINES: [&str; 4] = ["claude", "codex", "gemini", "api"];
/// Built-in code hosts.
pub const FORGES: [&str; 3] = ["github", "gitee", "gitea"];
pub const RECIPIENT_TYPES: [&str; 4] = ["open_id", "union_id", "user_id", "email"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Review {
    /// The default engine: one of ENGINES or `ext:<plugin>`.
    pub engine: String,
    /// Use another engine for some repositories (first match wins).
    pub per_repo: Vec<EngineRule>,
    /// Review with this engine when the chosen one cannot run (out of quota, signed out, not installed); "" = none,
    /// wait for it instead.
    pub fallback: String,
    /// Claude Code (the original settings, kept at the top level of [review] for older files).
    pub claude: String,
    pub claude_config_dir: String,
    pub model: String,
    pub timeout_min: f64,
    pub max_attempts: u32,
    pub ci_wait_max_min: f64,
    pub ci_none_grace_min: f64,
    pub rereview_days: f64,
    /// Add a short signature ("tail") at the end of every review Nod posts.
    pub footer_enabled: bool,
    /// The signature text (Markdown); empty = Nod's own, naming the engine. `{engine}` is replaced by the engine's
    /// name.
    pub footer: String,
    pub codex: Codex,
    pub gemini: Gemini,
    pub api: ApiEngine,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct EngineRule {
    /// Repository pattern, as in [scope].
    pub repo: String,
    pub engine: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Codex {
    pub exe: String,
    pub model: String,
}

impl Default for Codex {
    fn default() -> Self {
        Codex {
            exe: "codex".into(),
            model: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Gemini {
    pub exe: String,
    pub model: String,
    /// Gemini CLI has no operating-system sandbox here, only its read-only "plan" mode. Its verdicts never lead to an
    /// approval unless this is turned on explicitly.
    pub allow_approve: bool,
}

impl Default for Gemini {
    fn default() -> Self {
        Gemini {
            exe: "gemini".into(),
            model: String::new(),
            allow_approve: false,
        }
    }
}

/// Any OpenAI-compatible chat completions endpoint (OpenAI, DeepSeek, Qwen, OpenRouter, Ollama, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiEngine {
    pub base_url: String,
    pub model: String,
    /// Stored in the system keychain; only kept here when the keychain is unavailable.
    pub key: String,
    /// The diff is cut to this many KiB before it is sent.
    pub max_diff_kb: u64,
}

impl Default for ApiEngine {
    fn default() -> Self {
        ApiEngine {
            base_url: "https://api.openai.com/v1".into(),
            model: String::new(),
            key: String::new(),
            max_diff_kb: 200,
        }
    }
}

impl Default for Review {
    fn default() -> Self {
        Review {
            engine: "claude".into(),
            per_repo: vec![],
            fallback: String::new(),
            claude: "claude".into(),
            claude_config_dir: String::new(),
            model: String::new(),
            timeout_min: 25.0,
            max_attempts: 2,
            ci_wait_max_min: 60.0,
            ci_none_grace_min: 10.0,
            rereview_days: 14.0,
            footer_enabled: true,
            footer: String::new(),
            codex: Codex::default(),
            gemini: Gemini::default(),
            api: ApiEngine::default(),
        }
    }
}

/// Which pull requests get reviewed.
///
/// Patterns: `owner/repo`, `owner/*` (a whole user or organisation), `*/repo`, `*`, and `@me` for your own login,
/// e.g. `@me/*`. Matching is case-insensitive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scope {
    /// Only these repositories (empty = every repository you are asked to review in).
    pub repos: Vec<String>,
    pub exclude_repos: Vec<String>,
    /// Only PRs by these authors are reviewed when they ask you (logins, as in [approve]; empty = anyone). A PR you
    /// hand to Nod yourself ("Review" in the window) is reviewed whoever wrote it.
    pub authors: Vec<String>,
    pub exclude_authors: Vec<String>,
    /// Also review PRs that request a review from a team you are in, not only from you personally.
    pub team_requests: bool,
}

/// When an approval may be posted. These rules come on top of the quality rules (no high-severity issue, ...);
/// a PR outside them still gets a review with comments, just never an approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Approve {
    /// "auto": approve when every rule passes; "never": comment only.
    pub mode: String,
    /// Repositories where approving is allowed (patterns; empty = every reviewed repository).
    pub repos: Vec<String>,
    /// PR authors whose PRs may be approved (logins; `*`, `@me` and `name[bot]` allowed; empty = anyone).
    pub authors: Vec<String>,
    pub exclude_repos: Vec<String>,
    pub exclude_authors: Vec<String>,
}

impl Default for Approve {
    fn default() -> Self {
        Approve {
            mode: "auto".into(),
            repos: vec![],
            authors: vec![],
            exclude_repos: vec![],
            exclude_authors: vec![],
        }
    }
}

/// Case-insensitive match of `owner/repo` against a pattern (`owner/repo`, `owner/*`, `*/repo`, `*`, `@me/...`).
pub fn repo_matches(pattern: &str, repo: &str, me: &str) -> bool {
    let p = pattern.trim().replace("@me", me).to_lowercase();
    let r = repo.to_lowercase();
    if p == "*" {
        return true;
    }
    // `owner/*` matches everything below owner (some hosts nest groups); `*/name` matches the last segment.
    if let Some(prefix) = p.strip_suffix("/*") {
        return prefix != "*" && r.starts_with(&format!("{prefix}/"))
            || prefix == "*" && r.contains('/');
    }
    if let Some(name) = p.strip_prefix("*/") {
        return r.rsplit_once('/').is_some_and(|(_, n)| n == name);
    }
    p == r
}

/// Case-insensitive match of a PR author against a pattern: a login (`alice`, `dependabot[bot]`), `@me`, or a
/// wildcard pattern where `*` matches any run of characters (`*`, `*[bot]`, `renovate*`).
pub fn author_matches(pattern: &str, author: &str, me: &str) -> bool {
    let p = pattern.trim();
    let p = if p.eq_ignore_ascii_case("@me") {
        me.to_lowercase()
    } else {
        p.to_lowercase()
    };
    wildcard(&p, &author.to_lowercase())
}

/// `*` matches any (possibly empty) run of characters; everything else literally.
fn wildcard(p: &str, s: &str) -> bool {
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == s;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !s.starts_with(first)
        || !s[first.len()..].ends_with(last)
        || s.len() < first.len() + last.len()
    {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// `true` if selected by the include list (empty = everything) and not by the exclude list.
pub fn selected(include: &[String], exclude: &[String], m: impl Fn(&str) -> bool) -> bool {
    (include.is_empty() || include.iter().any(|p| m(p))) && !exclude.iter().any(|p| m(p))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Daemon {
    pub poll_seconds: f64,
    pub refresh_seconds: f64,
}

impl Default for Daemon {
    fn default() -> Self {
        Daemon {
            poll_seconds: 30.0,
            refresh_seconds: 120.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Feishu {
    pub app_id: String,
    pub app_secret: String,
    /// `<type>:<id>` with type in RECIPIENT_TYPES
    pub recipient: String,
    pub domain: String,
}

impl Default for Feishu {
    fn default() -> Self {
        Feishu {
            app_id: String::new(),
            app_secret: String::new(),
            recipient: String::new(),
            domain: "https://open.feishu.cn".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Webhook {
    pub url: String,
    /// HMAC-SHA256 key for the X-Nod-Signature header
    pub secret: String,
}

/// A chat robot reached through an incoming-webhook URL (DingTalk, WeCom, Slack). The URL carries the robot's token,
/// so it is stored in the system keychain like a password.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Robot {
    pub url: String,
    /// Signature secret (optional): DingTalk "加签", Feishu "签名校验"; WeCom and Slack have none.
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notify {
    /// Enabled channels (CHANNELS or `ext:<plugin>`). Older files have a single `channel` instead.
    pub channels: Vec<String>,
    /// Legacy single channel; read when `channels` is empty.
    pub channel: String,
    pub feishu: Feishu,
    pub webhook: Webhook,
    /// A Feishu / Lark group's custom bot (an incoming webhook), as opposed to `feishu`, the Feishu app.
    pub feishu_bot: Robot,
    pub dingtalk: Robot,
    pub wecom: Robot,
    pub slack: Robot,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            channels: vec![],
            channel: "none".into(),
            feishu: Feishu::default(),
            webhook: Webhook::default(),
            feishu_bot: Robot::default(),
            dingtalk: Robot::default(),
            wecom: Robot::default(),
            slack: Robot::default(),
        }
    }
}

impl Notify {
    /// The channels in effect (new list, or the legacy single channel).
    pub fn active(&self) -> Vec<String> {
        if !self.channels.is_empty() {
            return self
                .channels
                .iter()
                .filter(|c| *c != "none")
                .cloned()
                .collect();
        }
        if self.channel.is_empty() || self.channel == "none" {
            vec![]
        } else {
            vec![self.channel.clone()]
        }
    }
}

/// One code-host account.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ForgeAccount {
    /// Stable id: names the state file and keychain entry. `github` is the original GitHub account.
    pub id: String,
    /// FORGES or `ext:<plugin>`
    pub kind: String,
    /// Web address for self-hosted instances / GitHub Enterprise (empty = the public service).
    pub url: String,
    pub enabled: bool,
    /// Stored in the system keychain; only kept here when the keychain is unavailable. (GitHub signs in with Nod's OAuth App instead; see forge::github_auth.)
    pub token: String,
    /// Settings of an external plugin.
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    pub settings: toml::Table,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub language: String,
    /// Code-host accounts. Empty = the original single GitHub account (id `github`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forges: Vec<ForgeAccount>,
    /// Settings of external plugins, by plugin name.
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    pub plugins: toml::Table,
    pub review: Review,
    pub scope: Scope,
    pub approve: Approve,
    pub daemon: Daemon,
    pub notify: Notify,
    /// Runtime only, never written.
    #[serde(skip)]
    pub dry_run: bool,
    /// Development aid, not documented.
    #[serde(skip)]
    pub test_direct_button_on_own_pr: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "zh-CN".into(),
            forges: vec![],
            plugins: toml::Table::new(),
            review: Review::default(),
            scope: Scope::default(),
            approve: Approve::default(),
            daemon: Daemon::default(),
            notify: Notify::default(),
            dry_run: false,
            test_direct_button_on_own_pr: false,
        }
    }
}

fn is_repo_pattern(s: &str) -> bool {
    if s == "*" {
        return true;
    }
    let ok = |p: &str| {
        !p.is_empty()
            && (p == "*"
                || p == "@me"
                || p.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c)))
    };
    let parts: Vec<&str> = s.split('/').collect();
    parts.len() >= 2 && parts.iter().all(|p| ok(p))
}

/// `@me`, or a GitHub login made of letters, digits and `-`, optionally ending in `[bot]`, with `*` wildcards.
fn is_login_pattern(s: &str) -> bool {
    if s.eq_ignore_ascii_case("@me") {
        return true;
    }
    let core = s.strip_suffix("[bot]").unwrap_or(s);
    !core.is_empty()
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '*')
}

impl Config {
    pub fn validate(self) -> Result<Self> {
        let mut errs = vec![];
        if !LANGUAGES.contains(&self.language.as_str()) {
            errs.push(format!(
                "language must be one of {LANGUAGES:?}, got {:?}",
                self.language
            ));
        }
        let r = &self.review;
        if !APPROVE_MODES.contains(&self.approve.mode.as_str()) {
            errs.push(format!(
                "approve.mode must be one of {APPROVE_MODES:?}, got {:?}",
                self.approve.mode
            ));
        }
        for (name, v) in [
            ("timeout_min", r.timeout_min),
            ("ci_wait_max_min", r.ci_wait_max_min),
            ("ci_none_grace_min", r.ci_none_grace_min),
            ("rereview_days", r.rereview_days),
        ] {
            if v < 0.0 {
                errs.push(format!("review.{name} must be >= 0"));
            }
        }
        if r.max_attempts < 1 {
            errs.push("review.max_attempts must be >= 1".into());
        }
        if self.daemon.poll_seconds < 10.0 {
            errs.push(
                "daemon.poll_seconds must be >= 10 (GitHub search API is rate limited)".into(),
            );
        }
        for (name, list) in [
            ("scope.repos", &self.scope.repos),
            ("scope.exclude_repos", &self.scope.exclude_repos),
            ("approve.repos", &self.approve.repos),
            ("approve.exclude_repos", &self.approve.exclude_repos),
        ] {
            for x in list {
                if !is_repo_pattern(x) {
                    errs.push(format!("{name}: {x:?} is not owner/repo, owner/* or @me/*"));
                }
            }
        }
        for (name, list) in [
            ("approve.authors", &self.approve.authors),
            ("approve.exclude_authors", &self.approve.exclude_authors),
            ("scope.authors", &self.scope.authors),
            ("scope.exclude_authors", &self.scope.exclude_authors),
        ] {
            for x in list {
                if !is_login_pattern(x) {
                    errs.push(format!("{name}: {x:?} is not a GitHub login"));
                }
            }
        }
        let active = self.notify.active();
        let legacy = self.notify.channels.is_empty();
        if legacy
            && !CHANNELS.contains(&self.notify.channel.as_str())
            && !self.notify.channel.starts_with("ext:")
        {
            errs.push(format!(
                "notify.channel must be one of {CHANNELS:?}, got {:?}",
                self.notify.channel
            ));
        }
        for ch in &active {
            if !CHANNELS.contains(&ch.as_str()) && !ch.starts_with("ext:") {
                errs.push(format!("notify.channels: unknown channel {ch:?}"));
            }
        }
        let on = |c: &str| active.iter().any(|x| x == c);
        if on("feishu") {
            let f = &self.notify.feishu;
            if f.app_id.is_empty() {
                errs.push("notify.feishu.app_id is required for the Feishu channel".into());
            }
            if self.feishu_secret().is_empty() {
                errs.push(
                    "notify.feishu.app_secret (or env NOD_FEISHU_APP_SECRET) is required".into(),
                );
            }
            let (t, id) = f.recipient.split_once(':').unwrap_or(("", ""));
            if !RECIPIENT_TYPES.contains(&t) || id.is_empty() {
                errs.push(format!(
                    "notify.feishu.recipient must look like \"email:you@example.com\" or \"open_id:ou_xxx\" (types: {})",
                    RECIPIENT_TYPES.join(", ")
                ));
            }
        }
        let http = |u: &str| u.starts_with("http://") || u.starts_with("https://");
        if on("webhook") && !http(&self.notify.webhook.url) {
            errs.push("notify.webhook.url must be an http(s) URL for the webhook channel".into());
        }
        for (name, robot) in [
            ("feishu_bot", &self.notify.feishu_bot),
            ("dingtalk", &self.notify.dingtalk),
            ("wecom", &self.notify.wecom),
            ("slack", &self.notify.slack),
        ] {
            if on(name) && !http(&robot.url) {
                errs.push(format!(
                    "notify.{name}.url must be the robot's http(s) webhook URL"
                ));
            }
        }
        let engine_ok = |e: &str| ENGINES.contains(&e) || e.starts_with("ext:");
        if !engine_ok(&self.review.engine) {
            errs.push(format!(
                "review.engine must be one of {ENGINES:?}, got {:?}",
                self.review.engine
            ));
        }
        if !self.review.fallback.is_empty() && !engine_ok(&self.review.fallback) {
            errs.push(format!(
                "review.fallback must be empty or one of {ENGINES:?}, got {:?}",
                self.review.fallback
            ));
        }
        for rule in &self.review.per_repo {
            if !engine_ok(&rule.engine) || !is_repo_pattern(&rule.repo) {
                errs.push(format!(
                    "review.per_repo: {:?} -> {:?} is not a repository pattern and engine",
                    rule.repo, rule.engine
                ));
            }
        }
        let mut ids = std::collections::HashSet::new();
        for a in &self.forges {
            let id_ok = !a.id.is_empty()
                && a.id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !id_ok || !ids.insert(a.id.clone()) {
                errs.push(format!(
                    "forges: id {:?} must be unique and made of letters, digits, - and _",
                    a.id
                ));
            }
            if !FORGES.contains(&a.kind.as_str()) && !a.kind.starts_with("ext:") {
                errs.push(format!("forges.{}: unknown kind {:?}", a.id, a.kind));
            }
            if a.kind == "gitea" && !http(&a.url) {
                errs.push(format!("forges.{}: Gitea needs its web address", a.id));
            }
            if !a.url.is_empty() && !http(&a.url) && a.kind != "github" {
                errs.push(format!("forges.{}: url must start with http(s)://", a.id));
            }
        }
        if !self.forges.is_empty() && !self.forges.iter().any(|a| a.enabled) {
            errs.push("forges: enable at least one code host account".into());
        }
        if errs.is_empty() {
            Ok(self)
        } else {
            Err(anyhow!(
                "invalid configuration:\n  - {}",
                errs.join("\n  - ")
            ))
        }
    }

    /// The code-host accounts in effect: `[[forges]]`, or the implicit GitHub account of older configurations.
    pub fn accounts(&self) -> Vec<ForgeAccount> {
        if self.forges.is_empty() {
            return vec![ForgeAccount {
                id: "github".into(),
                kind: "github".into(),
                enabled: true,
                ..Default::default()
            }];
        }
        self.forges.clone()
    }

    /// The engine for `repo` (per-repository rules first, then the default).
    pub fn engine_for(&self, repo: &str, me: &str) -> String {
        self.review
            .per_repo
            .iter()
            .find(|r| repo_matches(&r.repo, repo, me))
            .map(|r| r.engine.clone())
            .unwrap_or_else(|| self.review.engine.clone())
    }

    pub fn feishu_secret(&self) -> String {
        std::env::var("NOD_FEISHU_APP_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.notify.feishu.app_secret.clone())
    }

    pub fn webhook_secret(&self) -> String {
        std::env::var("NOD_WEBHOOK_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.notify.webhook.secret.clone())
    }
}

pub fn parse(text: &str) -> Result<Config> {
    toml::from_str::<Config>(text).map_err(|e| anyhow!("{}", e.to_string().trim()))
}

pub fn load_from(path: &Path, validate: bool) -> Result<Config> {
    if !path.exists() {
        if validate {
            return Err(anyhow!(
                "no configuration at {}; finish the setup in the Nod window first",
                path.display()
            ));
        }
        return Ok(Config::default());
    }
    let text = fs::read_to_string(path)?;
    let cfg = parse(text.trim_start_matches('\u{feff}'))
        .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    if validate {
        cfg.validate()
    } else {
        Ok(cfg)
    }
}

/// The configuration with secrets filled in from the keychain, validated.
pub fn load() -> Result<Config> {
    let mut c = load_from(&paths::config_file(), false)?;
    if !paths::config_file().exists() {
        return Err(anyhow!(
            "no configuration at {}; finish the setup in the Nod window first",
            paths::config_file().display()
        ));
    }
    crate::secrets::hydrate(&mut c);
    let mut c = c.validate()?;
    // Development aid: show "Approve now" on your own PRs so the button can be tested end to end.
    c.test_direct_button_on_own_pr =
        std::env::var("NOD_TEST_DIRECT_ON_OWN").is_ok_and(|v| v == "1");
    Ok(c)
}

pub fn dumps(cfg: &Config) -> String {
    format!(
        "# Nod configuration. Docs: https://github.com/larrygogo/nod#configuration\n{}",
        toml::to_string_pretty(cfg).unwrap_or_default()
    )
}

pub fn save(cfg: &Config) -> Result<std::path::PathBuf> {
    let path = paths::config_file();
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, dumps(cfg))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?; // may hold the Feishu app secret
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut c = Config {
            language: "en".into(),
            ..Default::default()
        };
        c.scope.repos = vec!["acme/*".into()];
        c.approve.authors = vec!["dependabot[bot]".into()];
        c.notify.channel = "webhook".into();
        c.notify.webhook.url = "https://example.com/h".into();
        let back = parse(&dumps(&c)).unwrap().validate().unwrap();
        assert_eq!(back.language, "en");
        assert_eq!(back.scope.repos, vec!["acme/*"]);
        assert_eq!(back.approve.authors, vec!["dependabot[bot]"]);
        assert_eq!(back.notify.webhook.url, "https://example.com/h");
    }

    #[test]
    fn example_file_is_valid() {
        let c = parse(include_str!("../../examples/config.example.toml"))
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(c.notify.channel, "none");
    }

    #[test]
    fn validation_errors() {
        for (text, msg) in [
            ("language = \"fr\"", "language"),
            ("[approve]\nmode = \"yolo\"", "approve.mode"),
            ("[review]\napprove_policy = \"approve-clean\"", "unknown field"),
            ("[review]\ntimeout_min = \"ten\"", "invalid type"),
            ("[scope]\nrepos = [\"not a repo\"]", "owner/repo"),
            ("[approve]\nauthors = [\"bad name!\"]", "GitHub login"),
            ("[daemon]\npoll_seconds = 1", "poll_seconds"),
            ("unknown = 1", "unknown field"),
            ("[notify]\nchannel = \"feishu\"", "app_id"),
            ("[notify]\nchannel = \"webhook\"\n[notify.webhook]\nurl = \"ftp://x\"", "http"),
            (
                "[notify]\nchannel = \"feishu\"\n[notify.feishu]\napp_id = \"a\"\napp_secret = \"b\"\nrecipient = \"bob\"",
                "recipient",
            ),
        ] {
            let err = parse(text).and_then(|c| c.validate()).unwrap_err().to_string();
            assert!(err.contains(msg), "{text:?} -> {err}");
        }
    }

    #[test]
    fn patterns() {
        assert!(repo_matches("acme/*", "ACME/api", "me"));
        assert!(repo_matches("acme/api", "acme/api", "me"));
        assert!(!repo_matches("acme/api", "acme/web", "me"));
        assert!(repo_matches("@me/*", "Me/dotfiles", "me"));
        assert!(repo_matches("*/docs", "acme/docs", "me"));
        assert!(repo_matches("*", "x/y", "me"));
        assert!(author_matches("dependabot[bot]", "Dependabot[bot]", "me"));
        assert!(author_matches("@me", "ME", "me"));
        assert!(author_matches("*[bot]", "dependabot[bot]", "me"));
        assert!(author_matches("renovate*", "renovate[bot]", "me"));
        assert!(!author_matches("*[bot]", "alice", "me"));
        assert!(!author_matches("@meow", "xow", "x")); // @me is only replaced as a whole pattern
        assert!(
            is_login_pattern("dependabot[bot]")
                && is_login_pattern("*[bot]")
                && is_login_pattern("@me")
        );
        assert!(
            !is_login_pattern("a@me")
                && !is_login_pattern("bad name")
                && !is_login_pattern("[bot]")
        );
        let inc = vec!["acme/*".to_string()];
        let exc = vec!["acme/secret".to_string()];
        assert!(selected(&inc, &exc, |p| repo_matches(p, "acme/api", "me")));
        assert!(!selected(&inc, &exc, |p| repo_matches(
            p,
            "acme/secret",
            "me"
        )));
        assert!(!selected(&inc, &exc, |p| repo_matches(
            p,
            "other/api",
            "me"
        )));
        assert!(selected(&[], &[], |p| repo_matches(p, "any/thing", "me")));
    }

    #[test]
    fn ints_are_accepted_for_floats() {
        let c = parse("[review]\ntimeout_min = 30").unwrap();
        assert_eq!(c.review.timeout_min, 30.0);
    }
}
