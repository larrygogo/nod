# Security policy

Nod can approve pull requests in its user's name, so we treat bypasses of the approval rules or the review
sandbox as security issues. The design is described in [docs/security.md](docs/security.md).

## Reporting

Please **do not open a public issue**. Use GitHub's private vulnerability reporting
(*Security → Report a vulnerability* on the repository page). Include the version (shown at the bottom of the window's sidebar), your
platform, and steps or a pull request that reproduces the problem.

We aim to acknowledge reports within 5 working days and to ship a fix or mitigation for confirmed high-severity issues
within 30 days.

## In scope

- Making Nod approve a PR that the fixed rules should block.
- Making the reviewer run commands, write files, reach the network, or read files outside the checkout.
- Exfiltrating credentials through the review text or notifications.
- Approving through a Feishu click that did not come from the configured user.

Weaknesses of the language model itself (e.g. missing a subtle bug) are out of scope unless they lead to one of the
above.
