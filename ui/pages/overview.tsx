// Overview: is it working (and the fix if not); one sentence on whether anything waits on you and what Nod did this
// week, with the week drawn next to it; then the latest reviews by day, newest first, flagged in place.
import type { Row } from "../api";
import { accounts, activeChannels, api } from "../api";
import { useStore, stateLabel } from "../app";
import { Icon, Tile } from "../icons";
import { ago, lang, t, type Key } from "../i18n";
import { Button, Note } from "../ui";
import { daily, dayStart, needsYou, week } from "../derive";
import { ReviewRow } from "./reviews";
import { healthOf, usePluginHealth } from "../health";

/** Why a review waits on you, in words: Nod could not finish it, or it commented and did not approve (the first
 *  blocking reason says why), or an earlier approval still stands over a commit that did not pass. */
export function needWhy(r: Row): { text: string; hint: string } {
  if (r.status === "failed") return { text: t("ov.need_failed"), hint: t("ov.need_failed_hint") };
  if (r.earlier_approved) return { text: t("ov.need_stale"), hint: t("ov.need_stale_hint") + " " + r.reasons.join("；") };
  return {
    text: r.reasons[0] ?? t("ov.need_comment"),
    hint: r.reasons.length ? t("ov.need_comment_hint") + " " + r.reasons.join("；") : t("ov.need_comment_hint"),
  };
}

const id = (r: Row) => `${r.account}/${r.key}`;
/** "owner/repo#12" → "repo#12": enough to recognise a PR in a short line. */
const short = (key: string) => key.split("/").pop() ?? key;

export function dayLabel(ts: number): string {
  const d = dayStart(ts), today = dayStart(Date.now() / 1000);
  if (d === today) return t("day.today");
  if (d === dayStart(Date.now() / 1000, 1)) return t("day.yesterday");
  return new Date(ts * 1000).toLocaleDateString(lang(), { month: "short", day: "numeric" });
}

export function OverviewPage() {
  const { ov, draft, catalog, go, refresh, loaded } = useStore();
  const engineKey = (pid: string) => `engine:${pid}`;
  const s = ov.status;
  const plugin = (pid: string) => catalog.find((p) => p.id === pid);
  // A code host or engine in use that does not work, said with what it means: a broken account brings no requests;
  // a broken engine fails its reviews, unless the backup engine works (the backend switches to it by itself). A broken
  // backup alone is not worth the overview; the Plugins list marks it.
  const health = usePluginHealth(loaded.config);
  const cfg = loaded.config;
  const fb = cfg.review.fallback;
  const fbOk = !!fb && !!health[`engine:${fb}`] && healthOf(health[`engine:${fb}`]) !== "bad";
  const broken = Object.entries(health).filter(([k, r]) => healthOf(r) === "bad" && k !== `engine:${fb}`).map(([k, r]) => {
    const bad = r.find((c) => !c.ok)!;
    const pid = k.slice(k.indexOf(":") + 1);
    const forge = k.startsWith("forge:") ? accounts(cfg).find((a) => a.id === pid) : undefined;
    const x = forge ? (plugin(forge.kind)?.name ?? forge.kind) + (accounts(cfg).length > 1 ? ` (${forge.id})` : "") : plugin(pid)?.name ?? pid;
    const tone: "bad" | "warn" = forge || !fbOk ? "bad" : "warn";
    const title = forge ? t("ov.broken_forge", { x }) : fbOk ? t("ov.broken_fb", { x, y: plugin(fb)?.name ?? fb }) : t("ov.broken_engine", { x });
    return { k, tone, title, detail: [bad.detail, bad.fix].filter(Boolean).join(t("ov.broken_sep")) };
  });
  const brokenTone = broken.some((b) => b.tone === "bad") ? "bad" : "warn";
  const channels = activeChannels(draft);
  const problem = s.state === "error" || s.state === "paused";

  // One list, newest first, by day: rows that need you are flagged in place, never pinned above newer reviews.
  const latest = ov.rows.slice(0, 12);
  const groups: { label: string; rows: Row[] }[] = [];
  for (const r of latest) {
    const label = dayLabel(r.at);
    if (groups[groups.length - 1]?.label === label) groups[groups.length - 1].rows.push(r);
    else groups.push({ label, rows: [r] });
  }

  return (
    <div class="page">
      <header class="page-head">
        <div>
          <h1>{t("ov.title")}</h1>
          <div class={`status-line ${s.state}`}>
            <span class="dot" /><span>{problem ? t(`state.${s.state}` as Key) : stateLabel(s)}</span>
            {s.state !== "error" && <span class="muted">· {t("status.last_poll", { t: ago(s.last_poll) })}</span>}
          </div>
        </div>
        <Button icon="hand" onClick={() => go("reviews", "__hand__")}>{t("btn.by_hand")}</Button>
      </header>

      {problem && (
        <div class={`banner ${s.state === "error" ? "bad" : "warn"}`}>
          <Icon name={s.state === "error" ? "alert" : "pause"} />
          <div class="grow">
            <div class="banner-title">{stateLabel(s)}</div>
            {s.state === "error" && s.error && <details><summary>{t("status.detail")}</summary><code class="small">{s.error}</code></details>}
          </div>
          {s.state === "error" && s.action && <Button kind="primary" small onClick={() => s.action === "settings" || s.action === "plugins" ? go(s.action === "plugins" ? "plugins" : "settings") : api.fix(s.action!)}>{t("btn.fix")}</Button>}
          {s.state === "error" && <Button small icon="refresh" onClick={async () => { await api.restart(); setTimeout(refresh, 600); }}>{t("btn.retry")}</Button>}
          {s.state === "paused" && <Button kind="primary" small icon="play" onClick={async () => { await api.pause(false); setTimeout(refresh, 500); }}>{t("btn.resume")}</Button>}
        </div>
      )}

      {broken.length > 0 && (
        <div class={`banner ${brokenTone}`}>
          <Icon name="alert" />
          <div class="grow">
            <div class="banner-title">{broken.map((b) => b.title).join(t("ov.broken_join"))}</div>
            <div class="small">{broken.map((b) => b.detail).join(t("ov.broken_join"))}</div>
          </div>
          <Button kind="primary" small onClick={() => go("plugins", `__plugin:${broken[0].k}`)}>{t("ov.broken_go")}</Button>
        </div>
      )}

      {ov.legacy.length > 0 && <Note>{t("ov.legacy")} {ov.legacy.join("；")}</Note>}

      <Digest />

      <div class="chain">
        {[...accounts(draft).filter((a) => a.enabled).map((a) => [a.kind, `forge:${a.id}`]), [draft.review.engine, engineKey(draft.review.engine)],
          ...channels.map((c) => [c, ""])].map(([pid, hk], i) => {
          const p = plugin(pid);
          const h = hk ? healthOf(health[hk]) : null;
          return <span class={`chain-item${h ? " " + h : ""}`} key={`${pid}-${i}`} title={h ? t(h === "bad" ? "pl.broken" : "pl.attention") : undefined}>
            <Tile icon={p?.icon ?? "plugin"} size={18} />{p?.name ?? pid}{h && <Icon name="alert" size={12} />}</span>;
        })}
        {channels.length === 0 && <span class="chain-item muted">{t("ov.no_channel")}</span>}
        <button class="link small" onClick={() => go("plugins")}>{t("ov.change")}</button>
      </div>

      {groups.map((g, i) => (
        <section class="block" key={g.label}>
          <header class="block-head">
            <h2 class="block-title">{g.label}</h2>
            {i === 0 && <button class="link small" onClick={() => go("reviews")}>{t("btn.all_reviews")} <Icon name="chevron" size={14} /></button>}
          </header>
          <div class="list-card">{g.rows.map((r) => <ReviewRow key={id(r)} r={r} flag={needsYou(r) ? needWhy(r) : undefined} onClick={() => go("reviews", id(r))} />)}</div>
        </section>
      ))}
    </div>
  );
}

/** The first thing on the page: does anything wait on you (and which), what is happening now, and the week in one
 *  sentence with the days drawn next to it. */
function Digest() {
  const { ov, go } = useStore();
  const needs = ov.rows.filter(needsYou);
  const reviewing = ov.rows.filter((r) => r.status === "reviewing" || r.status === "queued");
  // after an error the rows may be stale: do not claim that nothing waits on you
  const stale = ov.status.state === "error";
  const ci = ov.rows.filter((r) => r.status === "awaiting_ci");
  const w = week(ov.rows);
  // English needs the singular for one; Chinese counts the same either way
  const n = (k: number, one: string, many: string) => (lang() === "en" ? `${k} ${k === 1 ? one : many}` : String(k));
  // two clauses that wrap between each other, never leaving two characters alone on the last line
  const sentence = !w.prs ? t("ov.week_none") : <>
    <span class="clause">{t("ov.week", { prs: n(w.prs, "PR", "PRs"), rounds: n(w.rounds, "round", "rounds") })}</span>
    <span class="clause">{t("ov.week_ok", { ok: w.approved }) + (w.held ? t("ov.week_held", { n: w.held }) : "") + t("ov.week_end")}</span>
  </>;
  const lead = !ov.rows.length ? t("ov.lead_new") : needs.length ? t(needs.length === 1 ? "ov.lead_need_one" : "ov.lead_need", { n: needs.length })
    : stale ? t("ov.lead_stale") : t("ov.lead_clear");
  const tone = !ov.rows.length || (stale && !needs.length) ? "" : needs.length ? "warn" : "good";
  return (
    <section class="digest">
      <div class="grow">
        <div class={`digest-lead ${tone}`}>
          {tone && <Icon name={needs.length ? "hand" : "check"} />}
          <span>{lead}</span>
        </div>
        <p class="digest-sub">{ov.rows.length ? sentence : t("ov.lead_new_sub")}</p>
        {!ov.rows.length && <div class="digest-chips"><Button small icon="hand" onClick={() => go("reviews", "__hand__")}>{t("ov.first_review")}</Button></div>}
        {(needs.length > 0 || reviewing.length > 0 || ci.length > 0) && (
          <div class="digest-chips">
            {needs.slice(0, 3).map((r) => (
              <button key={id(r)} class="chip warn" title={needWhy(r).hint} onClick={() => go("reviews", id(r))}>
                <b class="mono">{short(r.key)}</b><span class="ellipsis">{needWhy(r).text}</span>
              </button>
            ))}
            {needs.length > 3 && <button class="chip" onClick={() => go("reviews", "__filter:needs")}>{t("ov.more_needs", { n: needs.length - 3 })}</button>}
            {reviewing.slice(0, 2).map((r) => (
              <button key={id(r)} class="chip busy" onClick={() => go("reviews", id(r))}><span class="spinner" />
                <span class="ellipsis">{t(r.status === "queued" ? "ov.now_queued" : "ov.now_reviewing", { k: short(r.key) })}</span></button>
            ))}
            {reviewing.length > 2 && <button class="chip busy" onClick={() => go("reviews", "__filter:active")}><span class="ellipsis">{t("ov.more_active", { n: reviewing.length - 2 })}</span></button>}
            {ci.length > 0 && <button class="chip busy" onClick={() => go("reviews", id(ci[0]))}><Icon name="check" size={12} />
              <span class="ellipsis">{t("ov.now_ci", { n: ci.length })}</span></button>}
          </div>
        )}
      </div>
      {ov.rows.length > 0 && <WeekBars rows={ov.rows} />}
    </section>
  );
}

/** Seven columns, one per day: reviews Nod approved (green) under the ones it only commented on (amber). */
function WeekBars({ rows }: { rows: Row[] }) {
  const d = daily(rows, 7);
  const max = Math.max(1, ...d.map((x) => x.approved + x.commented));
  // "周一" / "Mon", not the one-letter form: a lone "一" reads as a dash
  const wd = (ts: number) => new Date(ts * 1000).toLocaleDateString(lang(), { weekday: "short" });
  return (
    <figure class="week" aria-label={t("ov.week_chart")}>
      <div class="week-bars">
        {d.map((x, i) => (
          <div class="week-col" title={t("ov.week_tip", { d: dayLabel(x.day), ok: x.approved, no: x.commented })}>
            <div class="week-stack">
              {x.commented > 0 && <span class="seg warn" style={{ height: `${(x.commented / max) * 100}%` }} />}
              {x.approved > 0 && <span class="seg good" style={{ height: `${(x.approved / max) * 100}%` }} />}
              {x.approved + x.commented === 0 && <span class="seg none" />}
            </div>
            <span class={`week-day${i === d.length - 1 ? " today" : ""}`}>{wd(x.day)}</span>
          </div>
        ))}
      </div>
      <figcaption class="week-legend">
        <span><i class="good" />{t("ov.legend_ok")}</span><span><i class="warn" />{t("ov.legend_no")}</span>
      </figcaption>
    </figure>
  );
}
