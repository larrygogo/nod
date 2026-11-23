// What the pages read out of the review rows: whether Nod approved, whether a PR waits on you, and the numbers behind
// the overview's sentence and charts. Pure functions of Row, so every page counts the same way.
import type { Finding, Row } from "./api";

export const SEV = ["high", "medium", "low", "nit"] as const;
export type Sev = (typeof SEV)[number];

export const isOpen = (r: Row) => r.pr_state !== "merged" && r.pr_state !== "closed";
export const isActive = (r: Row) => ["queued", "reviewing", "awaiting_ci", "retry"].includes(r.status);

/** Nod's approval stands for this PR: approved by the rules, by you, or an earlier approval that was not withdrawn. */
export const isApproved = (r: Row) => r.event === "APPROVE" || r.status === "direct_approved"
  || ((r.status === "closed" || r.status === "posted") && r.earlier_approved);

/** The review went up (comments always do) but Nod did not approve. Comments and approval are not exclusive: this is
 *  "commented and not approved", the only case where the comment is the whole outcome. */
export const isNotApproved = (r: Row) => !isApproved(r)
  && (r.event === "COMMENT" || (r.status === "closed" && r.commented));

/** An open PR that waits on you: the review failed, or Nod commented and did not approve, so approving is your call. */
export const needsYou = (r: Row) => isOpen(r) && (r.status === "failed" || (r.status === "posted" && r.event === "COMMENT"));

export function sevCounts(findings: Finding[]): Record<Sev, number> {
  const c = { high: 0, medium: 0, low: 0, nit: 0 };
  for (const f of findings) if (f.severity in c) c[f.severity as Sev]++;
  return c;
}

const DAY = 86400;
export function dayStart(ts: number): number {
  const d = new Date(ts * 1000);
  d.setHours(0, 0, 0, 0);
  return d.getTime() / 1000;
}

/** Every review round in the rows, with the PR it belongs to. */
export function allRounds(rows: Row[]) {
  return rows.flatMap((r) => r.rounds.map((x) => ({ ...x, row: r })));
}

/** The last `days` days, oldest first: how many reviews went up each day, split by whether Nod approved. */
export function daily(rows: Row[], days: number, now = Date.now() / 1000) {
  const today = dayStart(now);
  const out = Array.from({ length: days }, (_, i) => ({ day: today - (days - 1 - i) * DAY, approved: 0, commented: 0 }));
  for (const x of allRounds(rows)) {
    const i = Math.round((dayStart(x.at) - out[0].day) / DAY);
    if (i < 0 || i >= days) continue;
    if (x.event === "APPROVE") out[i].approved++;
    else if (x.event === "COMMENT" || x.comment_url) out[i].commented++;
  }
  return out;
}

/** The week in numbers: PRs reviewed, review rounds, PRs Nod approved, PRs held back with a high-severity issue. */
export function week(rows: Row[], now = Date.now() / 1000) {
  const from = dayStart(now) - 6 * DAY;
  const rounds = allRounds(rows).filter((x) => x.at >= from);
  const prs = new Set(rounds.map((x) => `${x.row.account}/${x.row.key}`));
  const inWeek = rows.filter((r) => prs.has(`${r.account}/${r.key}`));
  return {
    prs: prs.size,
    rounds: rounds.length,
    approved: inWeek.filter(isApproved).length,
    held: inWeek.filter((r) => !isApproved(r) && r.findings.some((f) => f.severity === "high")).length,
  };
}
