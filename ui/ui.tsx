// Small building blocks shared by every page.
import type { ComponentChildren, JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { createPortal } from "preact/compat";
import { Icon } from "./icons";
import { t } from "./i18n";

export function Button(p: {
  children: ComponentChildren; onClick?: () => unknown; kind?: "primary" | "ghost" | "danger" | "quiet";
  icon?: string; busy?: boolean; disabled?: boolean; small?: boolean; title?: string;
}) {
  const [running, setRunning] = useState(false);
  const busy = p.busy || running;
  return (
    <button class={`btn ${p.kind ?? "ghost"}${p.small ? " small" : ""}`} disabled={p.disabled || busy} title={p.title}
      onClick={async () => {
        const r = p.onClick?.();
        if (r instanceof Promise) {
          setRunning(true);
          try { await r; } finally { setRunning(false); }
        }
      }}>
      {busy ? <span class="spinner" /> : p.icon && <Icon name={p.icon} />}
      <span>{p.children}</span>
    </button>
  );
}

export function Badge({ tone = "muted", children }: { tone?: string; children: ComponentChildren }) {
  return <span class={`badge ${tone}`}>{children}</span>;
}

export function Switch({ on, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label?: string }) {
  return (
    <button role="switch" aria-checked={on} aria-label={label} class={`switch${on ? " on" : ""}`} onClick={() => onChange(!on)}>
      <span />
    </button>
  );
}

export function Card({ children, class: cls = "" }: { children: ComponentChildren; class?: string }) {
  return <section class={`card ${cls}`}>{children}</section>;
}

export function Section({ title, help, action, children }: {
  title: string; help?: string; action?: ComponentChildren; children: ComponentChildren;
}) {
  return (
    <section class="section">
      <header>
        <div><h2>{title}</h2>{help && <p class="muted">{help}</p>}</div>
        {action}
      </header>
      {children}
    </section>
  );
}

/** A labelled row: label + help on the left, control on the right (or below for wide controls). */
export function Row({ label, help, children, wide }: {
  label: string; help?: string; children: ComponentChildren; wide?: boolean;
}) {
  return (
    <div class={`row${wide ? " wide" : ""}`}>
      <div class="row-label"><div>{label}</div>{help && <div class="muted small">{help}</div>}</div>
      <div class="row-control">{children}</div>
    </div>
  );
}

export function TextInput(p: {
  value: string | number; onInput: (v: string) => void; placeholder?: string; type?: string; mono?: boolean; autoFocus?: boolean;
}) {
  // the autofocus attribute only counts when the page loads; an input that appears later (a dialog, a rule just
  // added) is focused here
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => { if (p.autoFocus) ref.current?.focus(); }, []);
  return (
    <input ref={ref} class={`input${p.mono ? " mono" : ""}`} type={p.type ?? "text"} value={p.value} placeholder={p.placeholder}
      spellcheck={false}
      onInput={(e) => p.onInput((e.target as HTMLInputElement).value)} />
  );
}

/** A secret that is an address (a robot's webhook URL): shown as typed. */
export function isAddress(f: { key: string; placeholder?: string | null }): boolean {
  return /(^|\.)url$/.test(f.key) || /^https?:\/\//.test(f.placeholder ?? "");
}

/** A secret typed by hand: hidden by default, with an eye to check what was typed or pasted. A secret that is an
 *  address (a robot's webhook URL) is shown as typed: you need to see it to know it is the right one, and it is still
 *  kept in the keychain once saved. */
export function SecretInput({ value, onInput, placeholder, address }: {
  value: string; onInput: (v: string) => void; placeholder?: string; address?: boolean;
}) {
  const [shown, setShown] = useState(false);
  if (address) return <TextInput mono value={value} placeholder={placeholder} onInput={onInput} />;
  return (
    <div class="secret-input">
      <TextInput mono type={shown ? "text" : "password"} value={value} placeholder={placeholder} onInput={onInput} />
      {value !== "" && (
        <button type="button" class="icon-btn" aria-label={t(shown ? "btn.hide" : "btn.show")} title={t(shown ? "btn.hide" : "btn.show")}
          onClick={() => setShown(!shown)}><Icon name={shown ? "eye-off" : "eye"} size={15} /></button>
      )}
    </div>
  );
}

/** A repository pattern as the backend accepts it (config.rs is_repo_pattern): owner/repo, owner/*, any-owner/repo, a lone star,
 *  @me/*. A pasted link becomes owner/repo. Returns null when it is not one. */
export function cleanRepo(raw: string): string | null {
  let s = raw.trim().replace(/\.git$/, "");
  const url = s.match(/^(?:https?:\/\/)?[^/\s]+\.[a-z]{2,}\/([^/\s]+)\/([^/\s#?]+)/i);
  if (url) s = `${url[1]}/${url[2]}`;
  s = s.replace(/#\d+$/, "");
  if (s === "*") return s;
  const parts = s.split("/");
  const ok = (p: string) => p === "*" || p === "@me" || /^[\p{L}\p{N}_.-]+$/u.test(p);
  return parts.length >= 2 && parts.every((p) => p && ok(p)) ? s : null;
}

/** A login pattern (config.rs is_login_pattern): @me, a login with * wildcards, optionally ending in [bot]. A
 *  leading @ and a pasted profile link are dropped. */
export function cleanLogin(raw: string): string | null {
  let s = raw.trim();
  const url = s.match(/^(?:https?:\/\/)?[^/\s]+\.[a-z]{2,}\/([^/\s?#]+)\/?$/i);
  if (url) s = url[1];
  if (s.toLowerCase() === "@me") return "@me";
  s = s.replace(/^@/, "");
  const core = s.endsWith("[bot]") ? s.slice(0, -5) : s;
  return core && /^[A-Za-z0-9*-]+$/.test(core) ? s : null;
}

/** A list edited as chips: type and press Enter (or comma, space, Tab), paste several at once, Backspace removes the
 *  last one. Each entry is checked as it is added, so a typo cannot reach the config; `suggest` offers the names
 *  Nod has already seen. */
export function TagInput({ value, onChange, kind, placeholder, suggest = [] }: {
  value: string[]; onChange: (v: string[]) => void; kind: "repo" | "login"; placeholder?: string; suggest?: string[];
}) {
  const [text, setText] = useState("");
  const [err, setErr] = useState("");
  const [focus, setFocus] = useState(false);
  const [hi, setHi] = useState(-1);
  // Esc hides the menu while the field keeps focus; typing or focusing again brings it back
  const [shut, setShut] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const clean = kind === "repo" ? cleanRepo : cleanLogin;
  const has = (x: string) => value.some((v) => v.toLowerCase() === x.toLowerCase());
  const q = text.trim().toLowerCase();
  const focused = focus || (typeof document !== "undefined" && document.activeElement === input.current);
  const options = focused && !shut ? suggest.filter((x) => !has(x) && (!q || x.toLowerCase().includes(q))).slice(0, 6) : [];

  const add = (raw: string) => {
    const items = raw.split(/[\s,，;；]+/).filter(Boolean);
    const next = [...value];
    const bad: string[] = [];
    for (const it of items) {
      const c = clean(it);
      if (!c) bad.push(it);
      else if (!next.some((v) => v.toLowerCase() === c.toLowerCase())) next.push(c);
    }
    if (next.length !== value.length) onChange(next);
    setErr(bad.length ? t(kind === "repo" ? "tag.bad_repo" : "tag.bad_login", { x: bad.join(" ") }) : "");
    setText(bad.join(" "));
    setHi(-1);
  };
  const remove = (i: number) => { onChange(value.filter((_, j) => j !== i)); input.current?.focus(); };

  const key = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown" && options.length) { e.preventDefault(); setHi((i) => Math.min(i + 1, options.length - 1)); return; }
    if (e.key === "ArrowUp" && options.length) { e.preventDefault(); setHi((i) => Math.max(i - 1, -1)); return; }
    if (e.key === "Escape") { setHi(-1); setShut(true); return; }
    if (e.key === "Enter" || ((e.key === "," || e.key === " " || e.key === "Tab") && text.trim())) {
      if (e.key === "Enter" && hi >= 0 && options[hi]) { e.preventDefault(); add(options[hi]); return; }
      if (!text.trim()) return;
      e.preventDefault();
      add(text);
      return;
    }
    if (e.key === "Backspace" && !text && value.length) { e.preventDefault(); remove(value.length - 1); }
  };

  return (
    <div class="tags-wrap">
      <div class={`input tags${err ? " invalid" : ""}`} onMouseDown={(e) => { if (e.target === e.currentTarget) { e.preventDefault(); input.current?.focus(); } }}>
        {value.map((v, i) => (
          <span class="tag" key={v}>
            <span class="mono">{v}</span>
            <button type="button" aria-label={t("btn.remove")} onClick={() => remove(i)}><Icon name="x" size={12} /></button>
          </span>
        ))}
        <input ref={input} class="mono" value={text} spellcheck={false} placeholder={value.length ? "" : placeholder}
          onInput={(e) => { setText((e.target as HTMLInputElement).value); setErr(""); setHi(-1); setShut(false); }}
          onKeyDown={key}
          onPaste={(e) => { const d = e.clipboardData?.getData("text") ?? ""; if (/[\s,，;；]/.test(d.trim())) { e.preventDefault(); add(text + d); } }}
          onFocus={() => { setFocus(true); setShut(false); }}
          onBlur={() => { setFocus(false); if (text.trim()) add(text); }} />
      </div>
      {options.length > 0 && (
        <ul class="select-menu tags-menu" role="listbox">
          {options.map((o, i) => (
            <li role="option" aria-selected={false} class={i === hi ? "hi" : ""} onMouseEnter={() => setHi(i)}
              onMouseDown={(e) => { e.preventDefault(); add(o); }}><span class="mono ellipsis">{o}</span></li>
          ))}
        </ul>
      )}
      {err && <div class="field-err">{err}</div>}
    </div>
  );
}

/** A dropdown drawn by the app: the system <select> list cannot be styled in the web view (grey, square, its own
 *  font), so the trigger is a button styled like the inputs and the list is the app's own menu. Keyboard: arrows,
 *  Home/End, Enter/Space to pick, Escape to close. The menu is fixed-positioned so panes that clip their overflow
 *  do not cut it off, and opens upwards when there is no room below. */
export function Select({ value, options, onChange }: { value: string; options: [string, string][]; onChange: (v: string) => void }) {
  const [open, setOpen] = useState(false);
  const [hi, setHi] = useState(0);
  const [pos, setPos] = useState({ left: 0, top: 0, width: 0 });
  const wrap = useRef<HTMLDivElement>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const current = options.findIndex(([v]) => v === value);

  const show = () => {
    const r = btn.current!.getBoundingClientRect();
    const h = Math.min(options.length * 32 + 10, 280);
    const up = r.bottom + 4 + h > window.innerHeight - 8 && r.top - 4 - h > 8;
    setPos({ left: r.left, width: r.width, top: up ? r.top - 4 - h : r.bottom + 4 });
    setHi(Math.max(current, 0));
    setOpen(true);
  };
  const pick = (i: number) => {
    setOpen(false);
    btn.current?.focus();
    if (options[i] && options[i][0] !== value) onChange(options[i][0]);
  };

  useEffect(() => {
    if (!open) return;
    const outside = (e: Event) => { if (!wrap.current?.contains(e.target as Node)) setOpen(false); };
    // the menu is fixed in place: anything that moves the trigger closes it
    const away = (e: Event) => { if (!(e.target as Element)?.closest?.(".select-menu")) setOpen(false); };
    const resize = () => setOpen(false);
    document.addEventListener("mousedown", outside, true);
    document.addEventListener("scroll", away, true);
    window.addEventListener("resize", resize);
    return () => {
      document.removeEventListener("mousedown", outside, true);
      document.removeEventListener("scroll", away, true);
      window.removeEventListener("resize", resize);
    };
  }, [open]);

  const key = (e: KeyboardEvent) => {
    const last = options.length - 1;
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) { e.preventDefault(); show(); }
      return;
    }
    const go: Record<string, () => void> = {
      ArrowDown: () => setHi((i) => Math.min(i + 1, last)),
      ArrowUp: () => setHi((i) => Math.max(i - 1, 0)),
      Home: () => setHi(0),
      End: () => setHi(last),
      Enter: () => pick(hi),
      " ": () => pick(hi),
      Escape: () => setOpen(false),
      Tab: () => setOpen(false),
    };
    if (go[e.key]) { if (e.key !== "Tab") e.preventDefault(); go[e.key](); }
  };

  return (
    <div class="select" ref={wrap}>
      <button type="button" ref={btn} class="input select-trigger" aria-haspopup="listbox" aria-expanded={open}
        onClick={() => (open ? setOpen(false) : show())} onKeyDown={key}>
        <span class="ellipsis">{current >= 0 ? options[current][1] : value}</span>
        <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 4.5L6 7.5L9 4.5" /></svg>
      </button>
      {open && (
        <ul class="select-menu" role="listbox" style={{ left: pos.left, top: pos.top, minWidth: pos.width }}>
          {options.map(([v, l], i) => (
            <li role="option" aria-selected={v === value} class={i === hi ? "hi" : ""}
              onMouseEnter={() => setHi(i)} onMouseDown={(e) => e.preventDefault()} onClick={() => pick(i)}>
              <span class="ellipsis">{l}</span>
              {v === value && <Icon name="check" size={14} />}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function Segmented({ value, options, onChange }: { value: string; options: [string, string][]; onChange: (v: string) => void }) {
  return (
    <div class="segmented" role="tablist">
      {options.map(([v, l]) => (
        <button role="tab" aria-selected={v === value} class={v === value ? "on" : ""} onClick={() => onChange(v)}>{l}</button>
      ))}
    </div>
  );
}

export function Empty({ icon = "info", children, action }: { icon?: string; children: ComponentChildren; action?: ComponentChildren }) {
  return <div class="empty"><Icon name={icon} size={20} /><p>{children}</p>{action}</div>;
}

/** An inline message: result of an action, with its tone. */
export function Note({ tone = "info", children }: { tone?: "info" | "good" | "warn" | "bad"; children: ComponentChildren }) {
  const icon = { info: "info", good: "check", warn: "alert", bad: "alert" }[tone];
  return <div class={`note ${tone}`}><Icon name={icon} /><div>{children}</div></div>;
}

export function Dialog({ title, onClose, children, footer }: {
  title: string; onClose: () => void; children: ComponentChildren; footer?: ComponentChildren;
}) {
  // drawn at the page root: a dialog opened inside a pane must not pick up the pane's styles or be clipped by it
  return createPortal(
    <div class="overlay" onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      onKeyDown={(e) => { if (e.key === "Escape") onClose(); }}>
      <div class="dialog" role="dialog" aria-label={title}>
        <header><h2>{title}</h2><button class="icon-btn" aria-label={t("btn.close")} onClick={() => onClose()}><Icon name="x" /></button></header>
        <div class="dialog-body">{children}</div>
        {footer && <footer>{footer}</footer>}
      </div>
    </div>,
    document.body,
  );
}

export const errText = (e: unknown) => t("err.prefix") + String(e);

export type Child = JSX.Element | string | null | false | undefined;
