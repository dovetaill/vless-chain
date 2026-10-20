import type { Check, Line, PanelState } from "../types";

export type Remote = {
  id: string;
  name: string;
  url: string;
  seen: number;
  error: string;
  snapshot:
    | (PanelState & {
        monitoring?: {
          conditions: Record<string, { active: boolean; message: string }>;
          checks?: Record<string, Check>;
        };
      })
    | null;
};
export type Task = {
  id: string;
  action: string;
  state: string;
  version?: string;
  lines?: Record<string, string[]>;
  at?: number;
  targets: {
    id: string;
    state: string;
    error?: string;
    result?: {
      detail?: string;
      error?: string;
      after?: Record<string, string>;
    };
  }[];
};
export const statuses: Record<string, string> = {
  pending: "等待执行",
  queued: "已排队",
  running: "执行中",
  succeeded: "成功",
  failed: "失败",
  unknown: "结果待确认",
  paused: "已暂停",
};
export const actions: Record<string, string> = {
  upgrade: "逐台升级",
  "restart-all": "整套重启",
  "restart-panel": "重启面板",
  "restart-agent": "重启 Agent",
  enable: "启用所选代理",
  disable: "停用所选代理",
};
export function requestId() {
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 15) | 64;
  b[8] = (b[8] & 63) | 128;
  const h = Array.from(b, (v) => v.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
export const PAGE_SIZE = 20;
export function recent(at: number | undefined, now: number) {
  return !!at && now - at <= 90 && now - at >= -30;
}
export function connected(r: Remote, now: number) {
  return !r.error && !!r.snapshot && recent(r.seen, now);
}
export function mismatch(r: Remote) {
  return !!(
    r.snapshot?.version &&
    r.snapshot.agent_version &&
    r.snapshot.version !== r.snapshot.agent_version
  );
}
export function localNode(r: Remote) {
  return r.snapshot?.nodes.find((n) => n.id === r.snapshot?.local_node_id);
}
export function latestCheck(r: Remote, line: Line) {
  return [
    r.snapshot?.checks[line.id],
    ...Object.entries(r.snapshot?.monitoring?.checks || {})
      .filter(
        ([key]) => key.startsWith("probe:") && key.endsWith(`:${line.id}`),
      )
      .map(([, value]) => value),
  ]
    .filter((check): check is Check => !!check)
    .sort((a, b) => b.checked_at - a.checked_at)[0];
}
export function health(r: Remote, now: number) {
  if (!r.snapshot) return "等待同步";
  if (!connected(r, now)) return "失联 / 数据过期";
  const node = localNode(r);
  if (!node) return "Agent 未接入";
  if (!node.online || !recent(node.seen, now)) return "Agent 离线";
  if (
    node.report.error ||
    !node.report.running ||
    !node.report.applied ||
    Object.values(r.snapshot.monitoring?.conditions || {}).some((c) => c.active)
  )
    return "需要关注";
  return "在线";
}
export function lineStatus(r: Remote, line: Line, now: number) {
  if (!connected(r, now)) return "状态未知";
  if (line.sync_status === "error") return "应用失败";
  if (line.sync_status !== "synced")
    return line.enabled ? "待应用" : "停用待生效";
  if (!line.enabled) return "已停用";
  if (line.rental_status === "expired") return "已到期";
  if (line.rental_status === "quota_exhausted" || line.traffic?.blocked)
    return "额度耗尽";
  if (line.rental_status === "offline") return "Agent 离线";
  const check = latestCheck(r, line);
  if (check && !check.ok) return "最近探测失败";
  return "已启用";
}
export function metrics(remotes: Remote[], now: number) {
  let rx = 0,
    tx = 0,
    upload = 0,
    download = 0,
    used = 0,
    hosts = 0,
    sampled = 0,
    lines = 0,
    usage = 0;
  for (const r of remotes) {
    const node = localNode(r),
      system = node?.report.system;
    if (
      connected(r, now) &&
      node?.online &&
      recent(node.seen, now) &&
      recent(system?.sampled_at, now) &&
      system?.network_rx_bps != null &&
      system.network_tx_bps != null
    ) {
      hosts++;
      rx += system.network_rx_bps;
      tx += system.network_tx_bps;
    }
    for (const line of r.snapshot?.lines || []) {
      lines++;
      const traffic = line.traffic;
      if (traffic) {
        usage++;
        used += traffic.upload + traffic.download;
      }
      if (
        connected(r, now) &&
        line.enabled &&
        line.rental_status !== "offline" &&
        traffic &&
        recent(traffic.sampled_at, now)
      ) {
        sampled++;
        upload += traffic.upload_bps;
        download += traffic.download_bps;
      }
    }
  }
  return { rx, tx, upload, download, used, hosts, sampled, lines, usage };
}

export function priority(r: Remote, now: number) {
  const status = health(r, now);
  return !connected(r, now)
    ? 0
    : ["Agent 离线", "Agent 未接入"].includes(status)
      ? 1
      : status !== "在线"
        ? 2
        : mismatch(r)
          ? 3
          : 4;
}
