# Webhook notifications

Set `notify.channel = "webhook"` and `notify.webhook.url`. Nod then `POST`s a JSON event whenever a pull
request's review state changes (queued, reviewing, verdict posted, approved directly, merged/closed…). Unchanged state
is not sent twice.

## Payload

```json
{
  "type": "pr.update",
  "key": "acme/api#42",
  "status": "posted",
  "headline": "Has issues",
  "pr": {
    "state": "open", "draft": false, "decision": "REVIEW_REQUIRED", "head": "1f3c…", "ci": "success",
    "title": "Add monthly export filter", "url": "https://github.com/acme/api/pull/42", "author": "alice",
    "additions": 128, "deletions": 14, "changed_files": 5, "head_ref": "feat/export", "base_ref": "main"
  },
  "verdict": {
    "summary": "One boundary bug lets next month's orders into the export.",
    "approve_recommended": false,
    "findings": [
      { "severity": "high", "path": "src/export/query.rs", "line": 88,
        "title": "Month boundary uses <=", "introduced_by_pr": true }
    ]
  },
  "event": "COMMENT",
  "reasons": ["high-severity issue found"],
  "review_url": "https://github.com/acme/api/pull/42#pullrequestreview-1",
  "note": "One boundary bug lets next month's orders into the export.",
  "sent_at": 1763452800
}
```

Other event types: `{"type": "alert", "text": "...", "sent_at": ...}` when a whole polling round fails (at most once
every 6 hours), and `{"type": "test", ...}` from *Send a test notification* in the app.

`status` is one of `queued`, `reviewing`, `reviewed`, `awaiting_ci`, `posted`, `direct_approved`, `failed`, `stale`,
`closed`, `aborted`, `dry-run`. Texts (`headline`, `reasons`) follow the configured `language`.

## Signature

With `notify.webhook.secret` (or `NOD_WEBHOOK_SECRET`) set, every request carries

```
X-Nod-Signature: sha256=<hex HMAC-SHA256 of the raw request body, keyed with the secret>
```

Verify it with a constant-time comparison before trusting the payload, e.g. in Python:

```python
import hashlib, hmac
expected = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
assert hmac.compare_digest(expected, request.headers["X-Nod-Signature"])
```

Webhooks have no buttons; approve on GitHub or use the Feishu channel for *Approve now*.
