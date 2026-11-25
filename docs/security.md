# Security model

Nod acts on GitHub **as you** — it can approve pull requests in your name. The design keeps that power
away from the language model and from the code under review.

## Threats considered

1. **Prompt injection** in the PR title, description, comments, commit messages or repository files
   (`CLAUDE.md`, README, code comments) trying to make the reviewer approve, leak secrets or run commands.
2. **Malicious repository content** — symlinks, huge diffs, files crafted to escape the checkout.
3. **Malformed model output** — a reply that "looks like" approval without being one.
4. **Someone else pressing "Approve now"** — a forwarded Feishu card, or another program connected to your app.
5. **Credential leakage** through the review text posted publicly on GitHub.
6. **The app window** — a compromised or misbehaving page in the window trying to run commands on your machine.
7. **Stored secrets** — tokens and keys read from the settings file by other programs or leaked through backups.
8. **Third-party plugins** — a buggy or hostile external plugin.

## Controls

| Threat | Control | Where |
|---|---|---|
| 1 | Claude runs as `claude -p --restricted --tools Read,Grep,Glob --strict-mcp-config --permission-mode dontAsk --disable-slash-commands` with hooks disabled. `--restricted` removes every tool that can run code and ignores user/project settings; no MCP servers are loaded. | `src-tauri/src/review.rs` `claude_args` |
| 1 | Other engines: Codex runs as `codex exec --sandbox read-only` (OS sandbox: no writes, no network) with `--output-schema`; Gemini CLI runs in read-only plan mode but has no OS sandbox, so its verdicts carry `no_approve` and never lead to an approval unless `review.gemini.allow_approve` is set; the OpenAI-compatible API engine runs no program: Nod itself serves four read-only tools to the model (`get_diff`, `read_file`, `list_dir`, `search`) that refuse absolute paths, `..`, `.git` and anything that resolves outside the checkout, and execute nothing; an endpoint without tool support gets only the diff. Each engine starts with credential-looking variables removed except its own login. | `reviewer::codex`, `reviewer::gemini`, `reviewer::api`, `reviewer::api_tools`, `reviewer::safe_env` |
| 1 | File tools are confined to the checkout plus one directory holding the pre-generated diff; git itself is never exposed to the model. Code-host credentials (the GitHub OAuth token, Gitee/Gitea tokens) reach git only through `GIT_CONFIG_*` environment variables of the git process, never the command line or the mirror's config. | `reviewer::Workspace::checkout`, `reviewer::git_env` |
| 1 | Environment variables that look like credentials (`*TOKEN*`, `*SECRET*`, `*API_KEY*`, `GH_*`, `GITHUB_*`, `FEISHU*`, `LARK*`, `NOD_*`, …) are removed before Claude starts, except Claude Code's own sign-in (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, and AWS / Google credentials only when `CLAUDE_CODE_USE_BEDROCK` / `CLAUDE_CODE_USE_VERTEX` is set), which it needs to run and which its read-only file tools cannot reveal. | `claude_env` |
| 1 | The prompt marks PR text and repository files as data, not instructions. Even if an injection succeeds, the model has no way to act: posting and approving are done by Nod. | `assets/review.*.md` |
| 1, 3 | **Approval is decided by fixed rules**, not by the model: no high-severity finding, no medium-severity finding introduced by the PR, `approve_recommended`, CI not failing, not your own PR, not a draft, an engine that is isolated well enough, an engine that saw the diff of every changed file (the API engine records the files it never saw; lock files, minified bundles and snapshots are listed but do not count) — and then only within `[approve]` (`mode`, repository and author allowlists). Exception: `[[approve.always]]` rules approve without these rules, by the user's explicit choice. | `review::decide` |
| 3 | Output must match a JSON schema with strict types (e.g. `approve_recommended` must be a boolean); anything else is a failed review, never an approval. | `validate_verdict` |
| 2 | The checkout has `core.symlinks=false`; symlinks become plain files. Each PR gets its own worktree, removed afterwards. | `checkout` |
| 2 | Reviews time out (`review.timeout_min`, default 25) and the whole process tree is killed. | `util::run_raw` |
| 4 | A click is accepted only from the exact message that was sent, in the chat it was sent to; if the recipient is configured by id, the clicker's id must match too. Approval is for the commit shown on the card; if the PR moved on, nothing is approved. | `FeishuNotifier::may_act`, `Engine::direct_approve` |
| 4 | One reviewing process per data directory (OS file lock), and one app instance (single-instance lock); one Feishu app per person (documented). | `daemon::try_lock`, `app::run` |
| 5 | GitHub sign-in: Nod's OAuth App with the device flow (scopes `repo`, `read:org`), no client secret in the binary. The token lives in the system keychain (a file readable only by you when no keychain exists), is bound to the host and App it was issued for, expiring tokens are refreshed under a lock (refresh tokens are single use), and it stops being used within 30 s of signing out. | `forge::github_auth` |
| 5 | Review text is scanned for GitHub tokens, `sk-` keys, Slack tokens, AWS keys and private keys before posting; a hit blocks the post. | `review::find_secrets` |
| 6 | The window loads only the bundled page (strict Content-Security-Policy, no remote content) and can call only the app's own commands; it has no shell, file-system or HTTP permissions. Links open only for `http(s)` URLs; "Fix" buttons run one of a fixed set of sign-in commands, filled in only from the saved settings (the `claude` path), never a command line made by the page. The `claude` path itself is a setting the window can change, like every other setting. | `capabilities/default.json`, `app::open_url`, `app::fix_target` |
| 7 | Gitee takes its token as an `access_token` query parameter (its API has no header option); Nod removes it from every error message it shows or logs. Tokens, app secrets, API keys and robot webhook URLs are kept in the system keychain (Windows Credential Manager, macOS Keychain, Secret Service); config.toml holds them only if the keychain is unavailable, and the window says so. The window is never sent a stored secret, only whether one is set. Development instances (NOD_CONFIG_DIR) use their own keychain namespace. | `secrets` |
| 8 | External plugins run as separate processes, with credential-looking environment variables removed; they receive only their own settings. A crash, a timeout or output that is not JSON-RPC fails that one call with a readable reason; the process is restarted on the next call. An external review engine never leads to an approval unless its manifest declares a strong (OS) sandbox — and that declaration is trusted, so install engines only from people you trust. | `plugins::external::Host` |
| — | If a later commit does not pass, Nod dismisses **its own** earlier approval (never one you gave by hand). | `withdraw_old_approvals` |

## Limits

- A successful prompt injection can still make the model *say* a change is clean. The fixed rules limit the damage
  (high/medium findings or failing CI block approval), but a cleverly hidden backdoor that the model misses would be
  approved with `approve.mode = "auto"`. Use `approve.mode = "never"`, or restrict auto-approval to low-risk repositories
  and authors with `approve.repos` / `approve.authors` (e.g. only `dependabot[bot]`).
- Claude Code's own sandboxing is trusted for `--restricted`. Keep Claude Code up to date.
- Dismissing an earlier approval may require permissions you do not have on protected branches; the notification
  then says the old approval is still in effect.
- Secrets are in the system keychain; the settings file (on macOS/Linux `0600`) only holds them when the keychain is
  unavailable. Anything running as your user can still ask the keychain for them.
- An external plugin runs with your user's rights, like any program you install. Nod isolates it from its own
  process and credentials, not from your machine.
