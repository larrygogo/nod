# Design system

Nod is a quiet utility: it lives in the tray and is opened to check on something or change a setting. The window
borrows from tools in the same family — Linear and Raycast (dense, keyboard-friendly lists, neutral surfaces, one
accent colour), Tailscale and 1Password (a status-first home, settings as calm grouped cards), GitHub Desktop (a list
with a detail pane). What we took and why:

| Reference | Taken | Why |
|---|---|---|
| Linear | 13 px base text, neutral greys, colour only for state, list + detail split | Review history is scanned, not read |
| Raycast | Grouped settings with one-line descriptions, toggles on the right | Plugin settings are many small choices |
| Tailscale | Home screen answers "is it working?" first, with the fix next to the problem | That is the first question every time |
| GitHub Desktop | Sidebar of places, detail pane, platform-native window chrome | Familiar to the audience |

## Information architecture

- **Overview** — one status line under the title (and a banner with the fix only when something is wrong — including
  a code host or engine in use that fails its local check, said with its consequence: no requests from that account,
  reviews failing, or "using the backup engine for now" when the backup works), then the
  digest: one sentence that answers "does anything wait on me?" (open PRs Nod did not approve, or whose review failed,
  with up to three of them as shortcuts and what is reviewing or waiting for CI right now), one sentence on the last
  seven days — rolling, not the calendar week, so a Monday is not empty — ("reviewed 8 PRs in 39 rounds, approved 6,
  held back 1 with a high-severity issue"), and those seven days drawn next to it. A reason is never invented: a
  review with no recorded reason says so, and after an error the digest does not claim that nothing waits on you. Then the plugins in use as a short chain and the latest reviews grouped by day, newest first; rows that
  wait on you are flagged in place, never pinned. No strip of today's counters: most days it read "0 0 0 0".
- **Reviews** — every review: search, filter by outcome (approved / not approved / in progress / failed), detail pane
  with verdict, reasons and findings. A PR reviewed more than once shows its rounds as small stacked columns (one per
  review, by severity) with a sentence ("5 reviews: high and medium issues went from 6 to 1"); earlier rounds stay
  folded.
  "Review a PR by hand" (dry run / track) lives here.
- **Plugins** — list and pane, like macOS System Settings or Raycast's extension settings. The list groups code
  hosts (accounts, with "+" to add one), review engines (plus the per-repository rules), notification channels and
  third-party plugins; each line shows its state (On / Off / Default) so the list is also the summary. The pane shows
  the selected one: what it is, what it can see/do, its settings and its self-test. Opening the page checks every
  account and engine in use with the saved settings and no model call; a broken one says "Not working" (or "Needs a
  look") in the list instead of "On", and its pane leads with the problem and the fix, above the settings. A clean result the page found by itself
  is not repeated where the pane already says it (GitHub's account section names who is signed in). GitHub's
  Enterprise and token fields fold under one line, as in Setup, open when one is in use. Narrow windows show the list, then
  the pane with a back link.
- **Settings** — one group at a time behind a segmented control: general (language, theme, start at sign-in),
  review (which PRs are reviewed and which are approved: each opens with one sentence that reads the lists back — "Nod reviews only PRs in my-org/*, except those by renovate[bot]" — then the four lists as a repository/author by only/never grid, so empty fields no longer have to be decoded as "everything"; automatic approval with its fixed rules listed one per line, the always-approve
  list, the comment footer, and "Running" — timeouts, retries, polling — folded at the end), about (files, version).
- **Setup** — first launch: code host → review engine → notifications → done. One option per line; the picked option
  opens its settings right below itself; the step buttons stay fixed at the bottom. GitHub leads with "Sign in" (the
  same device flow as Plugins) and checks by itself once signed in; settings that work as they are (catalog fields
  marked `optional`: Enterprise, a token, a detected CLI path, the default model) fold under one line. The engine
  step checks on its own when nothing it needs is empty, so "Next" is ready without hunting for a button.

## Window

- The title bar is the system's (keeps Windows snap layouts and native buttons); the window is given the app's theme
  so it is dark over a dark page.
- The window never scrolls; panes do, with thin scrollbars that fade into the background.
- Rows rather than cards: a list is one bordered container with dividers, not a stack of cards. Cards only group
  settings.
- Colour means state. Capability tags are neutral outlines; only warnings ("cannot be withdrawn", "no system sandbox")
  are amber. Status words are coloured text, not filled pills, in lists.
- Numbers get words: "3 findings · 1 high", never bare coloured counters. A chart always comes with the sentence it
  proves and a legend.
- States follow the approval, not the comment: comments always go up, so a review reads "approved", "not approved"
  (with the reason), "waiting for CI" or "reviewing" — never "commented" as if it were the opposite of approved.

## Tokens

Defined once in `ui/styles.css` as CSS variables, with light and dark values; the user can follow the system or force a
theme. The look: quiet, modern and soft — close to the AI clients people use daily (ChatGPT, Doubao) — with no hard
lines and no pure black.

| Token | Light | Dark | Use |
|---|---|---|---|
| `--surface` | `#ffffff` | `#1d1f22` | cards, panes, inputs |
| `--bg` | `#fafbfc` | `#141517` | content |
| `--sidebar` | `#f6f7f9` | `#191a1d` | sidebar |
| `--surface-2` / `-3` | `#f1f3f6` / `#ebedf2` | `#222328` / `#27282d` | hover / pressed, segmented track |
| `--raised` | `#ffffff` | `#2d2f34` | selected and current items, lifted by a shadow |
| `--border` / `-strong` | 6 % / 12 % of the text colour | 6 % / 12 % white | hairline dividers / the ring of inputs |
| `--text` / `-2` / `-3` | `#1d2129` / `#4e5969` / `#5f6673` | `#e8e9ed` / `#a9aeb8` / `#8f949e` | all ≥ 4.5:1 |
| `--accent` | `#165dff` (hover `#4080ff`, pressed `#0e42d2`, as text `#0e42d2`) | `#165dff` (as text `#6aa1ff`) | primary action, links |
| `--good` `--warn` `--bad` | `#007a33` `#a34700` `#cb2634` on light tints | `#3dd68c` `#ffb65d` `#ff8a8a` | state only |
| `--good-fill` `--warn-fill` `--bad-fill` | `#23c16b` `#ff9a2e` `#f2555a` | same as the text shades | chart bars and legends (the text shades are too dark for a shape) |

- **Neutrals**: one cool grey (HSL 222°); neighbouring layers differ by 1.5–2 % lightness, so surfaces separate by
  tone and shadow rather than by lines.
- **Elevation instead of borders and highlights**: cards, lists and choices float on `--shadow-card` (a 4 % ring plus
  a soft drop); the selected or current item (sidebar, plugin list, review list, setup step, segmented control) is
  lifted on `--raised` with `--shadow-raise` instead of being tinted. Inputs have a 12 % ring and a blue focus halo.
- **Type**: [Inter](https://rsms.me/inter/) for Latin letters and digits and Source Han Sans (as Noto Sans SC) for
  Chinese, both variable and bundled with the app (Chinese is split by character ranges and only the ranges in use are
  loaded). 13 px base, 12 px secondary, 15/20 px headings. Weights 400/500/600; in dark mode every weight is one step
  lighter (350/400/500), since light text on dark reads heavier. Digits that count or change are tabular (not the whole
  page: Inter's tabular figures also widen the hyphen). Icons inside running text sit 1/8 em lower. Paragraphs use
  `text-wrap: pretty` and headings `balance`, so no line ends with a lone character.
- **Spacing**: 4 px grid — 4, 8, 12, 16, 20, 24, 32.
- **Radius**: 6 px controls and badges, 8 px tiles and segmented tracks, 10 px cards, 999 px pills.
- **Icons**: 16 px line icons, 1.75 px stroke, `currentColor`. Plugin tiles are the product's initial in its brand
  colour on a light tint of it (12 % in light, 20 % in dark) with a thin ring; near-black brands (GitHub, Webhook) use
  the neutral surface. No third-party logos are bundled.
- **Motion**: 120 ms ease-out for hover and expand, 200 ms for panes and dialogs. Buttons, switches and choices
  shrink slightly when pressed and spring back (an overshooting curve); success messages leave after 2.5 s. None of it
  when the system asks for reduced motion.
- **States**: every async action shows progress in place (button label → spinner), success briefly, errors inline next
  to the control with the fix.
- **Density**: list rows 52 px, controls 30 px, the window works from 760 × 520.

## Writing

Short, plain, second person. Say what happened and what to do. Chinese and English are written separately, not
translated word for word. Technical detail goes behind "Details".
