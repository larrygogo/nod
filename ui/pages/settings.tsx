// Settings: general, approval rules, review scope, reviewing, files, about. Plugin settings live under Plugins.
import { useEffect, useState } from "preact/hooks";
import { api } from "../api";
import { Icon } from "../icons";
import { getTheme, setTheme, useStore, useUpdate, type Theme } from "../app";
import { ago, setLang, t } from "../i18n";
import { Card, TagInput, cleanLogin, cleanRepo, Note, Row, Section, Segmented, Select, Switch, TextInput, Button } from "../ui";

/** One side of an always-approve rule: typed freely, checked and tidied (a pasted link becomes owner/repo) when the
 *  field is left. Empty means "any". */
function RuleField({ value, kind, placeholder, suggest, onChange }: {
  value: string; kind: "repo" | "login"; placeholder: string; suggest: string[]; onChange: (v: string) => void;
}) {
  const [text, setText] = useState(value);
  const [bad, setBad] = useState(false);
  const [focus, setFocus] = useState(false);
  const [hi, setHi] = useState(-1);
  useEffect(() => { setText(value); }, [value]);
  // the app's own suggestion menu, like TagInput's: the system <datalist> popup cannot be styled in the web view
  const q = text.trim().toLowerCase();
  const options = focus ? suggest.filter((x) => x.toLowerCase() !== q && (!q || x.toLowerCase().includes(q))).slice(0, 6) : [];
  const commit = (raw0 = text) => {
    const raw = raw0.trim();
    setHi(-1);
    if (!raw) { setBad(false); onChange(""); return; }
    const c = (kind === "repo" ? cleanRepo : cleanLogin)(raw);
    setBad(!c);
    if (c) { setText(c); onChange(c); }
  };
  const key = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown" && options.length) { e.preventDefault(); setHi((i) => Math.min(i + 1, options.length - 1)); }
    else if (e.key === "ArrowUp" && options.length) { e.preventDefault(); setHi((i) => Math.max(i - 1, -1)); }
    else if (e.key === "Escape") { setHi(-1); setFocus(false); }
    else if (e.key === "Enter") { e.preventDefault(); const o = hi >= 0 ? options[hi] : undefined; if (o) setText(o); commit(o ?? text); }
  };
  return (
    <div class="tags-wrap">
      <input class={`input mono${bad ? " invalid" : ""}`} value={text} placeholder={placeholder} spellcheck={false}
        title={bad ? t(kind === "repo" ? "tag.bad_repo" : "tag.bad_login", { x: text }) : undefined}
        onInput={(e) => { setText((e.target as HTMLInputElement).value); setBad(false); setHi(-1); setFocus(true); }}
        onFocus={() => setFocus(true)} onBlur={() => { setFocus(false); commit(); }} onKeyDown={key} />
      {options.length > 0 && (
        <ul class="select-menu tags-menu" role="listbox">
          {options.map((o, i) => (
            <li role="option" aria-selected={false} class={i === hi ? "hi" : ""} onMouseEnter={() => setHi(i)}
              onMouseDown={(e) => { e.preventDefault(); setText(o); setFocus(false); commit(o); }}><span class="mono ellipsis">{o}</span></li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Always-approve rules: a repository and an author on one line, both must match; an empty side means any. */
function AlwaysRules({ repoHints, loginHints }: { repoHints: string[]; loginHints: string[] }) {
  const { draft, edit } = useStore();
  const rules = draft.approve.always ?? [];
  const set = (i: number, k: "repo" | "author", v: string) => edit((c) => { c.approve.always![i][k] = v; });
  return (
    <div class="always">
      {rules.length > 0 && (
        <div class="always-row head muted small"><span>{t("set.always.repo")}</span><span /><span>{t("set.always.author")}</span><span /></div>
      )}
      {rules.map((r, i) => (
        <div class="always-row">
          <RuleField kind="repo" value={r.repo} placeholder={t("set.always.any_repo")} suggest={repoHints} onChange={(v) => set(i, "repo", v)} />
          <span class="muted small">{t("set.always.and")}</span>
          <RuleField kind="login" value={r.author} placeholder={t("set.always.any_author")} suggest={loginHints} onChange={(v) => set(i, "author", v)} />
          <button class="icon-btn" aria-label={t("btn.remove")} onClick={() => edit((c) => { c.approve.always!.splice(i, 1); })}><Icon name="trash" /></button>
        </div>
      ))}
      {rules.length === 0 && <p class="muted small">{t("set.always.none")}</p>}
      <Button small icon="plus" onClick={() => edit((c) => { (c.approve.always ??= []).push({ repo: "", author: "" }); })}>{t("set.always.add")}</Button>
    </div>
  );
}

/** The update check: where it stands, a button to check now, and to install when there is something newer. */
function UpdateRow() {
  const { toast } = useStore();
  const [u, setU] = useUpdate();
  const [busy, setBusy] = useState(false);
  const help = !u ? "" : u.available ? t("up.available", { v: u.available })
    : u.error ? t("up.failed") : u.checked_at ? t("up.latest", { t: ago(u.checked_at) }) : t("up.not_checked");
  const check = async () => {
    setBusy(true);
    try { const s = await api.checkUpdate(); setU(s); if (s.error) toast(t("up.failed"), "bad"); }
    finally { setBusy(false); }
  };
  const install = async () => {
    setBusy(true);
    try { await api.installUpdate(); } catch (e) { toast(String(e), "bad"); } finally { setBusy(false); }
  };
  return (
    <Row label={t("up.title")} help={help}>
      {u?.available
        ? <Button kind="primary" icon="download" busy={busy || u.installing} onClick={install}>{t("up.install")}</Button>
        : <Button icon="refresh" busy={busy} onClick={check}>{t("up.check")}</Button>}
    </Row>
  );
}

export function SettingsPage() {
  const { draft, edit, ov, loaded, refresh, toast } = useStore();
  const [theme, setThemeState] = useState<Theme>(getTheme());
  // one group at a time: the page stays short and each group is one click away
  const [tab, setTab] = useState("general");
  // names Nod has seen in its reviews, offered while typing
  const seenRepos = [...new Set(ov.rows.map((r) => r.key.split("#")[0]))];
  const repoHints = [...new Set([...seenRepos.map((r) => `${r.split("/")[0]}/*`), ...seenRepos, "@me/*"])];
  const loginHints = [...new Set([...ov.rows.map((r) => r.author).filter(Boolean), "@me", "dependabot[bot]", "renovate[bot]"])];
  const num = (path: (c: typeof draft) => number, set: (c: typeof draft, v: number) => void) =>
    <TextInput type="number" value={path(draft)} onInput={(v) => edit((c) => set(c, Number(v)))} />;
  return (
    <div class="page narrow tabbed">
      <header class="page-head"><h1>{t("set.title")}</h1></header>
      <Segmented value={tab} onChange={setTab} options={[
        ["general", t("set.general")],
        ["review", t("set.review")], ["about", t("set.about")],
      ]} />
      {loaded.error && (
        <Note tone="bad"><b>{t("set.broken")}</b><br />{t("set.broken_hint")}
          <details><summary>{t("status.detail")}</summary><code class="small">{loaded.error}</code></details></Note>
      )}

      {tab === "general" && <Section title={t("set.general")}>
        <Card>
          <Row label={t("set.language")} help={t("set.language_help")}>
            <Select value={draft.language} options={[["zh-CN", "简体中文"], ["en", "English"]]}
              onChange={(v) => { setLang(v); edit((c) => { c.language = v; }); }} />
          </Row>
          <Row label={t("set.theme")}>
            <Segmented value={theme} onChange={(v) => { setTheme(v as Theme); setThemeState(v as Theme); }}
              options={[["system", t("set.theme.system")], ["light", t("set.theme.light")], ["dark", t("set.theme.dark")]]} />
          </Row>
          <Row label={t("set.autostart")} help={t("set.autostart_help")}>
            <Switch on={ov.autostart} label={t("set.autostart")} onChange={async (v) => {
              try { await api.autostart(v); } catch (e) { toast(String(e), "bad"); }
              await refresh();
            }} />
          </Row>
        </Card>
      </Section>}

      {/* What happens to a PR, in order: whether it is reviewed (AI review and comments), then whether an approval
          is added on top. Two rule sets, one page. */}
      {tab === "review" && <Section title={t("set.scope")} help={t("set.scope_help")}>
        <Card>
          <Lists verb="review" repoHints={repoHints} loginHints={loginHints} value={{
            repos: draft.scope.repos, authors: draft.scope.authors ?? [],
            exclude_repos: draft.scope.exclude_repos, exclude_authors: draft.scope.exclude_authors ?? [],
          }} onChange={(k, v) => edit((c) => { c.scope[k] = v; })} />
          <Row label={t("set.scope.team")} help={t("set.scope.team_help")}>
            <Switch on={draft.scope.team_requests} label={t("set.scope.team")} onChange={(v) => edit((c) => { c.scope.team_requests = v; })} /></Row>
        </Card>
      </Section>}

      {tab === "review" && <Section title={t("set.approve")} help={t("set.approve_help")}>
        <Card>
          <Row label={t("set.approve.on")} help={t("set.approve.on_help")}>
            <Switch on={draft.approve.mode !== "never"} label={t("set.approve.on")}
              onChange={(v) => edit((c) => { c.approve.mode = v ? "auto" : "never"; })} />
          </Row>
          {draft.approve.mode !== "never" && <>
            <ApproveRules />
            <Lists verb="approve" repoHints={repoHints} loginHints={loginHints} value={draft.approve}
              onChange={(k, v) => edit((c) => { c.approve[k] = v; })} />
            <Row label={t("set.ci_wait")}>{num((c) => c.review.ci_wait_max_min, (c, v) => { c.review.ci_wait_max_min = v; })}</Row>
            <Row label={t("set.ci_grace")} help={t("set.ci_grace_help")}>{num((c) => c.review.ci_none_grace_min, (c, v) => { c.review.ci_none_grace_min = v; })}</Row>
          </>}
        </Card>
      </Section>}

      {tab === "review" && <Section title={t("set.always")} help={t("set.always_help")}>
        <Card>
          {/* what "always" gives up, one per line, in the same shape as the approval rules above */}
          <div class="approve-rules always-note">
            <div class="approve-rules-title">{t("set.always.title")}</div>
            <ul>{(["set.always.findings", "set.always.ci", "set.always.failed", "set.always.switch"] as const).map((k) =>
              <li><Icon name="alert" size={13} />{t(k)}</li>)}
              {/* what it still never does (review::always_approve): own PRs, drafts, closed PRs */}
              <li class="still"><Icon name="info" size={13} />{t("set.always.still")}</li></ul>
          </div>
          <AlwaysRules repoHints={repoHints} loginHints={loginHints} />
        </Card>
      </Section>}


      {tab === "review" && <Section title={t("set.footer")}>
        <Card>
          <Row label={t("set.footer")} help={t("set.footer_help")}>
            <Switch on={draft.review.footer_enabled !== false} label={t("set.footer")}
              onChange={(v) => edit((c) => { c.review.footer_enabled = v; })} />
          </Row>
          {draft.review.footer_enabled !== false && (
            <Row label={t("set.footer_text")} help={t("set.footer_text_help")} wide>
              <TextInput value={draft.review.footer ?? ""} placeholder={t("set.footer_default")}
                onInput={(v) => edit((c) => { c.review.footer = v; })} />
            </Row>
          )}
        </Card>
      </Section>}

      {/* rarely touched: folded, so the page is about what gets reviewed and approved */}
      {tab === "review" && <details class="section advanced">
        <summary><Icon name="chevron" size={14} /><h2>{t("set.review_run")}</h2><span class="muted">{t("set.review_run_hint")}</span></summary>
        <Card>
          <Row label={t("set.timeout")}>{num((c) => c.review.timeout_min, (c, v) => { c.review.timeout_min = v; })}</Row>
          <Row label={t("set.attempts")}>{num((c) => c.review.max_attempts, (c, v) => { c.review.max_attempts = v; })}</Row>
          <Row label={t("set.rereview")}>{num((c) => c.review.rereview_days, (c, v) => { c.review.rereview_days = v; })}</Row>
          <Row label={t("set.poll")}>{num((c) => c.daemon.poll_seconds, (c, v) => { c.daemon.poll_seconds = v; })}</Row>
          <Row label={t("set.refresh")}>{num((c) => c.daemon.refresh_seconds, (c, v) => { c.daemon.refresh_seconds = v; })}</Row>
        </Card>
      </details>}

      {tab === "about" && <>
      <Section title={t("set.files")}>
        <Card class="row-buttons">
          <Button icon="folder" onClick={() => api.open("config")}>{t("set.open_config")}</Button>
          <Button icon="folder" onClick={() => api.open("data")}>{t("set.open_data")}</Button>
          <Button icon="file" onClick={() => api.open("log")}>{t("set.open_log")}</Button>
        </Card>
      </Section>

      <Section title={t("set.about")}>
        <Card>
          <Row label="nod" help={t("set.version", { v: ov.version })}>
            <Button kind="quiet" icon="external" onClick={() => api.url("https://github.com/larrygogo/nod")}>GitHub</Button>
          </Row>
          <UpdateRow />
        </Card>
      </Section>
      </>}
    </div>
  );
}

type ListKey = "repos" | "authors" | "exclude_repos" | "exclude_authors";

/** "a、b 等 3 个仓库" / "a, b and 1 more": a list short enough for one sentence. Chinese counts all of them, English
 *  the rest. */
function few(xs: string[], kind: "repo" | "login") {
  const head = xs.slice(0, 2).join(t("set.sum.sep"));
  return xs.length > 2 ? t(kind === "repo" ? "set.sum.more_repos" : "set.sum.more_authors", { x: head, n: xs.length, rest: xs.length - 2 }) : head;
}
/** No space between a Chinese word and the particle after it ("3 个仓库里"), one after a name ("my-org/* 里"). */
const tidy = (x: string) => x.replace(/([一-鿿]) (里|提)/g, "$1$2");

/** The four lists that narrow a rule (which PRs get reviewed, which get approved), read out as one sentence first:
 *  four empty fields say nothing until you work out that empty means "everything". Then the fields themselves, as a
 *  grid of repository/author by only/never, so the two that pair up sit side by side. */
function Lists({ verb, value, onChange, repoHints, loginHints }: {
  verb: "review" | "approve"; value: Record<ListKey, string[]>; onChange: (k: ListKey, v: string[]) => void;
  repoHints: string[]; loginHints: string[];
}) {
  const { repos, authors, exclude_repos: xr, exclude_authors: xa } = value;
  const what = [repos.length ? t("set.sum.in_only", { x: few(repos, "repo") }) : "", authors.length ? t("set.sum.by_only", { x: few(authors, "login") }) : ""]
    .filter(Boolean).join(t("set.sum.join"));
  const skip = [xr.length ? t("set.sum.in", { x: few(xr, "repo") }) : "", xa.length ? t("set.sum.by", { x: few(xa, "login") }) : ""]
    .filter(Boolean).join(t("set.sum.or"));
  const line = tidy(t(what ? `set.sum.${verb}` : `set.sum.${verb}_all`, { what, skip: skip ? t("set.sum.skip", { x: skip }) : "" }));
  const field = (k: ListKey, kind: "repo" | "login", placeholder: string) => (
    <TagInput kind={kind} suggest={kind === "repo" ? repoHints : loginHints} value={value[k]} placeholder={placeholder}
      onChange={(v) => onChange(k, v)} />
  );
  return (
    <div class="lists">
      <p class="lists-sum">{line}</p>
      <div class="lists-grid">
        <span /><span class="muted small">{t("set.list.repo")}</span><span class="muted small">{t("set.list.author")}</span>
        <span class="lists-label">{t(`set.list.only_${verb}`)}</span>
        {field("repos", "repo", t("set.list.any_repo"))}{field("authors", "login", t("set.list.any_author"))}
        <span class="lists-label">{t(`set.list.never_${verb}`)}</span>
        {field("exclude_repos", "repo", t("set.list.none"))}{field("exclude_authors", "login", t("set.list.none"))}
      </div>
      <p class="muted small">{t("set.list_help")}</p>
    </div>
  );
}

/** The fixed approval rules (review::decide in src-tauri/src/review.rs), one per line, so they can be counted at a glance. */
function ApproveRules() {
  const rules = ["set.rule.high", "set.rule.medium", "set.rule.engine", "set.rule.ci", "set.rule.own", "set.rule.lists"] as const;
  return (
    <div class="approve-rules">
      <div class="approve-rules-title">{t("set.rule.title")}</div>
      <ul>{rules.map((k) => <li><Icon name="check" size={13} />{t(k)}</li>)}</ul>
    </div>
  );
}
