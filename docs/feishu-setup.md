# Feishu / Lark setup

[中文](feishu-setup.zh-CN.md)

Nod sends one card per pull request through **your own** Feishu (Lark) bot and receives clicks on its
*Approve now* button over a long connection, so no public URL is needed.

> **Why your own app?** A Feishu long connection delivers each button click to only one of the programs connected to
> the app. If several people pointed Nod at the same app, their clicks would land on each other's machines.
> Create one app per person; it takes about five minutes.

## 1. Create the app

1. Open the developer console: <https://open.feishu.cn/app> (Lark: <https://open.larksuite.com/app>).
2. **Create Custom App** (企业自建应用). Any name and icon, e.g. "Code review"; a short description such as
   "PR auto review" — it is shown at the top of the chat.
3. **Add Features → Bot** (添加应用能力 → 机器人).

## 2. Permission

**Permissions & Scopes** (权限管理): enable **Send messages as an app** — `im:message:send_as_bot`.
Nothing else is needed. (Updating a card the app sent uses the same permission.)

## 3. Configure Nod and connect once

The console only lets you choose long-connection mode while a client is connected, so connect Nod first: in
Nod open *Settings → Notifications* (step 3 of the first-run setup), choose *Feishu / Lark cards*, enter the App
ID and App Secret (from "Credentials & Basic Info") and the recipient, and click *Save*. Nod connects to Feishu
right away; leave it running for the next step.

**Recipient** — who gets the cards — is one of:

| Form | Where to find it |
|---|---|
| `email:you@company.com` | your Feishu login email (the app must be able to see you — see step 5) |
| `open_id:ou_…` | per-app user id; shown in the API explorer when you call *Get user info* with this app |
| `union_id:on_…` / `user_id:…` | the same user across apps of your organization / your employee id |

`open_id` / `union_id` / `user_id` are stricter: a click is only accepted when the clicking user's id matches.
With `email`, a click is accepted only from the card that was sent into your private chat with the bot.

## 4. Card callbacks over the long connection

**Events & Callbacks → Callback Configuration** (事件与回调 → 回调配置):

1. Subscription mode: **Receive callbacks through persistent connection** (使用长连接接收回调) → Save.
   This succeeds only while Nod is running and not paused.
2. **Add callback**: **Card action trigger** (卡片回传交互, `card.action.trigger`).

## 5. Publish

**Version Management & Release** (版本管理与发布) → **Create version**. Set the availability range to just yourself.
In many organizations an app available only to its owner (and up to 5 others) is published without admin review.
Then **Publish**.

## 6. Verify

In *Settings → Notifications*, click *Send a test notification*; you should receive a "connectivity test" card.

## Troubleshooting

- **The health check says the app credentials were rejected** — wrong App ID/Secret, or the secret was reset in the console.
- **The test card cannot be sent** — the app is not published yet, you are not in its availability range, the
  *Send messages as an app* permission is missing, or the recipient is wrong. The error text from Feishu is shown.
- **The button shows an error toast** — Nod is not running or is paused (see the tray icon), or another program is
  connected to the same app's long connection and received the click. Only one program per app.
- **The card does not update** — the card was sent by an older version or a different app; new cards update in place.
- **Lark (international)** — set `notify.feishu.domain = "https://open.larksuite.com"`.
