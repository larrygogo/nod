// Small building blocks shared by every page.
import type { ComponentChildren, JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
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
