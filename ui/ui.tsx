// Small building blocks shared by every page.
import type { ComponentChildren, JSX } from "preact";
import { useState } from "preact/hooks";
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
  return (
    <input class={`input${p.mono ? " mono" : ""}`} type={p.type ?? "text"} value={p.value} placeholder={p.placeholder}
      autoFocus={p.autoFocus} spellcheck={false}
      onInput={(e) => p.onInput((e.target as HTMLInputElement).value)} />
  );
}

export function Lines({ value, onChange, placeholder }: { value: string[]; onChange: (v: string[]) => void; placeholder?: string }) {
  return (
    <textarea class="input mono" rows={Math.max(2, value.length + 1)} placeholder={placeholder} spellcheck={false}
      value={value.join("\n")}
      onInput={(e) => onChange((e.target as HTMLTextAreaElement).value.split(/[\n,]/).map((s) => s.trim()).filter(Boolean))} />
  );
}

export function Select({ value, options, onChange }: { value: string; options: [string, string][]; onChange: (v: string) => void }) {
  return (
    <select class="input" value={value} onChange={(e) => onChange((e.target as HTMLSelectElement).value)}>
      {options.map(([v, l]) => <option value={v}>{l}</option>)}
    </select>
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

export function Empty({ icon = "info", children }: { icon?: string; children: ComponentChildren }) {
  return <div class="empty"><Icon name={icon} size={20} /><p>{children}</p></div>;
}

/** An inline message: result of an action, with its tone. */
export function Note({ tone = "info", children }: { tone?: "info" | "good" | "warn" | "bad"; children: ComponentChildren }) {
  const icon = { info: "info", good: "check", warn: "alert", bad: "alert" }[tone];
  return <div class={`note ${tone}`}><Icon name={icon} /><div>{children}</div></div>;
}

export function Dialog({ title, onClose, children, footer }: {
  title: string; onClose: () => void; children: ComponentChildren; footer?: ComponentChildren;
}) {
  return (
    <div class="overlay" onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      onKeyDown={(e) => { if (e.key === "Escape") onClose(); }}>
      <div class="dialog" role="dialog" aria-label={title}>
        <header><h2>{title}</h2><button class="icon-btn" aria-label={t("btn.close")} onClick={onClose}><Icon name="x" /></button></header>
        <div class="dialog-body">{children}</div>
        {footer && <footer>{footer}</footer>}
      </div>
    </div>
  );
}

export const errText = (e: unknown) => t("err.prefix") + String(e);

export type Child = JSX.Element | string | null | false | undefined;
