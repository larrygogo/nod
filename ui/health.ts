// Whether the code hosts and engines in use work, checked with the saved settings and without calling a model (no
// usage spent): the Plugins list marks a broken one, the overview says so before anything else. Shared so both pages
// show the same answer, and cached briefly so moving between them does not start every check again.
import { useEffect, useState } from "preact/hooks";
import { accounts, api, type Check, type Config } from "./api";

export type Health = Record<string, Check[]>;
const TTL = 60_000;

/** The checks for one saved configuration: those finished (`done`) and how many are still out. */
let cache: { key: string; at: number; done: Health; pending: number } | null = null;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((f) => f());

/** The plugins a configuration relies on, as Plugins-page ids: `forge:<account>` and `engine:<id>`. */
export function inUse(cfg: Config): [string, () => Promise<Check[]>][] {
  const engines = [...new Set([cfg.review.engine, cfg.review.fallback, ...(cfg.review.per_repo ?? []).map((r) => r.engine)]
    .filter(Boolean))];
  return [
    ...accounts(cfg).filter((a) => a.enabled)
      .map((a) => [`forge:${a.id}`, () => api.checkPlugin(cfg, "forge", a.id, [], false)] as [string, () => Promise<Check[]>]),
    ...engines.map((e) => [`engine:${e}`, () => api.checkPlugin(cfg, "engine", e, [], false)] as [string, () => Promise<Check[]>]),
  ];
}

function start(cfg: Config, key: string) {
  const jobs = inUse(cfg);
  const c = { key, at: Date.now(), done: {} as Health, pending: jobs.length };
  cache = c;
  for (const [k, run] of jobs) {
    run().then((r) => { c.done[k] = r; }).catch(() => {}).finally(() => { c.pending--; if (cache === c) notify(); });
  }
}

/** `health`: results so far; `ready`: every check has answered (the overview waits for this, so a banner does not
 *  first say "reviews will fail" and then change its mind once the backup engine's check comes back). */
export function usePluginHealth(cfg: Config): { health: Health; ready: boolean } {
  const key = JSON.stringify(cfg);
  const read = () => (cache && cache.key === key ? { health: { ...cache.done }, ready: cache.pending === 0 } : { health: {}, ready: false });
  // finished results are there on the first frame: coming back to a page does not make its banner pop in late
  const [state, setState] = useState(read);
  useEffect(() => {
    if (!cache || cache.key !== key || (cache.pending === 0 && Date.now() - cache.at > TTL)) start(cfg, key);
    const f = () => setState(read());
    listeners.add(f);
    f();
    return () => { listeners.delete(f); };
  }, [key]);
  return state;
}

/** A self-test the user ran on the saved settings is the newest answer: it replaces the cached one, so a plugin that
 *  was just fixed stops showing as broken in the list and on the overview. */
export function recordHealth(cfg: Config, k: string, r: Check[]) {
  if (!cache || cache.key !== JSON.stringify(cfg)) return;
  cache.done[k] = r;
  notify();
}

// Checks that describe a deliberate limit rather than a problem: Gemini has no OS sandbox, so by design it only
// comments. Shown in its pane, but not a reason to mark it "Needs a look" (taste P5: a design choice is not a fault).
const NOTE_ONLY = new Set(["engine_isolation"]);

/** "bad" when something does not work, "warn" when it works with a caveat, null when fine or not checked yet. */
export function healthOf(r: Check[] | undefined): "bad" | "warn" | null {
  if (r?.some((c) => !c.ok)) return "bad";
  if (r?.some((c) => c.warn && !NOTE_ONLY.has(c.id))) return "warn";
  return null;
}
