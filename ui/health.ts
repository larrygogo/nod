// Whether the code hosts and engines in use work, checked with the saved settings and without calling a model (no
// usage spent): the Plugins list marks a broken one, the overview says so before anything else. Shared so both pages
// show the same answer, and cached briefly so moving between them does not start every check again.
import { useEffect, useState } from "preact/hooks";
import { accounts, api, type Check, type Config } from "./api";

export type Health = Record<string, Check[]>;
const TTL = 60_000;
// the running or finished checks for one configuration: a page that opens while they run waits for the same ones
let cache: { key: string; at: number; jobs: [string, Promise<Check[]>][] } | null = null;

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

export function usePluginHealth(cfg: Config): Health {
  const key = JSON.stringify(cfg);
  const [health, setHealth] = useState<Health>({});
  useEffect(() => {
    if (!cache || cache.key !== key || Date.now() - cache.at > TTL) {
      cache = { key, at: Date.now(), jobs: inUse(cfg).map(([k, run]) => [k, run()] as [string, Promise<Check[]>]) };
    }
    let live = true;
    setHealth({});
    for (const [k, job] of cache.jobs) job.then((r) => { if (live) setHealth((h) => ({ ...h, [k]: r })); }).catch(() => {});
    return () => { live = false; };
  }, [key]);
  return health;
}

/** "bad" when something does not work, "warn" when it works with a caveat, null when fine or not checked yet. */
export function healthOf(r: Check[] | undefined): "bad" | "warn" | null {
  if (r?.some((c) => !c.ok)) return "bad";
  if (r?.some((c) => c.warn)) return "warn";
  return null;
}
