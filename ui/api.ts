// Types and calls shared with the Rust side (src-tauri/src/app.rs).
import { invoke } from "@tauri-apps/api/core";

export interface Status {
  state: "setup" | "paused" | "starting" | "idle" | "reviewing" | "error";
  title: string | null;
  error: string | null;
  action: string | null;
  last_poll: number;
  active: number;
}
export interface Finding { severity: string; title: string; path: string; line: number | null; introduced_by_pr: boolean }
export interface Row {
  account: string; platform: string; engine: string;
  key: string; title: string; url: string; author: string; status: string; pr_state: string; event: string | null;
  summary: string | null; findings: Finding[]; reasons: string[]; review_url: string | null; error: string | null; wait_until: number; sha: string; earlier_approved: boolean; commented: boolean; at: number;
}
export interface Overview {
  version: string; configured: boolean; status: Status; rows: Row[]; legacy: string[]; old_install: boolean; autostart: boolean;
}
export interface Check {
  id: string; name: string; ok: boolean; warn: boolean; detail: string; fix: string; action: string | null; account: string | null;
}
export interface Field {
  key: string; label: string; kind: "text" | "password" | "number" | "bool" | "select";
  options: [string, string][]; placeholder: string; help: string; secret: boolean;
}
export interface Plugin {
  id: string; kind: "forge" | "engine" | "notify" | "invalid"; name: string; description: string; icon: string;
  builtin: boolean; fields: Field[]; badges: { tone: "good" | "warn" | "info"; text: string }[]; help_url: string; error: string | null;
}
export interface Account { id: string; kind: string; url: string; enabled: boolean; token: string; settings?: Record<string, unknown> }
// The configuration is edited as JSON; the Rust side validates it. Only the parts the window touches are typed.
export interface Config {
  language: string;
  forges?: Account[];
  plugins?: Record<string, Record<string, unknown>>;
  review: Record<string, any> & { engine: string; per_repo: { repo: string; engine: string }[] };
  scope: { repos: string[]; exclude_repos: string[]; authors?: string[]; exclude_authors?: string[]; team_requests: boolean };
  approve: { mode: string; repos: string[]; authors: string[]; exclude_repos: string[]; exclude_authors: string[]; always?: { repo: string; author: string }[] };
  daemon: { poll_seconds: number; refresh_seconds: number };
  notify: Record<string, any> & { channels: string[]; channel: string };
  [k: string]: unknown;
}
export interface GithubSession { signed_in: boolean; login: string; has_client_id: boolean }
export interface DeviceCode { device_code: string; user_code: string; verification_uri: string; interval: number; expires_in: number }
export interface GithubPoll { state: "pending" | "slow_down" | "done" | "expired" | "denied" | "failed"; interval: number; login: string; message: string }
export interface LoadedConfig { config: Config; secrets: Record<string, boolean>; error: string | null }

export const api = {
  overview: () => invoke<Overview>("overview"),
  config: () => invoke<LoadedConfig>("get_config"),
  save: (cfg: Config, clear: string[] = []) => invoke<string[]>("save_config", { cfg, clear }),
  catalog: (lang?: string) => invoke<Plugin[]>("catalog", { lang }),
  checkAll: (cfg: Config, live: boolean, clear: string[] = []) => invoke<Check[]>("check_tools", { cfg, live, clear }),
  checkPlugin: (cfg: Config, kind: string, id: string, clear: string[] = []) => invoke<Check[]>("check_plugin", { cfg, kind, id, clear }),
  testChannel: (cfg: Config, channel: string, clear: string[] = []) => invoke<string>("test_notification", { cfg, channel, clear }),
  fix: (action: string) => invoke<void>("fix", { action }),
  pause: (paused: boolean) => invoke<void>("set_paused", { paused }),
  restart: () => invoke<void>("restart_worker"),
  autostart: (on: boolean) => invoke<void>("set_autostart", { on }),
  track: (pr: string) => invoke<string>("track_pr", { pr }),
  importOld: () => invoke<string[]>("import_old"),
  open: (which: "config" | "data" | "log" | "plugins") => invoke<void>("open_location", { which }),
  url: (url: string) => invoke<void>("open_url", { url }).catch(() => {}),
  githubSession: (account: Account) => invoke<GithubSession>("github_session", { account }),
  githubLoginStart: (account: Account) => invoke<DeviceCode>("github_login_start", { account }),
  githubLoginPoll: (account: Account, deviceCode: string) => invoke<GithubPoll>("github_login_poll", { account, deviceCode }),
  githubLogout: (account: Account) => invoke<void>("github_logout", { account }),
  updateStatus: () => invoke<UpdateState>("update_status"),
  checkUpdate: () => invoke<UpdateState>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
};

export type UpdateState = {
  current: string; available: string | null; notes: string | null; checked_at: number; error: string | null; installing: boolean;
};

/** The channels in effect (new list or the legacy single channel), like Notify::active in Rust. */
export function activeChannels(c: Config): string[] {
  if (c.notify.channels?.length) return c.notify.channels.filter((x) => x !== "none");
  return c.notify.channel && c.notify.channel !== "none" ? [c.notify.channel] : [];
}

/** The accounts in effect: [[forges]], or the implicit GitHub account of older configurations. */
export function accounts(c: Config): Account[] {
  return c.forges?.length ? c.forges : [{ id: "github", kind: "github", url: "", enabled: true, token: "" }];
}

/** Read / write a dotted path ("review.codex.model") in the config draft. */
export function getPath(o: any, path: string): any {
  return path.split(".").reduce((x, k) => (x == null ? undefined : x[k]), o);
}
export function setPath(o: any, path: string, v: unknown) {
  const parts = path.split(".");
  let x = o;
  for (const k of parts.slice(0, -1)) x = x[k] ??= {};
  x[parts[parts.length - 1]] = v;
}
