//! Environment check: every prerequisite, with a plain explanation and a fix the app can start for the user.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::config::{Config, ForgeAccount};
use crate::notify::Notifier;
use crate::util::{run_raw, which, RunOpts};

/// What the "fix" button does. The window maps these to actions (open a terminal running a login, open a page…).
pub const FIX_INSTALL_GIT: &str = "install_git";
pub const FIX_GITHUB_INSTALL: &str = "github_install";
pub const FIX_INSTALL_CLAUDE: &str = "install_claude";
pub const FIX_CLAUDE_LOGIN: &str = "claude_login";
pub const FIX_SETTINGS: &str = "settings";
pub const FIX_PLUGINS: &str = "plugins";
pub const FIX_INSTALL_CODEX: &str = "install_codex";
pub const FIX_CODEX_LOGIN: &str = "codex_login";
pub const FIX_INSTALL_GEMINI: &str = "install_gemini";
pub const FIX_GEMINI_LOGIN: &str = "gemini_login";
pub const FIX_FEISHU_DOCS: &str = "feishu_docs";

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub name: String,
    pub ok: bool,
    /// not fatal
    pub warn: bool,
    pub detail: String,
    pub fix: String,
    pub action: Option<&'static str>,
    /// The code-host account the check is about (several accounts can be checked at once).
    pub account: Option<String>,
}

impl Check {
    fn detail_account(&mut self, id: &str) -> &mut Self {
        self.account = Some(id.to_string());
        self
    }
}

fn l(lang: &str, zh: &str, en: &str) -> String {
    if lang == "en" { en } else { zh }.to_string()
}

fn out(args: &[&str], secs: u64) -> (bool, String) {
    match run_raw(
        args,
        RunOpts {
            timeout: Duration::from_secs(secs),
            ..Default::default()
        },
    ) {
        Ok(o) => (
            o.status == Some(0) && !o.timed_out,
            format!("{}{}", o.stdout, o.stderr),
        ),
        Err(e) => (false, e.to_string()),
    }
}

pub struct Ctx<'a> {
    lang: &'a str,
    v: Vec<Check>,
}

impl<'a> Ctx<'a> {
    pub fn new(lang: &'a str) -> Ctx<'a> {
        Ctx { lang, v: vec![] }
    }

    pub fn done(self) -> Vec<Check> {
        self.v
    }
}

impl Ctx<'_> {
    fn push(
        &mut self,
        id: &'static str,
        name: (&str, &str),
        ok: bool,
        detail: impl Into<String>,
        fix: Option<((&str, &str), &'static str)>,
    ) -> &mut Check {
        let (fix, action) = match fix {
            Some(((zh, en), a)) => (l(self.lang, zh, en), Some(a)),
            None => (String::new(), None),
        };
        self.v.push(Check {
            id,
            name: l(self.lang, name.0, name.1),
            ok,
            warn: false,
            detail: detail.into(),
            fix,
            action,
            account: None,
        });
        self.v.last_mut().unwrap()
    }
}

/// The GitHub and Claude Code part of the check (needs no configuration).
fn git_check(c: &mut Ctx) {
    let lang = c.lang;
    let git = which("git");
    c.push(
        "git",
        ("git", "git"),
        git.is_some(),
        git.map(|p| p.display().to_string())
            .unwrap_or_else(|| l(lang, "没有找到", "not found")),
        Some((("安装 git", "Install git"), FIX_INSTALL_GIT)),
    );
    if c.v.last_mut().unwrap().ok {
        c.v.last_mut().unwrap().fix.clear();
        c.v.last_mut().unwrap().action = None;
    }
}

/// A GitHub account: signed in through Nod's GitHub App, the sign-in still accepted, and the App installed somewhere
/// (without an installation the token sees no repository at all).
fn github_check(c: &mut Ctx, acc: &ForgeAccount) {
    let lang = c.lang;
    let name = ("GitHub 登录", "GitHub sign-in");
    let gh = crate::forge::github::Gh::new(acc);
    if gh.session.client_id.trim().is_empty() {
        c.push(
            "github",
            name,
            false,
            l(
                lang,
                "没有配置 GitHub App 的 Client ID",
                "no GitHub App client ID is configured",
            ),
            Some((
                (
                    "在插件页的 GitHub 里填写 Client ID",
                    "Fill in the Client ID on the GitHub plugin page",
                ),
                FIX_PLUGINS,
            )),
        )
        .detail_account(&acc.id);
        return;
    }
    if crate::forge::github_auth::load(&acc.id).is_none() {
        c.push(
            "github",
            name,
            false,
            l(lang, "还没有登录", "not signed in"),
            Some((
                (
                    "在插件页的 GitHub 里点「登录 GitHub」",
                    "Click \"Sign in to GitHub\" on the GitHub plugin page",
                ),
                FIX_PLUGINS,
            )),
        )
        .detail_account(&acc.id);
        return;
    }
    let me = match crate::forge::Forge::current_user(&gh) {
        Ok(me) => me,
        Err(e) => {
            let msg = format!("{e:#}");
            let (detail, fix) = if crate::supervisor::is_network(&msg) {
                (
                    crate::util::truncate(&msg, 200),
                    (
                        "连不上 GitHub，检查网络或代理后点「重新检查」",
                        "Cannot reach GitHub; check the network or proxy, then check again",
                    ),
                )
            } else if msg.contains(crate::forge::github_auth::SIGNIN_REQUIRED) {
                (
                    l(lang, "登录已失效", "the sign-in is no longer valid"),
                    (
                        "在插件页的 GitHub 里重新登录",
                        "Sign in again on the GitHub plugin page",
                    ),
                )
            } else {
                (
                    crate::util::truncate(&msg, 200),
                    (
                        "在插件页的 GitHub 里重新登录",
                        "Sign in again on the GitHub plugin page",
                    ),
                )
            };
            c.push("github", name, false, detail, Some((fix, FIX_PLUGINS)))
                .detail_account(&acc.id);
            return;
        }
    };
    let signed_in = l(lang, "已登录：", "signed in as ") + &me;
    match gh.installations() {
        Ok(v) if v.is_empty() => {
            // the install page is known only for Nod's own App on github.com; an Enterprise App is installed from
            // the instance where it was registered
            let ours = gh.session.hosts == crate::forge::github_auth::Hosts::new("")
                && gh.session.client_id == crate::forge::github_auth::DEFAULT_CLIENT_ID
                && !crate::forge::github_auth::DEFAULT_APP_SLUG.is_empty();
            let ch = c.push(
                "github",
                name,
                true,
                signed_in,
                Some((
                    (
                        "GitHub App 还没有安装到任何账号或组织，所以看不到任何仓库。把它安装到要审查的仓库所在的账号或组织",
                        "The GitHub App is not installed on any account or organization yet, so no repository is visible. Install it where the repositories you review live",
                    ),
                    FIX_GITHUB_INSTALL,
                )),
            );
            ch.warn = true;
            if !ours {
                ch.action = None;
            }
            ch.detail_account(&acc.id);
        }
        Ok(v) => {
            let on = l(lang, " · App 已安装到 ", " · App installed on ") + &v.join(", ");
            c.push("github", name, true, signed_in + &on, None)
                .detail_account(&acc.id);
        }
        // the list is a convenience; failing to read it is not a failure of the sign-in
        Err(_) => {
            c.push("github", name, true, signed_in, None)
                .detail_account(&acc.id);
        }
    }
}
fn claude_check(c: &mut Ctx, claude: &str, cfg: &Config, live_claude: bool) {
    let lang = c.lang;
    match which(claude) {
        None => {
            c.push(
                "claude",
                ("Claude Code", "Claude Code"),
                false,
                format!("{claude:?} {}", l(lang, "没有找到", "not found")),
                Some((
                    (
                        "安装 Claude Code，或在设置里填写它的路径",
                        "Install Claude Code, or set its path in Settings",
                    ),
                    FIX_INSTALL_CLAUDE,
                )),
            );
        }
        Some(exe) => {
            let exe = exe.to_string_lossy().to_string();
            let (ok, ver) = out(&[&exe, "--version"], 30);
            c.push(
                "claude",
                ("Claude Code", "Claude Code"),
                ok,
                format!("{exe} {}", ver.trim()),
                (!ok).then_some((
                    ("重新安装 Claude Code", "Reinstall Claude Code"),
                    FIX_INSTALL_CLAUDE,
                )),
            );
            if ok && live_claude {
                let (good, detail) = claude_live(&exe, cfg);
                c.push(
                    "claude_login",
                    ("Claude 登录", "Claude sign-in"),
                    good,
                    detail,
                    (!good).then_some((
                        (
                            "在终端里运行一次 claude 并登录（会打开一个终端窗口）",
                            "Run claude once in a terminal and sign in (opens one for you)",
                        ),
                        FIX_CLAUDE_LOGIN,
                    )),
                );
            }
        }
    }
}

/// The git and Claude Code part of the check.
#[cfg(test)]
pub fn tools(lang: &str, claude: &str, cfg: &Config, live_claude: bool) -> Vec<Check> {
    let mut c = Ctx { lang, v: vec![] };
    git_check(&mut c);
    claude_check(&mut c, claude, cfg, live_claude);
    c.v
}

/// Everything the configuration uses: git, every enabled code-host account, every review engine in use.
pub fn all(cfg: &Config, live: bool) -> Vec<Check> {
    let mut c = Ctx {
        lang: &cfg.language,
        v: vec![],
    };
    git_check(&mut c);
    for acc in cfg.accounts().iter().filter(|a| a.enabled) {
        forge_check(&mut c, acc, cfg);
    }
    let mut engines = vec![cfg.review.engine.clone()];
    for r in &cfg.review.per_repo {
        if !engines.contains(&r.engine) {
            engines.push(r.engine.clone());
        }
    }
    for e in engines {
        engine_check(&mut c, cfg, &e, live);
    }
    c.v
}

/// One code-host account.
pub fn forge_check(c: &mut Ctx, acc: &ForgeAccount, cfg: &Config) {
    if acc.kind == "github" {
        github_check(c, acc);
        return;
    }
    let lang = c.lang;
    let name = crate::reviewer::platform(&acc.kind);
    let id: &'static str = "forge";
    match crate::forge::make(acc, cfg).and_then(|f| f.current_user()) {
        Ok(me) => {
            c.push(
                id,
                (name, name),
                true,
                l(lang, "已登录：", "signed in as ") + &me,
                None,
            )
            .detail_account(&acc.id);
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let network = crate::supervisor::is_network(&msg);
            let ch = c.push(
                id,
                (name, name),
                false,
                crate::util::truncate(&msg, 200),
                Some(if network {
                    (
                        (
                            "连不上，检查地址、网络或代理后重新检查",
                            "Cannot connect; check the address, network or proxy, then check again",
                        ),
                        FIX_PLUGINS,
                    )
                } else {
                    (
                        (
                            "检查令牌是否正确、是否过期、权限是否够",
                            "Check that the token is right, not expired and has enough scopes",
                        ),
                        FIX_PLUGINS,
                    )
                }),
            );
            ch.detail_account(&acc.id);
        }
    }
}

/// One review engine.
pub fn engine_check(c: &mut Ctx, cfg: &Config, engine: &str, live: bool) {
    let lang = c.lang;
    match engine {
        "claude" => claude_check(c, &cfg.review.claude, cfg, live),
        "codex" | "gemini" => {
            let (exe, name, install, login): (&str, &str, &str, &str) = if engine == "codex" {
                (
                    &cfg.review.codex.exe,
                    "Codex CLI",
                    FIX_INSTALL_CODEX,
                    FIX_CODEX_LOGIN,
                )
            } else {
                (
                    &cfg.review.gemini.exe,
                    "Gemini CLI",
                    FIX_INSTALL_GEMINI,
                    FIX_GEMINI_LOGIN,
                )
            };
            let Some(path) = which(exe) else {
                c.push(
                    "engine",
                    (name, name),
                    false,
                    format!("{exe:?} {}", l(lang, "没有找到", "not found")),
                    Some((
                        (
                            "安装它，或在插件设置里填写程序路径",
                            "Install it, or set its path in the plugin settings",
                        ),
                        install,
                    )),
                );
                return;
            };
            let p = path.to_string_lossy().to_string();
            let (ok, ver) = out(&[&p, "--version"], 30);
            if !ok {
                c.push(
                    "engine",
                    (name, name),
                    false,
                    crate::util::truncate(ver.trim(), 200),
                    Some((("重新安装", "Reinstall it"), install)),
                );
                return;
            }
            if engine == "codex" && live {
                let (signed, text) = out(&[&p, "login", "status"], 30);
                c.push(
                    "engine_login",
                    ("Codex 登录", "Codex sign-in"),
                    signed,
                    crate::util::truncate(text.trim(), 200),
                    (!signed).then_some((
                        (
                            "在终端里登录 Codex（会打开一个终端窗口）",
                            "Sign in to Codex in a terminal (opens one for you)",
                        ),
                        login,
                    )),
                );
            }
            c.push(
                "engine",
                (name, name),
                true,
                format!("{p} {}", ver.trim()),
                None,
            );
            if engine == "gemini" && !cfg.review.gemini.allow_approve {
                let ch = c.push(
                    "engine_isolation",
                    ("Gemini 隔离", "Gemini isolation"),
                    true,
                    l(
                        lang,
                        "没有系统沙箱：它的结论只评论、不自动 approve",
                        "No OS sandbox: its verdicts comment but never approve",
                    ),
                    None,
                );
                ch.warn = true;
            }
        }
        "api" => {
            let a = &cfg.review.api;
            let name = l(lang, "OpenAI 兼容 API", "OpenAI-compatible API");
            if a.model.is_empty() {
                c.push(
                    "engine",
                    (&name, &name),
                    false,
                    l(lang, "没有填写模型", "no model set"),
                    Some((
                        (
                            "在插件设置里填写模型名",
                            "Set the model in the plugin settings",
                        ),
                        FIX_PLUGINS,
                    )),
                );
                return;
            }
            let url = format!("{}/models", a.base_url.trim_end_matches('/'));
            let mut req = ureq::get(&url).timeout(Duration::from_secs(20));
            if !a.key.is_empty() {
                req = req.set("Authorization", &format!("Bearer {}", a.key));
            }
            match req.call() {
                Ok(_) => {
                    c.push(
                        "engine",
                        (&name, &name),
                        true,
                        format!("{} · {}", a.base_url, a.model),
                        None,
                    );
                }
                Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
                    c.push(
                        "engine",
                        (&name, &name),
                        false,
                        l(lang, "密钥被拒绝", "the key was rejected"),
                        Some((("检查 API 密钥", "Check the API key"), FIX_PLUGINS)),
                    );
                }
                // some compatible servers have no /models: reachable is good enough
                Err(ureq::Error::Status(_, _)) => {
                    c.push(
                        "engine",
                        (&name, &name),
                        true,
                        format!("{} · {}", a.base_url, a.model),
                        None,
                    );
                }
                Err(e) => {
                    c.push(
                        "engine",
                        (&name, &name),
                        false,
                        crate::util::truncate(&e.to_string(), 200),
                        Some((
                            ("检查接口地址和网络", "Check the base URL and the network"),
                            FIX_PLUGINS,
                        )),
                    );
                }
            }
        }
        e if e.starts_with("ext:") => {
            let name = e.trim_start_matches("ext:");
            match crate::plugins::external::find(name) {
                Ok(m) => {
                    c.push(
                        "engine",
                        (&m.display_name, &m.display_name),
                        true,
                        l(lang, "第三方插件已安装", "third-party plugin installed"),
                        None,
                    );
                }
                Err(err) => {
                    c.push(
                        "engine",
                        (name, name),
                        false,
                        format!("{err:#}"),
                        Some((
                            (
                                "重新安装这个插件，或换一个审查引擎",
                                "Reinstall the plugin, or choose another engine",
                            ),
                            FIX_PLUGINS,
                        )),
                    );
                }
            }
        }
        other => {
            c.push(
                "engine",
                (other, other),
                false,
                l(lang, "未知的审查引擎", "unknown review engine"),
                Some((
                    (
                        "在插件里重新选择审查引擎",
                        "Choose the review engine again under Plugins",
                    ),
                    FIX_PLUGINS,
                )),
            );
        }
    }
}

/// A tiny real request, so an expired login is found now rather than at the first review.
fn claude_live(exe: &str, cfg: &Config) -> (bool, String) {
    let mut args = vec![
        exe,
        "-p",
        "--restricted",
        "--tools",
        "",
        "--output-format",
        "json",
        "--no-session-persistence",
        "--settings",
        "{\"disableAllHooks\":true}",
    ];
    if !cfg.review.model.is_empty() {
        args.extend(["--model", cfg.review.model.as_str()]);
    }
    let env = crate::reviewer::claude::claude_env(cfg);
    let r = run_raw(
        &args,
        RunOpts {
            stdin: Some("Reply with the single word: ok"),
            timeout: Duration::from_secs(180),
            env: Some(&env),
            ..Default::default()
        },
    );
    match r {
        Ok(o) => match serde_json::from_str::<Value>(o.stdout.trim()) {
            Ok(res) if o.status == Some(0) && res["is_error"].as_bool() != Some(true) => {
                let model = res["modelUsage"]
                    .as_object()
                    .and_then(|m| m.keys().next().cloned())
                    .unwrap_or("?".into());
                (true, format!("model {model}"))
            }
            Ok(res) => (
                false,
                crate::util::truncate(&res["result"].to_string(), 200),
            ),
            Err(_) => (
                false,
                crate::util::truncate(format!("{}{}", o.stdout, o.stderr).trim(), 200),
            ),
        },
        Err(e) => (false, e.to_string()),
    }
}

#[allow(dead_code)]
fn _assert_object_safe(_: &dyn Notifier) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_claude_offers_install() {
        let cfg = Config::default();
        let v = tools("en", "definitely-not-a-claude-binary", &cfg, false);
        let c = v.iter().find(|c| c.id == "claude").unwrap();
        assert!(!c.ok);
        assert_eq!(c.action, Some(FIX_INSTALL_CLAUDE));
        assert!(c.detail.contains("not found"));
    }

    /// An older configuration (the implicit `github` account, used with `gh` before) shows "not signed in" with a
    /// way forward, instead of failing.
    #[test]
    fn github_account_without_sign_in() {
        crate::secrets::use_fake();
        let cfg = Config::default();
        let mut acc = cfg
            .accounts()
            .into_iter()
            .find(|a| a.kind == "github")
            .unwrap();
        acc.settings
            .insert("client_id".into(), toml::Value::String("Iv1.x".into()));
        let mut c = Ctx::new("en");
        forge_check(&mut c, &acc, &cfg);
        let ch = &c.v[0];
        assert!(!ch.ok && ch.detail == "not signed in" && ch.action == Some(FIX_PLUGINS));
        assert_eq!(ch.account.as_deref(), Some("github"));

        if crate::forge::github_auth::DEFAULT_CLIENT_ID.is_empty() {
            acc.settings.clear();
            let mut c = Ctx::new("en");
            forge_check(&mut c, &acc, &cfg);
            assert!(c.v[0].detail.contains("client ID"));
        }
    }
}
