# Nod

[简体中文](README.zh-CN.md)

**Your personal code-review agent.** When someone requests your review on GitHub, Gitee or Gitea, Nod reviews
the pull request on your own computer with the AI you choose (Claude Code, Codex, Gemini, or any OpenAI-compatible
model), approves only when **your rules** allow it, and lets you know.

It is a desktop app that lives in the system tray. Set it up once; from then on, when you are asked to review, it
checks the PR out locally, has the AI review it read-only, and posts the result to the code host **as you**: real
problems become inline comments, and it approves only when fixed rules say the change is clean. You hear about it
through a desktop notification, a Feishu/Lark card (with an *Approve now* button), DingTalk, WeCom, Slack or a webhook.

Code hosts, review engines and notification channels are all **plugins** you can combine; third parties can add new
ones without changing Nod — see [docs/plugins.md](docs/plugins.md).

- **Runs on your machine** with your own AI subscription and code-host accounts. Nothing to change in your
  repositories, no app to install on the code host; tokens live in the system keychain.
- **The model does not decide to approve.** Claude returns a structured verdict; fixed rules in the code decide.
- **The reviewer is isolated.** Claude Code only gets `Read`, `Grep` and `Glob`; Codex runs in its read-only OS
  sandbox; the API engine only receives the diff. None of them see your credentials. Engines without an OS sandbox
  (Gemini CLI) comment but do not approve by default.
- **No terminal needed.** Install, set up, browse reviews and pause from the window and tray. When you need to sign in
  to GitHub or Claude, the app opens a terminal with the command ready.

## How it works

```
code host (GitHub / Gitee / Gitea: someone requests your review)
   │  every 30 s: look up PRs that request your review
   ▼
Nod (in the tray) ──► "reviewing" notification (Feishu card / webhook)
   │ checks the PR out into its own directory, prepares the diff and commit list
   ▼
review engine: Claude Code / Codex / Gemini / API   (read-only, structured JSON verdict)
   │
   ▼
fixed rules ──► APPROVE: no high-severity issue, no medium issue introduced by the PR,
   │                     reviewer recommends approval, CI not failing, not your own PR, not a draft
   └──────────► COMMENT: everything else (inline comments plus the reasons it was not approved)
   ▼
review posted as you  +  notifications (desktop, Feishu, DingTalk, WeCom, Slack, webhook)
```

A PR you already reviewed is reviewed again when new commits arrive. If Nod approved an earlier commit and a
later one has problems, it dismisses its own approval (never one you gave by hand).

## Requirements

| | |
|---|---|
| A code host | GitHub: the [GitHub CLI](https://cli.github.com), signed in (*Fix* opens a terminal to sign in); Gitee / Gitea: an access token, see [docs/forges.md](docs/forges.md) |
| A review engine (any one) | [Claude Code](https://claude.com/claude-code), [Codex CLI](https://developers.openai.com/codex/cli) or [Gemini CLI](https://geminicli.com), signed in; or an OpenAI-compatible endpoint and key (a local Ollama works) |
| git | installed |
| Windows 10 1809+, macOS 12+, or Linux with a desktop | the Linux tray needs AppIndicator support (on GNOME, the AppIndicator extension) |

## Install

> **No release has been published yet.** Until then, build from source (see *Contributing*).

Download the installer for your platform from [Releases](https://github.com/larrygogo/nod/releases):

| Platform | File |
|---|---|
| Windows | `nod_x.y.z_x64-setup.exe` (installs for the current user, no admin rights needed) |
| macOS (Apple silicon / Intel) | `nod_x.y.z_aarch64.dmg` / `nod_x.y.z_x64.dmg` |
| Linux | `nod_x.y.z_amd64.AppImage`, or the `.deb` (Debian, Ubuntu) |

The installers are not code-signed yet, so the first launch shows a warning:

- **Windows**: when SmartScreen says "Windows protected your PC", click *More info* → *Run anyway*.
- **macOS**: in *System Settings → Privacy & Security*, click *Open Anyway*.

### First launch

Setup has three steps:

1. **Code host**: GitHub, Gitee or Gitea; enter the sign-in details and click *Check*.
2. **Review engine**: pick the default and click *Check* (one real call). Anything that fails has a *Fix* button:
   sign-ins open a terminal with the command ready, missing tools open the download page.
3. **Notifications**: pick any number of channels and send each a test. *Start when I sign in* is on by default.

Then Nod stays in the tray and gets to work. Closing the window does not stop reviewing; use *Pause reviewing*
or *Quit* in the tray menu.

### Upgrading from the command-line edition

If you used the command-line edition of Nod (the 0.1 previews), the app removes the background service it
registered (Windows Task Scheduler, macOS launchd, Linux systemd user unit) on first start, turns on *Start when I
sign in* in its place, and keeps using your settings and review history. A GitHub + Claude Code configuration works as
it is; plain-text secrets in it move to the system keychain the next time you save. You can delete the old program files by hand (`%LOCALAPPDATA%\Programs\Nod` on
Windows, `~/.local/share/Nod` on macOS and Linux).

If you used pr-autoreview, the first setup step offers *Import old settings*: settings, review history and the
repository cache are moved over.

### Uninstall

Quit from the tray menu, then uninstall the usual way (Windows *Settings → Apps*, drag the app to the Trash on macOS,
delete the AppImage or `apt remove Nod` on Linux). *Settings → Files* opens the config and data folders if you
want to delete them too.

## Using it

- **Tray**: no dot means waiting for review requests; green, reviewing; amber, paused; red, needs attention. The menu
  lists the five latest reviews; click one to open it.
- **Overview**: answers "is it working?" first, with the fix next to any problem; today's numbers, reviews worth a
  look, the latest reviews.
- **Reviews**: search and filter; the detail pane shows the verdict, why it was not approved, and the findings.
  *Review a PR by hand* runs a dry run (a real review that writes nothing) or tracks a PR nobody asked you about.
- **Plugins**: code-host accounts, review engines (default and per repository), notification channels — each with
  its settings and a self-test; third-party plugins show up here too.
- **Settings**: language, light or dark theme, start at sign-in, approval rules, scope, timeouts. Saving restarts
  reviewing with the new settings right away.

## Configuration

Everything is editable in Settings. You can also edit the file (Windows `%APPDATA%\nod\config.toml`, macOS
`~/Library/Application Support/nod/config.toml`, Linux `~/.config/nod/config.toml`); changes take effect
the next time the app starts. Every option is described in
[examples/config.example.toml](examples/config.example.toml); the common ones:

| Option | Default | |
|---|---|---|
| `language` | `zh-CN` | `zh-CN` or `en`: window, review comments and notifications |
| `scope.repos` / `scope.exclude_repos` | all | which repositories to review: `owner/repo`, `owner/*` (an organisation), `@me/*` (your own) |
| `scope.team_requests` | `false` | also review PRs that request a team you belong to |
| `approve.mode` | `auto` | `auto` (approve when every rule passes) or `never` (comment only) |
| `approve.repos` / `approve.authors` | all | auto-approve only in these repositories / for these authors (e.g. `dependabot[bot]`) |
| `approve.exclude_repos` / `exclude_authors` | | never auto-approve these |
| `review.model` | Claude Code's default | e.g. a faster model for small repositories |
| `review.claude_config_dir` | | which Claude Code profile to use (`CLAUDE_CONFIG_DIR`) |
| `review.engine` | `claude` | default engine: `claude`, `codex`, `gemini`, `api` or `ext:<plugin>` |
| `review.per_repo` | | another engine for some repositories, e.g. `[{ repo = "my-org/infra", engine = "codex" }]` |
| `[[forges]]` | one GitHub account | code-host accounts: `id`, `kind` (`github`, `gitee`, `gitea`), `url`, `enabled` |
| `notify.channels` | | channels on at once: `system`, `feishu`, `dingtalk`, `wecom`, `slack`, `webhook` |

Example: review everything in an organisation except one repository, but auto-approve only Dependabot's PRs and PRs
in your own repositories:

```toml
[scope]
repos = ["my-org/*", "@me/*"]
exclude_repos = ["my-org/legacy"]

[approve]
authors = ["dependabot[bot]"]
```

*Approve now* on a Feishu card is your own action and is not limited by `[approve]`. The approve lists only **narrow**
auto-approval: the quality rules (no high-severity issue, CI not failing, …) always apply.

Tokens, app secrets, API keys and robot webhook addresses live in the system keychain, not in the file (only if the
keychain is unavailable do they stay in the file, and the app tells you). The Feishu and webhook secrets can also come
from the environment: `NOD_FEISHU_APP_SECRET`, `NOD_WEBHOOK_SECRET`.

## Notifications

- **Desktop notifications**: one when a review is posted, fails, or a whole round goes wrong; click it to open the PR
  or the review. The in-between stages (queued, reviewing) show in the window and tray only.
- **DingTalk / WeCom / Slack**: group robots with the same outcome messages (DingTalk signing supported).
- **Feishu / Lark**: one card per PR, updated in place, with PR state, CI, size, verdict and findings, and an
  *Approve now* button that stops a running review and approves the commit shown on the card. **Everyone needs their
  own Feishu app**, because the long connection hands each click to only one connected program. See
  [docs/feishu-setup.md](docs/feishu-setup.md).
- **Webhook**: a signed JSON `POST` on every change, for Slack/Teams workflows, n8n or your own service. See
  [docs/webhook.md](docs/webhook.md).
- **None**: results only on GitHub and in the window.

To add a code host, engine or channel, write an external plugin in any language ([docs/plugins.md](docs/plugins.md))
or implement its trait in the source ([CONTRIBUTING.md](CONTRIBUTING.md)).

## Security model

- Every engine runs read-only: Claude Code as `claude -p --restricted --tools Read,Grep,Glob --strict-mcp-config` with
  hooks disabled; Codex as `codex exec --sandbox read-only`; Gemini CLI in read-only plan mode (no OS sandbox, so it
  does not approve by default); the API engine only receives the diff. Environment variables that look like
  credentials are removed before an engine starts.
- PR descriptions, comments and repository files reach the model as data only; the prompt tells it to ignore
  instructions in them. Even a successful prompt injection cannot do anything: Nod posts reviews and approvals
  itself, after the fixed rules pass.
- The model's output must match a schema; anything else counts as a failed review, never as a pass.
- Tokens and secrets live in the system keychain; external plugins run in their own process without your
  credentials, and a crash or garbage output only fails that one call.
- Review text is scanned for credential patterns before it is posted.
- Symbolic links in a PR are checked out as plain files.
- *Approve now* only accepts clicks on the card sent to you, in your private chat with the bot.
- The window can call only the app's own handful of commands, and only http(s) links can be opened.

Details and known limits: [docs/security.md](docs/security.md). To report a vulnerability, see
[SECURITY.md](SECURITY.md).

## Troubleshooting

- **Asked to review, nothing happens**: look at the tray icon and the status bar at the top of the window; problems
  are explained there with a fix button. By default only requests to **you** personally count; to include teams,
  tick *Also when a team I am in is requested* in Settings, and check that *What to review* does not exclude the
  repository. The log is under *Settings → Files → Show log*.
- **"Another Nod is already reviewing"**: the old command-line edition is still running. Quit Nod and
  open it again; it cleans up on start. If that does not help, end the old `nod.exe run` process in Task Manager.
- **gh or claude not found on macOS / Linux**: the app reads PATH from your login shell when it starts. Install
  `gh` and `claude` where your shell finds them, then quit and reopen the app, or set the full path of `claude` in
  Settings.
- **The Feishu button shows an error**: Nod must be running and not paused, and only one program may hold a
  Feishu app's long connection. See [docs/feishu-setup.md](docs/feishu-setup.md#troubleshooting).
- **Looks fine but was not approved**: the window, the card and the GitHub review all say which rule held it back,
  e.g. CI still running, or it is your own PR.

## Roadmap

- Automatic updates.
- More code hosts (GitLab, Bitbucket, Azure DevOps) and channels — contributions welcome as external plugins.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE)
