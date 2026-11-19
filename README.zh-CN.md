# Nod

[English](README.md)

**你的个人代码审查助手。** 别人在 GitHub、Gitee 或 Gitea 上请你审查 PR 时，Nod 在你自己的电脑上用你选的 AI 审查（Claude Code、Codex、Gemini 或任何兼容 OpenAI 接口的模型），只在**你定的规则**允许时才 approve，并通知你。

它是一个常驻在系统托盘的桌面应用。装好后走完一次引导，之后有人点名请你审查，它就把 PR 检出到本地，让 AI 在只读环境里审查，再**以你的身份**把结果发到代码平台：真实存在的问题写成行内评论；只有固定规则判定改动干净时才 approve。结果会通过系统通知、飞书卡片（带「直接通过」按钮）、钉钉、企业微信、Slack 或 Webhook 告诉你。

代码平台、审查引擎和通知渠道都是**插件**，可以自由组合；第三方也能不改源码加新的插件，见 [docs/plugins.md](docs/plugins.md)。

- **在本机运行**：用你自己的 AI 订阅和代码平台账号。不用改任何仓库，不用装平台应用；令牌存在系统钥匙串里。
- **approve 不由模型说了算**：Claude 只给出结构化结论，approve 与否由代码里的固定规则决定。
- **审查进程被隔离**：Claude Code 只有 `Read`、`Grep`、`Glob` 三个只读工具；Codex 在只读的系统沙箱里运行；API 引擎只拿到 diff。审查进程都读不到你的凭据。隔离较弱的引擎（如 Gemini CLI）默认只评论、不 approve。
- **不用碰终端**：安装、配置、查看审查记录、暂停都在窗口和托盘里完成。需要登录 GitHub 或 Claude 时，应用会替你打开终端并填好命令。

## 工作原理

```
代码平台（GitHub / Gitee / Gitea，有人点名请你审查）
   │  每 30 秒：查一遍请你审查的 PR
   ▼
Nod（托盘里常驻） ──► 通知「审查中」（飞书卡片 / Webhook）
   │ 把 PR 检出到独立目录，生成 diff 和提交列表
   ▼
审查引擎：Claude Code / Codex / Gemini / API   （只读，输出结构化 JSON 结论）
   │
   ▼
固定规则 ──► APPROVE：没有高风险问题、没有 PR 引入的中风险问题、
   │                    审查建议通过、CI 没失败、不是你自己的 PR、不是草稿
   └───────► COMMENT：其他情况（行内评论，并写明没通过的原因）
   ▼
以你的身份发评审  +  通知（系统、飞书、钉钉、企业微信、Slack、Webhook）
```

已经审过的 PR 有新提交时会重新审查。如果 Nod 之前 approve 过旧提交，而新提交有问题，它会撤销自己之前的 approve（你手动给的不会动）。

## 前提条件

| | |
|---|---|
| 代码平台 | GitHub：在插件页点「登录 GitHub」；Gitee / Gitea：一个访问令牌，见 [docs/forges.zh-CN.md](docs/forges.zh-CN.md) |
| 审查引擎（任选） | [Claude Code](https://claude.com/claude-code)、[Codex CLI](https://developers.openai.com/codex/cli)、[Gemini CLI](https://geminicli.com) 已登录；或一个 OpenAI 兼容接口的地址和密钥（本地 Ollama 也行） |
| git | 已安装 |
| Windows 10 1809 及以上、macOS 12 及以上，或带桌面环境的 Linux | Linux 托盘需要 AppIndicator 支持（GNOME 要装 AppIndicator 扩展） |

## 安装

> **还没有发布第一个 Release。** 发布前请按「参与开发」从源码构建。

到 [Releases](https://github.com/larrygogo/nod/releases) 下载对应平台的安装包：

| 平台 | 文件 |
|---|---|
| Windows | `nod_x.y.z_x64-setup.exe`（装到当前用户目录，不需要管理员权限） |
| macOS（Apple 芯片 / Intel） | `nod_x.y.z_aarch64.dmg` / `nod_x.y.z_x64.dmg` |
| Linux | `nod_x.y.z_amd64.AppImage`，或 `.deb`（Debian、Ubuntu） |

安装包暂时没有代码签名，第一次打开时系统会提示：

- **Windows**：SmartScreen 提示「已保护你的电脑」时，点「更多信息」→「仍要运行」。
- **macOS**：在「系统设置 → 隐私与安全性」里点「仍要打开」。

### 第一次打开

引导分三步：

1. **代码平台**：选 GitHub、Gitee 或 Gitea，填好登录信息，点「检查」确认能连上。
2. **审查引擎**：选一个默认引擎，点「检查」（会真实调用一次）。没通过的项旁边有「去修复」：需要登录的会打开终端并填好命令，需要安装的会打开下载页。
3. **通知**：可以选多个渠道，各发一条测试消息确认能收到。默认勾选「登录电脑时自动启动」。

完成后 Nod 留在托盘里开始工作。关掉窗口不会停止审查；要停就在托盘菜单里选「暂停审查」或「退出」。

### 从命令行版升级

如果装过 Nod 的命令行版本（0.1 预览版），新应用第一次启动时会自动移除它注册的后台服务（Windows 计划任务、macOS launchd、Linux systemd 用户服务），改为打开「登录电脑时自动启动」，配置和审查记录原样沿用。原来的 GitHub + Claude Code 配置不用改，打开就能用；配置里的明文密钥下次保存时会挪进系统钥匙串。旧的命令行程序文件可以手动删掉（Windows 在 `%LOCALAPPDATA%\Programs\Nod`，macOS / Linux 在 `~/.local/share/Nod`）。

装过 pr-autoreview 的，引导第一步会出现「导入旧配置」，配置、审查记录和仓库缓存都会搬过来。

### 卸载

在托盘菜单退出，然后用系统的方式卸载（Windows「设置 → 应用」，macOS 把应用拖进废纸篓，Linux 删除 AppImage 或 `apt remove Nod`）。配置和数据目录可以在「设置 → 文件位置」里打开后删除。

## 使用

- **托盘**：图标没有角标表示正在等待审查请求；绿点表示正在审查；黄点表示已暂停；红点表示出了问题。菜单里有最近 5 次审查，点击直接打开评审。
- **概览**：先回答「在不在正常工作」，出错时修复按钮就在旁边；还有今天的统计、需要你看一眼的审查、最近的审查。
- **审查记录**：搜索、按状态筛选，右侧看结论、没有 approve 的原因和问题清单。「手动审查一个 PR」可以试审（真实审查但不写入代码平台）或跟踪（没人点名也让它审）。
- **插件**：代码平台账号、审查引擎（默认引擎和按仓库指定）、通知渠道，每个都能配置和自检；第三方插件也在这里。
- **设置**：语言、浅色或深色主题、开机自启、approve 规则、审查范围、超时等。保存后立即按新设置重新开始。

## 配置

设置页里能改所有配置，也可以直接编辑配置文件（Windows `%APPDATA%\nod\config.toml`，macOS `~/Library/Application Support/nod/config.toml`，Linux `~/.config/nod/config.toml`），下次启动时生效。所有选项见 [examples/config.example.toml](examples/config.example.toml)，常用的几项如下：

| 配置项 | 默认值 | 说明 |
|---|---|---|
| `language` | `zh-CN` | `zh-CN` 或 `en`，决定界面、评审意见和通知用的语言 |
| `scope.repos` / `scope.exclude_repos` | 全部 | 审查哪些仓库：`owner/repo`、`owner/*`（整个组织）、`@me/*`（你自己的仓库） |
| `scope.team_requests` | `false` | 请你所在团队审查的 PR 也审 |
| `approve.mode` | `auto` | `auto`（规则全部通过就 approve）或 `never`（只评论） |
| `approve.repos` / `approve.authors` | 全部 | 只在这些仓库、只对这些作者自动 approve（例如 `dependabot[bot]`） |
| `approve.exclude_repos` / `exclude_authors` | | 这些仓库或作者永远不自动 approve |
| `review.model` | Claude Code 默认模型 | 例如小仓库可以换成更快的模型 |
| `review.claude_config_dir` | | 指定使用哪个 Claude Code 账号配置（`CLAUDE_CONFIG_DIR`） |
| `review.engine` | `claude` | 默认审查引擎：`claude`、`codex`、`gemini`、`api` 或 `ext:<插件名>` |
| `review.per_repo` | | 按仓库改用别的引擎，例如 `[{ repo = "my-org/infra", engine = "codex" }]` |
| `[[forges]]` | 一个 GitHub 账号 | 代码平台账号：`id`、`kind`（`github`、`gitee`、`gitea`）、`url`、`enabled` |
| `notify.channels` | | 同时启用的通知渠道：`system`、`feishu`、`dingtalk`、`wecom`、`slack`、`webhook` |

示例：审查组织里除一个仓库以外的所有仓库，但只自动 approve Dependabot 的 PR 和你自己仓库里的 PR：

```toml
[scope]
repos = ["my-org/*", "@me/*"]
exclude_repos = ["my-org/legacy"]

[approve]
authors = ["dependabot[bot]"]
```

飞书卡片上的「直接通过」是你本人的操作，不受 `[approve]` 限制。approve 名单只会**收紧**自动 approve：质量规则（没有高风险问题、CI 没失败等）始终有效。唯一的例外是 `[[approve.always]]` 规则（设置里的「无条件 approve」，每条可填仓库、作者或两者）：符合的 PR 只要请你审查就 approve，不看审查结果、不等 CI，评论照常发。

令牌、App Secret、API 密钥和机器人 Webhook 地址都存在系统钥匙串里，配置文件里没有明文（钥匙串不可用时才退回写进文件，并提示你）。飞书和 Webhook 的密钥也可以用环境变量提供：`NOD_FEISHU_APP_SECRET`、`NOD_WEBHOOK_SECRET`。

## 通知

- **系统通知**：审查结果发出、审查失败或整轮出错时弹一条，点击打开对应的 PR 或评审。中间阶段（排队、审查中）只在窗口和托盘里显示，不刷屏。
- **钉钉 / 企业微信 / Slack**：群机器人，发同样的结果通知（钉钉支持加签）。
- **飞书 / Lark**：每个 PR 一张卡片，原地刷新。卡片上有 PR 状态、CI、改动量、结论和问题清单，还有「直接通过」按钮：点击后会中止正在进行的审查，并 approve 卡片上显示的那个提交。**每个人都要建自己的飞书应用**，因为飞书长连接每次只把点击交给其中一个连接方。步骤见 [docs/feishu-setup.zh-CN.md](docs/feishu-setup.zh-CN.md)。
- **Webhook**：每次状态变化发一个带签名的 JSON `POST`，可以接到 Slack/Teams 工作流、n8n 或你自己的服务。格式和签名方式见 [docs/webhook.md](docs/webhook.md)。
- **不通知**：审查结果只出现在 GitHub 和窗口里。

要接入新的代码平台、审查引擎或通知渠道，可以写一个外部插件（任何语言，见 [docs/plugins.md](docs/plugins.md)），也可以在源码里实现对应的 trait（见 [CONTRIBUTING.md](CONTRIBUTING.md)）。

## 安全模型

- 每个审查引擎都在只读环境里运行：Claude Code 用 `claude -p --restricted --tools Read,Grep,Glob --strict-mcp-config` 并关闭 hooks；Codex 用 `codex exec --sandbox read-only`；Gemini 用只读的 plan 模式（没有系统沙箱，所以默认不 approve）；API 引擎只收到 diff。启动前，看起来像凭据的环境变量会被移除。
- PR 描述、评论和仓库里的文件都只当作数据传给模型，提示词明确要求它忽略其中的指令。即使提示注入成功，模型也做不了任何事：发评审和 approve 都由 Nod 在固定规则通过后执行。
- 模型输出必须符合 schema，格式不对一律按审查失败处理，绝不会当成通过。
- 令牌和密钥存在系统钥匙串里；外部插件在独立进程里运行，拿不到你的凭据，崩溃或乱输出只会让那一次调用失败。
- 评审内容发出前会扫描是否含有凭据特征。
- PR 里的符号链接会被检出为普通文件。
- 「直接通过」只接受来自发给你的那张卡片、并且是在你和机器人的私聊里点的。
- 窗口只能调用应用自带的少数几个操作，打开链接只允许 http(s) 网址。

更多细节和已知限制见 [docs/security.md](docs/security.md)。报告漏洞请按 [SECURITY.md](SECURITY.md) 的方式。

## 排障

- **被点名了却没反应**：看托盘图标和窗口顶部的状态栏，出错时那里会写原因并给出修复按钮。默认只处理直接点名请**你**审查的；想连请你所在团队审查的也处理，在设置里勾选「团队被点名时也审查」，另外检查「审查范围」有没有把仓库排除掉。日志在「设置 → 文件位置 → 查看日志」。
- **状态栏写着「另一个 Nod 正在审查」**：旧的命令行版还在运行。退出 Nod 再打开，它会自动清理；仍然不行就在任务管理器里结束旧的 `nod.exe run` 进程。
- **macOS / Linux 上找不到 claude**：应用启动时会读取你登录 shell 的 PATH。把 `claude` 装在 shell 能找到的位置后，在托盘里退出再打开；也可以在设置里填 `claude` 的完整路径。
- **飞书卡片收到了，点按钮却报错**：Nod 必须在运行且没有暂停；另外一个飞书应用的长连接只能由一个程序占用。见 [docs/feishu-setup.zh-CN.md](docs/feishu-setup.zh-CN.md#排障)。
- **看起来没问题却没有 approve**：窗口、卡片和 GitHub 评审里都会写明是哪条规则拦住了，比如 CI 还没跑完，或者这是你自己的 PR。

## 计划

- 自动更新。
- 更多代码平台（GitLab、Bitbucket、Azure DevOps）和通知渠道，欢迎以外部插件的形式贡献。

## 参与开发

见 [CONTRIBUTING.md](CONTRIBUTING.md)。更新记录见 [CHANGELOG.md](CHANGELOG.md)。

## 许可证

[MIT](LICENSE)
