import { useCallback, useEffect, useState } from "react";
import { api, ApiError } from "../api";
import type { PanelState } from "../types";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Modal, Status, errorText, stamp } from "./shared";
import { requestId, statuses } from "./fleet-state";

type Release = { version: string; xray: string; mihomo: string };
type Task = {
  id: string;
  action?: string;
  version?: string;
  state: string;
  detail?: string;
  error?: string;
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
  const [xrayChannel, setXrayChannel] = useState("recommended");
  const [confirm, setConfirm] = useState<{ id: string; action: string; version?: string; xray_channel?: string }>();
  const busy = submitting || (!!taskId && (!task || ["queued", "running"].includes(task.state)));
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
    if (submitting || !taskId || (task && !["queued", "running"].includes(task.state))) return;
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
          <dt>Xray（发行包）</dt><dd>{state.cores?.xray || "未知"}</dd>
          <dt>Mihomo（发行包）</dt><dd>{state.cores?.mihomo || "未知"}</dd>
        </dl>
        {state.cores?.xray_channel === "latest" && <p className="text-sm">当前 Xray 来源：官方最新版{state.cores.xray_prerelease ? "（预发布）" : ""}</p>}
        {!!state.agent_seen && <p className="muted">Agent 最近上报：{stamp(state.agent_seen)}</p>}
        {state.agent_version && state.version !== state.agent_version && (
          <Status tone="warn">面板与 Agent 版本不一致</Status>
        )}
        <p className="text-sm">
          {release ? `最新正式 Release：${release.version} · Xray ${release.xray || "未知"} · Mihomo ${release.mihomo || "未知"}` : "尚未获取最新正式 Release"}
        </p>
        <label className="block space-y-2 text-sm">
          <span>本次升级的 Xray 内核</span>
          <select className="w-full rounded-md border bg-background p-2" value={xrayChannel}
            disabled={busy} onChange={e => setXrayChannel(e.target.value)}>
            <option value="recommended">推荐稳定版（随发行包，含本项目修复补丁）</option>
            <option value="latest">官方最新版（含预发布，可能要求更新客户端）</option>
          </select>
        </label>
        <p className="muted">默认使用发行包验证过的内核。选择官方最新版仅作用于本次主动升级，不会后台自动更换；可重新选择推荐稳定版并升级，恢复推荐内核。</p>
        {!supported && (
          <p role="status" className="text-sm text-amber-700">
            当前部署未提供网页维护。仅支持 Linux systemd；该平台请先手工升级以安装维护组件。OpenRC、macOS 和纯 Agent 安装暂不支持。
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" disabled={checking || busy} onClick={() => void fetchRelease()}>
            {checking ? "正在检查…" : "检查最新版本"}
          </Button>
          <Button disabled={!supported || !release || checking || busy}
            onClick={() => setConfirm({ id: requestId(), action: "upgrade", version: release!.version, xray_channel: xrayChannel })}>
            升级并自动重启
          </Button>
          <Button variant="outline" disabled={!supported || !state.maintenance_actions?.includes("restart-all") || busy}
            onClick={() => setConfirm({ id: requestId(), action: "restart-all" })}>
            整套重启
          </Button>
        </div>
        <p className="muted">自动处理已安装的面板、Agent 及其代理内核、项目独立 HTTPS 服务。重启会中断本机连接，配置和业务数据保留。</p>
        {error && <p role="alert" className="text-sm text-red-600">{error}</p>}
        {connection && <p role="status" className="text-sm text-amber-700">{connection}</p>}
        {task && (
          <div role="status" className="space-y-2 rounded-lg border p-4 text-sm">
            <p>{task.action === "upgrade" ? `升级 ${task.version || ""}` : "维护任务"}：{statuses[task.state] || task.state}</p>
            <p className="break-all text-xs text-slate-500">任务 {task.id}</p>
            {task.error && <p className="text-red-600">{task.error}</p>}
            {task.detail && <p>{task.detail}</p>}
            {task.after && <p>安装版本：{Object.entries(task.after).map(([role, version]) => `${role} ${version}`).join(" · ")}</p>}
            {task.state === "succeeded" && task.action === "upgrade" && (
              <Button variant="outline" onClick={() => location.reload()}>刷新页面使用新版界面</Button>
            )}
          </div>
        )}
        {confirm && (
          <Modal title={confirm.action === "upgrade" ? `升级到 ${confirm.version}` : "整套重启当前服务器"}
            description={`会自动重启本项目相关服务，代理连接将中断。${confirm.xray_channel === "latest" ? "本次选择官方最新 Xray，包含预发布版本，现有客户端可能需要升级。" : ""}面板恢复后可能需要重新登录，回到此处可继续查看原任务。`}
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
