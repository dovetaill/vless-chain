import { useCallback, useEffect, useState } from "react";
import { ArrowUpCircle, CheckCircle2, Monitor, Moon, RefreshCw, SlidersHorizontal, Sun } from "lucide-react";
import { api, ApiError } from "../api";
import type { PanelState } from "../types";
import { usePreferences, workspaceModes, type ThemeSetting, type WorkspaceMode } from "../lib/preferences";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardHeading, CardTitle, CardToolbar } from "./ui/card";
import { InfoTip, Modal, Status, errorText, stamp } from "./shared";
import {
  coreDecisionId,
  forgetFailedDecision,
  requestId,
  statuses,
} from "./fleet-state";
import {
  NetworkTuning,
  type NetworkRequest,
  type NetworkStatus,
} from "./network-tuning";
import { upgradeVersionReached } from "./maintenance-state";
import { CoreSelector, initialCoreChoice, type CoreChoice } from "./core-selector";

type Release = { version: string };
type Task = {
  id: string;
  action?: string;
  version?: string;
  state: string;
  detail?: string;
  error?: string;
  core?: string;
  task_id?: string;
  core_version?: string;
  before?: Record<string, string>;
  finished?: number;
  after?: Record<string, string>;
  network?: { after: NetworkStatus };
  verification?: { state: string; detail: string };
};

const storageKey = "chain-maintenance-task";

export function Maintenance({ state }: { state: PanelState }) {
  const { theme, setTheme, mode, setMode } = usePreferences();
  const [release, setRelease] = useState<Release>();
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const [connection, setConnection] = useState("");
  const [taskId, setTaskId] = useState(() => localStorage.getItem(storageKey));
  const [task, setTask] = useState<Task>();
  const [submitting, setSubmitting] = useState(false);
  const [coreOpen, setCoreOpen] = useState(false);
  const [coreChoice, setCoreChoice] = useState<CoreChoice>(initialCoreChoice);
  const [networkOpen, setNetworkOpen] = useState(false);
  const [confirm, setConfirm] = useState<{
    id: string;
    action: string;
    version?: string;
    core?: string;
    task_id?: string;
    prerelease?: boolean;
    security_update?: boolean;
    rollback_safe?: boolean;
  }>();

  const busy =
    submitting ||
    (!!taskId &&
      (!task ||
        ["queued", "running"].includes(task.state)));
  const awaitingCore =
    task?.state === "awaiting_verification" ||
    Boolean(state.core_maintenance?.pending);

  useEffect(() => {
    const pending = state.core_maintenance?.pending;
    if (
      !pending ||
      submitting ||
      taskId === pending ||
      (taskId && (!task || task.task_id === pending))
    )
      return;
    setTask(undefined);
    setTaskId(pending);
    localStorage.setItem(storageKey, pending);
  }, [state.core_maintenance?.pending, submitting, taskId, task]);

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

  useEffect(() => {
    void fetchRelease();
  }, [fetchRelease]);

  useEffect(() => {
    if (
      submitting ||
      !taskId ||
      (task &&
        !["queued", "running", "awaiting_verification"].includes(task.state))
    )
      return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const result = await api<Task>(
          `/federation/task?id=${encodeURIComponent(taskId)}`,
        );
        if (stopped) return;
        forgetFailedDecision(result);
        setConnection("");
        setTask(
          result.state === "not_found"
            ? {
                id: taskId,
                state: "unknown",
                error: "未找到原任务记录，请核对当前版本和服务状态后再操作。",
              }
            : result,
        );
      } catch (e) {
        if (!stopped)
          setConnection(`暂时无法查询，恢复连接后继续查询原任务：${errorText(e)}`);
      }
      if (!stopped) timer = setTimeout(() => void poll(), 3000);
    };
    void poll();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  }, [taskId, task?.state, submitting]);

  async function submit(request: NonNullable<typeof confirm> | NetworkRequest) {
    localStorage.setItem(storageKey, request.id);
    setSubmitting(true);
    setTask({ ...request, state: "queued" });
    setTaskId(request.id);
    setConfirm(undefined);
    setNetworkOpen(false);
    setCoreOpen(false);
    setError("");
    try {
      const result = await api<Task>("/federation/task", "POST", request);
      forgetFailedDecision(result);
      localStorage.setItem(storageKey, result.id);
      setTaskId(result.id);
      setTask(result);
    } catch (e) {
      if (
        e instanceof ApiError &&
        e.status === 409 &&
        "task_id" in request &&
        request.task_id
      ) {
        try {
          const original = await api<Task>(
            `/federation/task?id=${encodeURIComponent(request.task_id)}`,
          );
          if (original.state !== "not_found") {
            localStorage.setItem(storageKey, original.id);
            setTaskId(original.id);
            setTask(original);
            setConnection("");
            return;
          }
        } catch {
          localStorage.setItem(storageKey, request.task_id);
          setTaskId(request.task_id);
          setTask(undefined);
          setConnection("提交结果尚未确认，正在查询原任务；不会重复执行。");
          return;
        }
      }
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
  const upToDate =
    !!release &&
    state.version === release.version &&
    (!state.local_node_id || state.agent_version === release.version);
  const historicalUpgrade = !!task && upgradeVersionReached(task, state);
  const TaskContainer = historicalUpgrade ? "details" : "div";

  return (
    <>
      <Card role="region" aria-label="版本与维护">
        <CardHeader>
          <CardHeading className="flex items-center gap-1.5 space-y-0">
            <CardTitle>版本与维护</CardTitle>
            <InfoTip>
              程序更新仅升级面板与 Agent，保留当前独立选择的代理内核版本；重启期间现有连接可能短暂重连。
            </InfoTip>
          </CardHeading>
          <CardToolbar>
            <Button
              variant="ghost"
              mode="icon"
              aria-label="刷新版本信息"
              title="刷新版本信息"
              disabled={checking || busy}
              onClick={() => void fetchRelease()}
            >
              <RefreshCw
                aria-hidden="true"
                className={checking ? "animate-spin" : ""}
              />
            </Button>
          </CardToolbar>
        </CardHeader>

        <CardContent className="space-y-4">
          <div className="flex flex-col gap-4 rounded-xl border border-border bg-muted/25 p-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-3.5">
              <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-primary/12 text-primary">
                {upToDate ? (
                  <CheckCircle2 aria-hidden="true" className="size-5.5" />
                ) : (
                  <ArrowUpCircle aria-hidden="true" className="size-5.5" />
                )}
              </div>
              <div>
                <p className="text-xs text-muted-foreground">本机面板版本</p>
                <p className="text-xl font-semibold tracking-tight tabular-nums text-foreground">
                  {state.version ? `v${state.version}` : "版本未知"}
                </p>
                <p role="status" className="mt-0.5 text-xs text-muted-foreground">
                  {checking
                    ? "正在检查更新…"
                    : upToDate
                      ? "已是最新正式版"
                      : release
                        ? `可升级至 v${release.version}`
                        : "暂时无法检查更新"}
                </p>
              </div>
            </div>
            <Button
              disabled={!supported || !release || checking || busy || upToDate}
              onClick={() =>
                void submit({
                  id: requestId(),
                  action: "upgrade",
                  version: release!.version,
                })
              }
            >
              {submitting || (task?.action === "upgrade" && busy)
                ? "正在升级…"
                : "升级并重启"}
            </Button>
          </div>

          <dl
            aria-label="本机组件版本"
            className="grid grid-cols-3 gap-3 text-xs"
          >
            {[
              ["Agent", state.agent_version],
              [
                "Xray",
                state.core_maintenance?.versions.xray || state.cores?.xray,
              ],
              [
                "Mihomo",
                state.core_maintenance?.versions.mihomo || state.cores?.mihomo,
              ],
            ].map(([name, version]) => (
              <div
                key={name}
                className="min-w-0 rounded-lg border border-border bg-card p-3"
              >
                <dt className="text-muted-foreground">{name}</dt>
                <dd className="mt-1 break-all text-sm font-semibold tabular-nums text-foreground">
                  {version || "尚未上报"}
                </dd>
              </div>
            ))}
          </dl>

          {state.agent_version && state.version !== state.agent_version && (
            <div>
              <Status tone="warn">面板与 Agent 版本不一致</Status>
            </div>
          )}

          {!supported && (
            <p
              role="status"
              className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-xs text-foreground"
            >
              当前部署未提供网页维护。Linux systemd / OpenRC 请先通过命令行升级以启用网页维护组件。
            </p>
          )}

          {state.core_maintenance?.blocked && (
            <p
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-xs font-medium text-destructive"
            >
              安全更新未完成，本机代理已停用；管理面板仍可使用，请先恢复安全内核。
            </p>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              disabled={
                busy || awaitingCore || !state.maintenance_actions?.includes("core-upgrade")
              }
              onClick={() => setCoreOpen(true)}
            >
              代理内核
            </Button>
            <Button
              variant="outline"
              disabled={
                busy ||
                !state.maintenance_actions?.includes("network-tune")
              }
              onClick={() => setNetworkOpen(true)}
            >
              网络设置 / BBR
            </Button>
          </div>

          <details className="details">
            <summary>其他维护</summary>
            <div className="mt-3.5 space-y-2.5">
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={
                    !supported ||
                    !state.maintenance_actions?.includes("restart-all") ||
                    busy
                  }
                  onClick={() =>
                    setConfirm({ id: requestId(), action: "restart-all" })
                  }
                >
                  整套重启
                </Button>
                {state.agent_seen ? (
                  <span className="text-xs text-muted-foreground tabular-nums">
                    Agent 最近心跳：{stamp(state.agent_seen)}
                  </span>
                ) : null}
              </div>
              {!state.maintenance_actions?.includes("network-tune") && (
                <p className="muted">网络设置需要本机安装新版维护组件。</p>
              )}
            </div>
          </details>

          {error && (
            <p role="alert" className="text-xs text-destructive">
              {error}
            </p>
          )}
          {connection && (
            <p role="status" className="text-xs text-amber-600 dark:text-amber-400">
              {connection}
            </p>
          )}

          {task && (
            <TaskContainer
              role={historicalUpgrade ? undefined : "status"}
              className={
                historicalUpgrade
                  ? "details text-xs"
                  : "rounded-xl border border-border bg-muted/20 p-4 text-xs"
              }
            >
              {historicalUpgrade && (
                <summary className="text-muted-foreground">
                  历史升级记录 · 当前组件版本已更新
                </summary>
              )}
              <div className={historicalUpgrade ? "mt-3.5 space-y-2.5" : "space-y-2.5"}>
                <p className="font-semibold text-foreground">
                  {task.action === "upgrade"
                    ? `升级 ${task.version || ""}`
                    : "维护任务"}
                  ：{statuses[task.state] || task.state}
                </p>
                {task.error && <p className="text-destructive">{task.error}</p>}
                {["failed", "unknown"].includes(task.state) &&
                  state.core_maintenance?.pending &&
                  state.core_maintenance.pending !== task.id && (
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => {
                        setTask(undefined);
                        setTaskId(state.core_maintenance!.pending!);
                        localStorage.setItem(
                          storageKey,
                          state.core_maintenance!.pending!,
                        );
                      }}
                    >
                      返回待验证内核任务
                    </Button>
                  )}
                {(task.verification?.detail || task.detail) && (
                  <p className="text-muted-foreground">
                    {task.verification?.detail || task.detail}
                  </p>
                )}
                {task.network && (
                  <div className="space-y-1 text-muted-foreground">
                    <p>
                      系统默认：{task.network.after.congestion_control} +{" "}
                      {task.network.after.qdisc}
                    </p>
                    <p>
                      网卡队列：
                      {task.network.after.interfaces
                        .map((q) => `${q.dev} / ${q.parent}: ${q.kind}`)
                        .join("；") || "未知"}
                    </p>
                    {task.network.after.warning && (
                      <p className="text-amber-600 dark:text-amber-400">
                        {task.network.after.warning}
                      </p>
                    )}
                  </div>
                )}
                {task.core_version && (
                  <p className="text-muted-foreground">
                    {task.core} 目标版本：{task.core_version}
                  </p>
                )}
                {task.state === "awaiting_verification" &&
                  task.verification &&
                  task.verification.state !== "manual" && (
                    <p>正在后台自动验收，可关闭此页面。</p>
                  )}
                {task.core &&
                  ["failed", "unknown"].includes(task.state) &&
                  state.core_maintenance?.pending === task.id && (
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={submitting}
                      onClick={() =>
                        setConfirm({
                          id: coreDecisionId(task.id, "core-reject"),
                          action: "core-reject",
                          core: task.core,
                          task_id: task.id,
                        })
                      }
                    >
                      恢复中断的内核切换
                    </Button>
                  )}
                {task.core &&
                  task.state === "awaiting_verification" &&
                  (!task.verification ||
                    task.verification.state === "manual") && (
                    <div className="space-y-2 pt-1">
                      <p>
                        请使用升级前的原链接验证连接、实际传输及所需的 UDP 功能。
                      </p>
                      <div className="flex flex-wrap gap-2">
                        <Button
                          size="sm"
                          disabled={submitting}
                          onClick={() =>
                            setConfirm({
                              id: coreDecisionId(task.id, "core-confirm"),
                              action: "core-confirm",
                              core: task.core,
                              task_id: task.id,
                            })
                          }
                        >
                          原链接已验证通过
                        </Button>
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={submitting}
                          onClick={() =>
                            setConfirm({
                              id: coreDecisionId(task.id, "core-reject"),
                              action: "core-reject",
                              core: task.core,
                              task_id: task.id,
                            })
                          }
                        >
                          报告链接验证失败
                        </Button>
                      </div>
                    </div>
                  )}
                <details className="details details-compact">
                  <summary className="text-muted-foreground">
                    任务详情
                  </summary>
                  <div className="mt-2.5 space-y-1.5 leading-relaxed text-muted-foreground">
                    <p className="mono break-all text-[11px]">
                      任务 ID: {task.id}
                    </p>
                    {task.after && (
                      <p>
                        已安装：
                        {Object.entries(task.after)
                          .map(([role, version]) => `${role} ${version}`)
                          .join(" · ")}
                      </p>
                    )}
                  </div>
                </details>
                {task.state === "succeeded" && task.action === "upgrade" && (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => location.reload()}
                  >
                    刷新页面使用新版界面
                  </Button>
                )}
              </div>
            </TaskContainer>
          )}

          {coreOpen && (
            <Modal
              title="更新本机代理内核"
              close={() => setCoreOpen(false)}
              submit={
                !busy && coreChoice.version.trim()
                  ? `更新 ${coreChoice.core} ${coreChoice.version.trim()}`
                  : undefined
              }
              action={() =>
                submit({
                  id: requestId(),
                  action: "core-upgrade",
                  ...coreChoice,
                  version: coreChoice.version.trim(),
                })
              }
            >
              <CoreSelector
                value={coreChoice}
                onChange={setCoreChoice}
                disabled={busy}
              />
            </Modal>
          )}

          {networkOpen && (
            <NetworkTuning
              targets={[{ id: "local", name: "本机", snapshot: state }]}
              close={() => setNetworkOpen(false)}
              submitLocal={submit}
            />
          )}

          {confirm && (
            <Modal
              title={
                confirm.action.startsWith("core-")
                  ? "处理待验证内核任务"
                  : "整套重启当前服务器"
              }
              description={
                confirm.action === "core-confirm"
                  ? "确认已用升级前的原链接完成实际传输验证。"
                  : confirm.action === "core-reject" &&
                      task?.state === "awaiting_verification"
                    ? "将此次验收标记为未通过，Agent 与现有服务保持运行。"
                    : confirm.action === "core-reject"
                      ? "恢复中断的内核切换；缺少安全回退版本时隔离代理内核并保留 Agent 心跳。"
                      : "重启面板与 Agent 服务，代理连接将短暂重连。"
              }
              submit="确认执行"
              close={() => setConfirm(undefined)}
              action={() => submit(confirm)}
            />
          )}
        </CardContent>
      </Card>

      <Card>
          <CardHeader>
            <CardHeading className="flex items-center gap-2 space-y-0">
              <SlidersHorizontal className="size-4 text-primary" />
              <CardTitle>界面与导航偏好</CardTitle>
            </CardHeading>
          </CardHeader>
          <CardContent className="grid gap-5 md:grid-cols-2">
            <div className="field">
              <span>外观主题</span>
              <div className="grid grid-cols-3 gap-2">
                {(
                  [
                    ["light", "浅色", Sun],
                    ["dark", "深色", Moon],
                    ["system", "跟随系统", Monitor],
                  ] as const
                ).map(([id, label, Icon]) => (
                  <button
                    key={id}
                    type="button"
                    onClick={() => setTheme(id as ThemeSetting)}
                    className={`flex cursor-pointer items-center justify-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium transition-colors ${
                      theme === id
                        ? "border-primary bg-primary/10 text-primary"
                        : "border-border bg-card text-muted-foreground hover:bg-accent hover:text-foreground"
                    }`}
                  >
                    <Icon className="size-3.5" />
                    {label}
                  </button>
                ))}
              </div>
            </div>

            <div className="field">
              <label htmlFor="pref-workspace-mode">
                使用模式
              </label>
              <select
                id="pref-workspace-mode"
                aria-label="使用模式"
                className="select"
                value={mode}
                onChange={(e) => setMode(e.target.value as WorkspaceMode)}
              >
                {workspaceModes.map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.shortLabel} — {item.description}
                  </option>
                ))}
              </select>
            </div>
          </CardContent>
      </Card>
    </>
  );
}
