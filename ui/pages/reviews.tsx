// Reviews: search and filter on the left, the selected review on the right; "review a PR by hand" as a dialog.
import { useEffect, useMemo, useState } from "preact/hooks";
import { api, type Row } from "../api";
import { useStore } from "../app";
import { Icon, Tile } from "../icons";
import { ago, has, t, type Key } from "../i18n";
import { Badge, Button, Dialog, Empty, Note, Segmented, TextInput, errText } from "../ui";

/** Nod's review result. The PR's own state (merged, closed) is shown apart, by prState. */
/** "15:30" today, "11-20 15:30" another day. */
export function clock(ts: number): string {
  const d = new Date(ts * 1000), now = new Date();
  const hm = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  return d.toDateString() === now.toDateString() ? hm : `${d.getMonth() + 1}-${d.getDate()} ${hm}`;
}

export function statusLabel(r: Row): string {
  if (r.status === "retry" && r.wait_until > 0) return t("st.waiting");
  const k = (r.status === "posted" || r.status === "closed") && r.earlier_approved ? "st.posted.APPROVE"
    : (r.status === "posted" || r.status === "closed") && r.event ? `st.posted.${r.event}`
    : r.status === "closed" && r.commented ? "st.posted.COMMENT"
    : r.status === "closed" && r.summary ? (r.pr_state === "merged" ? "st.merged_first" : "st.closed_first")
    : r.status === "closed" ? "st.not_posted" : `st.${r.status}`;
  return has(k) ? t(k) : r.status;
}

/** The PR itself, in GitHub's colours: open green, merged purple, closed red, draft grey. */
export function prState(r: Row): { text: string; tone: string } | null {
  const tone = { open: "good", merged: "merged", closed: "bad", draft: "muted" }[r.pr_state];
  return tone ? { text: t(`pr.${r.pr_state}` as Key), tone } : null;
}
export function tone(r: Row): string {
  if (r.status === "direct_approved" || r.event === "APPROVE" || ((r.status === "closed" || r.status === "posted") && r.earlier_approved)) return "good";
  if (r.status === "failed") return "bad";
  if (r.status === "retry" && r.wait_until > 0) return "warn";
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

export function ReviewRow({ r, onClick, selected, flag }: { r: Row; onClick: () => void; selected?: boolean; flag?: { text: string; hint: string } }) {
  const f = findingsText(r);
  // open is the usual case: only a merged, closed or draft PR is worth a word in the list
  const pr = r.pr_state !== "open" ? prState(r) : null;
  return (
    <button class={`review-row${selected ? " selected" : ""}${flag ? " flagged" : ""}`} onClick={onClick}>
      <span class={`state-dot ${tone(r)}`} />
      <span class="grow">
        <span class="row-title ellipsis">{r.title || r.key}</span>
        <span class="row-sub ellipsis">{flag && <span class="row-flag" title={flag.hint}>{flag.text}</span>}{r.key} · {r.author} · {ago(r.at)}{pr && <> · <span class={`pr-state ${pr.tone}`}>{pr.text}</span></>}</span>
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
  approved: (r) => r.event === "APPROVE" || r.status === "direct_approved" || ((r.status === "closed" || r.status === "posted") && r.earlier_approved),
  commented: (r) => r.event === "COMMENT" || (r.status === "closed" && r.commented),
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
  const { catalog, toast, refresh } = useStore();
  const [busy, setBusy] = useState(false);
  const waiting = r.status === "retry" && r.wait_until > 0;
  const again = async () => {
    setBusy(true);
    try { await api.track(r.url || r.key); toast(t("rv.again_started")); setTimeout(refresh, 800); }
    catch (e) { toast(errText(e), "bad"); }
    finally { setBusy(false); }
  };
  // "Approve" as you, like the Feishu card's button: two clicks, since it posts to GitHub in your name
  const [arming, setArming] = useState(false);
  const [sent, setSent] = useState(false);
  useEffect(() => { setArming(false); setSent(false); }, [r.key, r.head]);
  useEffect(() => { if (!arming) return; const id = setTimeout(() => setArming(false), 5000); return () => clearTimeout(id); }, [arming]);
  const canApprove = !!r.head && r.pr_state !== "merged" && r.pr_state !== "closed" && r.pr_state !== "draft"
    && r.status !== "direct_approved" && !(r.event === "APPROVE" && r.status === "posted");
  const approve = async () => {
    if (!arming) { setArming(true); return; }
    setArming(false);
    try {
      await api.approve(r.account, r.key, r.head);
      setSent(true);
      toast(t("rv.approve_sent"));
      [800, 2500, 6000].forEach((ms) => setTimeout(refresh, ms));
    } catch (e) { toast(errText(e), "bad"); }
  };
  const pname = (id: string) => catalog.find((p) => p.id === id)?.name ?? id;
  const engineIcon = catalog.find((p) => p.id === r.engine)?.icon ?? "plugin";
  return (
    <article class="detail">
      <header>
        <div class="badges">
          <Badge tone={tone(r)}>{statusLabel(r)}</Badge>
          {prState(r) && <Badge tone={prState(r)!.tone}>{prState(r)!.text}</Badge>}
        </div>
        <h2>{r.title || r.key}</h2>
        <div class="muted small">{r.key} · {r.author} · {ago(r.at)}</div>
        <div class="detail-meta">
          <span><Tile icon={catalog.find((p) => p.id === r.platform)?.icon ?? "plugin"} size={18} /> {pname(r.platform)} · {r.account}</span>
          {r.engine && <span><Tile icon={engineIcon} size={18} /> {pname(r.engine)}</span>}
        </div>
        <div class="detail-actions">
          {r.url && <Button small icon="external" onClick={() => api.url(r.url)}>{t("btn.open_pr")}</Button>}
          {r.review_url && <Button small icon="external" onClick={() => api.url(r.review_url!)}>
            {t(r.status === "closed" && !r.event ? "btn.open_last_review" : "btn.open_review")}</Button>}
          {canApprove && (
            <Button small kind={arming ? "primary" : "ghost"} icon="check" busy={r.direct_busy || (sent && !r.action_msg)} onClick={approve}>
              {arming ? t("rv.approve_confirm", { sha: r.sha }) : t("rv.approve")}
            </Button>
          )}
          {(r.status === "failed" || waiting) && r.pr_state !== "merged" && r.pr_state !== "closed"
            && <Button small icon="refresh" disabled={busy} onClick={again}>{t("rv.again")}</Button>}
        </div>
      </header>
      {r.status === "posted" && r.event === "COMMENT" && r.earlier_approved && <Note>{t("rv.still_approved", { sha: r.sha })}</Note>}
      {r.status === "closed" && !r.event && r.summary && (
        <Note>{t(r.commented ? "rv.closed_commented" : r.earlier_approved ? "rv.not_posted_earlier" : "rv.not_posted", { sha: r.sha })}</Note>
      )}
      {r.action_msg && !r.direct_busy && r.status !== "direct_approved" && <Note tone="warn">{r.action_msg}</Note>}
      {r.summary && <p class="summary">{r.summary}</p>}
      {!r.summary && !r.error && tone(r) === "busy" && <p class="muted">{t("rv.waiting")}</p>}
      {waiting && <Note tone="warn">{t("rv.quota_wait", { at: clock(r.wait_until) })}</Note>}
      {r.error && (waiting ? <p class="muted small" style={{ wordBreak: "break-word" }}>{r.error}</p> : <Note tone="bad">{r.error}</Note>)}
      {r.reasons.length > 0 && !(r.status === "closed" && !r.event && r.summary) && (
        <section><h3>{t(r.status === "closed" ? "rv.closed_note" : "rv.blocked")}</h3><ul class="reasons">{r.reasons.map((x) => <li>{x}</li>)}</ul></section>
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

/** What the backend will make of pasted text (src-tauri/src/forge/mod.rs clean_ref + parse_url), shown while
 *  typing so a wrong paste is obvious before anything runs. The backend stays the authority. */
export function recognizePr(text: string): string | null {
  const t = text.trim();
  const m = t.match(/https?:\/\/[^\s"'`<>“”‘’「」（）【】《》，。；！？、]+/i) ?? (/^[\w.-]+\.\w+\//.test(t) ? [`https://${t}`] : null);
  if (m) {
    const rest = m[0].replace(/^https?:\/\//i, "").split(/[?#]/)[0];
    const parts = rest.split("/").filter(Boolean).slice(1);
    for (let i = parts.length - 2; i >= 0; i--) {
      if ((parts[i] === "pull" || parts[i] === "pulls") && /^\d+$/.test(parts[i + 1]) && i >= 2) return `${parts.slice(0, i).join("/")}#${parts[i + 1]}`;
    }
    return null;
  }
  const k = t.replace(/^[`"'<(\[]+|[`"'>)\].,，。]+$/g, "").replace(/\s+/g, "");
  return /^[^/#\s]+(\/[^/#\s]+)+#\d+$/.test(k) ? k : null;
}

function HandDialog({ onClose }: { onClose: () => void }) {
  const [pr, setPr] = useState("");
  const [msg, setMsg] = useState<{ tone: "info" | "good" | "bad"; text: string } | null>(null);
  const [running, setRunning] = useState(false);
  const [started, setStarted] = useState(false);
  const key = recognizePr(pr);
  const review = async () => {
    if (!key || running || started) return;
    setRunning(true); setMsg(null);
    try { setMsg({ tone: "good", text: t("hand.started", { k: await api.track(pr) }) }); setStarted(true); }
    catch (e) { setMsg({ tone: "bad", text: errText(e) }); }
    finally { setRunning(false); }
  };
  return (
    <Dialog title={t("hand.title")} onClose={onClose}>
      <p class="muted">{t("hand.intro")}</p>
      <div onKeyDown={(e) => { if (e.key === "Enter") review(); }}>
        <TextInput value={pr} onInput={(v) => { setPr(v); setStarted(false); setMsg(null); }} placeholder="https://github.com/owner/repo/pull/123" mono autoFocus />
      </div>
      <p class={`small ${key ? "hand-key" : "muted"}`}>{pr.trim() ? (key ? t("hand.recognized", { k: key }) : t("hand.unrecognized")) : t("hand.formats")}</p>
      <div class="hand-main">
        {started ? <Button kind="primary" onClick={onClose}>{t("hand.see_records")}</Button>
          : <Button kind="primary" icon="play" busy={running} disabled={!key} onClick={review}>{t("hand.review")}</Button>}
      </div>
      {msg && <Note tone={msg.tone}>{msg.text}</Note>}
    </Dialog>
  );
}