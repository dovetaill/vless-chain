import type { Check, Line, PanelState } from "../types";

export type Remote = {
  id: string;
  name: string;
  url: string;
  seen: number;
  error: string;
  policy_error?: string;
  snapshot:
    | (PanelState & {
        monitoring?: {
          conditions: Record<string, { active: boolean; message: string; good?: number }>;
          checks?: Record<string, Check>;
          effective?: { interval: number; recoveries: number };
        };
      })
    | null;
};
export type Task = {
  id: string;
  archived?: boolean;
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
      maintenance_state?: string;
      verification?: { state: string; detail?: string };
    };
  }[];
};
export function verificationFailed(target: Task["targets"][number]) {
  if (target.result?.verification) return target.result.verification.state === "failed";
  return (target.error || target.result?.error || "").startsWith("组件维护已完成，但整链验证失败");
}
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
// Snapshot timestamps use the remote clock; seen uses the controller clock.
export function remoteNow(r: Remote, now: number) {
  return r.snapshot?.time ? r.snapshot.time + (now - r.seen) : now;
}
export function issues(r: Remote, now: number): { title: string; detail: string; evidence?: string }[] {
  if (r.error) return [{ title: "同步失败", detail: `${r.error}。请检查远端面板连接与日志；这不代表代理已停止。` }];
  if (!r.snapshot) return [{ title: "等待首次同步", detail: "接入后约 30 秒同步一次；尚无运行数据。" }];
  if (!recent(r.seen, now)) return [{ title: "同步数据过期", detail: "总控未取得近期快照，当前运行状态未知。请刷新总控并检查同步日志；历史用量仍保留。" }];
  const result: { title: string; detail: string; evidence?: string }[] = [];
  const offset = r.snapshot.time ? Math.round(r.snapshot.time - r.seen) : 0;
  if (Math.abs(offset) > 60) result.push({ title: "机器时钟不一致", detail: `远端快照时间比总控${offset > 0 ? "快" : "慢"}约 ${Math.abs(offset)} 秒；不能据此确定哪台不准。请在两台机器的“日志与诊断”查看时间同步状态。` });
  if (r.policy_error) result.push({ title: "监控策略同步失败", detail: `${r.policy_error}。状态数据已同步，请稍后重试或检查远端版本与日志。` });
  const node = localNode(r);
  if (!node) result.push({ title: "Agent 未接入", detail: "远端面板可同步，但本机 Agent 尚未接入，请检查安装状态。" });
  else {
    if (!node.online || !recent(node.seen, remoteNow(r, now))) result.push({ title: "Agent 上报中断", detail: "面板可同步，但 Agent 没有近期上报。请查看 Agent 服务日志；代理实际连通性尚未确认。" });
    else if (node.report.error) result.push({ title: "Agent 执行失败", detail: node.report.error });
    else if (!node.report.running) result.push({ title: "代理内核未运行", detail: "请查看 Agent 与内核日志，确认配置已应用且启动成功。" });
    else if (!node.report.applied) result.push({ title: "配置尚未应用", detail: "请在 VPS 详情选择“预览并应用配置”。" });
  }
  for (const [key, condition] of Object.entries(r.snapshot.monitoring?.conditions || {})) {
    if (condition.active && key.startsWith("probe:")) {
      const check = r.snapshot.monitoring?.checks?.[key];
      const policy = r.snapshot.monitoring?.effective;
      const recovering = check?.ok === true;
      const progress = recovering ? `最近一次已通过，正在自动确认恢复${policy ? `（${condition.good || 0}/${policy.recoveries}）` : ""}。` : "系统会自动复查，达到恢复条件后自动解除。";
      result.push({
        title: recovering ? "入口恢复确认中" : check?.outbound?.ok ? "入口检测异常，出口正常" : "代理入口检测异常",
        detail: `${condition.message}。${check?.checked_at ? `最近检测 ${new Date(check.checked_at * 1000).toLocaleString("zh-CN", { hour12: false })}。` : ""}${progress}${policy ? `检测间隔 ${policy.interval} 秒。` : ""}`,
        evidence: check?.detail,
      });
      continue;
    }
    if (condition.active) result.push({
      title: key.startsWith("ntp:") ? "自动校时未启用" : key.startsWith("clock:") ? "系统时钟偏差" : "监控告警",
      detail: condition.message + (/^(ntp|clock):/.test(key) ? "。请打开“日志与诊断”查看校时状态与操作结果。" : ""),
    });
  }
  if (mismatch(r)) result.push({ title: "组件版本不一致", detail: "面板与 Agent 版本不同，请更新本 VPS 已安装组件。" });
  return result;
}
export function health(r: Remote, now: number) {
  return issues(r, now)[0]?.title || "在线";
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
      recent(node.seen, remoteNow(r, now)) &&
      recent(system?.sampled_at, remoteNow(r, now)) &&
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
        recent(traffic.sampled_at, remoteNow(r, now))
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
    : ["Agent 上报中断", "Agent 未接入"].includes(status)
      ? 1
      : status !== "在线"
        ? 2
        : mismatch(r)
          ? 3
          : 4;
}
