import assert from "node:assert/strict";
import { connected, health, issues, lineStatus, metrics, remoteNow, verificationFailed, maintenanceSucceeded, taskNeedsAttention, taskStatus, type Remote, type Task } from "./fleet-state";
import type { Line, PanelState } from "../types";

const now = 1000;
const remote: Remote = {
  id: "vps", name: "VPS", url: "https://example.test", seen: now, error: "",
  snapshot: {
    time: now, local_node_id: "agent", checks: {}, lines: [],
    nodes: [{ id: "agent", online: true, seen: now, report: {
      running: true, applied: true, error: "",
      system: { sampled_at: now, network_rx_bps: 25, network_tx_bps: 30 },
    } }],
  } as unknown as PanelState,
};
assert.equal(health(remote, now), "在线");
assert.equal(health({ ...remote, seen: now - 100 }, now), "同步数据过期");
assert.equal(health({ ...remote, error: "HTTP 401" }, now), "同步失败");
assert.match(issues({ ...remote, error: "HTTP 401" }, now)[0].detail, /HTTP 401/);
assert.equal(health({ ...remote, snapshot: null }, now), "等待首次同步");
const skewed = structuredClone(remote);
skewed.snapshot!.time += 360;
skewed.snapshot!.nodes[0].seen += 360;
skewed.snapshot!.nodes[0].report.system.sampled_at! += 360;
assert.equal(connected(skewed, now), true);
assert.equal(health(skewed, now), "机器时钟不一致");
assert.equal(remoteNow(skewed, now + 10), 1370);
assert.equal(metrics([skewed], now).hosts, 1);
assert.equal(metrics([skewed], now + 100).hosts, 0);
const line = { enabled: true, sync_status: "synced", rental_status: "active", traffic: {
  sampled_at: 1360, upload: 100, download: 50, upload_bps: 2, download_bps: 3,
} } as Line;
skewed.snapshot!.lines.push(line);
assert.equal(metrics([skewed], now).sampled, 1);
assert.equal(metrics([skewed], now + 100).sampled, 0);
assert.equal(metrics([skewed], now + 100).used, 150);
assert.equal(lineStatus(skewed, line, now + 100), "状态未知");
assert.equal(lineStatus(skewed, line, now), "已启用");
const warning = structuredClone(remote);
warning.snapshot!.monitoring = { conditions: { "ntp:agent": { active: true, message: "自动时间同步未启用" } } };
assert.equal(health(warning, now), "自动校时未启用");
assert.equal(connected(warning, now), true);
assert.match(issues(warning, now)[0].detail, /日志与诊断/);
warning.policy_error = "HTTP 500";
assert.equal(health(warning, now), "监控策略同步失败");
assert.equal(connected(warning, now), true);
warning.snapshot!.nodes[0].report.applied = false;
assert.ok(issues(warning, now).some((issue) => issue.title === "配置尚未应用"));
warning.snapshot!.nodes[0].online = false;
assert.ok(issues(warning, now).some((issue) => issue.title === "Agent 上报中断"));
assert.equal(issues({ ...remote, snapshot: { ...remote.snapshot!, time: 0 } }, now).length, 0);
const probeWarning = structuredClone(remote);
probeWarning.snapshot!.monitoring = {
  effective: { interval: 300, recoveries: 2 },
  conditions: { "probe:local:line": { active: true, message: "REALITY：本机入口 127.0.0.1:20000 · SNI example.com 未通过", good: 0 } },
  checks: { "probe:local:line": { ok: false, checked_at: now, detail: "Connection reset by peer\n自动分段检查：出口通过", outbound: { ok: true, detail: "HTTP 200" } } },
};
assert.equal(health(probeWarning, now), "入口检测异常，出口正常");
assert.equal(connected(probeWarning, now), true);
assert.match(issues(probeWarning, now)[0].detail, /20000.*SNI example.com/);
assert.match(issues(probeWarning, now)[0].detail, /自动复查.*300 秒/);
assert.match(issues(probeWarning, now)[0].evidence!, /Connection reset by peer/);
assert.equal(issues(probeWarning, now)[0].probeLineId, "line");
probeWarning.snapshot!.monitoring.conditions["probe:local:line"].active = false;
assert.equal(health(probeWarning, now), "连接检测待确认", "首次检测失败不能显示为在线且无异常");
assert.match(issues(probeWarning, now)[0].detail, /尚未达到连续失败告警阈值/);
probeWarning.snapshot!.monitoring.conditions["probe:local:line"].active = true;
probeWarning.snapshot!.monitoring.checks!["probe:local:line"] = { ok: true, checked_at: now + 300, detail: "HTTP 200" };
probeWarning.snapshot!.monitoring.conditions["probe:local:line"].good = 1;
assert.equal(health(probeWarning, now), "入口恢复确认中");
assert.match(issues(probeWarning, now)[0].detail, /1\/2/);
probeWarning.snapshot!.monitoring.conditions["probe:local:line"].active = false;
assert.equal(health(probeWarning, now), "在线");
console.log("fleet-state assertions passed");

assert.equal(verificationFailed({id:"vps", state:"failed", result:{maintenance_state:"succeeded", verification:{state:"failed", detail:"probe failed"}}}), true);
assert.equal(verificationFailed({id:"vps", state:"failed", error:"组件维护已完成，但整链验证失败：timeout"}), true);
assert.equal(verificationFailed({id:"vps", state:"failed", error:"安装失败"}), false);
assert.equal(verificationFailed({id:"vps", state:"succeeded", error:"组件维护已完成，但整链验证失败：old", result:{verification:{state:"succeeded"}}}), false);
const maintained: Task = {id:"task", action:"upgrade", state:"succeeded", targets:[{id:"vps", state:"succeeded", result:{maintenance_state:"succeeded", verification:{state:"failed"}}}]};
assert.equal(maintenanceSucceeded(maintained.targets[0]), true);
assert.equal(taskNeedsAttention(maintained), true);
assert.equal(taskStatus(maintained), "维护完成 · 连接待确认");
maintained.targets[0].result!.verification!.state = "pending";
assert.equal(taskStatus(maintained), "维护完成 · 连接待确认");
maintained.targets[0].result!.verification!.state = "succeeded";
assert.equal(taskStatus(maintained), "成功");
assert.equal(taskNeedsAttention(maintained), false, "验证恢复后自动移入历史");
maintained.state = "completed";
assert.equal(taskNeedsAttention(maintained), true);
maintained.archived = true;
assert.equal(taskNeedsAttention(maintained), false, "归档不再占用待处理列表");
