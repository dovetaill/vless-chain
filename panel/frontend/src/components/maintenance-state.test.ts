import assert from "node:assert/strict";
import { upgradeVersionReached } from "./maintenance-state";
import type { PanelState } from "../types";

const task = { action: "upgrade", state: "failed", version: "0.2.66", before: { panel: "0.2.61", agent: "0.2.61" }, finished: 950 };
const state = {
  time: 1000, version: "0.2.66", agent_version: "0.2.66", agent_seen: 999, local_node_id: "local",
  nodes: [{ id: "local", seen: 999, online: true, report: { applied: true, running: false, error: "" } }],
} as unknown as PanelState;
assert.equal(upgradeVersionReached(task, state), true, "新版本已上报，空配置节点无需代理内核运行");
assert.equal(upgradeVersionReached({ ...task, before: { panel: "0.2.61" } }, { ...state, nodes: [], agent_version: null }), true);
assert.equal(upgradeVersionReached(task, { ...state, version: "0.2.67", agent_version: "0.3.0" }), true);
assert.equal(upgradeVersionReached({ ...task, version: "0.2.9" }, state), true, "版本按数值比较");
for (const changes of [
  { version: "0.2.65" }, { agent_version: "0.2.65" }, { agent_version: null },
  { version: "0.2.66-beta" }, { time: 1040 }, { time: 998 }, { agent_seen: 998 },
  { nodes: [] }, { local_node_id: "other" },
] as Partial<PanelState>[]) {
  assert.equal(upgradeVersionReached(task, { ...state, ...changes }), false, JSON.stringify(changes));
}
for (const changes of [
  { state: "running" }, { state: "unknown" }, { action: "restart-all" },
  { before: undefined }, { before: {} }, { before: { agent: "0.2.61" } },
  { before: { panel: "0.2.61", unknown: "0.2.61" } }, { finished: undefined },
  { finished: 999 }, { finished: 1001 }, { version: undefined },
  { version: "0.2.66-beta" }, { xray_channel: "latest" },
] as Partial<Parameters<typeof upgradeVersionReached>[0]>[]) {
  assert.equal(upgradeVersionReached({ ...task, ...changes }, state), false, JSON.stringify(changes));
}
for (const report of [{ applied: false }, { error: "内核启动失败" }]) {
  const failed = structuredClone(state);
  Object.assign(failed.nodes[0].report, report);
  assert.equal(upgradeVersionReached(task, failed), false);
}
const offline = structuredClone(state);
offline.nodes[0].online = false;
assert.equal(upgradeVersionReached(task, offline), false);
for (const pending of [{ pending: "core-task" }, { blocked: { core: "xray" } }]) {
  assert.equal(upgradeVersionReached(task, { ...state, core_maintenance: pending as PanelState["core_maintenance"] }), false);
}
console.log("maintenance-state assertions passed");
