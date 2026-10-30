import { useCallback, useEffect, useState } from "react";
import { ArrowUpCircle, CheckCircle2, RefreshCw } from "lucide-react";
import { api, ApiError } from "../api";
import type { PanelState } from "../types";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Modal, Status, errorText, stamp } from "./shared";
import { requestId, statuses } from "./fleet-state";
import { NetworkTuning, type NetworkRequest, type NetworkStatus } from "./network-tuning";

type Release = { version: string };
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
  network?: { after: NetworkStatus };
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
  const [networkOpen, setNetworkOpen] = useState(false);
  const [confirm, setConfirm] = useState<{ id: string; action: string; version?: string; core?: string; task_id?: string }>();
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
  async function submit(request: NonNullable<typeof confirm> | NetworkRequest) {
    localStorage.setItem(storageKey, request.id);
    setSubmitting(true);
    setTask({ ...request, state: "queued" });
    setTaskId(request.id);
    setConfirm(undefined);
    setNetworkOpen(false);
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
  }
  const supported = !!state.maintenance;
  const networkAdmin = !state.viewer || state.viewer.role === "admin";
  const upToDate = !!release && state.version === release.version && (!state.agent_version || state.agent_version === release.version);
  return (
    <Card role="region" aria-label="版本与维护">
      <CardHeader>
        <CardTitle>版本与维护</CardTitle>
        <Button variant="ghost" mode="icon" aria-label="刷新版本信息" title="刷新版本信息"
          disabled={checking || busy} onClick={() => void fetchRelease()}>
          <RefreshCw aria-hidden="true" className={checking ? "animate-spin" : ""} />
        </Button>
      </CardHeader>
      <CardContent className="space-y-5">
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
              {upToDate ? <CheckCircle2 aria-hidden="true" className="size-6" /> : <ArrowUpCircle aria-hidden="true" className="size-6" />}
            </div>
            <div>
              <p className="text-xs text-muted-foreground">本机面板</p>
              <p className="text-2xl font-semibold tracking-tight tabular-nums">{state.version ? `v${state.version}` : "版本未知"}</p>
              <p role="status" className="mt-1 text-sm text-muted-foreground">
                {checking ? "正在检查更新…" : upToDate ? "已是最新正式版" : release ? `可升级至 v${release.version}` : "暂时无法检查更新"}
              </p>
            </div>
          </div>
          <Button disabled={!supported || !release || checking || busy || upToDate}
            onClick={() => void submit({ id: requestId(), action: "upgrade", version: release!.version })}>
            {submitting || (task?.action === "upgrade" && busy) ? "正在升级…" : "升级并重启"}
          </Button>
        </div>
        <dl aria-label="本机组件版本" className="grid grid-cols-3 gap-3 rounded-lg border bg-muted/30 p-3 text-sm">
          {[
            ["Agent", state.agent_version],
            ["Xray", state.core_maintenance?.versions.xray || state.cores?.xray],
            ["Mihomo", state.core_maintenance?.versions.mihomo || state.cores?.mihomo],
          ].map(([name, version]) => <div key={name} className="min-w-0">
            <dt className="text-xs text-muted-foreground">{name}</dt>
            <dd className="mt-1 break-all font-medium tabular-nums">{version || "尚未上报"}</dd>
          </div>)}
        </dl>
        {state.agent_version && state.version !== state.agent_version && <Status tone="warn">面板与 Agent 版本不一致</Status>}
        <p className="text-xs text-muted-foreground">按 GitHub 最新正式发行版统一更新本机组件。重启会短暂断开连接，配置和业务数据保留。</p>
        {!supported && (
          <p role="status" className="text-sm text-amber-700">
            当前部署未提供网页维护。Linux systemd / OpenRC（含 Alpine）请先手工升级以安装维护组件；macOS 和纯 Agent 安装暂不支持。
          </p>
        )}
        {state.core_maintenance?.blocked && <p role="alert" className="text-red-600">安全更新未完成，本机代理已停用；管理面板仍可使用。请先恢复安全内核。</p>}
        <details className="border-t pt-3 text-sm">
          <summary className="cursor-pointer text-muted-foreground">其他维护</summary>
          <div className="mt-3 flex flex-wrap gap-2">
            <Button variant="outline" disabled={!supported || !state.maintenance_actions?.includes("restart-all") || busy}
              onClick={() => setConfirm({ id: requestId(), action: "restart-all" })}>整套重启</Button>
            <Button variant="outline" disabled={!networkAdmin || busy || !state.maintenance_actions?.includes("network-tune")}
              onClick={() => setNetworkOpen(true)}>网络算法 / BBR</Button>
          </div>
          <p className="mt-2 text-xs text-muted-foreground">仅操作本机；其他 VPS 请前往多机总控。{state.agent_seen ? `Agent 最近上报：${stamp(state.agent_seen)}` : ""}</p>
          {!networkAdmin ? <p className="muted">网络算法设置仅管理员可操作。</p>
            : !state.maintenance_actions?.includes("network-tune") && <p className="muted">网络设置需要本机安装新版维护组件。</p>}
        </details>
        {error && <p role="alert" className="text-sm text-red-600">{error}</p>}
        {connection && <p role="status" className="text-sm text-amber-700">{connection}</p>}
        {task && (
          <div role="status" className="space-y-2 rounded-lg border p-4 text-sm">
            <p>{task.action === "upgrade" ? `升级 ${task.version || ""}` : "维护任务"}：{statuses[task.state] || task.state}</p>
            {task.error && <p className="text-red-600">{task.error}</p>}
            {["failed", "unknown"].includes(task.state) && state.core_maintenance?.pending && state.core_maintenance.pending !== task.id &&
              <Button variant="outline" onClick={() => { setTask(undefined); setTaskId(state.core_maintenance!.pending!); localStorage.setItem(storageKey, state.core_maintenance!.pending!); }}>返回待验证内核任务</Button>}
            {task.detail && <p>{task.detail}</p>}
            {task.network && <div className="space-y-1">
              <p>系统默认：{task.network.after.congestion_control} + {task.network.after.qdisc}</p>
              <p>网卡实际队列：{task.network.after.interfaces.map(q => `${q.dev} / ${q.parent}: ${q.kind}`).join("；") || "未知"}</p>
              {task.network.after.warning && <p className="text-amber-700">{task.network.after.warning}</p>}
            </div>}
            {task.core_version && <p>{task.core} 目标版本：{task.core_version}</p>}
            {task.core && (task.state === "awaiting_verification" || state.core_maintenance?.pending === task.id) && <div className="space-y-2">
              <p>请使用升级前的原链接验证连接、实际传输及需要的 UDP 功能；未验证不能标记成功。</p>
              <div className="flex flex-wrap gap-2">
                <Button disabled={submitting} onClick={() => setConfirm({ id: requestId(), action: "core-confirm", core: task.core, task_id: task.id })}>原链接已验证通过</Button>
                <Button variant="outline" disabled={submitting} onClick={() => setConfirm({ id: requestId(), action: "core-reject", core: task.core, task_id: task.id })}>连接失败，恢复或停用</Button>
              </div>
            </div>}
            <details>
              <summary className="cursor-pointer text-slate-500">任务详情</summary>
              <p className="break-all text-xs text-slate-500">任务 {task.id}</p>
              {task.after && <p>安装版本：{Object.entries(task.after).map(([role, version]) => `${role} ${version}`).join(" · ")}</p>}
            </details>
            {task.state === "succeeded" && task.action === "upgrade" && (
              <Button variant="outline" onClick={() => location.reload()}>刷新页面使用新版界面</Button>
            )}
          </div>
        )}
        {networkOpen && <NetworkTuning targets={[{ id: "local", name: "本机", snapshot: state }]}
          close={() => setNetworkOpen(false)} submitLocal={submit} />}
        {confirm && (
          <Modal title={confirm.action.startsWith("core-") ? "处理待验证内核任务" : "整套重启当前服务器"}
            description={confirm.action.startsWith("core-") ? "确认通过表示已完成原链接的实际传输验证。连接失败时恢复安全版本或停用代理，管理面板保持可用。" : "会重启本项目相关服务，代理连接将短暂中断。面板恢复后可能需要重新登录，原任务会继续显示。"}
            submit="确认执行" close={() => setConfirm(undefined)} action={() => submit(confirm)} />
        )}
      </CardContent>
    </Card>
  );
}
