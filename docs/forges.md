# Connecting Gitee and Gitea

[简体中文](forges.zh-CN.md)

GitHub: click **Sign in to GitHub** on the GitHub plugin page. Nod shows a one-time code and opens GitHub; paste the code and authorize. Nod acts as you and reaches the repositories you can reach. If an organization restricts third-party apps, request access for Nod on the authorization page (or later under Settings → Applications) and an owner approves it once. GitHub Enterprise: register an OAuth App with Device Flow on your instance and fill in its Client ID on the same page. Gitee and Gitea / Forgejo need an access token,
which Nod keeps in the system keychain — never in the settings file.

In Nod, go to *Plugins → Code hosts → Add account*, pick the platform, enter the token (and the address for
Gitea), and click *Self-test*. Connect as many accounts as you like, e.g. your company's Gitee and your personal
GitHub.

## Gitee

1. On gitee.com: *Settings → Security → Personal access tokens* → generate a token.
2. Scopes: **user_info**, **projects**, **pull_requests**.
3. In Nod, paste the token; leave the address empty for gitee.com.

Nod then:

- finds open pull requests that list you among the **reviewers** (审查人员). Gitee has no cross-repository search
  for review requests, so Nod lists the repositories you can access (up to 500, refreshed every ten minutes)
  and asks each one. Limiting *What to review* in Settings saves requests and makes each check faster;
- posts inline comments and a summary comment. If Gitee does not accept a line, that finding is posted as a regular
  comment naming the file and line instead of being lost;
- **approves** (审查通过) when every rule passes.

Two limits:

- **An approval cannot be withdrawn.** Gitee does not let a reviewer undo it; only an administrator can reset reviews.
  If a later commit has problems, Nod cannot dismiss its earlier approval, and the notification says the old
  approval still stands. If that worries you, allow auto-approval only for some repositories (*Settings →
  Auto-approve*).
- CI state comes from the commit's check runs; a repository without checks counts as having no CI.

Gitee's API takes the token as an `access_token` parameter; Nod removes it from every error message and log
line.

## Gitea / Forgejo (incl. Codeberg)

1. *Settings → Applications → Manage access tokens* → generate a token.
2. Scopes: **read:user**, **write:repository**, **write:issue**.
3. In Nod enter the instance address (e.g. `https://codeberg.org`) and the token.

Nod finds pull requests that request your review (Gitea 1.19 or later) and posts one review with event
APPROVED or COMMENT. It tries to dismiss its own approval if a later commit has problems; Gitea only lets repository administrators do
that, and without that right the notification says the old approval still stands. CI state comes from the commit
status.

## Troubleshooting

- **Self-test says 401 or 403**: the token is wrong, expired or missing a scope.
- **Self-test cannot connect**: check that the address starts with `https://` and opens in a browser, and check the
  proxy.
- **The PR is found but the checkout fails**: the token must be able to read the repository. For the checkout, the
  token reaches git only through its environment, never on a command line or in the repository's config.
- **Gitee checks are slow**: you can access many repositories. List the ones to review under *What to review*, e.g.
  `my-org/*`.
