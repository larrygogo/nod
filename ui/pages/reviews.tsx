// Reviews: search and filter on the left, the selected review on the right; "review a PR by hand" as a dialog.
import { useEffect, useMemo, useState } from "preact/hooks";
import { api, type Row } from "../api";
import { useStore } from "../app";
import { Icon, Tile } from "../icons";
import { ago, has, t, type Key } from "../i18n";
import { Badge, Button, Dialog, Empty, Note, Segmented, TextInput, errText } from "../ui";

export function statusLabel(r: Row): string {
  const k = r.status === "posted" && r.event ? `st.posted.${r.event}` : `st.${r.status}`;
  return has(k) ? t(k) : r.status;
}
export function tone(r: Row): string {
  if (r.status === "direct_approved" || r.event === "APPROVE") return "good";
  if (r.status === "failed") return "bad";
  if (["queued", "reviewing", "awaiting_ci", "retry"].includes(r.status)) return "busy";
  if (r.event === "COMMENT") return "warn";
  return "muted";
}
const SEV = ["high", "medium", "low", "nit"];

/** "4 findings · 1 high": the count, and the worst severity when it matters. */
export function findingsText(r: Row): { text: string; tone: string } | null {
  const n = r.findings.length;
  if (!n) return null;
  const high = r.findings.filter((f) => f.severity === "high").length;
  const medium = r.findings.filter((f) => f.severity === "medium").length;
  const worst = high ? t("rv.n_high", { n: high }) : medium ? t("rv.n_medium", { n: medium }) : "";
  return { text: (n === 1 ? t("rv.one_finding") : t("rv.n_findings", { n })) + (worst ? " · " + worst : ""), tone: high ? "bad" : medium ? "warn" : "" };
}

export function ReviewRow({ r, onClick, selected, flag }: { r: Row; onClick: () => void; selected?: boolean; flag?: string }) {
  const f = findingsText(r);
  return (
    <button class={`review-row${selected ? " selected" : ""}${flag ? " flagged" : ""}`} onClick={onClick}>
      <span class={`state-dot ${tone(r)}`} />
      <span class="grow">
        <span class="row-title ellipsis">{r.title || r.key}</span>
        <span class="row-sub ellipsis">{flag && <span class="row-flag">{flag}</span>}{r.key} · {r.author} · {ago(r.at)}</span>
      </span>
      <span class="row-right">
        <span class={`state-label ${tone(r)}`}>{statusLabel(r)}</span>
        {f && <span class={`find ${f.tone}`}>{f.text}</span>}
      </span>
    </button>
  );
}

type Filter = "all" | "approved" | "commented" | "active" | "failed";
const match: Record<Filter, (r: Row) => boolean> = {
  all: () => true,
  approved: (r) => r.event === "APPROVE" || r.status === "direct_approved",
  commented: (r) => r.event === "COMMENT",
  active: (r) => ["queued", "reviewing", "awaiting_ci", "retry"].includes(r.status),
  failed: (r) => r.status === "failed",
};

export function ReviewsPage() {
  const { ov, review, go } = useStore();
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [hand, setHand] = useState(review === "__hand__");
  useEffect(() => { if (review === "__hand__") { setHand(true); go("reviews", null as unknown as string); } }, [review]);

  const rows = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return ov.rows.filter((r) => match[filter](r)
      && (!needle || [r.title, r.key, r.author].some((x) => x.toLowerCase().includes(needle))));
  }, [ov.rows, q, filter]);
  const id = (r: Row) => `${r.account}/${r.key}`;
  // wide windows always show a review on the right (the first one until another is picked)
  const wide = typeof window !== "undefined" && window.innerWidth > 900;
  const picked = ov.rows.find((r) => id(r) === review) ?? null;
  const selected = picked ?? (wide ? rows[0] ?? null : null);

  return (
    <div class={`split${picked ? " has-sel" : ""}`}>
      <div class="split-list">
        <header class="page-head"><h1>{t("rv.title")}</h1>
          <Button icon="hand" small onClick={() => setHand(true)}>{t("btn.by_hand")}</Button></header>
        <div class="toolbar">
          <div class="search"><Icon name="search" /><TextInput value={q} onInput={setQ} placeholder={t("rv.search")} /></div>
          <Segmented value={filter} onChange={(v) => setFilter(v as Filter)} options={[
            ["all", t("rv.all")], ["approved", t("rv.f.approved")], ["commented", t("rv.f.commented")],
            ["active", t("rv.f.active")], ["failed", t("rv.f.failed")],
          ]} />
        </div>
        <div class="list scroll">
          {rows.length === 0 ? <Empty icon="search">{ov.rows.length ? t("rv.none") : t("ov.empty")}</Empty>
            : rows.map((r) => <ReviewRow r={r} selected={!!selected && id(r) === id(selected)} onClick={() => go("reviews", id(r))} />)}
        </div>
      </div>
      <div class="split-detail">
        {selected ? <><button class="link back" onClick={() => go("reviews", null as unknown as string)}><Icon name="chevron" size={14} /> {t("rv.title")}</button><Detail r={selected} /></> : <Empty icon="file">{t("rv.pick")}</Empty>}
      </div>
      {hand && <HandDialog onClose={() => setHand(false)} />}
    </div>
  );
}

export function Detail({ r }: { r: Row }) {
  const { catalog } = useStore();
  const pname = (id: string) => catalog.find((p) => p.id === id)?.name ?? id;
  const engineIcon = catalog.find((p) => p.id === r.engine)?.icon ?? "plugin";
  return (
    <article class="detail">
      <header>
        <Badge tone={tone(r)}>{statusLabel(r)}</Badge>
        <h2>{r.title || r.key}</h2>
        <div class="muted small">{r.key} · {r.author} · {ago(r.at)}</div>
        <div class="detail-meta">
          <span><Tile icon={catalog.find((p) => p.id === r.platform)?.icon ?? "plugin"} size={18} /> {pname(r.platform)} · {r.account}</span>
          {r.engine && <span><Tile icon={engineIcon} size={18} /> {pname(r.engine)}</span>}
        </div>
        <div class="detail-actions">
          {r.url && <Button small icon="external" onClick={() => api.url(r.url)}>{t("btn.open_pr")}</Button>}
          {r.review_url && <Button small icon="external" onClick={() => api.url(r.review_url!)}>{t("btn.open_review")}</Button>}
        </div>
      </header>
      {r.summary && <p class="summary">{r.summary}</p>}
      {!r.summary && !r.error && tone(r) === "busy" && <p class="muted">{t("rv.waiting")}</p>}
      {r.error && <Note tone="bad">{r.error}</Note>}
      {r.reasons.length > 0 && (
        <section><h3>{t("rv.blocked")}</h3><ul class="reasons">{r.reasons.map((x) => <li>{x}</li>)}</ul></section>
      )}
      {(r.summary || r.findings.length > 0) && (
        <section>
          <h3>{t("rv.findings")} <span class="muted">{r.findings.length || ""}</span></h3>
          {r.findings.length === 0 ? <p class="muted">{t("rv.no_findings")}</p> : (
            <ul class="findings">
              {[...r.findings].sort((a, b) => SEV.indexOf(a.severity) - SEV.indexOf(b.severity)).map((f) => (
                <li>
                  <span class={`sev ${f.severity}`}>{t(`sev.${f.severity}` as Key)}</span>
                  <div class="grow">
                    <div>{f.title}</div>
                    <div class="mono small muted">{f.path}{f.line ? `:${f.line}` : ""} · {f.introduced_by_pr ? t("rv.introduced") : t("rv.old")}</div>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
    </article>
  );
}

function HandDialog({ onClose }: { onClose: () => void }) {
  const [pr, setPr] = useState("");
  const [msg, setMsg] = useState<{ tone: "info" | "good" | "bad"; text: string } | null>(null);
  const [result, setResult] = useState<Row | null>(null);
  const [running, setRunning] = useState(false);
  return (
    <Dialog title={t("hand.title")} onClose={onClose}>
      <p class="muted">{t("hand.intro")}</p>
      <TextInput value={pr} onInput={setPr} placeholder="https://github.com/owner/repo/pull/123" mono autoFocus />
      <div class="hand-actions">
        <div><Button kind="primary" icon="play" busy={running} disabled={!pr.trim()} onClick={async () => {
          setRunning(true); setResult(null); setMsg({ tone: "info", text: t("hand.running") });
          try { setResult(await api.dryRun(pr)); setMsg(null); }
          catch (e) { setMsg({ tone: "bad", text: errText(e) }); }
          finally { setRunning(false); }
        }}>{t("hand.dry")}</Button><p class="muted small">{t("hand.dry_help")}</p></div>
        <div><Button icon="plus" disabled={!pr.trim() || running} onClick={async () => {
          try { setMsg({ tone: "good", text: t("hand.tracked", { k: await api.track(pr) }) }); }
          catch (e) { setMsg({ tone: "bad", text: errText(e) }); }
        }}>{t("hand.track")}</Button><p class="muted small">{t("hand.track_help")}</p></div>
      </div>
      {msg && <Note tone={msg.tone}>{msg.text}</Note>}
      {result && <div class="hand-result"><Detail r={result} /></div>}
    </Dialog>
  );
}
