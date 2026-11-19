# Contributing

Thanks for helping! Issues and pull requests are welcome in English or Chinese.

## Development

Requirements: Rust 1.89+ (stable), [Bun](https://bun.sh) for the window's front end, and on Linux the WebKitGTK and
AppIndicator development packages (`libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev`).
`git` and Claude Code are only needed to run real reviews.

```sh
bun install
bun run build                               # the window, into dist/ (the app embeds it)
cd src-tauri
cargo test                                  # unit and concurrency tests, no network needed
cargo clippy --all-targets -- -D warnings
cargo fmt
cd ..
bunx tauri dev                              # run the app with live reload of the window
bunx tauri build                            # installers in src-tauri/target/release/bundle/
```

Try changes without touching your real installation by pointing the app at scratch directories. With
`NOD_DRY_RUN=1` the reviewing process reviews but never writes to GitHub:

```sh
NOD_CONFIG_DIR=/tmp/rw/config NOD_HOME=/tmp/rw/data NOD_DRY_RUN=1 bunx tauri dev
```

With these overrides the app also leaves an installed command-line edition's background service alone.

## Layout

| | |
|---|---|
| `ui/` | the window (Preact + TypeScript): `app.tsx` shell and state, `pages/` (overview, reviews, plugins, settings, setup), `styles.css` design tokens (see [docs/design.md](docs/design.md)) |
| `src-tauri/src/main.rs` | entry: the app, or the internal reviewing process (`worker.rs`) |
| `src-tauri/src/app.rs` | window commands, start-up, single instance, autostart |
| `src-tauri/src/tray.rs` | tray icon with a status dot, and its menu |
| `src-tauri/src/supervisor.rs` | starts, restarts and pauses the reviewing process; the status shown in the tray and window |
| `src-tauri/src/worker.rs` | the reviewing process (exits when the app does) |
| `src-tauri/src/osnotify.rs` | desktop notifications that open a link when clicked |
| `src-tauri/src/legacy.rs` | removes the command-line edition's background service |
| `src-tauri/src/config.rs`, `paths.rs` | configuration file and locations |
| `src-tauri/src/forge/` | `Forge` trait; GitHub (REST + GraphQL, signed in through the Nod GitHub App in `github_auth.rs`), Gitee and Gitea (REST, `http.rs` + a fake server for tests) |
| `src-tauri/src/reviewer/` | `Reviewer`/`Engine`, the shared checkout, Claude Code, Codex, Gemini, OpenAI-compatible API |
| `src-tauri/src/plugins/` | the plugin catalog the window builds its forms from; external plugins (JSON-RPC host and adapters) |
| `src-tauri/src/secrets.rs` | system keychain for tokens and secrets |
| `src-tauri/src/review.rs` | prompt, verdict validation, **the approval rules**, the review body |
| `src-tauri/src/engine.rs` | per-PR state machine and `state.json` |
| `src-tauri/src/daemon.rs` | polling loop, worker thread, refresher, "Approve now" |
| `src-tauri/src/doctor.rs` | the health check and its fix actions |
| `src-tauri/src/notify/` | `Notifier` trait; desktop, Feishu cards + long connection, webhook |
| `src-tauri/src/i18n.rs` | review, card and notification text, Chinese and English (`ui/i18n.ts` has the window's) |
| `src-tauri/assets/` | review prompts and the verdict JSON schema |

## Adding a plugin

The quickest way is an external plugin in any language: see [docs/plugins.md](docs/plugins.md). To ship one with
Nod instead:

- **Code host**: implement `forge::Forge` in `src-tauri/src/forge/` (use `http::Api` for REST and `http::fake` in
  tests), add the kind to `FORGES` in `config.rs` and to `forge::make`.
- **Review engine**: implement `reviewer::Engine` in `src-tauri/src/reviewer/` — build the prompt with
  `review::build_prompt`, run the tool read-only, return a verdict through `verdict_from_text` or `validate_verdict`,
  set `meta.engine` and, if the tool has no OS sandbox, `meta.no_approve`. Add it to `ENGINES` and `Router::engine`.
- **Notification channel**: see below.

Every built-in also needs an entry in `plugins/catalog.rs` (name, description, settings fields, badges); a test checks
that each catalog field is a real config path, and the window builds the plugin's form from it. Secret fields go into
`secrets::fields`.

### Notification channels

Channels live in `src-tauri/src/notify/`. Implement the `Notifier` trait (all methods have defaults):

- `update(key, entry, status)` — show the current state of one PR; deduplicate unchanged content yourself
  (see `card_sig` / `webhook_sig`). Called on every state change and on each refresh.
- `alert(text)` / `test()` — a round failed / *Send a test notification*.
- For buttons: `supports_actions() -> true`, a blocking `listen(on_action)` that delivers clicks, `may_act(ids, entry)`
  that proves the click came from the configured user, and `render_for` to return an updated message.

Then add the channel name to `CHANNELS` and its settings struct in `src-tauri/src/config.rs`, construct it in
`notify::make_one`, describe it in `plugins/catalog.rs`, and document it in the README. Channels that only announce
outcomes can reuse `SystemNotifier::message` (see `notify/robot.rs`).

## Guidelines

- **Anything that changes when Nod approves** (`review::decide`, the sandbox flags, `claude_env`,
  `may_act`, `find_secrets`) needs a test and a note in `docs/security.md`.
- New user-facing text goes into `src-tauri/src/i18n.rs` or `ui/i18n.ts` with both languages (the TypeScript types
  make a missing English string a build error; a Rust test checks that placeholders match).
- Keep the app small: prefer the standard library and small crates; Preact is the only front-end dependency;
  reviewing code stays synchronous (threads, no async runtime). Follow [docs/design.md](docs/design.md) for the UI.
- Tests must not need the network, a GitHub sign-in or Claude. Use the fakes in `src-tauri/src/engine_tests.rs`.
- Update `CHANGELOG.md` under *Unreleased*.

## Releasing

Update the version in `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `package.json` and `CHANGELOG.md`, then push
a tag `vX.Y.Z`. The release workflow builds and publishes the installers for Windows, macOS (arm64, x64) and Linux
(x64, arm64).
