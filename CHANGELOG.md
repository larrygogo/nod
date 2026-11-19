# Changelog

All notable changes are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/) and the
project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- GitHub signs in with an OAuth device-flow login from the GitHub plugin page (the way the GitHub CLI does); `gh`
  is no longer needed or used. A personal access token can be entered instead of signing in. GitHub Enterprise
  accounts can set their own OAuth App's client ID.
- **Review** (Reviews and Overview pages) is one real review of a PR someone sent you directly, under the usual
  approval rules; *Track* and *Dry run* are gone. Pasted links are recognized with surrounding spaces, inside a
  sentence, or ending in `/files`, `/changes`, `/commits/…`, and the review starts at once.
- Overview: one list of the latest reviews, newest first; the ones that need you are flagged in place.
- Small brand tiles use a solid fill so they stay legible at 18 px.

### Added

- Webhook recognises chat and push apps and sends in their format: Feishu / Lark, DingTalk, WeCom, Slack, Discord,
  Microsoft Teams, Google Chat, Telegram, Bark, Server酱, ntfy. New channel: Feishu / Lark group bot.

- Review signature: turn the note at the end of each review off, or write your own (Settings → Review; `{engine}`
  names the engine). The default note now names the engine that actually reviewed instead of always saying Claude.

### Fixed

- A PR merged or closed while waiting for CI no longer claims it was not approved because CI was still running.

## [0.1.0]

First public release: a desktop app.

- Watches GitHub for pull requests that request **your** review (every 30 s) and reviews them with Claude Code in a
  read-only sandbox (`--restricted`, `Read`/`Grep`/`Glob` only, credentials stripped from the environment).
- Approves only when fixed rules pass; otherwise posts one review with inline comments and the blocking reasons.
- Re-reviews when new commits arrive; dismisses its own earlier approval if a later commit has problems.
- Lives in the system tray (status dot: reviewing, paused, needs attention; pause, resume, quit); starts at sign-in.
- Window: first-run setup with a health check whose *Fix* buttons open a terminal for sign-ins or the download page;
  recent reviews with verdicts and findings; dry runs and tracking a PR by hand; every setting, applied on save.
- Problems are explained in plain words in the window and tray, with a fix button; a single network hiccup does not
  raise an alarm.
- Notifications: desktop notifications that open the PR when clicked; Feishu/Lark interactive cards updated in place,
  with an *Approve now* button over a long connection (no public URL); generic signed webhook; or none.
- Chinese and English for the window, review comments, notifications and documentation.
- Plugins everywhere:
  - code hosts: GitHub (incl. Enterprise), Gitee, Gitea / Forgejo; several accounts at once;
  - review engines: Claude Code, OpenAI Codex CLI, Gemini CLI, any OpenAI-compatible API; a default plus
    per-repository rules;
  - notification channels: desktop, Feishu/Lark, DingTalk, WeCom, Slack, webhook; several at once;
  - third-party plugins: a program plus `plugin.toml`, JSON-RPC 2.0 over stdio (docs/plugins.md).
- Tokens and secrets are kept in the system keychain.
- Window redesigned (docs/design.md): Overview, Reviews with search and a detail pane, Plugins, Settings, light and
  dark themes; the tray menu lists the latest reviews.
- Installers: Windows (NSIS, per user), macOS (DMG, arm64 and x64), Linux (AppImage and deb). About 8 MB of memory
  while in the tray; the window's web view is released when the window is closed.
- Upgrading from the command-line previews: their background service is removed on first start and replaced by
  start-at-sign-in; settings and review history carry over. pr-autoreview settings can be imported during setup.
