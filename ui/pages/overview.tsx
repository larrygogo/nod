// Overview: is it working (and the fix if not), today's numbers, what needs you, the latest reviews.
import { accounts, activeChannels, api } from "../api";
import { useStore, stateLabel } from "../app";
import { Icon, Tile } from "../icons";
import { ago, t, type Key } from "../i18n";
import { Button, Empty, Note } from "../ui";
import { ReviewRow } from "./reviews";

/** Why a review needs you, in words: Nod could not finish it (failed), or it found a high-risk issue and only
 *  commented, so the call is yours. */
function needWhy(r: { status: string }): { text: string; hint: string } {
  return r.status === "failed"
    ? { text: t("ov.need_failed"), hint: t("ov.need_failed_hint") }
    : { text: t("ov.need_high"), hint: t("ov.need_high_hint") };
}

export function OverviewPage() {
  const { ov, draft, catalog, go, refresh } = useStore();
  const s = ov.status;
  const plugin = (id: string) => catalog.find((p) => p.id === id);
  const dayStart = new Date(); dayStart.setHours(0, 0, 0, 0);
  const today = ov.rows.filter((r) => r.at * 1000 >= dayStart.getTime());
  const count = (f: (r: typeof today[0]) => boolean) => today.filter(f).length;
  const needs = ov.rows.filter((r) => r.status === "failed"
    || (r.event === "COMMENT" && r.findings.some((f) => f.severity === "high"))).slice(0, 5);
  const needKeys = new Set(needs.map((r) => `${r.account}/${r.key}`));
  // One list, newest first: rows that need you are flagged in place instead of pinned above newer reviews.
  const latest = ov.rows.slice(0, 8);
  const recent = [...latest, ...needs.filter((r) => !latest.includes(r))].sort((a, b) => b.at - a.at);
  const forges = accounts(draft).filter((a) => a.enabled).map((a) => a.kind);
  const channels = activeChannels(draft);
  const problem = s.state === "error" || s.state === "paused";

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

      {ov.legacy.length > 0 && <Note>{t("ov.legacy")} {ov.legacy.join("；")}</Note>}

      <section class="metrics" aria-label={t("ov.today")}>
        <div class="metrics-title">{t("ov.today")}</div>
        <Metric label={t("ov.reviewed")} value={count((r) => !!r.event || r.status === "failed")} />
        <Metric label={t("ov.approved")} value={count((r) => r.event === "APPROVE" || r.status === "direct_approved")} tone="good" />
        <Metric label={t("ov.commented")} value={count((r) => r.event === "COMMENT")} tone="warn" />
        <Metric label={t("ov.failed")} value={count((r) => r.status === "failed")} tone="bad" />
      </section>

      <div class="chain">
        {[...forges, draft.review.engine, ...channels].map((id, i) => {
          const p = plugin(id);
          return <span class="chain-item" key={`${id}-${i}`}><Tile icon={p?.icon ?? "plugin"} size={18} />{p?.name ?? id}</span>;
        })}
        {channels.length === 0 && <span class="chain-item muted">{t("ov.no_channel")}</span>}
        <button class="link small" onClick={() => go("plugins")}>{t("ov.change")}</button>
      </div>

      <section class="block">
        <header class="block-head">
          <h2 class="block-title">{t("ov.recent")}</h2>
          {ov.rows.length > 0 && <button class="link small" onClick={() => go("reviews")}>{t("btn.all_reviews")} <Icon name="chevron" size={14} /></button>}
        </header>
        {ov.rows.length === 0 ? <div class="list-card"><Empty icon="list">{t("ov.empty")}</Empty></div>
          : recent.length > 0 && <div class="list-card">{recent.map((r) => <ReviewRow r={r} flag={needKeys.has(`${r.account}/${r.key}`) ? needWhy(r) : undefined} onClick={() => go("reviews", `${r.account}/${r.key}`)} />)}</div>}
      </section>
    </div>
  );
}

function Metric({ label, value, tone }: { label: string; value: number; tone?: string }) {
  return (
    <div class={`metric ${value > 0 ? tone ?? "" : ""}`}>
      <div class="metric-value">{value}</div>
      <div class="metric-label">{label}</div>
    </div>
  );
}
