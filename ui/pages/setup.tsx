// First launch: code host → review engine → notifications → done.
import { useEffect, useState } from "preact/hooks";
import { activeChannels, api, getPath, setPath, type Check, type Config, type Plugin } from "../api";
import { useStore } from "../app";
import { Icon, Tile } from "../icons";
import { lang, setLang, t } from "../i18n";
import { Badge, Button, Note, Select, Switch, TextInput, errText } from "../ui";

function Checks({ checks }: { checks: Check[] }) {
  return (
    <ul class="checks">
      {checks.map((c) => (
        <li class={c.ok ? (c.warn ? "warn" : "ok") : "fail"}>
          <span class="mark"><Icon name={c.ok ? (c.warn ? "alert" : "check") : "x"} size={12} /></span>
          <div class="grow"><div><b>{c.name}</b> <span class="muted">{c.detail}</span></div>{c.fix && <div class="muted small">{c.fix}</div>}</div>
          {c.action && c.action !== "plugins" && <Button small onClick={() => api.fix(c.action!)}>{t("btn.fix")}</Button>}
        </li>
      ))}
    </ul>
  );
}

/** One option. When it is picked, its settings open right below it (not at the end of a long list). */
function Choice({ p, selected, onPick, multi, children }: {
  p: Plugin; selected: boolean; onPick: () => void; multi?: boolean; children?: preact.ComponentChildren;
}) {
  return (
    <div class={`choice-wrap${selected ? " selected" : ""}`}>
      <button class={`choice${selected ? " selected" : ""}`} onClick={onPick} aria-pressed={selected}>
        <Tile icon={p.icon} size={28} />
        <span class="grow">
          <span class="choice-name">{p.name}
            {p.badges.map((b) => <Badge tone={b.tone === "warn" ? "warn" : "cap"}>{b.text}</Badge>)}</span>
          <span class="muted small">{p.description}</span>
        </span>
        <span class={`pick${multi ? " square" : ""}`}>{selected && <Icon name="check" size={12} />}</span>
      </button>
      {selected && children && <div class="choice-body">{children}</div>}
    </div>
  );
}

function FieldList({ p, get, set }: { p: Plugin; get: (k: string) => unknown; set: (k: string, v: unknown) => void }) {
  if (!p.fields.length) return null;
  return (
    <div class="fields">
      {p.fields.map((f) => (
        <label class={`field${f.kind === "bool" ? " inline" : ""}`}>
          <span class="field-label">{f.label}</span>
          {f.kind === "bool" ? <Switch on={!!get(f.key)} onChange={(v) => set(f.key, v)} />
            : f.kind === "select" ? <Select value={String(get(f.key) ?? "")} options={f.options} onChange={(v) => set(f.key, v)} />
              : <TextInput type={f.kind === "password" ? "password" : f.kind === "number" ? "number" : "text"} mono
                value={(get(f.key) as string) ?? ""} placeholder={f.placeholder} onInput={(v) => set(f.key, f.kind === "number" ? Number(v) : v)} />}
          {f.help && <span class="muted small">{f.help}</span>}
        </label>
      ))}
    </div>
  );
}

export function Setup() {
  const { draft, edit, catalog, ov, reloadConfig, refresh, toast } = useStore();
  const [step, setStep] = useState(0);
  const [checks, setChecks] = useState<Check[] | null>(null);
  const [checking, setChecking] = useState(false);
  const [msg, setMsg] = useState<{ tone: "good" | "bad" | "info"; text: string } | null>(null);
  const [autostart, setAutostart] = useState(true);

  // a new installation: the window's language, desktop notifications on
  useEffect(() => {
    edit((c) => {
      c.language = lang();
      if (!activeChannels(c).length) { c.notify.channels = ["system"]; c.notify.channel = "none"; }
      if (!c.forges?.length) c.forges = [{ id: "github", kind: "github", url: "", enabled: true, token: "" }];
    });
  }, []);
  const accKind = draft.forges?.[0]?.kind;
  // a result belongs to what was checked: clear it when the step or the choice changes
  useEffect(() => { setChecks(null); setMsg(null); }, [step, accKind, draft.review.engine]);

  const forges = catalog.filter((p) => p.kind === "forge");
  const engines = catalog.filter((p) => p.kind === "engine");
  const channels = catalog.filter((p) => p.kind === "notify");
  const acc = draft.forges?.[0];
  const forge = forges.find((p) => p.id === acc?.kind) ?? forges[0];
  const engine = engines.find((p) => p.id === draft.review.engine) ?? engines[0];

  const runCheck = async (f: () => Promise<Check[]>) => {
    setChecking(true); setChecks(null);
    try { setChecks(await f()); } catch (e) { setMsg({ tone: "bad", text: errText(e) }); } finally { setChecking(false); }
  };
  const allOk = checks?.every((c) => c.ok);

  const steps = [t("ob.s1"), t("ob.s2"), t("ob.s3")];
  return (
    <div class="setup">
      <div class="setup-side">
        <div class="brand big"><img src="./logo.svg" alt="" />Nod</div>
        <h1>{t("ob.welcome")}</h1>
        <p class="muted">{t("ob.intro")}</p>
        <ol class="steps">
          {steps.map((s, i) => <li class={i === step ? "current" : i < step ? "done" : ""}><span>{i < step ? <Icon name="check" size={12} /> : i + 1}</span>{s}</li>)}
        </ol>
        <div class="grow" />
        <Select value={lang()} options={[["zh-CN", "简体中文"], ["en", "English"]]}
          onChange={(v) => { setLang(v); edit((c: Config) => { c.language = v; }); }} />
      </div>

      <div class="setup-main">
        <div class="setup-scroll">
        {step === 0 && <>
          <h2>{t("ob.s1")}</h2><p class="muted">{t("ob.s1_help")}</p>
          {ov.old_install && <Note>{t("ob.old")} <Button small onClick={async () => {
            try { const r = await api.importOld(); toast(r.join(" · ")); await reloadConfig(); await refresh(); }
            catch (e) { setMsg({ tone: "bad", text: errText(e) }); }
          }}>{t("btn.import")}</Button></Note>}
          <div class="choices">
            {forges.map((p) => <Choice p={p} selected={forge?.id === p.id} onPick={() => edit((c) => {
              c.forges = [{ id: p.id.replace("ext:", ""), kind: p.id, url: "", enabled: true, token: "" }];
            })}>{forge?.id === p.id && acc && p.fields.length > 0 && <FieldList p={p} get={(k) => getPath(acc, k)} set={(k, v) => edit((c) => setPath(c.forges![0], k, v))} />}</Choice>)}
          </div>
          <div class="setup-check">
            <Button icon="check" busy={checking} onClick={() => runCheck(() => api.checkPlugin(draft, "forge", acc!.id))}>{t("btn.check")}</Button>
            {!checks && <span class="muted small">{t("ob.check_first")}</span>}
          </div>
        </>}

        {step === 1 && <>
          <h2>{t("ob.s2")}</h2><p class="muted">{t("ob.s2_help")}</p>
          <div class="choices">
            {engines.map((p) => <Choice p={p} selected={engine?.id === p.id} onPick={() => edit((c) => { c.review.engine = p.id; })}>
              {p.fields.length > 0 && <FieldList p={p} get={(k) => getPath(draft, k)} set={(k, v) => edit((c) => setPath(c, k, v))} />}
            </Choice>)}
          </div>
          <div class="setup-check">
            <Button icon="check" busy={checking} onClick={() => runCheck(() => api.checkPlugin(draft, "engine", engine.id))}>{t("btn.check")}</Button>
            {checking ? <span class="muted small">{t("ck.running")}</span> : !checks && <span class="muted small">{t("ob.check_first")}</span>}
          </div>
        </>}

        {step === 2 && <>
          <h2>{t("ob.s3")}</h2><p class="muted">{t("ob.s3_help")}</p>
          <div class="choices">
            {channels.map((p) => {
              const on = activeChannels(draft).includes(p.id);
              return <Choice multi p={p} selected={on} onPick={() => edit((c) => {
                const cur = activeChannels(c).filter((x) => x !== p.id);
                c.notify.channels = on ? cur : [...cur, p.id]; c.notify.channel = "none";
              })}>{p.fields.length > 0 && <FieldList p={p} get={(k) => getPath(draft, k)} set={(k, v) => edit((c) => setPath(c, k, v))} />}</Choice>;
            })}
          </div>
          <div class="setup-check">
            {activeChannels(draft).map((ch) => (
              <Button small icon="bell" onClick={async () => {
                try { await api.testChannel(draft, ch); setMsg({ tone: "good", text: t("btn.sent") }); }
                catch (e) { setMsg({ tone: "bad", text: errText(e) }); }
              }}>{t("btn.test")} · {catalog.find((p) => p.id === ch)?.name ?? ch}</Button>
            ))}
          </div>
          <label class="field inline autostart"><Switch on={autostart} onChange={setAutostart} /><span>{t("ob.autostart")}</span></label>
        </>}

        {checks && <div class={`setup-result ${allOk ? "good" : "bad"}`}>{allOk && <Note tone="good">{t("ck.all_ok")}</Note>}<Checks checks={checks} /></div>}
        {msg && msg.tone !== "bad" && <Note tone={msg.tone}>{msg.text}</Note>}
        </div>

        <footer class="setup-nav">
          {step > 0 && <Button kind="quiet" onClick={() => setStep(step - 1)}>{t("btn.back")}</Button>}
          <div class="grow">{msg?.tone === "bad" && <div class="footer-error"><Icon name="alert" /> <span>{msg.text}</span></div>}</div>
          {step < 2 && <Button kind={allOk ? "primary" : "ghost"} onClick={() => setStep(step + 1)}>{allOk ? t("btn.next") : t("btn.skip")}</Button>}
          {step === 2 && <Button kind="primary" onClick={async () => {
            setMsg(null);
            try {
              await api.save(draft);
              await api.autostart(autostart).catch(() => {});
              await reloadConfig(); await refresh();
            } catch (e) { setMsg({ tone: "bad", text: errText(e) }); }
          }}>{t("ob.finish")}</Button>}
        </footer>
      </div>
    </div>
  );
}
