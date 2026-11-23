// Reviews: search and filter on the left, the selected review on the right; "review a PR by hand" as a dialog.
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import { api, type Row } from "../api";
import { useStore } from "../app";
import { Icon, Tile } from "../icons";
import { ago, has, t, type Key } from "../i18n";
import { Badge, Button, Dialog, Empty, Note, Segmented, TextInput, errText } from "../ui";
import { SEV, isApproved, isNotApproved, needsYou, sevCounts } from "../derive";

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
  if (isApproved(r)) return "good";
  if (r.status === "failed") return "bad";
  if (r.status === "retry" && r.wait_until > 0) return "warn";
  if (["queued", "reviewing", "awaiting_ci", "retry"].includes(r.status)) return "busy";
  if (r.event === "COMMENT") return "warn";
  return "muted";
}

/** "4 findings · 1 high": the count, and the worst severity when it matters. */
/** "3 issues · 1 high" for a row, and `short` ("1 high", or "3 issues" when none is high or medium) for the narrow
 *  review list, where the long form leaves the title a few characters. */
export function findingsText(r: Row): { text: string; short: string; tone: string } | null {
  const n = r.findings.length;
  if (!n) return null;
  const high = r.findings.filter((f) => f.severity === "high").length;
  const medium = r.findings.filter((f) => f.severity === "medium").length;
  const worst = high ? t("rv.n_high", { n: high }) : medium ? t("rv.n_medium", { n: medium }) : "";
  const count = n === 1 ? t("rv.one_finding") : t("rv.n_findings", { n });
  return { text: count + (worst ? " · " + worst : ""), short: worst || count, tone: high ? "bad" : medium ? "warn" : "" };
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
        <span class="row-sub ellipsis">{flag && <span class="row-flag" title={flag.hint}>{flag.text}</span>}<Key k={r.key} /> · {r.author} · {ago(r.at)}{pr && <> · <span class={`pr-state ${pr.tone}`}>{pr.text}</span></>}</span>
      </span>
      <span class="row-right">
        <span class={`state-label ${tone(r)}`}>{statusLabel(r)}</span>
        {f && <span class={`find ${f.tone}`} title={f.text}><span class="find-full">{f.text}</span><span class="find-short">{f.short}</span></span>}
      </span>
    </button>
  );
}

// "needs" is the overview's "N more": open PRs that wait on you; not a tab, a removable bar above the list
/** "owner/repo#12": the owner is dropped in the narrow review list (CSS), so the PR number is not the part cut off. */
function Key({ k }: { k: string }) {
  const i = k.indexOf("/");
  return i > 0 && k.indexOf("/", i + 1) < 0 ? <span title={k}><span class="key-owner">{k.slice(0, i + 1)}</span>{k.slice(i + 1)}</span> : <>{k}</>;
}

type Filter = "all" | "approved" | "commented" | "active" | "failed" | "needs";
const match: Record<Filter, (r: Row) => boolean> = {
  all: () => true,
  approved: isApproved,
  commented: isNotApproved,
  active: (r) => ["queued", "reviewing", "awaiting_ci", "retry"].includes(r.status),
  failed: (r) => r.status === "failed",
  needs: needsYou,
};

/** An empty list says why: a search that found nothing names what was searched and offers to clear it; an empty
 *  filter says what it means ("no failed reviews" is good news, not "no matching reviews"). */
function NoRows({ q, filter, anything, onClear }: { q: string; filter: Filter; anything: boolean; onClear: () => void }) {
  if (!anything) return <Empty icon="list">{t("ov.empty")}</Empty>;
  if (q) return <Empty icon="search" action={<Button kind="quiet" small onClick={onClear}>{t("rv.clear_search")}</Button>}>{t("rv.none_q", { q })}</Empty>;
  const icon = filter === "failed" || filter === "needs" ? "check" : "info";
  return <Empty icon={icon}>{t(`rv.none_${filter}` as "rv.none")}</Empty>;
}

export function ReviewsPage() {
  const { ov, review, go } = useStore();
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [hand, setHand] = useState(review === "__hand__");
  // a PR just sent for review by hand: opened as soon as its row is there (it may take a poll to appear)
  const [want, setWant] = useState<string | null>(null);
  useEffect(() => {
    const r = want && ov.rows.find((x) => x.key === want);
    if (r) {
      setWant(null); setQ(""); setFilter("all"); go("reviews", `${r.account}/${r.key}`);
      // it may sit far down the list
      setTimeout(() => document.querySelector(".split-list .review-row.selected")?.scrollIntoView({ block: "nearest" }), 50);
    }
  }, [want, ov.rows]);
  useEffect(() => {
    if (review === "__hand__") { setHand(true); go("reviews", null as unknown as string); }
    // the overview's "N more" opens the list filtered
    if (review?.startsWith("__filter:")) { setFilter(review.slice(9) as Filter); go("reviews", null as unknown as string); }
  }, [review]);

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
          {/* Esc or the × clears it, like a system search field */}
          <div class="search" onKeyDown={(e) => { if (e.key === "Escape" && q) { e.stopPropagation(); setQ(""); } }}>
            <Icon name="search" /><TextInput value={q} onInput={setQ} placeholder={t("rv.search")} />
            {q && <button class="search-clear" aria-label={t("rv.clear_search")} title={t("rv.clear_search")} onClick={() => setQ("")}><Icon name="x" size={12} /></button>}
          </div>
          <Segmented value={filter} onChange={(v) => setFilter(v as Filter)} options={[
            ["all", t("rv.all")], ["approved", t("rv.f.approved")], ["commented", t("rv.f.commented")],
            ["active", t("rv.f.active")], ["failed", t("rv.f.failed")],
          ]} />
          {want && (
            <div class="filter-bar"><Icon name="refresh" size={14} /><span class="grow">{t("rv.want", { k: want })}</span>
              <button class="icon-btn" aria-label={t("btn.cancel")} title={t("btn.cancel")} onClick={() => setWant(null)}><Icon name="x" size={14} /></button></div>
          )}
          {filter === "needs" && (
            <div class="filter-bar"><Icon name="hand" size={14} /><span class="grow">{t("rv.f.needs_n", { n: ov.rows.filter(needsYou).length })}</span>
              <button class="icon-btn" aria-label={t("rv.f.clear")} title={t("rv.f.clear")} onClick={() => setFilter("all")}><Icon name="x" size={14} /></button></div>
          )}
        </div>
        <div class="list scroll">
          {rows.length === 0 ? <NoRows q={q.trim()} filter={filter} anything={ov.rows.length > 0} onClear={() => setQ("")} />
            : rows.map((r) => <ReviewRow key={id(r)} r={r} selected={!!selected && id(r) === id(selected)} onClick={() => go("reviews", id(r))} />)}
        </div>
      </div>
      <div class="split-detail">
        {selected ? <><button class="link back" onClick={() => go("reviews", null as unknown as string)}><Icon name="chevron" size={14} /> {t("rv.title")}</button><Detail r={selected} /></> : (wide && rows.length === 0) ? null : <Empty icon="file">{t("rv.pick")}</Empty>}
      </div>
      {hand && <HandDialog onClose={(k) => { setHand(false); if (typeof k === "string" && k) setWant(k); }} />}
    </div>
  );
}

/** The review prompt (src-tauri/assets/review.*.md) marks an issue already raised in the PR's discussion by a fixed
 *  suffix on its title; shown as a tag instead, so titles stay readable. */
const RAISED = /\s*(（已有评论提过）|\(raised in discussion\))\s*$/i;
export function splitRaised(title: string): [string, boolean] {
  return RAISED.test(title) ? [title.replace(RAISED, ""), true] : [title, false];
}

/** The findings of one review, worst first. */
function FindingList({ findings }: { findings: Row["findings"] }) {
  if (findings.length === 0) return <p class="muted">{t("rv.no_findings")}</p>;
  return (
    <ul class="findings">
      {[...findings].sort((a, b) => SEV.indexOf(a.severity as never) - SEV.indexOf(b.severity as never)).map((f) => (
        <li>
          <span class={`sev ${f.severity}`}>{t(`sev.${f.severity}` as Key)}</span>
          <div class="grow">
            <div>{splitRaised(f.title)[0]}{splitRaised(f.title)[1] && <span class="raised">{t("rv.raised")}</span>}
              {/* most issues come with the PR; the ones that were already there are the exception worth a word (they do
                  not hold back an approval) */}
              {!f.introduced_by_pr && <span class="raised">{t("rv.old")}</span>}</div>
            {/* a long path is cut from the left, so the file name and line stay in view */}
            {f.path && <div class="find-path mono small muted" title={`${f.path}${f.line ? `:${f.line}` : ""}`}>
              <bdi>{f.path}{f.line ? `:${f.line}` : ""}</bdi></div>}
          </div>
        </li>
      ))}
    </ul>
  );
}

type Round = Row["rounds"][number];

/** Several reviews of one PR, drawn: one column per review, oldest first, stacked by severity, so "fewer issues each
 *  round" (or not) is seen at a glance. The sentence says the same in words, counting high and medium. */
function RoundTrend({ rounds }: { rounds: Round[] }) {
  // a round with no summary was not reviewed (e.g. approved directly before Nod got to it): it says nothing about issues
  const xs = [...rounds].reverse().filter((x) => x.summary || x.findings.length > 0);
  if (xs.length < 2) return null;
  const max = Math.max(1, ...xs.map((x) => x.findings.length));
  const serious = (x: Round) => { const c = sevCounts(x.findings); return c.high + c.medium; };
  const first = serious(xs[0]), last = serious(xs[xs.length - 1]);
  const line = last < first ? t("rd.trend_down", { n: xs.length, a: first, b: last })
    : last > first ? t("rd.trend_up", { n: xs.length, a: first, b: last }) : t("rd.trend_same", { n: xs.length, a: last });
  return (
    <figure class="trend">
      <figcaption>{line}</figcaption>
      <div class="trend-bars" style={{ gridTemplateColumns: `repeat(${xs.length}, minmax(0, 28px))` }}>
        {xs.map((x, i) => {
          const c = sevCounts(x.findings);
          const o = roundOutcome(x);
          return (
            <div class="trend-col" title={`${x.sha} · ${clock(x.at)} · ${o.text} · ${x.findings.length ? t("rd.n_findings", { n: x.findings.length }) : t("rd.no_findings")}`}>
              <div class="trend-stack">
                {SEV.slice().reverse().map((s) => c[s] > 0 && <span class={`seg ${s}`} style={{ height: `${(c[s] / max) * 100}%` }} />)}
                {x.findings.length === 0 && <span class="seg none" />}
              </div>
              <span class={`trend-n ${o.tone}`}>{x.event === "APPROVE" && !x.withdrawn ? <Icon name="check" size={11} /> : i + 1}</span>
            </div>
          );
        })}
      </div>
      <div class="trend-legend">{(["high", "medium", "low", "nit"] as const).map((s) => <span><i class={s} />{t(`sev.${s}` as Key)}</span>)}
        <span><Icon name="check" size={11} />{t("rd.approved")}</span></div>
    </figure>
  );
}

/** What went up to GitHub for one review. */
function roundOutcome(x: Round): { text: string; tone: string } {
  if (x.event === "APPROVE") {
    return x.withdrawn ? { text: t("rd.withdrawn"), tone: "muted" } : { text: t(x.direct ? "rd.direct" : "rd.approved"), tone: "good" };
  }
  if (x.event === "COMMENT") return { text: t("rd.commented"), tone: "warn" };
  if (x.comment_url) return { text: t("rd.commented_waiting"), tone: "warn" };
  return { text: t("rd.not_posted"), tone: "muted" };
}

/** "⋯": the less common actions, out of the way. */
function MoreMenu({ items }: { items: { label: string; icon: string; danger?: boolean; onClick: () => void }[] }) {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: Event) => { if (!wrap.current?.contains(e.target as Node)) setOpen(false); };
    document.addEventListener("mousedown", away, true);
    return () => document.removeEventListener("mousedown", away, true);
  }, [open]);
  if (items.length === 0) return null;
  return (
    <div class="more" ref={wrap}>
      <button class="btn ghost small icon-only" aria-label={t("btn.more")} title={t("btn.more")} aria-expanded={open}
        onClick={() => setOpen(!open)} onKeyDown={(e) => { if (e.key === "Escape") setOpen(false); }}><Icon name="more" /></button>
      {open && (
        <ul class="select-menu more-menu" role="menu">
          {items.map((it) => (
            <li role="menuitem" class={it.danger ? "danger" : ""} onMouseDown={(e) => e.preventDefault()}
              onClick={() => { setOpen(false); it.onClick(); }}><Icon name={it.icon} size={14} /><span>{it.label}</span></li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function Detail({ r }: { r: Row }) {
  const { catalog, toast, refresh } = useStore();
  const waiting = r.status === "retry" && r.wait_until > 0;
  const [confirm, setConfirm] = useState<"" | "approve" | "withdraw">("");
  const [sent, setSent] = useState(false);
  useEffect(() => { setConfirm(""); setSent(false); }, [r.key, r.head, r.status]);
  const later = () => [800, 2500, 6000].forEach((ms) => setTimeout(refresh, ms));
  const open = r.pr_state !== "merged" && r.pr_state !== "closed";
  const canWithdraw = open && r.my_approvals;
  const canApprove = !!r.head && open && r.pr_state !== "draft"
    && r.status !== "direct_approved" && !(r.event === "APPROVE" && r.status === "posted");
  const canAgain = open && r.pr_state !== "draft" && !["queued", "reviewing"].includes(r.status);
  const again = async () => {
    try { await api.track(r.url || r.key, true); toast(t("rv.again_started")); later(); }
    catch (e) { toast(errText(e), "bad"); }
  };
  const act = async (what: "approve" | "withdraw") => {
    setConfirm("");
    try {
      if (what === "approve") await api.approve(r.account, r.key, r.head); else await api.withdraw(r.account, r.key);
      setSent(true);
      toast(t(what === "approve" ? "rv.approve_sent" : "rv.withdraw_sent"));
      later();
    } catch (e) { toast(errText(e), "bad"); }
  };
  const pname = (id: string) => catalog.find((p) => p.id === id)?.name ?? id;
  const engineIcon = catalog.find((p) => p.id === r.engine)?.icon ?? "plugin";
  // the latest review is shown in full; the ones before it one line each, opened on demand
  const latest = r.rounds[0];
  const earlier = r.rounds.slice(1);
  // the main button: approve when that is possible, else review again; the rest go in "⋯"
  const more = [
    ...(canAgain && canApprove ? [{ label: t("rv.again"), icon: "refresh", onClick: again }] : []),
    ...(canWithdraw ? [{ label: t("rv.withdraw"), icon: "x", danger: true, onClick: () => setConfirm("withdraw") }] : []),
  ];
  return (
    <article class="detail">
      <header>
        <div class="badges">
          <Badge tone={tone(r)}>{statusLabel(r)}</Badge>
          {prState(r) && <Badge tone={prState(r)!.tone}>{prState(r)!.text}</Badge>}
        </div>
        <h2>{r.title || r.key}</h2>
        <div class="muted small detail-sub">
          <span><span class="nowrap">{r.key}</span> · <span class="nowrap">{r.author}</span> · <span class="nowrap">{ago(r.at)}</span></span>
          {/* the two links wrap together: one left alone on the next line reads like a stray */}
          <span class="detail-links">
            {r.url && <button class="link small" onClick={() => api.url(r.url)}>{t("btn.open_pr")} <Icon name="external" size={12} /></button>}
            {r.review_url && <button class="link small" onClick={() => api.url(r.review_url!)}>
              {t(r.status === "closed" && !r.event ? "btn.open_last_review" : "btn.open_review")} <Icon name="external" size={12} /></button>}
          </span>
        </div>
        <div class="detail-bar">
          <div class="detail-meta">
            <span><Tile icon={catalog.find((p) => p.id === r.platform)?.icon ?? "plugin"} size={18} /> {pname(r.platform)}{r.account.toLowerCase() !== r.platform.toLowerCase() && pname(r.platform).toLowerCase() !== r.account.toLowerCase() ? ` · ${r.account}` : ""}</span>
            {r.engine && <span><Tile icon={engineIcon} size={18} /> {pname(r.engine)}</span>}
          </div>
          <div class="detail-actions">
            {canApprove && (
              <Button small kind="primary" icon="check" busy={r.direct_busy || (sent && !r.action_msg)} onClick={() => setConfirm("approve")}>{t("rv.approve")}</Button>
            )}
            {!canApprove && canAgain && <Button small icon="refresh" onClick={again}>{t("rv.again")}</Button>}
            <MoreMenu items={more} />
          </div>
        </div>
      </header>
      {confirm && (
        <Dialog title={t(confirm === "approve" ? "rv.approve_title" : "rv.withdraw_title")} onClose={() => setConfirm("")}
          footer={<>
            <Button kind="quiet" onClick={() => setConfirm("")}>{t("btn.cancel")}</Button>
            <Button kind={confirm === "approve" ? "primary" : "danger"} onClick={() => act(confirm as "approve" | "withdraw")}>
              {t(confirm === "approve" ? "rv.approve" : "rv.withdraw")}</Button>
          </>}>
          <p>{t(confirm === "approve" ? "rv.approve_body" : "rv.withdraw_body", { sha: r.sha, key: r.key })}</p>
        </Dialog>
      )}
      {r.status === "posted" && r.event === "COMMENT" && r.earlier_approved && <Note>{t("rv.still_approved", { sha: r.sha })}</Note>}
      {r.status === "closed" && !r.event && r.summary && (
        <Note>{t(r.commented ? "rv.closed_commented" : r.earlier_approved ? "rv.not_posted_earlier" : "rv.not_posted", { sha: r.sha })}</Note>
      )}
      {r.action_msg && !r.direct_busy && r.status !== "direct_approved" && <Note tone="warn">{r.action_msg}</Note>}
      {waiting && <Note tone="warn">{t("rv.quota_wait", { at: clock(r.wait_until) })}</Note>}
      {r.error && (waiting ? <p class="muted small" style={{ wordBreak: "break-word" }}>{r.error}</p> : <Note tone="bad">{r.error}</Note>)}

      {r.rounds.length >= 2 && <RoundTrend rounds={r.rounds} />}

      {(r.summary || r.findings.length > 0 || tone(r) === "busy") && (
        <section class="round current">
          <h3>
            {t("rd.latest")}
            {latest && latest.sha === r.sha && <span class="muted small round-head">
              <span class="mono">{latest.sha}</span> · {ago(latest.at)} · <span class={`state-label ${roundOutcome(latest).tone}`}>{roundOutcome(latest).text}</span>
            </span>}
          </h3>
          {r.summary && <p class="summary">{r.summary}</p>}
          {!r.summary && !r.error && tone(r) === "busy" && <p class="muted">{t("rv.waiting")}</p>}
          {r.reasons.length > 0 && !(r.status === "closed" && !r.event && r.summary) && (
            <div class="sub"><h4>{t(r.status === "closed" ? "rv.closed_note" : "rv.blocked")}</h4><ul class="reasons">{r.reasons.map((x) => <li>{x}</li>)}</ul></div>
          )}
          {(r.summary || r.findings.length > 0) && (
            <div class="sub"><h4>{t("rv.findings")} <span class="muted">{r.findings.length || ""}</span></h4><FindingList findings={r.findings} /></div>
          )}
        </section>
      )}

      {earlier.length > 0 && (
        <section class="rounds">
          <h3>{t("rd.earlier")} <span class="muted">{earlier.length}</span></h3>
          {earlier.map((x) => {
            const o = roundOutcome(x);
            const n = x.findings.length;
            // the review marks issues already raised in the discussion: list the new ones, fold the rest
            const fresh = x.findings.filter((f) => !splitRaised(f.title)[1]);
            const again = x.findings.filter((f) => splitRaised(f.title)[1]);
            return (
              <details key={`${x.sha}|${x.at}`} class="round">
                <summary>
                  <span class="mono">{x.sha}</span>
                  <span class="muted">{ago(x.at)}</span>
                  <span class={`state-label ${o.tone}`}>{o.text}</span>
                  <span class="muted grow">{n ? t("rd.n_findings", { n }) : t("rd.no_findings")}{again.length > 0 && fresh.length > 0 && <> · {t("rd.n_new", { n: fresh.length })}</>}</span>
                  <Icon name="chevron" size={14} />
                </summary>
                <div class="round-body">
                  {x.summary && <p class="summary">{x.summary}</p>}
                  {(x.summary || n > 0) && (again.length === 0 ? <FindingList findings={x.findings} /> : <>
                    {fresh.length > 0 && <FindingList findings={fresh} />}
                    <details class="raised-fold"><summary><Icon name="chevron" size={13} />{t(fresh.length ? "rd.more_raised" : "rd.all_raised", { n: again.length })}</summary>
                      <FindingList findings={again} /></details>
                  </>)}
                  <div class="round-links">
                    {x.comment_url && x.comment_url !== x.url && <button class="link small" onClick={() => api.url(x.comment_url!)}>{t("rd.open_comments")} <Icon name="external" size={12} /></button>}
                    {x.url && <button class="link small" onClick={() => api.url(x.url!)}>{t("btn.open_review")} <Icon name="external" size={12} /></button>}
                  </div>
                </div>
              </details>
            );
          })}
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

/** `onClose(k)`: closed after starting a review of `k`, which the page then opens. */
function HandDialog({ onClose }: { onClose: (started?: string) => void }) {
  const [pr, setPr] = useState("");
  const [msg, setMsg] = useState<{ tone: "info" | "good" | "bad"; text: string } | null>(null);
  const [running, setRunning] = useState(false);
  const [started, setStarted] = useState<string | null>(null);
  const key = recognizePr(pr);
  const review = async () => {
    if (!key || running || started) return;
    setRunning(true); setMsg(null);
    try { const k = await api.track(pr); setMsg({ tone: "good", text: t("hand.started", { k }) }); setStarted(k); }
    catch (e) { setMsg({ tone: "bad", text: errText(e) }); }
    finally { setRunning(false); }
  };
  return (
    // closing it (×, Esc, outside) opens nothing: only "Open this review" does
    <Dialog title={t("hand.title")} onClose={() => onClose()}>
      <p class="muted">{t("hand.intro")}</p>
      <div onKeyDown={(e) => { if (e.key === "Enter") review(); }}>
        <TextInput value={pr} onInput={(v) => { setPr(v); setStarted(null); setMsg(null); }} placeholder="https://github.com/owner/repo/pull/123" mono autoFocus />
      </div>
      <p class={`small ${key ? "hand-key" : "muted"}`}>{pr.trim() ? (key ? t("hand.recognized", { k: key }) : t("hand.unrecognized")) : t("hand.formats")}</p>
      <div class="hand-main">
        {started ? <Button kind="primary" onClick={() => onClose(started)}>{t("hand.see_review")}</Button>
          : <Button kind="primary" icon="play" busy={running} disabled={!key} onClick={review}>{t("hand.review")}</Button>}
      </div>
      {msg && <Note tone={msg.tone}>{msg.text}</Note>}
    </Dialog>
  );
}