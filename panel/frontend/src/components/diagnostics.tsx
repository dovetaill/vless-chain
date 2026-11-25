import { useEffect, useRef, useState } from "react";
import { Clock, Download, Eraser, Pause, Play, RefreshCw } from "lucide-react";
import { api } from "../api";
import { InfoTip, Modal, download, errorText, stamp } from "./shared";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { Input } from "./ui/input";

type Logs = {
  logging_enabled?: boolean;
  retention_hours?: number;
  max_mib?: number;
  clock?: { detail?: string; ntp_enabled?: boolean; timezone?: string };
  text: string;
  sampled_at: number;
  sources: string[];
  detail: string;
};

export function Diagnostics({
  nodes,
  lineId = "",
  canSyncTime = true,
  close,
  request = api,
}: {
  nodes: { id: string; name: string }[];
  lineId?: string;
  canSyncTime?: boolean;
  close: () => void;
  request?: typeof api;
}) {
  const [node, setNode] = useState(
    nodes[0]?.id || "00000000-0000-0000-0000-000000000000",
  );
  const [source, setSource] = useState(lineId && nodes.length ? "xray" : "panel");
  const [filter, setFilter] = useState(lineId);
  const [archive, setArchive] = useState(false);
  const [follow, setFollow] = useState(true);
  const [data, setData] = useState<Logs>();
  const [cleared, setCleared] = useState<{
    previous: string[];
    lines: string[];
  } | null>(null);
  const [error, setError] = useState("");
  const [target, setTarget] = useState("https://example.com/");
  const [diagnosing, setDiagnosing] = useState(false);
  const [diagnostic, setDiagnostic] = useState("");
  const [task, setTask] = useState<string>();
  const [taskMessage, setTaskMessage] = useState("");
  const [taskFailed, setTaskFailed] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [loading, setLoading] = useState(false);
  const [savingLogs, setSavingLogs] = useState(false);
  const [retentionValue, setRetentionValue] = useState<string>();
  const [retentionUnit, setRetentionUnit] = useState(1);
  const [sizeValue, setSizeValue] = useState<string>();

  useEffect(() => {
    setRetentionValue(undefined);
    setRetentionUnit(1);
    setSizeValue(undefined);
  }, [node, source]);

  const requester = useRef(request);
  requester.current = request;
  const inFlight = useRef<Promise<unknown>>(Promise.resolve());
  const [tick, setTick] = useState(0);
  const following = useRef(follow);
  following.current = follow;

  useEffect(() => {
    setCleared(null);
  }, [node, source, filter, archive]);

  useEffect(() => {
    if (!node || diagnosing || savingLogs) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    setData(undefined);
    setError("");
    async function refresh() {
      await inFlight.current;
      if (stopped) return;
      setLoading(true);
      const pending = requester.current<Logs>(
        `/diagnostics/${node}/logs`,
        "POST",
        { source, line_id: filter, archive },
      );
      inFlight.current = pending.catch(() => undefined);
      try {
        const value = await pending;
        if (!stopped) {
          setData(value);
          setError("");
          setCleared((previous) => {
            if (!previous) return null;
            const lines = value.text ? value.text.split("\n") : [];
            let overlap = Math.min(previous.previous.length, lines.length);
            while (
              overlap &&
              !previous.previous
                .slice(-overlap)
                .every((line, i) => line === lines[i])
            )
              overlap--;
            return {
              previous: lines,
              lines: [...previous.lines, ...lines.slice(overlap)].slice(-500),
            };
          });
        }
      } catch (e) {
        if (!stopped) setError(errorText(e));
      } finally {
        if (!stopped) setLoading(false);
      }
      if (!stopped && following.current)
        timer = setTimeout(() => {
          if (following.current) void refresh();
        }, 3000);
    }
    void refresh();
    return () => {
      stopped = true;
      clearTimeout(timer);
      setLoading(false);
    };
  }, [node, source, filter, archive, tick, diagnosing, savingLogs]);

  useEffect(() => {
    if (!task) return;
    let stopped = false;
    let failures = 0;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const value = await requester.current<{
          state: string;
          detail?: string;
          error?: string;
        }>("/federation/task", "POST", { action: "status", id: task });
        if (stopped) return;
        failures = 0;
        setTaskFailed(value.state === "failed");
        setTaskMessage(
          [
            value.error,
            value.detail && !value.error?.includes(value.detail)
              ? value.detail
              : "",
          ]
            .filter(Boolean)
            .join("\n") ||
            ({
              queued: "校时任务排队中",
              running: "正在配置并同步系统时钟…",
              succeeded: "自动校时已完成",
              failed: "自动校时失败",
            }[value.state] || `校时任务：${value.state}`),
        );
        if (["succeeded", "failed"].includes(value.state)) {
          setTask(undefined);
          setTick((v) => v + 1);
        } else timer = setTimeout(() => void poll(), 3000);
      } catch (e) {
        if (!stopped) {
          failures += 1;
          if (failures < 10) {
            setTaskMessage(
              `暂时无法读取校时任务结果（${errorText(e)}），正在按原任务继续查询…`,
            );
            timer = setTimeout(() => void poll(), 3000);
          } else {
            setTaskFailed(true);
            setTaskMessage(
              `无法确认校时任务结果：${errorText(e)}；请刷新日志与时钟状态核对。`,
            );
            setTask(undefined);
          }
        }
      }
    }
    void poll();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  }, [task]);

  async function diagnose() {
    setDiagnosing(true);
    setFollow(false);
    setDiagnostic("正在分段检查…");
    try {
      await inFlight.current;
      const result = await requester.current<{
        stage: string;
        result: { ok: boolean; detail: string };
        note: string;
      }>(`/diagnostics/${node}/path`, "POST", { line_id: lineId, url: target });
      let text = `${result.stage}：${result.result.ok ? "通过" : "失败"}\n${result.result.detail}\n${result.note}`;
      if (result.result.ok) {
        const full = await requester.current<{ ok: boolean; detail: string }>(
          `/lines/${lineId}/probe`,
          "POST",
          { mode: "http", url: target },
        );
        text += `\n完整链路（面板发起）${full.ok ? "通过" : "失败"}：${full.detail}`;
        if (!full.ok)
          text +=
            "\n出口已通过，故障范围缩小到入口、REALITY/TLS 握手或本机转发；请查看对应节点日志。";
      } else
        text +=
          "\n入口尚未验证。先按出口日志检查证书时间、SNI、认证或后续节点。";
      setDiagnostic(text);
    } catch (e) {
      setDiagnostic(errorText(e));
    } finally {
      setDiagnosing(false);
    }
  }

  async function saveLogging(enabled?: boolean) {
    if (data?.logging_enabled === undefined) return;
    const retention_hours =
      Number(
        retentionValue ?? String((data.retention_hours || 1) / retentionUnit),
      ) * retentionUnit;
    const max_mib = Number(sizeValue ?? String(data.max_mib || 50));
    if (
      enabled === undefined &&
      (!Number.isInteger(retention_hours) ||
        retention_hours < 1 ||
        retention_hours > 720 ||
        !Number.isInteger(max_mib) ||
        max_mib < 1 ||
        max_mib > 1024)
    ) {
      setError("保留时长须为 1 小时至 30 天，大小上限须为 1 至 1024 MiB 的整数");
      return;
    }
    setSavingLogs(true);
    try {
      await inFlight.current;
      const value = await requester.current<Logs>(
        `/diagnostics/${node}/logs`,
        "POST",
        {
          source,
          line_id: filter,
          archive,
          ...(enabled === undefined ? { retention_hours, max_mib } : { enabled }),
        },
      );
      setData(value);
      setCleared(null);
      setRetentionValue(undefined);
      setSizeValue(undefined);
      setError("");
      setTick((v) => v + 1);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSavingLogs(false);
    }
  }

  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const sourceNames: Record<string, string> = {
    panel: "控制台应用",
    agent: "节点 Agent",
    xray: "Xray 内核",
    mihomo: "Mihomo 内核",
  };
  const visibleLogs = cleared ? cleared.lines.join("\n") : data?.text;
  const readableLogs = visibleLogs?.replace(
    /^(\d{10})\s+(.*)$/gm,
    (_, timestamp: string, body: string) => {
      if (source === "xray")
        body = body.replace(/^\d{4}\/\d{2}\/\d{2} \d{2}:\d{2}:\d{2}(?:\.\d+)?\s+/, "");
      return `${stamp(Number(timestamp))} ${body}`;
    },
  );

  return (
    <Modal wide title="日志与诊断" close={close}>
      <div className="min-w-0 space-y-4">
        <section
          className="space-y-3 rounded-xl border border-border bg-muted/20 p-3.5"
          aria-label="日志筛选"
        >
          <div className="grid gap-3.5 sm:grid-cols-2">
            <label className="field">
              <span>日志来源</span>
              <select
                className="select"
                aria-label="日志来源"
                value={source}
                disabled={diagnosing || savingLogs}
                onChange={(e) => setSource(e.target.value)}
              >
                {Array.from(
                  new Set([
                    "panel",
                    ...(nodes.length ? ["agent", "xray", "mihomo"] : []),
                    ...(data?.sources || []),
                  ]),
                ).map((s) => (
                  <option key={s} value={s}>
                    {sourceNames[s] || `历史内核 · ${s.replace("legacy-", "")}`}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              <span>节点</span>
              <select
                className="select"
                aria-label="日志节点"
                value={node}
                disabled={source === "panel" || diagnosing || savingLogs}
                onChange={(e) => setNode(e.target.value)}
              >
                {!nodes.length && <option value={node}>当前 VPS 控制台</option>}
                {nodes.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </select>
            </label>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border pt-2.5">
            <div className="flex flex-wrap gap-x-4 gap-y-1.5 text-xs">
              {lineId && (
                <label className="flex items-center gap-1.5">
                  <input
                    type="checkbox"
                    disabled={diagnosing}
                    checked={!!filter}
                    onChange={(e) => setFilter(e.target.checked ? lineId : "")}
                  />
                  仅此代理
                </label>
              )}
              <label className="flex items-center gap-1.5">
                <input
                  type="checkbox"
                  disabled={diagnosing}
                  checked={archive}
                  onChange={(e) => setArchive(e.target.checked)}
                />
                包含近期归档
              </label>
            </div>

            <div className="flex flex-wrap items-center gap-1.5">
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={diagnosing}
                onClick={() => {
                  setFollow(!follow);
                  if (!follow) setTick((v) => v + 1);
                }}
              >
                {follow ? <Pause /> : <Play />}
                {follow ? "暂停跟随" : "继续跟随"}
              </Button>
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={loading || diagnosing}
                onClick={() => setTick((v) => v + 1)}
              >
                <RefreshCw className={loading ? "animate-spin" : ""} />
                {loading ? "读取中…" : "刷新日志"}
              </Button>
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={!data}
                onClick={() =>
                  setCleared({
                    previous: data?.text ? data.text.split("\n") : [],
                    lines: [],
                  })
                }
              >
                <Eraser />
                清屏
              </Button>
              {cleared && (
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => setCleared(null)}
                >
                  显示历史
                </Button>
              )}
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={!data}
                onClick={() => data && download(`chain-${source}-logs.txt`, data.text)}
              >
                <Download />
                下载脱敏日志
              </Button>
            </div>
          </div>
        </section>

        <section className="min-w-0 space-y-2" aria-label="日志内容">
          {error && (
            <div
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-xs text-destructive"
            >
              <p className="font-semibold">日志读取失败</p>
              <p className="mt-1 break-words">{error}</p>
            </div>
          )}
          <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
            <div className="flex items-center gap-2">
              <Badge variant="secondary" appearance="light" size="xs">
                {sourceNames[source] || source}
              </Badge>
              <span>{follow ? "每 3 秒自动刷新" : "已暂停自动刷新"}</span>
            </div>
            <span role="status" className="tabular-nums">
              {data
                ? `采样 ${stamp(data.sampled_at)} · 时区 ${localZone}`
                : error
                  ? "未取得日志"
                  : "正在读取…"}
            </span>
          </div>
          <pre
            tabIndex={0}
            aria-label="日志记录"
            className="mono max-h-[44vh] min-h-56 overflow-auto whitespace-pre-wrap break-all rounded-xl border border-zinc-800 bg-zinc-950 p-4 text-xs leading-6 text-zinc-100"
          >
            {readableLogs ||
              (cleared
                ? "已清屏，等待新日志写入（可点击「显示历史」恢复视图）。"
                : data
                  ? "当前来源或筛选条件下暂无日志。"
                  : "正在读取日志…")}
          </pre>
        </section>

        <section
          className="space-y-2.5 rounded-xl border border-border bg-muted/15 p-3.5"
          aria-label="VPS 时间同步"
        >
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="flex items-center gap-1.5">
                <Clock className="size-3.5 text-primary" />
                <h3 className="text-xs font-semibold text-foreground">
                  VPS 时间同步
                </h3>
                <InfoTip>
                  校准当前面板所在 VPS 的系统时钟并保留原时区，不影响其他远端节点。
                </InfoTip>
              </div>
              <p className="mt-1 text-xs text-muted-foreground">
                {data?.clock
                  ? `所选节点上报：${data.clock.detail || "时间状态未知"}`
                  : "REALITY / TLS 握手要求两端系统时钟误差在合理范围内。"}
                {data?.clock?.timezone && ` · 时区 ${data.clock.timezone}`}
                {data?.clock?.ntp_enabled === false && " · NTP 未启用"}
              </p>
            </div>
            {canSyncTime && (
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={!!task || submitting}
                onClick={async () => {
                  if (
                    !window.confirm(
                      "在当前查看的控制台所在 VPS 启用并校准系统时间？必要时安装系统时间服务，可能改变整机时间；保持现有时区。此操作不修改其他远程 Agent。",
                    )
                  )
                    return;
                  setSubmitting(true);
                  setTaskFailed(false);
                  setTaskMessage("正在提交校时任务…");
                  const id = crypto.randomUUID();
                  try {
                    await requester.current("/federation/task", "POST", {
                      id,
                      action: "sync-time",
                    });
                    setTask(id);
                    setTaskMessage("校时任务已提交，等待返回…");
                  } catch (e) {
                    setTaskFailed(true);
                    setTaskMessage(errorText(e));
                  } finally {
                    setSubmitting(false);
                  }
                }}
              >
                {task || submitting ? "正在校时…" : "启用本 VPS 自动校时"}
              </Button>
            )}
          </div>
          {taskMessage && (
            <div
              role={taskFailed ? "alert" : "status"}
              className={`rounded-lg border p-3 text-xs ${
                taskFailed
                  ? "border-destructive/30 bg-destructive/10 text-destructive"
                  : "border-border bg-card text-foreground"
              }`}
            >
              <p className="mb-1 font-semibold">
                {taskFailed
                  ? "自动校时未完成"
                  : task || submitting
                    ? "自动校时进行中"
                    : "自动校时结果"}
              </p>
              <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words font-sans">
                {taskMessage}
              </pre>
            </div>
          )}
        </section>

        {lineId && (
          <details className="details">
            <summary>分段连接检查</summary>
            <div className="mt-3.5 space-y-3.5">
              <label className="field">
                <span>检查网址</span>
                <Input
                  type="url"
                  value={target}
                  onChange={(e) => setTarget(e.target.value)}
                />
              </label>
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  type="button"
                  size="sm"
                  disabled={diagnosing}
                  onClick={() => void diagnose()}
                >
                  {diagnosing ? "正在分段检查…" : "执行详细检查"}
                </Button>
              </div>
              {diagnostic && (
                <pre className="mono max-h-56 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted p-3 text-xs">
                  {diagnostic}
                </pre>
              )}
            </div>
          </details>
        )}

        <details className="details" aria-label="日志存储设置">
          <summary>日志存储上限与开关设置</summary>
          <div className="mt-3.5 space-y-3.5">
            <div className="grid gap-3.5 sm:grid-cols-2">
              <label className="field">
                <span>日志总大小上限（MiB）</span>
                <Input
                  type="number"
                  min={1}
                  max={1024}
                  step={1}
                  value={sizeValue ?? data?.max_mib ?? 50}
                  disabled={savingLogs || data?.max_mib === undefined}
                  onChange={(e) => setSizeValue(e.target.value)}
                />
              </label>
              <div className="field">
                <label htmlFor="log-retention">保留时长</label>
                <div className="flex gap-2">
                  <Input
                    id="log-retention"
                    type="number"
                    min={1}
                    max={720 / retentionUnit}
                    step={1}
                    value={
                      retentionValue ??
                      (data?.retention_hours || 1) / retentionUnit
                    }
                    disabled={savingLogs || data?.retention_hours === undefined}
                    onChange={(e) => setRetentionValue(e.target.value)}
                  />
                  <select
                    className="select w-28 shrink-0"
                    aria-label="日志保留时长单位"
                    value={retentionUnit}
                    disabled={savingLogs || data?.retention_hours === undefined}
                    onChange={(e) => {
                      setRetentionUnit(Number(e.target.value));
                      setRetentionValue("1");
                    }}
                  >
                    <option value={1}>小时</option>
                    <option value={24}>天</option>
                  </select>
                </div>
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={
                  savingLogs ||
                  loading ||
                  diagnosing ||
                  data?.retention_hours === undefined
                }
                onClick={() => void saveLogging()}
              >
                保存限制并清理过期日志
              </Button>
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={
                  savingLogs ||
                  loading ||
                  diagnosing ||
                  data?.logging_enabled === undefined
                }
                onClick={() => void saveLogging(!data?.logging_enabled)}
              >
                {savingLogs
                  ? "保存中…"
                  : data?.logging_enabled === false
                    ? "开启日志记录"
                    : "关闭并清理日志"}
              </Button>
              <InfoTip>
                按 UTC 日期与 1 MiB 分段压缩归档，达到任一上限自动滚动清理最旧分片；关闭后立即清空本组件受管日志（不影响系统 journal）。
              </InfoTip>
            </div>
          </div>
        </details>
      </div>
    </Modal>
  );
}
