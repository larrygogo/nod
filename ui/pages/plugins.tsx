// Plugins: a list of everything that can be plugged in on the left (code-host accounts, review engines, notification
// channels, third-party plugins), the selected one on the right. Every form is built from the plugin's own field list
// (catalog), so a third-party plugin gets the same settings UI as a built-in one.
import { useEffect, useRef, useState } from "preact/hooks";
import { accounts, activeChannels, api, getPath, setPath, type Account, type Check, type Config, type DeviceCode, type Field, type GithubSession, type Plugin } from "../api";
import { usePluginHealth, healthOf } from "../health";
import { useStore } from "../app";
import { Icon, Tile } from "../icons";
import { t } from "../i18n";
import { Badge, Button, Empty, Note, SecretInput, Select, Switch, TextInput, errText, isAddress } from "../ui";

// ------------------------------------------------------------------ shared pieces

/** One field of a plugin's settings. `secretName` is the keychain entry (to show "saved" instead of a value). */
function FieldInput({ f, value, onChange, secretName }: {
  f: Field; value: unknown; onChange: (v: unknown) => void; secretName?: string;
}) {
  const { loaded, cleared, setCleared } = useStore();
  if (f.kind === "bool") return <Switch on={!!value} onChange={onChange} label={f.label} />;
  if (f.kind === "select") return <Select value={String(value ?? "")} options={f.options} onChange={onChange} />;
  const saved = !!(f.secret && secretName && loaded.secrets[secretName]);
  const clearing = !!secretName && cleared.includes(secretName);
  const placeholder = clearing ? t("pl.will_clear") : saved ? t("pl.saved_secret") : f.placeholder;
  const typed = (v: string) => { if (clearing) setCleared(secretName!, false); onChange(f.kind === "number" ? Number(v) : v); };
  const input = f.kind === "password"
    ? <SecretInput value={String(value ?? "")} placeholder={placeholder} onInput={typed} address={isAddress(f)} />
    : <TextInput type={f.kind === "number" ? "number" : "text"} mono={f.kind !== "number"}
      value={(value as string | number) ?? ""} placeholder={placeholder} onInput={typed} />;
  if (!saved) return input;
  return (
    <div class="secret-row">
      {input}
      <button class="link small" onClick={() => setCleared(secretName!, !clearing)}>{clearing ? t("pl.keep_secret") : t("pl.clear_secret")}</button>
    </div>
  );
}

function Fields({ fields, get, set, secretName }: {
  fields: Field[]; get: (k: string) => unknown; set: (k: string, v: unknown) => void; secretName: (k: string) => string;
}) {
  if (!fields.length) return null;
  return (
    <div class="fields">
      {fields.map((f) => (
        <label class={`field${f.kind === "bool" ? " inline" : ""}`}>
          <span class="field-label">{f.label}</span>
          <FieldInput f={f} value={get(f.key)} onChange={(v) => set(f.key, v)} secretName={secretName(f.key)} />
          {f.help && <span class="muted small">{f.help}</span>}
        </label>
      ))}
    </div>
  );
}

function Checks({ checks }: { checks: Check[] }) {
  return (
    <ul class="checks">
      {checks.map((c) => (
        <li class={c.ok ? (c.warn ? "warn" : "ok") : "fail"}>
          <span class="mark"><Icon name={c.ok ? (c.warn ? "alert" : "check") : "x"} size={12} /></span>
          <div class="grow">
            <div><b>{c.name}</b> <span class="muted">{c.detail}</span></div>
            {c.fix && <div class="muted small">{c.fix}</div>}
          </div>
          {c.action && c.action !== "plugins" && <Button small onClick={() => api.fix(c.action!)}>{t("btn.fix")}</Button>}
        </li>
      ))}
    </ul>
  );
}

/** The right-hand pane for one plugin: what it is, what it can do, its settings and its self-test. */
function Pane({ p, sub, control, children, onCheck, testLabel, extra, account, health }: {
  p: Plugin; sub?: string; control?: preact.ComponentChildren; children?: preact.ComponentChildren; account?: preact.ComponentChildren;
  onCheck?: () => Promise<Check[] | string>; testLabel?: string; extra?: preact.ComponentChildren;
  /** what the page found when it opened (saved settings, no model call), shown until the user runs a check */
  health?: Check[];
}) {
  const [ran, setResult] = useState<Check[] | string | null>(null);
  const result = ran ?? health ?? null;
  // something wrong is the first thing to read, above the settings that fix it; a clean result stays at the bottom
  const trouble = Array.isArray(result) && result.some((c) => !c.ok || c.warn);
  const [err, setErr] = useState<string | null>(null);
  return (
    <article class="pane">
      <header class="pane-head">
        <Tile icon={p.icon} size={40} />
        <div class="grow">
          <h2>{p.name} {!p.builtin && <Badge tone="info">{t("pl.third")}</Badge>}</h2>
          {sub && <div class="muted small">{sub}</div>}
        </div>
        {control && <div class="pane-control">{control}</div>}
      </header>
      <p class="pane-desc">{p.description}</p>
      {p.badges.length > 0 && <div class="caps">{p.badges.map((b) => <Badge tone={b.tone === "warn" ? "warn" : "cap"}>{b.text}</Badge>)}</div>}
      {trouble && <section class="pane-section"><Checks checks={result as Check[]} /></section>}
      {account && <section class="pane-section"><h3>{t("gh.account")}</h3>{account}</section>}
      <section class="pane-section">
        <h3>{t("pl.settings")}</h3>
        {children || <p class="muted">{t("pl.no_settings")}</p>}
      </section>
      {(onCheck || p.help_url) && (
        <section class="pane-section">
          <div class="pane-actions">
            {onCheck && <Button icon="check" onClick={async () => {
              setErr(null); setResult(null);
              try { setResult(await onCheck()); } catch (e) { setErr(errText(e)); }
            }}>{testLabel ?? t("pl.self_test")}</Button>}
            {p.help_url && <Button kind="quiet" icon="external" onClick={() => api.url(p.help_url)}>{t("btn.docs")}</Button>}
          </div>
          {err && <Note tone="bad">{err}</Note>}
          {typeof result === "string" && <Note tone="good">{t("btn.sent")}</Note>}
          {Array.isArray(result) && !trouble && <Checks checks={result} />}
        </section>
      )}
      {extra}
    </article>
  );
}

/** One line in the left-hand list. */
function Item({ icon, name, sub, state, selected, onClick }: {
  icon: string; name: string; sub?: string; state?: preact.ComponentChildren; selected: boolean; onClick: () => void;
}) {
  return (
    <button class={`pl-item${selected ? " selected" : ""}`} onClick={onClick} aria-current={selected}>
      <Tile icon={icon} size={24} />
      <span class="grow">
        <span class="pl-name ellipsis">{name}</span>
        {sub && <span class="pl-sub ellipsis">{sub}</span>}
      </span>
      {state}
    </button>
  );
}

const On = ({ on }: { on: boolean }) => <span class={`onoff${on ? " on" : ""}`}>{on ? t("pl.enabled") : t("pl.disabled")}</span>;

function Group({ title, help, action, children }: {
  title: string; help?: string; action?: preact.ComponentChildren; children: preact.ComponentChildren;
}) {
  return (
    <section class="pl-group">
      <header><span>{title}</span>{action}</header>
      {help && <p class="pl-help">{help}</p>}
      {children}
    </section>
  );
}

/** A group's items with the ones not in use folded behind one line ("6 more"), so the list shows what is set up.
 *  The selected item always shows; with nothing in use, everything shows, to pick from. */
function Folded({ items, more }: { items: { key: string; inUse: boolean; selected: boolean; node: preact.ComponentChildren }[]; more: string }) {
  const [open, setOpen] = useState(false);
  const visible = items.filter((x) => x.inUse || x.selected);
  if (open || visible.length === 0 || visible.length === items.length) return <>{items.map((x) => x.node)}</>;
  const rest = items.length - visible.length;
  return (
    <>
      {visible.map((x) => x.node)}
      <button class="pl-more" onClick={() => setOpen(true)}><Icon name="chevron" size={13} />{more.replace("{n}", String(rest))}</button>
    </>
  );
}

/** Keychain name of a field: built-in fields use their config path; third-party plugin fields
 *  (`plugins.<name>.<key>`) are stored as `plugin.<name>.<key>`. */
const pluginSecretName = (k: string) => (k.startsWith("plugins.") ? "plugin." + k.slice("plugins.".length) : k);

/** Older configurations have no [[forges]] (one implicit GitHub account): make it explicit before editing. */
function materialize(c: Config) {
  if (!c.forges || c.forges.length === 0) c.forges = accounts(c).map((a) => ({ ...a }));
}

function setChannels(c: Config, list: string[]) {
  c.notify.channels = list;
  c.notify.channel = "none"; // the list replaces the legacy single channel
}

const hostOf = (a: Account) => a.url || (a.kind === "github" ? "github.com" : a.kind === "gitee" ? "gitee.com" : "");

// ------------------------------------------------------------------ page

export function PluginsPage() {
  const { draft, edit, catalog, cleared, loaded } = useStore();
  const forgeKinds = catalog.filter((p) => p.kind === "forge");
  const engines = catalog.filter((p) => p.kind === "engine");
  const channels = catalog.filter((p) => p.kind === "notify");
  const third = catalog.filter((p) => !p.builtin);
  const list = accounts(draft);
  const on = activeChannels(draft);
  const rules = draft.review.per_repo ?? [];
  const [sel, setSel] = useState<string>(() => `forge:${list[0]?.id ?? ""}`);
  const [adding, setAdding] = useState(false);
  // narrow windows show either the list or the pane
  const [showPane, setShowPane] = useState(false);
  const pick = (id: string) => { setAdding(false); setSel(id); setShowPane(true); };

  // Everything in use is checked when the page opens, with the saved settings and without calling a model: the list
  // then says which one is broken, instead of every line reading "On" until you think of pressing Self-test.
  const health = usePluginHealth(loaded.config);
  // the overview's "Claude Code is not working" opens this page on that plugin
  const { review, go } = useStore();
  useEffect(() => {
    if (review?.startsWith("__plugin:")) { pick(review.slice(9)); go("plugins", null as unknown as string); }
  }, [review]);
  // a problem replaces the line's usual state word; a working one keeps it
  const flag = (k: string, normal?: preact.ComponentChildren) => {
    const h = healthOf(health[k]);
    return h ? <span class={`onoff ${h}`}>{t(h === "bad" ? "pl.broken" : "pl.attention")}</span> : normal;
  };

  // the selection may point at an account that was just removed: fall back to the first one
  const current = sel.startsWith("forge:") && !list.some((a) => `forge:${a.id}` === sel) ? `forge:${list[0]?.id}` : sel;
  const name = (id: string) => catalog.find((p) => p.id === id)?.name ?? id;

  let pane: preact.ComponentChildren = null;
  if (adding) {
    pane = <AddAccount kinds={forgeKinds} taken={list.map((a) => a.id)} onDone={(id) => { setAdding(false); if (id) setSel(`forge:${id}`); }} />;
  } else if (current.startsWith("forge:")) {
    const i = list.findIndex((a) => `forge:${a.id}` === current);
    const a = list[i];
    const p = a && forgeKinds.find((k) => k.id === a.kind);
    pane = !a ? null : !p ? <Note tone="bad">{a.id}: {a.kind}</Note> : (
      <Pane key={`${a.id}|${a.kind}`} p={p} sub={`${a.id}${hostOf(a) ? " · " + hostOf(a) : ""}`}
        control={<Switch on={a.enabled} label={t("pl.enabled")} onChange={(v) => edit((c) => { materialize(c); c.forges![i].enabled = v; })} />}
        onCheck={() => api.checkPlugin(draft, "forge", a.id, cleared)} health={health[`forge:${a.id}`]}
        account={a.kind === "github" ? <GithubSignIn account={a} usingToken={!!a.token || (!!loaded.secrets[`forge.${a.id}.token`] && !cleared.includes(`forge.${a.id}.token`))} /> : undefined}
        extra={list.length > 1 && <section class="pane-section"><Button kind="danger" icon="trash" onClick={() => edit((c) => { materialize(c); c.forges!.splice(i, 1); })}>{t("pl.remove_account")}</Button></section>}>
        {p.fields.length > 0 && <Fields fields={p.fields}
          get={(k) => getPath(a, k)}
          set={(k, v) => edit((c) => { materialize(c); setPath(c.forges![i], k, v); })}
          secretName={(k) => (k === "token" ? `forge.${a.id}.token` : k.startsWith("settings.") ? `forge.${a.id}.${k}` : "")} />}
      </Pane>
    );
  } else if (current.startsWith("engine:")) {
    const p = engines.find((e) => `engine:${e.id}` === current);
    const isDefault = draft.review.engine === p?.id;
    const isFallback = !isDefault && draft.review.fallback === p?.id;
    pane = p && (
      <Pane key={p.id} p={p} sub={isFallback ? t("pl.fallback_help") : undefined}
        control={isDefault ? <Badge tone="good">{t("pl.default")}</Badge> : <>
          <Button kind="quiet" title={t("pl.fallback_help")}
            onClick={() => edit((c) => { c.review.fallback = isFallback ? "" : p.id; })}>{t(isFallback ? "pl.no_fallback" : "pl.use_fallback")}</Button>
          <Button onClick={() => edit((c) => { c.review.engine = p.id; if (c.review.fallback === p.id) c.review.fallback = ""; })}>{t("pl.use_default")}</Button>
        </>}
        onCheck={() => api.checkPlugin(draft, "engine", p.id, cleared)} health={health[`engine:${p.id}`]}>
        {p.fields.length > 0 && <Fields fields={p.fields} get={(k) => getPath(draft, k)} set={(k, v) => edit((c) => setPath(c, k, v))} secretName={pluginSecretName} />}
      </Pane>
    );
  } else if (current === "rules") {
    pane = <Rules engines={engines} />;
  } else if (current.startsWith("notify:")) {
    const p = channels.find((e) => `notify:${e.id}` === current);
    pane = p && (
      <Pane key={p.id} p={p}
        control={<Switch on={on.includes(p.id)} label={p.name} onChange={(v) => edit((c) => {
          const cur = activeChannels(c).filter((x) => x !== p.id);
          setChannels(c, v ? [...cur, p.id] : cur);
        })} />}
        testLabel={t("btn.test")}
        onCheck={() => api.testChannel(draft, p.id, cleared)}>
        {p.fields.length > 0 && <Fields fields={p.fields} get={(k) => getPath(draft, k)} set={(k, v) => edit((c) => setPath(c, k, v))} secretName={pluginSecretName} />}
      </Pane>
    );
  } else if (current === "third") {
    pane = <Third plugins={third} />;
  }

  const isSel = (id: string) => !adding && current === id;
  return (
    <div class={`split plugins${showPane ? " has-sel" : ""}`}>
      <div class="split-list pl-list">
        <header class="page-head"><h1>{t("pl.title")}</h1></header>
        <div class="scroll">
          <Group title={t("pl.forges")} help={t("pl.forges_help")} action={<button class="icon-btn" title={t("pl.add_account")} aria-label={t("pl.add_account")} onClick={() => { setAdding(true); setShowPane(true); }}><Icon name="plus" /></button>}>
            {list.map((a) => {
              const p = forgeKinds.find((k) => k.id === a.kind);
              return <Item key={`${a.id}|${a.kind}`} icon={p?.icon ?? "plugin"} name={p?.name ?? a.kind} sub={`${a.id}${hostOf(a) ? " · " + hostOf(a) : ""}`}
                state={a.enabled ? flag(`forge:${a.id}`, <On on />) : <On on={false} />} selected={isSel(`forge:${a.id}`)} onClick={() => pick(`forge:${a.id}`)} />;
            })}
            {adding && <div class="pl-item selected"><span class="tile-icon small"><Icon name="plus" size={14} /></span><span class="pl-name">{t("pl.add_account")}</span></div>}
          </Group>
          <Group title={t("pl.engines")} help={t("pl.engines_help")}>
            <Folded more={t("pl.more_engines")} items={engines.map((p) => ({
              key: p.id, selected: isSel(`engine:${p.id}`),
              inUse: draft.review.engine === p.id || draft.review.fallback === p.id || rules.some((r) => r.engine === p.id),
              node: <Item key={p.id} icon={p.icon} name={p.name}
                state={flag(`engine:${p.id}`, draft.review.engine === p.id ? <span class="onoff on">{t("pl.default")}</span>
                  : draft.review.fallback === p.id ? <span class="onoff">{t("pl.fallback")}</span> : undefined)}
                selected={isSel(`engine:${p.id}`)} onClick={() => pick(`engine:${p.id}`)} />,
            }))} />
            <Item icon="rules" name={t("pl.per_repo")}
              sub={rules.length ? rules.map((r) => `${r.repo || "…"} → ${name(r.engine)}`).join(", ") : t("pl.rules_none")}
              selected={isSel("rules")} onClick={() => pick("rules")} />
          </Group>
          <Group title={t("pl.channels")} help={t("pl.channels_help")}>
            <Folded more={t("pl.more_channels")} items={channels.map((p) => ({
              key: p.id, selected: isSel(`notify:${p.id}`), inUse: on.includes(p.id),
              node: <Item key={p.id} icon={p.icon} name={p.name} state={<On on={on.includes(p.id)} />}
                selected={isSel(`notify:${p.id}`)} onClick={() => pick(`notify:${p.id}`)} />,
            }))} />
          </Group>
          <Group title={t("pl.third")}>
            <Item icon="plugin" name={t("pl.third")} sub={third.length ? third.map((p) => p.name).join(", ") : t("pl.no_third")}
              state={third.some((p) => p.kind === "invalid") ? <span class="onoff bad">{t("pl.invalid_short")}</span> : undefined}
              selected={isSel("third")} onClick={() => pick("third")} />
          </Group>
        </div>
      </div>
      <div class="split-detail">
        <button class="link back" onClick={() => setShowPane(false)}><Icon name="chevron" size={14} /> {t("pl.title")}</button>
        {pane}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ GitHub sign-in (device flow)

/** Sign in with Nod's OAuth App: show a one-time code, open github.com/login/device, poll until the user approves.
 *  Same flow as GitHub Desktop and the GitHub CLI; nothing to paste back into Nod. */
export function GithubSignIn({ account, usingToken, onSignedIn }: { account: Account; usingToken: boolean; onSignedIn?: () => void }) {
  const [session, setSession] = useState<GithubSession | null>(null);
  const [code, setCode] = useState<DeviceCode | null>(null);
  const [copied, setCopied] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // bumping the run id stops an older polling loop (cancel, a new attempt, or leaving the page)
  const run = useRef(0);
  const key = `${account.id}|${account.url}|${String(account.settings?.client_id ?? "")}`;

  const refresh = () => api.githubSession(account).then(setSession).catch((e) => setErr(errText(e)));
  // a new address or client ID is a different sign-in: stop a pending one and show the code screen no longer
  useEffect(() => { refresh(); return () => { run.current++; setCode(null); setBusy(false); }; }, [key]);

  const copy = (text: string) => navigator.clipboard?.writeText(text).then(() => setCopied(true)).catch(() => {});

  const start = async () => {
    setErr(null); setBusy(true); setCopied(false);
    const me = ++run.current;
    try {
      const d = await api.githubLoginStart(account);
      if (me !== run.current) return;
      setCode(d);
      copy(d.user_code);
      api.url(d.verification_uri);
      let wait = Math.max(d.interval, 1);
      const until = Date.now() + d.expires_in * 1000;
      let failures = 0;
      while (me === run.current && Date.now() < until) {
        await new Promise((r) => setTimeout(r, wait * 1000));
        if (me !== run.current) return;
        let r;
        try { r = await api.githubLoginPoll(account, d.device_code); failures = 0; }
        catch (e) {
          // one dropped request should not throw away a code the user may be typing in right now
          if (++failures < 4) continue;
          if (me === run.current) setErr(t("gh.poll_failed") + errText(e));
          break;
        }
        // cancelled while the request was out: if GitHub finished anyway, the sign-in is stored; show the truth
        if (me !== run.current) { if (r.state === "done") refresh(); return; }
        if (r.state === "pending") continue;
        if (r.state === "slow_down") { wait = Math.max(r.interval, wait + 5); continue; }
        if (r.state === "done") { setSession({ signed_in: true, login: r.login, has_client_id: true }); onSignedIn?.(); break; }
        setErr(r.state === "expired" ? t("gh.expired") : r.state === "denied" ? t("gh.denied") : r.message);
        break;
      }
      if (me === run.current && Date.now() >= until) setErr(t("gh.expired"));
    } catch (e) {
      if (me === run.current) setErr(errText(e));
    } finally {
      if (me === run.current) { setCode(null); setBusy(false); }
    }
  };
  const cancel = () => { run.current++; setCode(null); setBusy(false); };

  if (usingToken && !code) return <Note tone="info">{t("gh.using_token")}</Note>;
  if (!session) return <p class="muted">…</p>;
  if (code) {
    return (
      <div class="gh-device">
        <p>{t("gh.enter_code")}</p>
        <div class="gh-code-row">
          <code class="gh-code">{code.user_code}</code>
          <Button small icon={copied ? "check" : undefined} onClick={() => copy(code.user_code)}>{copied ? t("gh.copied") : t("gh.copy")}</Button>
        </div>
        <p class="muted small">{t("gh.waiting")}</p>
        <div class="pane-actions">
          <Button small icon="external" onClick={() => api.url(code.verification_uri)}>{t("gh.reopen")}</Button>
          <Button small kind="quiet" onClick={cancel}>{t("btn.cancel")}</Button>
        </div>
      </div>
    );
  }
  return (
    <>
      {session.signed_in ? (
        <div class="gh-signed">
          <span class="state-dot good" />
          <span class="grow">{session.login ? <>{t("gh.signed_in_as")} <b>{session.login}</b></> : t("gh.signed_in")}</span>
          <Button small onClick={start} disabled={busy || !session.has_client_id}>{t("gh.switch")}</Button>
          <Button small kind="quiet" onClick={async () => { cancel(); await api.githubLogout(account).catch((e) => setErr(errText(e))); refresh(); }}>{t("gh.sign_out")}</Button>
        </div>
      ) : !session.has_client_id ? (
        <Note tone="warn">{t("gh.no_client_id")}</Note>
      ) : (
        <>
          <Button kind="primary" icon="key" busy={busy} onClick={start}>{t("gh.sign_in")}</Button>
          <p class="muted small">{t("gh.sign_in_help")}</p>
        </>
      )}
      {err && <Note tone="bad">{err}</Note>}
    </>
  );
}

// ------------------------------------------------------------------ panes that are not a single plugin

function AddAccount({ kinds, taken, onDone }: { kinds: Plugin[]; taken: string[]; onDone: (id: string | null) => void }) {
  const { edit } = useStore();
  const [kind, setKind] = useState(kinds.find((k) => k.id !== "github")?.id ?? kinds[0]?.id ?? "gitee");
  const [id, setId] = useState("");
  const ok = /^[A-Za-z0-9_-]+$/.test(id) && !taken.includes(id);
  return (
    <article class="pane">
      <header class="pane-head"><span class="tile-icon"><Icon name="plus" size={20} /></span>
        <div class="grow"><h2>{t("pl.add_account")}</h2><div class="muted small">{t("pl.forges_help")}</div></div></header>
      <div class="choices">
        {kinds.map((k) => (
          <button class={`choice${k.id === kind ? " selected" : ""}`} onClick={() => setKind(k.id)} aria-pressed={k.id === kind}>
            <Tile icon={k.icon} size={28} />
            <span class="grow"><span class="choice-name">{k.name}</span><span class="muted small">{k.description}</span></span>
            <span class="pick">{k.id === kind && <Icon name="check" size={12} />}</span>
          </button>
        ))}
      </div>
      <label class="field"><span class="field-label">{t("pl.account_id")}</span>
        <TextInput value={id} onInput={setId} placeholder={kind.replace("ext:", "") + "-work"} mono autoFocus /></label>
      <div class="pane-actions">
        <Button kind="primary" disabled={!ok}
          onClick={() => { edit((c) => { materialize(c); c.forges!.push({ id, kind, url: "", enabled: true, token: "" } as Account); }); onDone(id); }}>
          {t("btn.add")}</Button>
        <Button kind="quiet" onClick={() => onDone(null)}>{t("btn.cancel")}</Button>
      </div>
    </article>
  );
}

function Rules({ engines }: { engines: Plugin[] }) {
  const { draft, edit } = useStore();
  const rules = draft.review.per_repo ?? [];
  return (
    <article class="pane">
      <header class="pane-head"><Tile icon="rules" size={40} />
        <div class="grow"><h2>{t("pl.per_repo")}</h2><div class="muted small">{t("pl.per_repo_help")}</div></div></header>
      <section class="pane-section">
        {rules.length === 0 && <p class="muted">{t("pl.rules_none")}</p>}
        <div class="rules">
          {rules.map((r, i) => (
            <div class="rule">
              <TextInput value={r.repo} mono placeholder={t("pl.repo_pattern")} onInput={(v) => edit((c) => { c.review.per_repo[i].repo = v; })} />
              <Icon name="chevron" />
              <Select value={r.engine} options={engines.map((e) => [e.id, e.name])} onChange={(v) => edit((c) => { c.review.per_repo[i].engine = v; })} />
              <button class="icon-btn" aria-label={t("btn.remove")} onClick={() => edit((c) => { c.review.per_repo.splice(i, 1); })}><Icon name="trash" /></button>
            </div>
          ))}
        </div>
        <div class="pane-actions">
          <Button icon="plus" onClick={() => edit((c) => { (c.review.per_repo ??= []).push({ repo: "", engine: engines.find((e) => e.id !== c.review.engine)?.id ?? "claude" }); })}>{t("pl.add_rule")}</Button>
        </div>
      </section>
    </article>
  );
}

function Third({ plugins }: { plugins: Plugin[] }) {
  return (
    <article class="pane">
      <header class="pane-head"><Tile icon="plugin" size={40} /><div class="grow"><h2>{t("pl.third")}</h2></div>
        <div class="pane-control"><Button icon="folder" onClick={() => api.open("plugins")}>{t("btn.open_folder")}</Button></div></header>
      <p class="pane-desc">{t("pl.third_help")}</p>
      <section class="pane-section">
        {plugins.length === 0 ? <Empty icon="puzzle">{t("pl.no_third")}</Empty> : (
          <div class="stack">
            {plugins.map((p) => p.kind === "invalid"
              ? <Note tone="bad"><b>{p.name}</b> · {t("pl.invalid")}: {p.error}</Note>
              : <div class="third-row"><Tile icon={p.icon} size={24} /><span class="grow">{p.name}</span>
                <span class="muted small">{t(p.kind === "forge" ? "pl.forges" : p.kind === "engine" ? "pl.engines" : "pl.channels")}</span></div>)}
          </div>
        )}
      </section>
    </article>
  );
}
