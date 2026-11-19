# Releasing

1. Move the `[Unreleased]` notes in `CHANGELOG.md` under `## [x.y.z] - date`, and set the same version in
   `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
2. Push to `main` and wait for CI.
3. `git tag vx.y.z && git push origin vx.y.z`. The `release` workflow builds every platform, signs the update
   packages, writes `latest.json` and creates the GitHub release with the CHANGELOG section as its notes.

## Updates and the signing key

Installed copies read `https://github.com/larrygogo/nod/releases/latest/download/latest.json` and install a package
only if its signature matches the public key in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`).

- The private key is the repository secret `TAURI_SIGNING_PRIVATE_KEY` (no password). Keep a copy offline: the
  maintainer's is `~/.tauri/nod.key`.
- If the key is lost, installed copies can no longer be updated automatically: a release signed with a new key must
  be installed by hand once.
- Ordinary builds (CI, `bunx tauri build`) do not need the key; only the release adds
  `--config src-tauri/tauri.release.conf.json`, which turns on the signed update packages.
