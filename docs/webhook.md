# Webhook notifications

Set `notify.channel = "webhook"` and `notify.webhook.url`. Nod then `POST`s a JSON event whenever a pull
request's review state changes (queued, reviewing, verdict posted, approved directly, merged/closed…). Unchanged state
is not sent twice.

## Chat and push apps

If the address belongs to one of these apps, Nod sends in that app's own format instead of the JSON below, and only
outcomes (review posted, failed, approved directly; a whole round failing), like the app's own channel would:

| App | Address looks like |
|---|---|
| Feishu / Lark group bot | `https://open.feishu.cn/open-apis/bot/v2/hook/…` (also `open.larksuite.com`) |
| DingTalk | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| WeCom (企业微信) | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |
| Slack | `https://hooks.slack.com/services/…` |
| Discord | `https://discord.com/api/webhooks/…` |
| Microsoft Teams | a Workflows URL (`…logic.azure.com…/workflows/…` or `…powerplatform.com…/workflows/…`) or `…webhook.office.com/…` |
| Google Chat | `https://chat.googleapis.com/v1/spaces/…/messages?key=…&token=…` |
| Telegram | `https://api.telegram.org/bot<token>/sendMessage?chat_id=<chat>` |
| Bark | `https://api.day.app/<key>` |
| Server酱 | `https://sctapi.ftqq.com/<SendKey>.send` |
| ntfy | `https://ntfy.sh/<topic>` or your own `ntfy.` server |

For Feishu and DingTalk the signing secret is the bot's own (签名校验 / 加签). Feishu, DingTalk, WeCom and Slack also
have channels of their own on the Plugins page.

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
    "additions": 128, "deletions": 14, "changed_files": 5, "head_ref": "feat/export", "base_ref": "main",
    "mine_approved": false
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
