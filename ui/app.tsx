// The window: sidebar + pages, the shared state (overview, settings draft, plugin catalog) and the save bar.
import { createContext } from "preact";
import { useCallback, useContext, useEffect, useMemo, useState } from "preact/hooks";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, type Config, type LoadedConfig, type Overview, type Plugin, type Status, type UpdateState } from "./api";
import { Icon } from "./icons";
import { ago, setLang, t, type Key } from "./i18n";
import { Button, errText } from "./ui";
import { OverviewPage } from "./pages/overview";
import { ReviewsPage } from "./pages/reviews";
import { PluginsPage } from "./pages/plugins";
import { SettingsPage } from "./pages/settings";
import { Setup } from "./pages/setup";

export type Page = "overview" | "reviews" | "plugins" | "settings";

export interface Store {
  ov: Overview;
  loaded: LoadedConfig;
  draft: Config;
  /** Change the settings draft (shown in the save bar until saved). */
  edit: (f: (c: Config) => void) => void;
  /** Stored secrets (keychain names) the user asked to delete; sent with the next save. */
  cleared: string[];
  setCleared: (name: string, on: boolean) => void;
  catalog: Plugin[];
  page: Page;
  go: (p: Page, review?: string) => void;
  review: string | null;
  refresh: () => Promise<void>;
  reloadConfig: () => Promise<void>;
  toast: (text: string, tone?: "good" | "bad") => void;
}

export const StoreCtx = createContext<Store>(null as unknown as Store);
export const useStore = () => useContext(StoreCtx);

const clone = <T,>(x: T): T => JSON.parse(JSON.stringify(x));

// ------------------------------------------------------------------ theme (a per-window preference)

export type Theme = "system" | "light" | "dark";
export function getTheme(): Theme {
  try { return (localStorage.getItem("theme") as Theme) || "system"; } catch { return "system"; }
}
export function setTheme(v: Theme) {
  try { localStorage.setItem("theme", v); } catch { /* private storage: just apply */ }
  applyTheme(v);
}
function applyTheme(v: Theme) {
  if (v === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", v);
  // the title bar is drawn by the system: give the window the same theme so it does not stay light over a dark page
  try { getCurrentWindow().setTheme(v === "system" ? null : v).catch(() => {}); } catch { /* not in the app (tests, browser) */ }
}
applyTheme(getTheme());

// ------------------------------------------------------------------ app

export function App() {
  const [ov, setOv] = useState<Overview | null>(null);
  const [loaded, setLoaded] = useState<LoadedConfig | null>(null);
  const [draft, setDraft] = useState<Config | null>(null);
  const [catalog, setCatalog] = useState<Plugin[]>([]);
  const [page, setPage] = useState<Page>("overview");
  const [review, setReview] = useState<string | null>(null);
  const [toastMsg, setToast] = useState<{ text: string; tone: string } | null>(null);
  const [fatal, setFatal] = useState<string | null>(null);
  const [cleared, setClearedList] = useState<string[]>([]);

  const refresh = useCallback(async () => setOv(await api.overview()), []);
  const reloadConfig = useCallback(async () => {
    const l = await api.config();
    setLang(l.config.language);
    setLoaded(l);
    setDraft(clone(l.config));
    setClearedList([]);
    setCatalog(await api.catalog());
    if (l.error) setPage("settings");
  }, []);

  useEffect(() => {
    (async () => {
      try {
        await reloadConfig();
        await refresh();
      } catch (e) { setFatal(errText(e)); }
    })();
    const un = listen<Status>("status", (e) => setOv((o) => (o ? { ...o, status: e.payload } : o)));
    const timer = setInterval(() => { if (document.visibilityState === "visible") refresh().catch(() => {}); }, 3000);
    return () => { clearInterval(timer); un.then((f) => f()); };
  }, []);

  const toast = useCallback((text: string, tone: "good" | "bad" = "good") => {
    setToast({ text, tone });
    setTimeout(() => setToast(null), 2500); // success messages leave after 2.5 s
  }, []);

  // plugin names and descriptions follow the language picked in the window, even before it is saved
  const draftLang = draft?.language;
  useEffect(() => { if (draftLang) api.catalog(draftLang).then(setCatalog).catch(() => {}); }, [draftLang]);

  const dirty = useMemo(() => cleared.length > 0
    || (!!(loaded && draft) && JSON.stringify(loaded.config) !== JSON.stringify(draft)), [loaded, draft, cleared]);

  if (fatal) return <div class="fatal"><Icon name="alert" /> {fatal}</div>;
  if (!ov || !loaded || !draft) return <div class="boot"><span class="spinner" /></div>;

  const store: Store = {
    ov, loaded, draft, catalog, page, review, refresh, reloadConfig, toast, cleared,
    setCleared: (name, on) => setClearedList((l) => (on ? [...l.filter((x) => x !== name), name] : l.filter((x) => x !== name))),
    edit: (f) => setDraft((d) => { const c = clone(d!); f(c); return c; }),
    go: (p, r) => { setPage(p); if (r !== undefined) setReview(r); },
  };

  if (!ov.configured) {
    return <StoreCtx.Provider value={store}><div class="frame"><TitleBar /><div class="frame-body"><Setup /></div></div></StoreCtx.Provider>;
  }

  return (
    <StoreCtx.Provider value={store}>
      <div class="frame">
      <TitleBar sidebar />
      <div class="shell">
        <Sidebar />
        <main class="main">
          {page === "overview" && <OverviewPage />}
          {page === "reviews" && <ReviewsPage />}
          {page === "plugins" && <PluginsPage />}
          {page === "settings" && <SettingsPage />}
        </main>
        {dirty && <SaveBar />}
        {toastMsg && <div class={`toast ${toastMsg.tone}`} role="status">{toastMsg.text}</div>}
      </div>
      </div>
    </StoreCtx.Provider>
  );
}

/** Which title bar the window has (see show_main in app.rs): our own on Windows, ours with the system traffic
 *  lights on macOS, the native one on Linux. */
const platform: "mac" | "windows" | "linux" = /Mac/.test(navigator.userAgent) ? "mac"
  : /Windows/.test(navigator.userAgent) ? "windows" : "linux";
const isMac = platform === "mac";
/** Linux keeps the native title bar, so the window draws none and the logo stays in the sidebar. */
const ownTitleBar = platform !== "linux";

/** The window's own title bar: the system frame is off (see show_main in app.rs). Its empty parts move the window
 *  and maximize on double-click; on Windows and Linux it carries the window buttons, on macOS it leaves room for the
 *  traffic lights. With `sidebar`, the left part continues the sidebar so the two read as one column. */
function TitleBar({ sidebar = false }: { sidebar?: boolean }) {
  const [max, setMax] = useState(false);
  useEffect(() => {
    if (!ownTitleBar) return;
    const w = getCurrentWindow();
    const sync = () => w.isMaximized().then(setMax).catch(() => {});
    sync();
    let off: (() => void) | undefined;
    w.onResized(sync).then((f) => (off = f)).catch(() => {});
    return () => off?.();
  }, []);
  const w = () => getCurrentWindow();
  if (!ownTitleBar) return null;
  return (
    <header class={`titlebar${isMac ? " mac" : ""}`} data-tauri-drag-region>
      <div class={`tb-brand${sidebar ? " side" : ""}`} data-tauri-drag-region><img src="./logo.svg" alt="" />Nod</div>
      <div class="tb-fill" data-tauri-drag-region />
      {!isMac && (
        <div class="tb-controls">
          <button class="tb-btn" aria-label={t("win.minimize")} title={t("win.minimize")} onClick={() => w().minimize()}>
            <svg width="10" height="10" viewBox="0 0 10 10"><path d="M0 5.5h10" stroke="currentColor" /></svg>
          </button>
          <button class="tb-btn" aria-label={t(max ? "win.restore" : "win.maximize")} title={t(max ? "win.restore" : "win.maximize")} onClick={() => w().toggleMaximize()}>
            {max
              ? <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor"><path d="M2.5 2.5V.5h7v7h-2" /><rect x=".5" y="2.5" width="7" height="7" /></svg>
              : <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor"><rect x=".5" y=".5" width="9" height="9" /></svg>}
          </button>
          <button class="tb-btn close" aria-label={t("win.close")} title={t("win.close")} onClick={() => w().close()}>
            <svg width="10" height="10" viewBox="0 0 10 10" stroke="currentColor"><path d="M.5.5l9 9M9.5.5l-9 9" /></svg>
          </button>
        </div>
      )}
    </header>
  );
}

function Sidebar() {
  const { page, go, ov } = useStore();
  const items: [Page, string, Key][] = [
    ["overview", "home", "nav.overview"],
    ["reviews", "list", "nav.reviews"],
    ["plugins", "puzzle", "nav.plugins"],
    ["settings", "settings", "nav.settings"],
  ];
  return (
    <aside class="sidebar">
      {!ownTitleBar && <div class="brand"><img src="./logo.svg" alt="" />Nod</div>}
      <nav>
        {items.map(([id, icon, label]) => (
          <button class={`nav${page === id ? " active" : ""}`} onClick={() => go(id)}>
            <Icon name={icon} /><span>{t(label)}</span>
            {id === "reviews" && ov.status.active > 0 && <span class="count">{ov.status.active}</span>}
          </button>
        ))}
      </nav>
      <UpdateNotice />
      <StatusWidget />
    </aside>
  );
}

export function stateLabel(s: Status) {
  return s.state === "error" && s.title ? s.title : t(`state.${s.state}` as Key);
}

/** "Nod x.y.z is out": shown above the status while a newer release is available. Installing restarts Nod, so it
 *  waits for a click; the running reviews are picked up again after the restart. */
export function useUpdate() {
  const [u, setU] = useState<UpdateState | null>(null);
  useEffect(() => {
    const load = () => api.updateStatus().then(setU).catch(() => {});
    load();
    const id = setInterval(load, 60_000);
    return () => clearInterval(id);
  }, []);
  return [u, setU] as const;
}

function UpdateNotice() {
  const { toast } = useStore();
  const [u, setU] = useUpdate();
  if (!u?.available) return null;
  const go = async () => {
    setU({ ...u, installing: true });
    try { await api.installUpdate(); } catch (e) { toast(errText(e), "bad"); setU({ ...u, installing: false }); }
  };
  return (
    <div class="update-notice">
      <div class="grow">
        <div class="update-title">{t("up.available", { v: u.available })}</div>
        <div class="muted small">{t("up.restart_hint")}</div>
      </div>
      <Button small kind="primary" icon="download" busy={u.installing} disabled={u.installing} onClick={go}>{t(u.installing ? "up.installing" : "up.install")}</Button>
    </div>
  );
}

function StatusWidget() {
  const { ov, refresh } = useStore();
  const s = ov.status;
  const paused = s.state === "paused";
  return (
    <div class={`status-widget ${s.state}`}>
      <span class="dot" />
      <div class="status-text-wrap">
        <span class="status-line ellipsis">{t(`state.${s.state}` as Key)}</span>
        <span class="muted ellipsis">{s.active > 0 ? t("status.active", { n: s.active }) : t("status.last_poll", { t: ago(s.last_poll) })}</span>
      </div>
      <button class="icon-btn" title={paused ? t("btn.resume") : t("btn.pause")} aria-label={paused ? t("btn.resume") : t("btn.pause")}
        onClick={async () => { await api.pause(!paused); setTimeout(refresh, 500); }}>
        <Icon name={paused ? "play" : "pause"} />
      </button>
    </div>
  );
}

function SaveBar() {
  const { draft, reloadConfig, toast, cleared } = useStore();
  const [err, setErr] = useState<string | null>(null);
  return (
    <div class="savebar" role="region" aria-label={t("save.dirty")}>
      <span><Icon name="info" /> {t("save.dirty")}</span>
      {err && <span class="bad-text small">{err}</span>}
      <div class="grow" />
      <Button kind="quiet" onClick={() => { setErr(null); return reloadConfig(); }}>{t("btn.discard")}</Button>
      <Button kind="primary" onClick={async () => {
        setErr(null);
        try {
          const kept = await api.save(draft, cleared);
          await reloadConfig();
          toast(kept.length ? t("set.kept", { f: kept.join(", ") }) : t("btn.saved"), kept.length ? "bad" : "good");
        } catch (e) { setErr(String(e)); }
      }}>{t("btn.save")}</Button>
    </div>
  );
}
