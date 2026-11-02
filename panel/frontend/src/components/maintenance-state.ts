import type { PanelState } from "../types";

export function upgradeVersionReached(task: {
  action?: string;
  state: string;
  version?: string;
  before?: Record<string, string>;
  finished?: number;
  xray_channel?: string;
}, state: PanelState) {
  if (task.action !== "upgrade" || task.state !== "failed" || !task.before?.panel
    || !task.finished || task.finished > state.time || task.xray_channel === "latest"
    || state.core_maintenance?.blocked || state.core_maintenance?.pending) return false;
  const target = task.version?.match(/^(\d+)\.(\d+)\.(\d+)$/)?.slice(1).map(Number);
  if (!target?.every(Number.isSafeInteger)) return false;
  return Object.keys(task.before).every(role => {
    if (role !== "panel" && role !== "agent") return false;
    const current = (role === "panel" ? state.version : state.agent_version)
      ?.match(/^(\d+)\.(\d+)\.(\d+)$/)?.slice(1).map(Number);
    if (!current?.every(Number.isSafeInteger)) return false;
    const difference = current.findIndex((part, index) => part !== target[index]);
    if (difference >= 0 && current[difference] < target[difference]) return false;
    if (role === "panel") return true;
    const node = state.nodes.find(n => n.id === state.local_node_id);
    return !!node && node.online && node.seen === state.agent_seen
      && node.seen > task.finished! && node.seen <= state.time && state.time - node.seen <= 30
      && node.report.applied && !node.report.error;
  });
}
