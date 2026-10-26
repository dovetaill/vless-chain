import { useCallback, useEffect, useState } from "react";
import { api, ApiError } from "../api";
import type { PanelState } from "../types";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Modal, Status, errorText, stamp } from "./shared";
import { requestId, statuses } from "./fleet-state";
import { CoreSelector, initialCoreChoice, type CoreChoice } from "./core-selector";

type Release = { version: string; xray: string; mihomo: string };
type Task = {
  id: string;
  action?: string;
  version?: string;
  state: string;
  detail?: string;
  error?: string;
  core?: string;
  core_version?: string;
  after?: Record<string, string>;
};
const storageKey = "chain-maintenance-task";

export function Maintenance({ state }: { state: PanelState }) {
  const [release, setRelease] = useState<Release>();
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const [connection, setConnection] = useState("");
  const [taskId, setTaskId] = useState(() => localStorage.getItem(storageKey));
  const [task, setTask] = useState<Task>();
  const [submitting, setSubmitting] = useState(false);
  const [core, setCore] = useState<CoreChoice>(initialCoreChoice);
  const [confirm, setConfirm] = useState<{ id: string; action: string; version?: string; xray_channel?: string; core?: string; task_id?: string } & Partial<CoreChoice>>();
  const busy = submitting || (!!taskId && (!task || ["queued", "running", "awaiting_verification"].includes(task.state)));
  useEffect(() => {
    if (state.core_maintenance?.pending && !submitting) { setTaskId(state.core_maintenance.pending); localStorage.setItem(storageKey, state.core_maintenance.pending); }
  }, [state.core_maintenance?.pending]);
  const fetchRelease = useCallback(async () => {
    setChecking(true);
    setRelease(undefined);
    setError("");
    try {
      setRelease(await api<Release>("/fleet/release/latest"));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setChecking(false);
    }
  }, []);
  useEffect(() => { void fetchRelease(); }, [fetchRelease]);
  useEffect(() => {
    if (submitting || !taskId || (task && !["queued", "running", "awaiting_verification"].includes(task.state))) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const result = await api<Task>(`/federation/task?id=${encodeURIComponent(taskId)}`);
        if (stopped) return;
        setConnection("");
        setTask(result.state === "not_found"
          ? { id: taskId, state: "unknown", error: "未找到原任务记录，请核对当前版本和服务状态后再操作。" }
          : result);
      } catch (e) {
        if (!stopped) setConnection(`暂时无法查询，恢复连接后继续查询原任务：${errorText(e)}`);
      }
      if (!stopped) timer = setTimeout(() => void poll(), 3000);
    };
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, [taskId, task?.state, submitting]);
  const supported = !!state.maintenance;
  return (
    <Card>
      <CardHeader><CardTitle>版本与维护</CardTitle></CardHeader>
      <CardContent className="space-y-4">
        <p className="muted">以下操作作用于当前服务器；其他 VPS 请在多机总控中选择。</p>
        <dl className="grid grid-cols-2 gap-3 text-sm">
          <dt>面板</dt><dd>{state.version || "未知"}</dd>
          <dt>本机 Agent</dt><dd>{state.agent_version || "未知（尚未上报）"}</dd>
          <dt>Xray（本机安装）</dt><dd>{state.core_maintenance?.versions.xray || state.cores?.xray || "未知"}</dd>
          <dt>Mihomo（本机安装）</dt><dd>{state.core_maintenance?.versions.mihomo || state.cores?.mihomo || "未知"}</dd>
        </dl>
        {state.cores?.xray_channel === "latest" && <p className="text-sm">当前 Xray 来源：官方最新版{state.cores.xray_prerelease ? "（预发布）" : ""}</p>}
        {!!state.agent_seen && <p className="muted">Agent 最近上报：{stamp(state.agent_seen)}</p>}
        {state.agent_version && state.version !== state.agent_version && (
          <Status tone="warn">面板与 Agent 版本不一致</Status>
        )}
        <p className="text-sm">
          {release ? `最新正式 Release：${release.version} · Xray ${release.xray || "未知"} · Mihomo ${release.mihomo || "未知"}` : "尚未获取最新正式 Release"}
        </p>
        <p className="muted">已独立选择的内核版本会在面板更新时保留。需要切换内核时使用下方独立升级入口。</p>
        {!supported && (
          <p role="status" className="text-sm text-amber-700">
            当前部署未提供网页维护。Linux systemd / OpenRC（含 Alpine）请先手工升级以安装维护组件；macOS 和纯 Agent 安装暂不支持。
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" disabled={checking || busy} onClick={() => void fetchRelease()}>
            {checking ? "正在检查…" : "检查最新版本"}
          </Button>
          <Button disabled={!supported || !release || checking || busy}
            onClick={() => setConfirm({ id: requestId(), action: "upgrade", version: release!.version })}>
            升级并自动重启
          </Button>
          <Button variant="outline" disabled={!supported || !state.maintenance_actions?.includes("restart-all") || busy}
            onClick={() => setConfirm({ id: requestId(), action: "restart-all" })}>
            整套重启
          </Button>
        </div>
        <p className="muted">自动处理已安装的面板、Agent 及其代理内核、项目独立 HTTPS 服务。重启会中断本机连接，配置和业务数据保留。</p>
        <CoreSelector value={core} onChange={setCore} disabled={busy} />
        {!state.maintenance_actions?.includes("core-upgrade") && <p className="muted">独立内核升级需要本机先安装新版维护组件。</p>}
        {state.core_maintenance?.blocked && <p role="alert" className="text-red-600">安全更新未完成，本机代理已停用；管理面板仍可使用。请安装安全的 {state.core_maintenance.blocked.core} 版本恢复服务。</p>}
        <div className="flex flex-wrap gap-2">
          <Button disabled={busy || !state.maintenance_actions?.includes("core-upgrade") || !/^\d+\.\d+\.\d+$/.test(core.version)}
            onClick={() => setConfirm({ id: requestId(), action: "core-upgrade", ...core })}>仅升级 {core.core === "xray" ? "Xray" : "Mihomo"}</Button>
          <Button variant="outline" disabled={busy || !!state.core_maintenance?.blocked || !state.core_maintenance?.history[core.core]?.safe}
            onClick={() => setConfirm({ id: requestId(), action: "core-rollback", core: core.core })}>恢复上一版 {state.core_maintenance?.history[core.core]?.version}</Button>
        </div>
        {error && <p role="alert" className="text-sm text-red-600">{error}</p>}
        {connection && <p role="status" className="text-sm text-amber-700">{connection}</p>}
        {task && (
          <div role="status" className="space-y-2 rounded-lg border p-4 text-sm">
            <p>{task.action === "upgrade" ? `升级 ${task.version || ""}` : "维护任务"}：{statuses[task.state] || task.state}</p>
            <p className="break-all text-xs text-slate-500">任务 {task.id}</p>
            {task.error && <p className="text-red-600">{task.error}</p>}
            {["failed", "unknown"].includes(task.state) && state.core_maintenance?.pending && state.core_maintenance.pending !== task.id &&
              <Button variant="outline" onClick={() => { setTask(undefined); setTaskId(state.core_maintenance!.pending!); localStorage.setItem(storageKey, state.core_maintenance!.pending!); }}>返回待验证内核任务</Button>}
            {task.detail && <p>{task.detail}</p>}
            {task.core_version && <p>{task.core} 目标版本：{task.core_version}</p>}
            {task.core && (task.state === "awaiting_verification" || state.core_maintenance?.pending === task.id) && <div className="space-y-2">
              <p>请使用升级前的原链接验证连接、实际传输及需要的 UDP 功能；未验证不能标记成功。</p>
              <div className="flex flex-wrap gap-2">
                <Button disabled={submitting} onClick={() => setConfirm({ id: requestId(), action: "core-confirm", core: task.core as CoreChoice["core"], task_id: task.id })}>原链接已验证通过</Button>
                <Button variant="outline" disabled={submitting} onClick={() => setConfirm({ id: requestId(), action: "core-reject", core: task.core as CoreChoice["core"], task_id: task.id })}>连接失败，恢复或停用</Button>
              </div>
            </div>}
            {task.after && <p>安装版本：{Object.entries(task.after).map(([role, version]) => `${role} ${version}`).join(" · ")}</p>}
            {task.state === "succeeded" && task.action === "upgrade" && (
              <Button variant="outline" onClick={() => location.reload()}>刷新页面使用新版界面</Button>
            )}
          </div>
        )}
        {confirm && (
          <Modal title={confirm.action.startsWith("core-") ? `${confirm.core} · ${{"core-upgrade":"升级到 " + confirm.version,"core-rollback":"恢复上一版","core-confirm":"确认原链接与实际传输正常","core-reject":"报告连接失败"}[confirm.action]}` : confirm.action === "upgrade" ? `升级到 ${confirm.version}` : "整套重启当前服务器"}
            description={confirm.action.startsWith("core-") ? "内核操作保持 UUID、密钥和链接参数。升级及恢复会重启代理；安全更新没有安全回退版本时会停用代理并保留面板。确认通过表示你已完成原链接的实际传输验证。" : `会自动重启本项目相关服务，代理连接将中断。${confirm.xray_channel === "latest" ? "本次选择官方最新 Xray，包含预发布版本，现有客户端可能需要升级。" : ""}面板恢复后可能需要重新登录，回到此处可继续查看原任务。`}
            submit="确认执行" close={() => setConfirm(undefined)} action={async () => {
              const request = confirm;
              localStorage.setItem(storageKey, request.id);
              setSubmitting(true);
              setTask({ ...request, state: "queued" });
              setTaskId(request.id);
              setConfirm(undefined);
              setError("");
              try {
                setTask(await api<Task>("/federation/task", "POST", request));
              } catch (e) {
                if (e instanceof ApiError && e.status >= 400 && e.status < 500) {
                  setTask({ ...request, state: "failed", error: errorText(e) });
                } else {
                  setConnection("提交结果尚未确认，正在查询原任务；不会重复执行。");
                }
              } finally {
                setSubmitting(false);
              }
            }} />
        )}
      </CardContent>
    </Card>
  );
}
