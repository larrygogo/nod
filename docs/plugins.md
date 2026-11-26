# Plugins

Everything Nod connects to is a plugin of one of three kinds:

| Kind | Built in | What it does |
|---|---|---|
| **Code host** (`forge`) | GitHub (incl. Enterprise), Gitee, Gitea / Forgejo | finds the PRs you are asked to review, checks them out, posts reviews |
| **Review engine** (`engine`) | Claude Code, OpenAI Codex CLI, Google Antigravity CLI, Kimi Code CLI, OpenAI-compatible API | reads the change and returns a structured verdict |
| **Notification channel** (`notify`) | desktop, Feishu/Lark, DingTalk, WeCom, Slack, webhook | tells you about outcomes |

Whatever the engine says, **approval is decided by Nod's fixed rules** (see [security.md](security.md)); an
engine that does not declare a read-only sandbox never leads to an approval.

You can add a plugin of any kind without changing nod: a program plus a `plugin.toml`, run as a child process
that speaks JSON-RPC 2.0 over stdin/stdout — the same idea as the Language Server Protocol or MCP's stdio transport.

## Installing a plugin

1. Put the plugin's folder into the plugins folder (*Plugins → Open plugins folder* in the app):
   - Windows `%APPDATA%\nod\plugins\<name>\`
   - macOS `~/Library/Application Support/nod/plugins/<name>/`
   - Linux `~/.config/nod/plugins/<name>/`
2. Restart Nod. The plugin appears in its section under *Plugins*, marked *Third-party*; configure and enable it
   there like a built-in one. A folder whose `plugin.toml` cannot be read is listed with the reason.

[`examples/plugins/notify-log`](../examples/plugins/notify-log) is a complete notification channel in 60 lines of
Python that appends every notification to a file.

## `plugin.toml`

```toml
name = "notify-log"            # id: letters, digits, - and _; the plugin is referred to as ext:<name>
kind = "notify"                # forge | engine | notify
display_name = "Log file"
description = "Appends each notification as one JSON line to a file."
version = "1.0.0"
command = ["python3", "plugin.py"]   # program and arguments; paths inside the plugin folder are resolved there
timeout_s = 30                 # per call (review calls use the review timeout instead)

[[settings]]                   # the settings form in the app is built from these
key = "path"
label = "文件路径"
label_en = "File path"
type = "text"                  # text | password | number | bool | select
options = []                   # for select
required = true
placeholder = "~/notifications.jsonl"
help = ""

[capabilities]                 # review engines only
repo_access = "full"           # "full": reads the checkout; "diff": only gets the diff
isolation = "weak"             # "strong" only if the engine runs in a read-only OS sandbox
```

## Protocol

One JSON-RPC 2.0 message per line in each direction. Nod sends requests; the plugin answers each with a
`result` or an `error` (`{"code": …, "message": "…"}`; the message is shown to the user). The plugin may also send
notifications `{"jsonrpc":"2.0","method":"log","params":{"message":"…"}}`, which go to Nod's log. Anything on
stdout that is not JSON-RPC is a protocol error. Use stderr for your own diagnostics; its last lines are shown when
the plugin exits unexpectedly.

The process is started on first use, kept running, and receives `{"method":"shutdown"}` (a notification) before
Nod stops it. If it crashes, times out or prints garbage, that one call fails with a readable reason, the
process is stopped, and the next call starts it again.

It runs with Nod's environment minus anything that looks like a credential; secrets reach it only through its
settings.

### Every plugin

| Method | Params | Result |
|---|---|---|
| `initialize` | `{protocol: 1, settings: {…}, language: "zh-CN"\|"en", data_dir}` | anything, e.g. `{name, version}` |

`data_dir` is a folder the plugin may use (`<data dir>/plugins/<name>`; create it if you need it).

### Notification channels (`kind = "notify"`)

| Method | Params | Result |
|---|---|---|
| `notify.send` | `{kind: "approve"\|"comment"\|"failed"\|"direct"\|"alert", key: "owner/repo#12", title, text, url}` | anything |
| `notify.test` | `{}` | a short string shown to the user |

Nod calls `notify.send` once per outcome (a review posted, failed, approved directly) and for whole-round
failures (`kind = "alert"`); it remembers what was sent, so the plugin does not need to deduplicate.

### Review engines (`kind = "engine"`)

| Method | Params | Result |
|---|---|---|
| `review.run` | `{pr: {repo, number, title, author, url, head, base, draft, state, body}, checkout, context_dir, prompt, schema, language, platform}` | the verdict, or `{verdict: …}` |

- `checkout` is the PR's head checked out in a temporary worktree (empty when `repo_access = "diff"`);
  `context_dir` holds `diff.patch`, `diffstat.txt`, `commits.txt`, `merge_base.txt`.
- `prompt` is Nod's full review instruction in the user's language; `schema` is the verdict's JSON Schema.
- The verdict must match the schema exactly (`summary`, `findings[]` with `severity` high/medium/low/nit, `path`,
  `line`, `title`, `body`, `introduced_by_pr`, plus `approve_recommended`, `unable_reason`). Anything else counts as a
  failed review.

### Code hosts (`kind = "forge"`)

A code-host plugin is used through an account (*Plugins → Code hosts → Add account*). Its settings also contain
the account's `url` and `token` (the token comes from the system keychain).

| Method | Params | Result |
|---|---|---|
| `forge.currentUser` | `{}` | login (string) |
| `forge.requestedPrs` | `{teams}` | `[["owner/repo", 12], …]` — open PRs that request your review |
| `forge.fetchPr` | `{repo, number}` | `{repo, number, title, author, url, head, base, draft, state: "open"\|"closed"\|"merged", body}` |
| `forge.ciState` | `{pr}` | `"success"\|"failure"\|"pending"\|"none"` |
| `forge.prStatus` | `{repo, number}` | `{state, draft, decision, head, ci, title, url, author, additions?, deletions?, changed_files?, head_ref, base_ref, mine_approved?}` — `mine_approved`: whether your own approval still stands (omit if the host cannot tell; `decision` is used instead) |
| `forge.discussion` | `{pr}` | existing comments as text (for the reviewer's context) |
| `forge.diffLines` | `{pr}` | `{"path": [line, …]}` — lines inline comments may be placed on |
| `forge.postReview` | `{pr, review: {commit_id, event: "APPROVE"\|"COMMENT", body, comments: [{path, line, side, body}]}}` | `{url, id}` |
| `forge.listReviews` | `{pr}` | `[[id, state], …]` (state `"APPROVED"` for approvals) |
| `forge.dismissReview` | `{pr, id, message}` | anything — withdraws Nod's own approval |
| `forge.remote` | `{repo}` | `{url, header?}` — HTTPS clone URL and an optional auth header for git |
| `forge.headRef` | `{number}` | the ref holding the PR head, e.g. `"refs/pull/12/head"` |

## Writing a built-in plugin

Built-ins are Rust implementations of the same three traits — `forge::Forge`, `reviewer::Engine`,
`notify::Notifier` — plus an entry in `plugins/catalog.rs` that describes their settings. See
[CONTRIBUTING.md](../CONTRIBUTING.md).
