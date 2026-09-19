import type { BackgroundItem, BackgroundKind, BackgroundSummary } from "./types";

const KIND_LABEL: Record<BackgroundKind, string> = {
  monitor: "Monitor",
  shell: "Shell",
  wakeup: "Wakeup",
  subagent: "Sub-agent",
  workflow: "Workflow",
};

const CAUSE_TEXT: Record<string, string> = {
  new_build: "restart onto a new build",
  respawn: "worker restart",
  wedge_kill: "stuck agent killed",
  user_stop: "stopped by the owner",
  idle_cap: "idle for 24 h",
};

function span(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

/** Clock time (HH:MM UTC) an ISO timestamp falls on. */
export function clockTime(iso: string): string {
  return `${new Date(iso).toISOString().slice(11, 16)} UTC`;
}

export function describeBackgroundItem(item: BackgroundItem, now: number): string {
  const parts = [KIND_LABEL[item.kind], item.label ?? item.id];
  if (item.ended) {
    const cause = item.ended.cause ? ` (${CAUSE_TEXT[item.ended.cause] ?? item.ended.cause})` : "";
    parts.push(`${item.ended.reason.replace("_", " ")} ${clockTime(item.ended.at)}${cause}`);
  } else {
    parts.push("live");
  }
  parts.push(span(now - Date.parse(item.started_at)));
  if (!item.ended && item.expires_at) {
    const verb = item.kind === "wakeup" ? "fires" : "times out";
    parts.push(`${verb} in ${span(Date.parse(item.expires_at) - now)}`);
  }
  return parts.join(" · ");
}

export function hasUnacknowledgedLoss(summary: BackgroundSummary, ackAt: string | null): boolean {
  if (!summary.lost_since) return false;
  return !ackAt || Date.parse(summary.lost_since) > Date.parse(ackAt);
}

const WAITABLE = ["subagent", "workflow", "shell"] as const satisfies readonly BackgroundKind[];
const NOUN: Record<(typeof WAITABLE)[number], [string, string]> = {
  subagent: ["sub-agent", "sub-agents"],
  workflow: ["workflow", "workflows"],
  shell: ["shell command", "shell commands"],
};

export function waitingOn(summary: BackgroundSummary): string | null {
  const parts = WAITABLE.map((kind) => {
    const n = summary.items.filter((i) => i.kind === kind && !i.ended).length;
    return n ? `${n} ${NOUN[kind][n === 1 ? 0 : 1]}` : null;
  }).filter((p): p is string => p !== null);
  return parts.length ? parts.join(", ") : null;
}
