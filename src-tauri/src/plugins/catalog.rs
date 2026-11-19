//! What the window knows about every plugin: name, description, icon, settings (the form is built from these) and
//! badges that state what the plugin can see or do.

use serde::Serialize;

use super::external;

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    /// Config path for engines and channels (`review.codex.model`); a field of the account for code hosts (`url`).
    pub key: String,
    pub label: String,
    /// text | password | number | bool | select
    pub kind: String,
    pub options: Vec<(String, String)>,
    pub placeholder: String,
    pub help: String,
    /// Stored in the system keychain; the window never receives the value.
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Badge {
    /// good | warn | info
    pub tone: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plugin {
    /// `github`, `codex`, `feishu`, `ext:<name>` …
    pub id: String,
    /// forge | engine | notify
    pub kind: String,
    pub name: String,
    pub description: String,
    /// Icon name the window knows (brand or generic).
    pub icon: String,
    pub builtin: bool,
    pub fields: Vec<Field>,
    pub badges: Vec<Badge>,
    /// Setup guide.
    pub help_url: String,
    /// Could not be loaded (external plugins).
    pub error: Option<String>,
}

struct L<'a>(&'a str);
impl L<'_> {
    fn t(&self, zh: &str, en: &str) -> String {
        if self.0 == "en" { en } else { zh }.to_string()
    }
}

fn f(key: &str, label: String, kind: &str) -> Field {
    Field {
        key: key.into(),
        label,
        kind: kind.into(),
        options: vec![],
        placeholder: String::new(),
        help: String::new(),
        secret: kind == "password",
    }
}

impl Field {
    fn ph(mut self, p: &str) -> Self {
        self.placeholder = p.into();
        self
    }
    fn help(mut self, h: String) -> Self {
        self.help = h;
        self
    }
}

fn b(tone: &'static str, text: String) -> Badge {
    Badge { tone, text }
}

#[allow(clippy::too_many_arguments)]
fn p(
    id: &str,
    kind: &str,
    name: &str,
    description: String,
    icon: &str,
    fields: Vec<Field>,
    badges: Vec<Badge>,
    help_url: &str,
) -> Plugin {
    Plugin {
        id: id.into(),
        kind: kind.into(),
        name: name.into(),
        description,
        icon: icon.into(),
        builtin: true,
        fields,
        badges,
        help_url: help_url.into(),
        error: None,
    }
}

const DOCS: &str = "https://github.com/larrygogo/nod/blob/main/docs";

pub fn catalog(lang: &str) -> Vec<Plugin> {
    let l = L(lang);
    let zh = lang != "en";
    let doc = |name: &str| format!("{DOCS}/{name}{}.md", if zh { ".zh-CN" } else { "" });
    let mut v = vec![
        // ------------------------------------------------------------ code hosts
        p("github", "forge", "GitHub",
          l.t("一键登录，以你的身份审查。也可以填个人访问令牌。", "Sign in with one click; Nod reviews as you. A personal access token works too."),
          "github",
          vec![
              f("url", l.t("Enterprise 地址", "Enterprise address"), "text").ph(&l.t("github.com 留空", "empty for github.com")),
              f("settings.client_id", l.t("Enterprise Client ID", "Enterprise client ID"), "text").ph("Ov23li…"),
              f("token", l.t("个人访问令牌（可选）", "Personal access token (optional)"), "password").ph("ghp_…")
                  .help(l.t("填了就不用登录。需要 repo 权限。", "Replaces the sign-in. Needs the repo scope.")),
          ],
          vec![], "https://docs.github.com/en/apps/oauth-apps/using-oauth-apps/authorizing-oauth-apps"),
        p("gitee", "forge", "Gitee",
          l.t("需要私人令牌。Gitee 不支持撤销已给出的通过。", "Gitee (gitee.com). Needs a personal access token. Gitee does not let a reviewer withdraw an approval."),
          "gitee",
          vec![
              f("token", l.t("私人令牌", "Personal access token"), "password")
                  .help(l.t("在「设置 → 私人令牌」生成，勾选 user_info、projects、pull_requests。", "Generate it under Settings → Personal access tokens with user_info, projects and pull_requests.")),
              f("url", l.t("地址（gitee.com 留空）", "Address (empty for gitee.com)"), "text").ph("https://gitee.com"),
          ],
          vec![b("info", l.t("令牌存系统钥匙串", "Token kept in the system keychain")), b("warn", l.t("通过后无法撤销", "Approvals cannot be withdrawn"))], &doc("forges")),
        p("gitea", "forge", "Gitea / Forgejo",
          l.t("自建 Gitea / Forgejo（含 Codeberg），需要访问令牌。", "Self-hosted Gitea or Forgejo (Codeberg too). Needs an access token."),
          "gitea",
          vec![
              f("url", l.t("地址", "Address"), "text").ph("https://gitea.example.com"),
              f("token", l.t("访问令牌", "Access token"), "password")
                  .help(l.t("在「设置 → 应用」创建，权限勾选 read:user、write:repository、write:issue。", "Create it under Settings → Applications with read:user, write:repository and write:issue.")),
          ],
          vec![b("info", l.t("令牌存系统钥匙串", "Token kept in the system keychain"))], &doc("forges")),
        // ------------------------------------------------------------ review engines
        p("claude", "engine", "Claude Code",
          l.t("用你自己的 Claude 订阅，只开放只读工具。", "Anthropic's Claude Code on your own subscription, with only the read-only Read, Grep and Glob tools."),
          "claude",
          vec![
              f("review.claude", l.t("claude 程序", "claude program"), "text").ph("claude"),
              f("review.claude_config_dir", l.t("配置目录（换账号时填）", "Config directory (for another account)"), "text"),
              f("review.model", l.t("模型（留空用默认）", "Model (empty = default)"), "text"),
          ],
          vec![b("good", l.t("读整个仓库", "Reads the whole repository")), b("good", l.t("只读，不能执行命令", "Read-only, no commands"))],
          "https://docs.claude.com/en/docs/claude-code/setup"),
        p("codex", "engine", "OpenAI Codex",
          l.t("在只读沙箱里运行：不能改文件、不能联网。", "OpenAI's Codex CLI in its read-only OS sandbox: reads files and runs read-only commands; no writes, no network."),
          "openai",
          vec![
              f("review.codex.exe", l.t("codex 程序", "codex program"), "text").ph("codex"),
              f("review.codex.model", l.t("模型（留空用默认）", "Model (empty = default)"), "text"),
          ],
          vec![b("good", l.t("读整个仓库", "Reads the whole repository")), b("good", l.t("系统沙箱只读", "Read-only OS sandbox"))],
          "https://developers.openai.com/codex/cli"),
        p("gemini", "engine", "Gemini CLI",
          l.t("只读 plan 模式运行。没有系统沙箱，默认只评论、不 approve。", "Google's Gemini CLI in read-only plan mode. There is no OS sandbox, so by default it comments but never approves."),
          "gemini",
          vec![
              f("review.gemini.exe", l.t("gemini 程序", "gemini program"), "text").ph("gemini"),
              f("review.gemini.model", l.t("模型（留空用默认）", "Model (empty = default)"), "text"),
              f("review.gemini.allow_approve", l.t("允许触发自动 approve（隔离较弱）", "Let its verdicts lead to auto-approval (weaker isolation; at your own risk)"), "bool"),
          ],
          vec![b("good", l.t("读整个仓库", "Reads the whole repository")), b("warn", l.t("无系统沙箱，默认不 approve", "No OS sandbox; does not approve by default"))],
          "https://geminicli.com/docs"),
        p("api", "engine", l.t("OpenAI 兼容 API", "OpenAI-compatible API").as_str(),
          l.t("OpenAI、DeepSeek、通义千问、Ollama 等兼容接口。只看 diff，看不到仓库。", "Any OpenAI-compatible service: OpenAI, DeepSeek, Qwen, OpenRouter, a local Ollama… The model sees only the diff, not the repository."),
          "api",
          vec![
              f("review.api.base_url", l.t("接口地址", "Base URL"), "text").ph("https://api.deepseek.com/v1")
                  .help(l.t("Ollama 填 http://localhost:11434/v1", "For Ollama: http://localhost:11434/v1")),
              f("review.api.model", l.t("模型", "Model"), "text").ph("deepseek-chat"),
              f("review.api.key", l.t("API 密钥（本地模型可留空）", "API key (may be empty for local models)"), "password"),
              f("review.api.max_diff_kb", l.t("diff 最多发送（KB）", "Send at most this much diff (KB)"), "number"),
          ],
          vec![b("warn", l.t("只看 diff", "Sees only the diff")), b("good", l.t("不接触本机文件", "Never touches local files"))], ""),
        // ------------------------------------------------------------ notifications
        p("system", "notify", l.t("系统通知", "Desktop notifications").as_str(),
          l.t("审查完成或出错时弹系统通知，点击打开 PR。", "A desktop notification when a review is posted or something fails; click it to open the PR."),
          "bell", vec![], vec![], ""),
        p("feishu", "notify", l.t("飞书应用", "Feishu / Lark app").as_str(),
          l.t("每个 PR 一张卡片，原地刷新，可直接通过。需自建飞书应用。", "One card per PR, updated in place, with an Approve now button. Needs your own Feishu app."),
          "feishu",
          vec![
              f("notify.feishu.app_id", "App ID".into(), "text").ph("cli_…"),
              f("notify.feishu.app_secret", "App Secret".into(), "password"),
              f("notify.feishu.recipient", l.t("接收人", "Recipient"), "text").ph("email:you@example.com"),
              f("notify.feishu.domain", l.t("开放平台地址", "Open platform URL"), "text")
                  .help(l.t("Lark 国际版填 https://open.larksuite.com", "Lark: https://open.larksuite.com")),
          ],
          vec![b("good", l.t("可直接通过", "Approve from chat"))], &doc("feishu-setup")),
        p("feishu_bot", "notify", l.t("飞书群机器人", "Feishu / Lark group bot").as_str(),
          l.t("群里的自定义机器人，粘贴它的 Webhook 地址即可，不用建应用。", "A group's custom bot: paste its webhook address, no app needed."),
          "feishu",
          vec![
              f("notify.feishu_bot.url", "Webhook".into(), "password").ph("https://open.feishu.cn/open-apis/bot/v2/hook/…"),
              f("notify.feishu_bot.secret", l.t("签名校验密钥（可选）", "Signature secret (optional)"), "password"),
          ],
          vec![b("info", l.t("只读通知", "Notifications only"))], ""),
        p("dingtalk", "notify", l.t("钉钉", "DingTalk").as_str(),
          l.t("群自定义机器人，建议开启加签。", "A custom group robot. Choose the signing (加签) security setting."),
          "dingtalk",
          vec![
              f("notify.dingtalk.url", "Webhook".into(), "password").ph("https://oapi.dingtalk.com/robot/send?access_token=…"),
              f("notify.dingtalk.secret", l.t("加签密钥（SEC 开头）", "Signing secret (starts with SEC)"), "password"),
          ],
          vec![b("info", l.t("只读通知", "Notifications only"))], ""),
        p("wecom", "notify", l.t("企业微信", "WeCom").as_str(),
          l.t("群机器人的 Webhook 地址。", "A group robot: group settings → robots → add, then copy its webhook address."),
          "wecom",
          vec![f("notify.wecom.url", "Webhook".into(), "password").ph("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…")],
          vec![b("info", l.t("只读通知", "Notifications only"))], ""),
        p("slack", "notify", "Slack",
          l.t("Slack 的 Incoming Webhook。", "A Slack incoming webhook."),
          "slack",
          vec![f("notify.slack.url", "Webhook".into(), "password").ph("https://hooks.slack.com/services/…")],
          vec![b("info", l.t("只读通知", "Notifications only"))], "https://api.slack.com/messaging/webhooks"),
        p("webhook", "notify", "Webhook",
          l.t("状态变化时发送带签名的 JSON POST。", "A signed JSON POST on every change, for your own service or automation."),
          "webhook",
          vec![
              f("notify.webhook.url", l.t("地址", "URL"), "text").ph("https://"),
              f("notify.webhook.secret", l.t("签名密钥（可选）", "Signing secret (optional)"), "password"),
          ],
          vec![], &format!("{DOCS}/webhook.md")),
    ];
    let (ok, bad) = external::discover();
    for m in ok {
        let mut fields: Vec<Field> = m
            .settings
            .iter()
            .map(|s| {
                let label = if !zh && !s.label_en.is_empty() {
                    s.label_en.clone()
                } else {
                    s.label.clone()
                };
                let key = if m.kind == "forge" {
                    format!("settings.{}", s.key)
                } else {
                    format!("plugins.{}.{}", m.name, s.key)
                };
                let mut fl = f(&key, label, &s.r#type)
                    .ph(&s.placeholder)
                    .help(s.help.clone());
                fl.options = s.options.iter().map(|o| (o.clone(), o.clone())).collect();
                fl
            })
            .collect();
        if m.kind == "forge" {
            fields.insert(0, f("url", l.t("地址", "Address"), "text"));
            fields.insert(1, f("token", l.t("令牌", "Token"), "password"));
        }
        let mut badges = vec![b("info", l.t("第三方插件", "Third-party plugin"))];
        if m.kind == "engine" {
            badges.push(if m.capabilities.isolation == "strong" {
                b(
                    "good",
                    l.t("声明为只读沙箱", "Declares a read-only sandbox"),
                )
            } else {
                b(
                    "warn",
                    l.t(
                        "隔离未声明，不自动 approve",
                        "Isolation not declared; never approves",
                    ),
                )
            });
        }
        v.push(Plugin {
            id: format!("ext:{}", m.name),
            kind: m.kind.clone(),
            name: m.display_name.clone(),
            description: m.description.clone(),
            icon: "plugin".into(),
            builtin: false,
            fields,
            badges,
            help_url: String::new(),
            error: None,
        });
    }
    for (folder, err) in bad {
        v.push(Plugin {
            id: format!("ext:{folder}"),
            kind: "invalid".into(),
            name: folder,
            description: String::new(),
            icon: "plugin".into(),
            builtin: false,
            fields: vec![],
            badges: vec![],
            help_url: String::new(),
            error: Some(err),
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_is_described_and_its_fields_exist_in_the_config() {
        let cfg = serde_json::to_value(crate::config::Config::default()).unwrap();
        let ids: Vec<String> = catalog("en")
            .iter()
            .filter(|p| p.builtin)
            .map(|p| p.id.clone())
            .collect();
        for k in crate::config::FORGES.iter().chain(&crate::config::ENGINES) {
            assert!(ids.contains(&k.to_string()), "{k} missing from the catalog");
        }
        for c in crate::config::CHANNELS.iter().filter(|c| **c != "none") {
            assert!(ids.contains(&c.to_string()), "{c} missing from the catalog");
        }
        for p in catalog("zh-CN")
            .iter()
            .filter(|p| p.builtin && p.kind != "forge")
        {
            for fl in &p.fields {
                let mut v = &cfg;
                for part in fl.key.split('.') {
                    v = &v[part];
                }
                assert!(!v.is_null(), "{}: {} is not a config path", p.id, fl.key);
            }
        }
    }
}
