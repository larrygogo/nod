# Changelog

All notable changes are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/) and the
project uses [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-11-19

First public release: a desktop app.

- Watches GitHub for pull requests that request **your** review (every 30 s) and reviews them with Claude Code in a
  read-only sandbox (`--restricted`, `Read`/`Grep`/`Glob` only, credentials stripped from the environment).
- Approves only when fixed rules pass; otherwise posts one review with inline comments and the blocking reasons.
- Re-reviews when new commits arrive; dismisses its own earlier approval if a later commit has problems.
- Lives in the system tray (status dot: reviewing, paused, needs attention; pause, resume, quit); starts at sign-in.
- Window: first-run setup with a health check whose *Fix* buttons point at the missing piece;
  recent reviews with verdicts and findings; **Review** a PR someone sent you by link; every setting, applied on save.
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

### More in this release

- *Approve now* in the Reviews detail pane (two clicks), like the Feishu card's button: approves exactly the commit
  shown, as you; a newer push, your own PR or a closed one is refused, with the reason on the record.
- *Review again* in the Reviews detail pane for any open PR that is not being reviewed: the current commit is
  reviewed from scratch and its outcome posted again, whatever happened before (not only after a failure).
- *Withdraw approval* in the Reviews detail pane (two clicks): dismisses the approvals Nod gave on the PR (its own and
  *Approve now*), so it can no longer be merged on them. Approvals given on GitHub itself are left alone.
- A record whose earlier approval (Nod's or yours) still counts on GitHub shows *Approved*; the overview flags it
  when the latest commit did not pass.
- Always approve (`[[approve.always]]` rules, Settings → Review; each a repository, an author, or both, which must
  then both match): matching PRs are approved
  whenever you are asked, whatever the review finds, without waiting for CI and even if the review fails; the
  findings are still posted as comments. Your own PRs, drafts and closed PRs are still never approved.
- An approval says why ("every approval rule passed" or "on the always-approve list"); the review signature no
  longer lists the rules, which was incomplete. Settings spell out every approval rule.
- Robot webhook addresses are shown as you type them (still kept in the keychain once saved); other secrets have an
  eye to show what was typed.
- A PR merged or closed before a new commit's verdict could go up says so ("merged before posting", with the
  reason), and its review link is labelled as the last review, instead of a bare "not posted".
- Overview flags every open PR that was only commented on, with the reason it was not approved, not only those with a
  high-severity issue.
- Updates: Nod checks GitHub for a newer release at start and every 6 hours and offers it in the window and the tray
  menu; one click downloads it, checks its signature and restarts into it. The deb package is updated by hand.
- Comments go up as soon as the review is done; the approval follows as a review of its own once CI passes (before,
  both waited for CI together). That early review is headed "no blocking issues; approves once CI passes", not "not
  approved", and a review with no findings posts nothing early: just the approval after CI. If CI fails, a short
  review says why it was not approved. *Approve directly* posts
  the findings with the approval when they are not on GitHub yet, instead of dropping them.
- No CI: the wait for checks to appear counts from when Nod first saw the commit, and is skipped in repositories
  whose earlier PRs never had CI. The card says until when it waits.
- Settings: one *Review* tab with two rule sets: *which PRs to review* (repositories, and now authors:
  `scope.authors` / `scope.exclude_authors`) and *auto-approve*, an on/off on top of the comments with its own
  repository and author lists and the CI waits.
- GitHub signs in with an OAuth device-flow login from the GitHub plugin page (the way the GitHub CLI does); `gh`
  is no longer needed or used. A personal access token can be entered instead of signing in. GitHub Enterprise
  accounts can set their own OAuth App's client ID.
- **Review** (Reviews and Overview pages) is one real review of a PR someone sent you directly, under the usual
  approval rules; *Track* and *Dry run* are gone. Pasted links are recognized with surrounding spaces, inside a
  sentence, or ending in `/files`, `/changes`, `/commits/…`, and the review starts at once.
- Overview: one list of the latest reviews, newest first; the ones that need you are flagged in place.
- Small brand tiles use a solid fill so they stay legible at 18 px.
- When a review engine is out of quota or rate limited, Nod waits and tries again by itself (at the reset time the
  engine gives, else after 15 minutes, doubling up to 6 hours) instead of giving up after two quick tries; these
  waits do not use up `max_attempts`, and you are told once. Other errors wait a minute before the retry.
- Fallback engine (`review.fallback`, Plugins → engine → *Make fallback*): reviews instead when the chosen engine is
  out of quota, signed out or not installed. This now works for Codex and Gemini too: their error used to be cut to the
  start of the output (a banner and the echoed prompt), so a signed-out or out-of-quota Codex failed the review
  instead of handing over or waiting for the quota.
- Plugins and Overview check the code hosts and engines in use when opened (installed, signed in; no model call): a
  broken one is marked in the list, and the overview says what it means, such as "reviews go to the backup engine" or
  "new reviews will fail".
- Issues still open from an earlier review (or raised in the discussion) are no longer commented again on their
  lines with every new commit: the review lists them once, folded ("13 raised before and still open"), and only new
  issues get line comments. A PR with ten commits used to collect the same fourteen comments ten times. One that
  still blocks the approval stays in the open list, marked "raised before". With nothing new to say, no review goes
  up while CI runs: the list comes once, with the approval.
- "Approved by an earlier approval" now means your own approval still stands on GitHub. When a new commit dismissed
  it, someone else's approval no longer makes the window, tray or notification say the PR is approved by you.
- When the review engine is signed out (such as Claude Code's "Failed to authenticate: OAuth session expired and could
  not be refreshed"), the backup engine takes over if one is set; otherwise the PR waits instead of failing, without
  using up attempts, and its card says in words how to sign in again. Nod tries again at least every half hour.
- A dropped connection to the code host (such as "tls connection init failed: unexpected end of file") no longer
  sends a "whole round failed" notification: only one that lasts ten minutes does, saying in words that the code host
  has been unreachable and reviewing resumes by itself. Other failures (signed out, no access) are still told at once.
- A PR whose comments went up while CI ran, and that got a new commit before the approval, no longer sits as
  "Outdated": the new commit is reviewed on the next poll. GitHub had dropped the review request when the comments
  went up, so nothing brought it back until someone asked again.
- A desktop notification for a review that was not approved says "Not approved" (it used to say "Commented", though
  the comments go up either way) and gives the reasons, such as "high-severity issue found", instead of the summary.
- The tray's latest reviews say how each one ended in words ("Not approved · Fix login", "Waiting for CI · …")
  instead of ✓ / • / ✕ marks, and its idle line reads the same as the window's.
- *Review again* on a failed or waiting review, and pasting the link of a failed PR into **Review**, starts it over;
  before, the same commit was never reviewed again.
- Webhook recognises chat and push apps and sends in their format: Feishu / Lark, DingTalk, WeCom, Slack, Discord,
  Microsoft Teams, Google Chat, Telegram, Bark, Server酱, ntfy. New channel: Feishu / Lark group bot.
- Review signature: turn the note at the end of each review off, or write your own (Settings → Review; `{engine}`
  names the engine). The default note now names the engine that actually reviewed instead of always saying Claude.
- A PR merged or closed while waiting for CI no longer claims it was not approved because CI was still running.
