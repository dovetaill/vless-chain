import {
  Activity,
  ArrowDownUp,
  CheckCircle2,
  Database,
  ExternalLink,
  FileText,
  Gauge,
  Globe,
  LayoutDashboard,
  MoreHorizontal,
  Network,
  Pencil,
  Power,
  RefreshCw,
  Server,
  Settings2,
  Share2,
  Shield,
  ShieldCheck,
  Sliders,
  Trash2,
  Wrench,
} from "lucide-react";
import { DropdownMenu } from "radix-ui";
import "./fleet.css";
import { publishChecked, api, remoteApi } from "../api";
import { Diagnostics } from "./diagnostics";
import { NetworkTuning, type NetworkTarget } from "./network-tuning";
import { LimitsDialog } from "./rental-editor";
import { AccessPolicyEditor } from "./sharing";
import { AccessDialog } from "./access-dialog";
import { ProbeEditor } from "./probe-editor";
import { ExportDialog } from "./proxy-list";
import { FleetLineBatch } from "./fleet-line-batch";
import { CoreSelector, initialCoreChoice, type CoreChoice } from "./core-selector";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Check, External, Line, PanelState, Preview } from "../types";
import { matchesLine, parseExternalSummary } from "../lib/business";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { Input } from "./ui/input";
import { Progress } from "./ui/progress";
import { Card, CardContent, CardHeader, CardHeading, CardTitle, CardToolbar } from "./ui/card";
import {
  Field,
  CopyButton,
  InfoTip,
  Modal,
  Status,
  bytes,
  errorText,
  field,
  stamp,
} from "./shared";
import { LineEditor } from "./line-editor";
import { PolicyFields, defaultPolicy, type Policy } from "./management";
import {
  PAGE_SIZE,
  priority,
  actions,
  connected,
  health,
  issues,
  remoteNow,
  latestCheck,
  lineStatus,
  localNode,
  metrics,
  mismatch,
  recent,
  statuses,
  verificationFailed,
  maintenanceSucceeded,
  taskNeedsAttention,
  taskStatus,
  requestId,
  coreDecisionId,
  forgetFailedDecision,
  type Remote,
  type Task,
} from "./fleet-state";

type FleetState = {
  server_now?: number;
  remotes: Remote[];
  controller: string;
  policy: Policy | null;
  tasks: Record<string, Task>;
};

type Tab = "overview" | "vps" | "lines" | "externals" | "tasks" | "settings";

const tabs: [Tab, string][] = [
  ["overview", "总览"],
  ["vps", "VPS"],
  ["lines", "所有代理"],
  ["externals", "外部代理"],
  ["tasks", "维护任务"],
  ["settings", "总控设置"],
];

type Point = {
  at: number;
  rx: number | null;
  tx: number | null;
  hosts: number;
};

const tabIcons = {
  overview: LayoutDashboard,
  vps: Server,
  lines: Network,
  externals: Globe,
  tasks: Wrench,
  settings: Settings2,
};

const key = (r: Remote, line: Line) => `${r.id}:${line.id}`;
const rate = (value: number, valid: boolean) =>
  valid ? `${bytes(value)}/s` : "—";

function lineStatusTone(label: string): "good" | "warn" | "bad" | "info" | "muted" {
  if (label === "已启用") return "good";
  if (label === "已停用") return "muted";
  if (label.includes("失败") || label.includes("异常")) return "bad";
  if (label.includes("待生效")) return "info";
  return "warn";
}

function Pager({
  total,
  page,
  change,
}: {
  total: number;
  page: number;
  change: (page: number) => void;
}) {
  if (total <= 0) return null;
  const pages = Math.max(1, Math.ceil(total / PAGE_SIZE));
  return (
    <div className="fleet-pagination">
      <span className="muted">
        共 {total} 条 · 每页 {PAGE_SIZE} 条 · 第 {page} / {pages} 页
      </span>
      <div className="flex gap-2">
        <Button
          size="sm"
          variant="outline"
          disabled={page <= 1}
          onClick={() => change(page - 1)}
        >
          上一页
        </Button>
        <Button
          size="sm"
          variant="outline"
          disabled={page >= pages}
          onClick={() => change(page + 1)}
        >
          下一页
        </Button>
      </div>
    </div>
  );
}

function Versions({ remote: r }: { remote: Remote }) {
  return (
    <span className="inline-flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
      <span>
        面板 {r.snapshot?.version || "未知"} · Agent{" "}
        {r.snapshot?.agent_version || "版本未知（尚未上报）"}
      </span>
      {mismatch(r) && (
        <Badge variant="warning" appearance="light" size="xs">
          版本不一致
        </Badge>
      )}
    </span>
  );
}

function Trend({ points, now }: { points: Point[]; now: number }) {
  const [selected, setSelected] = useState<number>();
  const active = points.find((p) => p.at === selected) || points.at(-1);
  const largest = Math.max(1, ...points.flatMap((p) => [p.rx || 0, p.tx || 0]));
  const start = now - 600;
  const xPct = (at: number) => Math.max(1, Math.min(99, ((at - start) / 600) * 100));
  const yPct = (val: number) => 94 - Math.min(88, (val / largest) * 88);

  function paths(key: "rx" | "tx") {
    const lines: string[] = [], areas: string[] = [];
    let segment: string[] = [], firstX = 0;
    points.forEach((point, i) => {
      const value = point[key];
      if (value == null) return;
      const x = xPct(point.at);
      if (!segment.length) firstX = x;
      segment.push(`${segment.length ? "L" : "M"} ${x} ${yPct(value)}`);
      if (points[i + 1]?.[key] != null) return;
      if (segment.length > 1) {
        const line = segment.join(" ");
        lines.push(line);
        areas.push(`${line} L ${x} 96 L ${firstX} 96 Z`);
      }
      segment = [];
    });
    return { line: lines.join(" "), area: areas.join(" ") };
  }
  const rx = paths("rx"), tx = paths("tx");

  return (
    <Card className="fleet-trend">
      <CardHeader>
        <CardHeading className="flex items-center gap-1.5 space-y-0">
          <CardTitle>网卡速率 · 最近 10 分钟</CardTitle>
          <InfoTip>
            当前页面会话实时采样；每个采样点记录当时在线且上报有效的 VPS 台数，离线或超时区间留空，不伪造零值。
          </InfoTip>
        </CardHeading>
        <CardToolbar className="gap-3 text-xs text-muted-foreground">
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2 rounded-full bg-blue-500" />
            接收
          </span>
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2 rounded-full bg-emerald-500" />
            发送
          </span>
        </CardToolbar>
      </CardHeader>
      <CardContent>
        <div className="flex justify-between text-[11px] text-muted-foreground tabular-nums">
          <span>峰值上限 {bytes(largest)}/s</span>
          <span>点击采样点查看详情</span>
        </div>
        <div
          className="fleet-chart relative overflow-hidden border"
          aria-label={`网卡速率趋势，共 ${points.length} 个采样点`}
        >
          <div className="pointer-events-none absolute inset-x-0 top-1/4 border-t border-dashed border-border/60" />
          <div className="pointer-events-none absolute inset-x-0 top-1/2 border-t border-dashed border-border/80">
            <span className="ml-2 rounded bg-card/90 px-1.5 py-0.5 text-[10px] text-muted-foreground tabular-nums">
              {bytes(largest / 2)}/s
            </span>
          </div>
          <div className="pointer-events-none absolute inset-x-0 top-3/4 border-t border-dashed border-border/60" />

          <svg
            viewBox="0 0 100 100"
            preserveAspectRatio="none"
            className="pointer-events-none absolute inset-0 h-full w-full"
            aria-hidden="true"
          >
            <defs>
              <linearGradient id="fleetRxFill" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="#3b82f6" stopOpacity="0.24" />
                <stop offset="100%" stopColor="#3b82f6" stopOpacity="0.01" />
              </linearGradient>
              <linearGradient id="fleetTxFill" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="#10b981" stopOpacity="0.2" />
                <stop offset="100%" stopColor="#10b981" stopOpacity="0.01" />
              </linearGradient>
            </defs>
            {rx.area && <path d={rx.area} fill="url(#fleetRxFill)" />}
            {tx.area && <path d={tx.area} fill="url(#fleetTxFill)" />}
            {rx.line && (
              <path
                d={rx.line}
                fill="none"
                stroke="#3b82f6"
                strokeWidth="1.4"
                vectorEffect="non-scaling-stroke"
              />
            )}
            {tx.line && (
              <path
                d={tx.line}
                fill="none"
                stroke="#10b981"
                strokeWidth="1.4"
                vectorEffect="non-scaling-stroke"
              />
            )}
          </svg>

          {points.map((p, i) => {
            const isSelected = active?.at === p.at;
            return (
              <button
                type="button"
                key={`${p.at}:${i}`}
                aria-label={`查看采样 ${stamp(p.at)}`}
                onClick={() => setSelected(p.at)}
                className={`group absolute inset-y-0 flex w-3 -translate-x-1/2 items-end justify-center gap-px focus-visible:outline focus-visible:outline-primary ${
                  isSelected ? "bg-primary/10" : "hover:bg-muted/40"
                }`}
                style={{
                  left: `clamp(6px, ${((p.at - start) / 600) * 100}%, calc(100% - 6px))`,
                }}
              >
                {p.rx != null && (
                  <span
                    className="w-1 rounded-t-xs bg-blue-500/85 transition-opacity group-hover:opacity-100"
                    style={{ height: `${Math.max(3, (p.rx / largest) * 90)}%` }}
                  />
                )}
                {p.tx != null && (
                  <span
                    className="w-1 rounded-t-xs bg-emerald-500/85 transition-opacity group-hover:opacity-100"
                    style={{ height: `${Math.max(3, (p.tx / largest) * 90)}%` }}
                  />
                )}
                {p.rx == null && (
                  <span className="mb-1 size-2 rounded-full border border-muted-foreground/60" />
                )}
              </button>
            );
          })}
        </div>
        <div className="flex justify-between text-[11px] text-muted-foreground tabular-nums">
          <span>{stamp(start)}</span>
          <span>现在 · 基线 0 B/s</span>
        </div>
        <div
          role="status"
          className="mt-3 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-muted/25 px-3 py-2 text-xs tabular-nums"
        >
          {active ? (
            <>
              <span className="text-muted-foreground">
                {stamp(active.at)} · 有效 {active.hosts} 台
              </span>
              <span className="font-medium text-foreground">
                ↓ {rate(active.rx || 0, active.rx != null)} · ↑{" "}
                {rate(active.tx || 0, active.tx != null)}
              </span>
            </>
          ) : (
            <span className="text-muted-foreground">等待采样</span>
          )}
        </div>
        <details className="details mt-3">
          <summary>采样明细（{points.length}）</summary>
          <div className="mt-2.5 max-h-48 overflow-auto rounded-md border border-border">
            <table className="data-table">
              <thead>
                <tr>
                  <th>采样时间</th>
                  <th>有效 VPS</th>
                  <th>接收 / 发送</th>
                </tr>
              </thead>
              <tbody>
                {points
                  .slice()
                  .reverse()
                  .map((p, i) => (
                    <tr key={i}>
                      <td className="tabular-nums text-xs">{stamp(p.at)}</td>
                      <td className="tabular-nums text-xs">{p.hosts} 台</td>
                      <td className="tabular-nums text-xs">
                        {rate(p.rx || 0, p.rx != null)} /{" "}
                        {rate(p.tx || 0, p.tx != null)}
                      </td>
                    </tr>
                  ))}
              </tbody>
            </table>
          </div>
        </details>
      </CardContent>
    </Card>
  );
}

export function Fleet({ state }: { state: PanelState }) {
  const [data, setData] = useState<FleetState>();
  const [error, setError] = useState(""),
    [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false),
    [tab, setTab] = useState<Tab>("overview");
  const [search, setSearch] = useState(""),
    [filter, setFilter] = useState(""),
    [host, setHost] = useState("");
  const [page, setPage] = useState(1);
  const [showTaskHistory, setShowTaskHistory] = useState(false);
  const [sort, setSort] = useState("default");
  const views = useRef<
    Partial<
      Record<
        Tab,
        {
          search: string;
          filter: string;
          host: string;
          page: number;
          sort: string;
        }
      >
    >
  >({});
  const [now, setNow] = useState(() => Date.now() / 1000);
  const serverClock = useRef({
    at: Date.now() / 1000,
    received: performance.now(),
  });
  const controllerNow = () =>
    serverClock.current.at +
    (performance.now() - serverClock.current.received) / 1000;
  const [connectionError, setConnectionError] = useState("");
  const [pendingApply, setPendingApply] = useState<string[]>([]);
  const [selected, setSelected] = useState<string[]>([]),
    [selectedLines, setSelectedLines] = useState<string[]>([]);
  const [includeLocal, setIncludeLocal] = useState(false);
  const [modal, setModal] = useState<ReactNode>(null),
    [detail, setDetail] = useState<string>();
  const [taskId, setTaskId] = useState<string>();
  const [probeFeedback, setProbeFeedback] = useState<{
    key: string;
    text: string;
  }>();
  const [editor, setEditor] = useState<{
    remote: Remote;
    state: PanelState;
    line?: Line;
  }>();
  const [release, setRelease] = useState<{
      version: string;
      xray: string;
      mihomo: string;
    }>(),
    [releaseLoading, setReleaseLoading] = useState(false);
  const [maintenanceTab, setMaintenanceTab] = useState("");
  const [coreChoice, setCoreChoice] = useState<CoreChoice>(initialCoreChoice);
  const [invite, setInvite] = useState(""),
    [policy, setPolicy] = useState<Policy>(defaultPolicy),
    [override, setOverride] = useState(false);
  const [points, setPoints] = useState<Point[]>([]);
  const lastSample = useRef("");

  const refresh = useCallback(async (initial = false) => {
    const value = await api<FleetState>("/fleet");
    serverClock.current = {
      at: value.server_now ?? Date.now() / 1000,
      received: performance.now(),
    };
    setNow(serverClock.current.at);
    setConnectionError("");
    setData((old) => ({
      ...value,
      remotes: value.remotes.map((r) => {
        const previous = old?.remotes.find((p) => p.id === r.id);
        return previous && previous.seen > r.seen && !r.error ? previous : r;
      }),
    }));
    if (initial) {
      setOverride(!!value.policy);
      setPolicy(value.policy || defaultPolicy);
    }
  }, []);

  useEffect(() => {
    let polling = false;
    const poll = async (initial = false) => {
      if (polling) return;
      polling = true;
      try {
        await refresh(initial);
      } catch (e) {
        setConnectionError(errorText(e));
      } finally {
        polling = false;
      }
    };
    void poll(true);
    const timer = setInterval(() => {
      setNow(controllerNow());
      void poll();
    }, 5000);
    return () => clearInterval(timer);
  }, [refresh]);

  const fetchRelease = useCallback(async () => {
    setReleaseLoading(true);
    setRelease(undefined);
    try {
      setRelease(await api("/fleet/release/latest"));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setReleaseLoading(false);
    }
  }, []);

  useEffect(() => {
    void fetchRelease();
  }, [fetchRelease]);

  const remotes = data?.remotes || [],
    stats = metrics(remotes, now);

  useEffect(() => {
    if (!data) return;
    const m = metrics(data.remotes, now);
    const signature = data.remotes
      .map(
        (r) =>
          `${r.id}:${localNode(r)?.report.system?.sampled_at}:${connected(r, now)}:${recent(localNode(r)?.seen, remoteNow(r, now))}:${recent(localNode(r)?.report.system?.sampled_at, remoteNow(r, now))}`,
      )
      .join("|");
    if (signature === lastSample.current) return;
    lastSample.current = signature;
    setPoints((old) =>
      [
        ...old.filter((p) => now - p.at <= 600),
        {
          at: now,
          rx: m.hosts ? m.rx : null,
          tx: m.hosts ? m.tx : null,
          hosts: m.hosts,
        },
      ].slice(-120),
    );
  }, [data, now]);

  function navigate(next: Tab, remote?: string) {
    views.current[tab] = { search, filter, host, page, sort };
    const previous = views.current[next];
    setTab(next);
    setSearch(remote === undefined ? previous?.search || "" : "");
    setFilter(remote === undefined ? previous?.filter || "" : "");
    setHost(remote ?? previous?.host ?? "");
    setPage(remote === undefined ? previous?.page || 1 : 1);
    setSort(previous?.sort || "default");
    setDetail(undefined);
  }

  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  function toggle(
    id: string,
    values: string[],
    update: (next: string[]) => void,
  ) {
    update(
      values.includes(id) ? values.filter((v) => v !== id) : [...values, id],
    );
  }

  const allLines = remotes.flatMap((r) =>
    (r.snapshot?.lines || []).map((line) => ({ r, line })),
  );
  const chosen = remotes.filter((r) => selected.includes(r.id));
  const chosenLines = allLines.filter(({ r, line }) =>
    selectedLines.includes(key(r, line)),
  );
  const tasks = Object.values(data?.tasks || {}).sort(
    (a, b) => (b.at || 0) - (a.at || 0),
  );
  const running = tasks.filter(taskNeedsAttention);
  const visibleTasks = showTaskHistory ? tasks : running;
  const matches = (text: string) =>
    text.toLowerCase().includes(search.toLowerCase());

  const filteredVps = remotes.filter(
    (r) =>
      matches(`${r.name} ${r.url}`) &&
      (!filter ||
        (filter === "mismatch"
          ? mismatch(r)
          : filter === "offline"
            ? !connected(r, now)
            : health(r, now) !== "在线" || mismatch(r))),
  );

  const filteredLines = allLines.filter(
    ({ r, line }) =>
      (!host || r.id === host) &&
      matchesLine(
        line,
        {
          customers: r.snapshot?.customers || [],
          nodes: r.snapshot?.nodes || [],
        },
        search,
        `${r.name} ${r.url}`,
      ) &&
      (!filter ||
        (filter === "disabled"
          ? !line.enabled
          : filter === "enabled"
            ? line.enabled
            : !["已启用", "已停用"].includes(lineStatus(r, line, now)))),
  );

  if (sort === "attention")
    filteredVps.sort((a, b) => priority(a, now) - priority(b, now));
  if (sort === "rate")
    filteredVps.sort((a, b) => {
      const x = metrics([a], now),
        y = metrics([b], now);
      return (y.hosts ? y.rx + y.tx : -1) - (x.hosts ? x.rx + x.tx : -1);
    });
  if (sort === "attention")
    filteredLines.sort(
      (a, b) =>
        Number(["已启用", "已停用"].includes(lineStatus(a.r, a.line, now))) -
        Number(["已启用", "已停用"].includes(lineStatus(b.r, b.line, now))),
    );
  if (sort === "rate")
    filteredLines.sort((a, b) => {
      const speed = (v: typeof a) =>
        connected(v.r, now) &&
        v.line.enabled &&
        v.line.rental_status !== "offline" &&
        v.line.traffic &&
        recent(v.line.traffic.sampled_at, now)
          ? v.line.traffic.upload_bps + v.line.traffic.download_bps
          : -1;
      return speed(b) - speed(a);
    });

  const externals = remotes
    .flatMap((r) =>
      (r.snapshot?.externals || []).map((external) => ({
        r,
        external,
        ...parseExternalSummary(external.url),
      })),
    )
    .filter(({ r, external, protocol, target }) => {
      if (host && r.id !== host) return false;
      const haystack = `${r.name} ${r.url} ${external.name} ${protocol} ${target}`.toLowerCase();
      return search
        .toLowerCase()
        .trim()
        .split(/\s+/)
        .every((word) => haystack.includes(word));
    });

  const total =
    tab === "vps"
      ? filteredVps.length
      : tab === "lines"
        ? filteredLines.length
        : tab === "externals"
          ? externals.length
          : visibleTasks.length;

  const currentPage = Math.min(page, Math.max(1, Math.ceil(total / PAGE_SIZE)));
  const slice = <T,>(rows: T[]) =>
    rows.slice((currentPage - 1) * PAGE_SIZE, currentPage * PAGE_SIZE);
  const close = () => setModal(null);

  async function freshState(r: Remote) {
    const state = await remoteApi(r.id)<NonNullable<Remote["snapshot"]>>(
      "/state",
    );
    setData(
      (old) =>
        old && {
          ...old,
          remotes: old.remotes.map((item) =>
            item.id === r.id
              ? { ...item, snapshot: state, seen: controllerNow(), error: "" }
              : item,
          ),
        },
    );
    return state;
  }

  async function recheck(r: Remote, lineId: string) {
    const k = `${r.id}:${lineId}`;
    setProbeFeedback({ key: k, text: "正在复查连接…" });
    await run(async () => {
      try {
        const result = await remoteApi(r.id)<Check>(
          `/lines/${lineId}/probe`,
          "POST",
          {
            mode: "http",
            url: r.snapshot?.monitoring?.effective?.url || defaultPolicy.url,
          },
        );
        const state = await freshState(r);
        const condition = state.monitoring?.conditions[`probe:local:${lineId}`];
        setProbeFeedback({
          key: k,
          text: result.ok
            ? `本次连接检测通过。${condition?.active ? (state.monitoring?.effective?.enabled ? `恢复确认 ${condition.good || 0}/${state.monitoring.effective.recoveries}，系统会继续自动复查。` : "自动检测未启用，不会自动复查。") : ""}`
            : `本次连接检测未通过，已更新检测详情；${state.monitoring?.effective?.enabled ? "系统会继续自动复查。" : "自动检测未启用，不会自动复查。"}`,
        });
      } catch (e) {
        setProbeFeedback({ key: k, text: errorText(e) });
        throw e;
      }
    });
  }

  const markPending = (r: Remote) =>
    setPendingApply((old) => [...new Set([...old, r.id])]);

  async function prepareApply(r: Remote) {
    const preview = await remoteApi(r.id)<Preview>("/preview", "POST", {});
    setModal(
      <Modal
        title={`应用修改 · ${r.name}`}
        description="配置已保存，尚未下发。以下代理在重载期间可能短暂重连。"
        submit="确认并应用"
        close={close}
        action={async () => {
          await publishChecked(remoteApi(r.id));
          setPendingApply((old) => old.filter((id) => id !== r.id));
          setMessage(`${r.name}：已提交应用，请查看代理的同步状态。`);
          close();
          await freshState(r);
        }}
      >
        {preview.message && <p className="text-xs text-muted-foreground">{preview.message}</p>}
        <div className="flex max-h-60 flex-wrap gap-1.5 overflow-auto">
          {preview.affected_lines.map((name, i) => (
            <Status key={i} tone="info">
              {name}
            </Status>
          ))}
        </div>
      </Modal>,
    );
  }

  async function openLine(r: Remote, line?: Line) {
    const state = await freshState(r);
    const current = line && state.lines.find((l) => l.id === line.id);
    if (line && !current) throw new Error("该代理已不存在，请刷新列表。");
    setEditor({ remote: r, state, line: current });
    setDetail(undefined);
  }

  async function lineAction(r: Remote, line: Line, action: string) {
    const state = await freshState(r);
    const current = state.lines.find((item) => item.id === line.id);
    if (!current) throw new Error("该代理已不存在，请刷新列表。");
    const request = remoteApi(r.id);
    const saved = async () => {
      await freshState(r);
    };
    if (action === "toggle") {
      await request("/lines/operate", "POST", {
        line_ids: [current.id],
        action: current.enabled ? "disable" : "enable",
      });
      await saved();
    } else if (action === "limits")
      setModal(
        <LimitsDialog
          lines={[current]}
          request={request}
          close={close}
          saved={saved}
        />,
      );
    else if (action === "access")
      setModal(
        <AccessPolicyEditor
          line={current}
          request={request}
          close={close}
          saved={saved}
        />,
      );
    else if (action === "share")
      setModal(
        <AccessDialog
          line={current}
          state={state}
          request={request}
          remote
          close={close}
        />,
      );
    else if (action === "probe")
      setModal(
        <ProbeEditor
          line={current}
          state={state}
          request={request}
          close={close}
          saved={saved}
        />,
      );
    else if (action === "diagnostics")
      setModal(
        <Diagnostics
          nodes={state.nodes.filter((n) =>
            current.hops.some((h) => h.node_id === n.id),
          )}
          lineId={current.id}
          request={request}
          close={close}
        />,
      );
  }

  function exportLines() {
    const items = chosenLines.map(({ r, line }) => ({
      r,
      line: { ...line, name: `${r.name} / ${line.name}` },
    }));
    setModal(
      <ExportDialog
        lines={items.map((item) => item.line)}
        requestForLine={(line) =>
          remoteApi(items.find((item) => item.line === line)!.r.id)
        }
        close={close}
      />,
    );
  }

  async function openExternal(r: Remote, external?: External) {
    const state = await freshState(r),
      current = external && state.externals.find((e) => e.id === external.id);
    if (external && !current) throw new Error("该外部代理已不存在。");
    setDetail(undefined);
    setModal(
      <Modal
        title={`${current ? "编辑" : "导入"}外部代理 · ${r.name}`}
        description={`目标节点：${r.name}。保存后将预览受影响的链式代理。`}
        submit="保存"
        protectChanges
        close={close}
        action={async (values) => {
          await remoteApi(r.id)(
            current ? `/externals/${current.id}` : "/externals",
            "POST",
            { name: field(values, "name"), url: field(values, "url") },
          );
          close();
          markPending(r);
          setMessage(`${r.name}：配置已保存，尚未应用。`);
          try {
            await freshState(r);
            await prepareApply(r);
          } catch (e) {
            setError(
              `配置已保存，但读取应用预览失败：${errorText(e)}。请在 VPS 详情中继续应用。`,
            );
          }
        }}
      >
        <Field label="名称">
          <Input name="name" defaultValue={current?.name} maxLength={80} />
        </Field>
        <Field label="分享链接或单节点配置">
          <textarea
            className="textarea mono"
            name="url"
            rows={7}
            defaultValue={current?.url}
            required
          />
        </Field>
      </Modal>,
    );
  }

  function remove(r: Remote, line?: Line, external?: External) {
    setModal(
      <Modal
        title={`确认删除 · ${r.name}`}
        description="删除后无法撤销；同步配置期间可能短暂中断关联连接。"
        submit="确认删除"
        close={close}
        action={async () => {
          if (line)
            await remoteApi(r.id)("/lines/operate", "POST", {
              line_ids: [line.id],
              action: "delete",
            });
          else await remoteApi(r.id)(`/externals/${external!.id}`, "DELETE");
          close();
          setMessage(
            `${r.name}：已删除${line ? "并提交下发" : "，尚未应用"}。`,
          );
          try {
            await freshState(r);
            if (!line) {
              markPending(r);
              await prepareApply(r);
            }
          } catch (e) {
            setError(`删除已完成，但更新状态失败：${errorText(e)}`);
          }
        }}
      >
        <p className="rounded-lg border border-border bg-muted/25 px-3 py-2 text-xs font-medium">
          {line?.name || external?.name}
        </p>
      </Modal>,
    );
  }

  function archiveTask(task: Task) {
    if (
      !window.confirm(
        "移入历史任务？将停止此任务的自动复查，也不会继续未执行的 VPS；原始结果保留，这不代表连接问题已修复。",
      )
    )
      return;
    void run(async () => {
      await api(`/fleet/batch/${task.id}/archive`, "POST", {});
      await refresh();
    });
  }

  function batch(action: string) {
    const isLines = ["enable", "disable"].includes(action),
      targets = isLines
        ? remotes.filter((r) => chosenLines.some((v) => v.r.id === r.id))
        : chosen;
    const lineMap: Record<string, string[]> = {};
    if (isLines)
      for (const { r, line } of chosenLines)
        (lineMap[r.id] ||= []).push(line.id);
    if (!targets.length) return;
    const id = requestId(),
      version =
        action === "core-upgrade" ? coreChoice.version : release?.version || "";
    const blocked = targets.filter(
      (r) =>
        !connected(r, now) ||
        (!isLines && !r.snapshot?.maintenance) ||
        (action === "restart-all" &&
          !r.snapshot?.maintenance_actions?.includes(action)) ||
        (action === "core-upgrade" &&
          !r.snapshot?.maintenance_actions?.includes(action)),
    );

    setModal(
      <Modal
        wide
        title={`确认${actions[action]}`}
        description={
          action === "core-upgrade"
            ? `并行更新 ${targets.length} 台 VPS 的 ${coreChoice.core.toUpperCase()} 内核至 ${version}（最多 10 台并发，UUID / 密钥与原链接保持不变）。${
                coreChoice.security_update && !coreChoice.rollback_safe
                  ? "若切换失败且无法安全回退，将隔离代理内核并保留 Agent 心跳。"
                  : "切换异常时自动尝试安全回退。"
              }`
            : `对以下 ${targets.length} 台 VPS${
                isLines ? `（共 ${chosenLines.length} 条代理）` : ""
              } 执行${actions[action]}${
                action === "upgrade"
                  ? `（目标 v${version}，保留已独立指定的内核版本）`
                  : ""
              }。最多 5 台并发滚动执行，单台异常不会阻塞其余机器。`
        }
        submit={
          (action === "core-upgrade"
            ? blocked.length === targets.length
            : blocked.length > 0) ||
          (action === "upgrade" && !release)
            ? undefined
            : "确认执行"
        }
        close={close}
        action={async () => {
          await api("/fleet/batch", "POST", {
            id,
            targets: targets.map((r) => r.id),
            action,
            version,
            ...(action === "core-upgrade" ? coreChoice : {}),
            lines: lineMap,
          });
          close();
          setTaskId(id);
          setMessage(
            "任务已提交，可在维护任务中查看各 VPS 的维护和自动验证结果。",
          );
          try {
            await refresh();
          } catch (e) {
            setError(`任务已提交，状态刷新失败：${errorText(e)}`);
          }
        }}
      >
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <Badge variant="primary" appearance="light" size="sm">
            已选 {targets.length} 台 VPS
          </Badge>
          {isLines && (
            <Badge variant="info" appearance="light" size="sm">
              共 {chosenLines.length} 条代理
            </Badge>
          )}
          {version && ["upgrade", "core-upgrade"].includes(action) && (
            <Badge variant="secondary" appearance="light" size="sm">
              目标 {action === "core-upgrade" ? `${coreChoice.core.toUpperCase()} ${version}` : `v${version}`}
            </Badge>
          )}
          {blocked.length > 0 && (
            <Badge variant="warning" appearance="light" size="sm">
              {blocked.length} 台存在前置限制
            </Badge>
          )}
        </div>

        <div className="max-h-72 overflow-y-auto rounded-lg border border-border">
          <table className="data-table">
            <thead>
              <tr>
                <th>目标 VPS</th>
                <th>在线状态</th>
                <th>
                  {isLines
                    ? "涉及代理"
                    : action === "upgrade"
                      ? "版本对比"
                      : action === "core-upgrade"
                        ? "目标内核"
                        : "当前组件"}
                </th>
              </tr>
            </thead>
            <tbody>
              {targets.map((r) => {
                const statusLabel = health(r, now);
                const panelVer = r.snapshot?.version || "未知";
                const agentVer = r.snapshot?.agent_version || "未知";
                const alreadyLatest =
                  action === "upgrade" &&
                  !!version &&
                  panelVer === version &&
                  agentVer === version;
                const hostLines = isLines
                  ? chosenLines
                      .filter((v) => v.r.id === r.id)
                      .map((v) => v.line.name)
                  : [];
                return (
                  <tr key={r.id}>
                    <td>
                      <strong className="font-semibold text-foreground">
                        {r.name}
                      </strong>
                      <p className="mono mt-0.5 text-[11px] text-muted-foreground break-all">
                        {r.url}
                      </p>
                    </td>
                    <td className="whitespace-nowrap">
                      <Status tone={statusLabel === "在线" ? "good" : "warn"}>
                        {statusLabel}
                      </Status>
                    </td>
                    <td>
                      {isLines ? (
                        <p className="text-xs leading-relaxed text-muted-foreground">
                          {hostLines.join("、")}
                        </p>
                      ) : action === "upgrade" ? (
                        <div className="flex flex-wrap items-center gap-1.5 text-xs tabular-nums">
                          <span className="text-muted-foreground">
                            {panelVer}
                            {mismatch(r) ? ` (Agent ${agentVer})` : ""}
                          </span>
                          <span className="text-muted-foreground">→</span>
                          <span className="font-semibold text-foreground">
                            {version || "待获取"}
                          </span>
                          {alreadyLatest && (
                            <Badge variant="success" appearance="light" size="xs">
                              已是最新
                            </Badge>
                          )}
                        </div>
                      ) : action === "core-upgrade" ? (
                        <span className="mono text-xs font-medium">
                          {coreChoice.core.toUpperCase()} {version}
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground tabular-nums">
                          面板 {panelVer} · Agent {agentVer}
                        </span>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        {blocked.length > 0 && (
          <p
            role="alert"
            className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-xs leading-relaxed text-amber-800 dark:text-amber-300"
          >
            {action === "core-upgrade" && blocked.length < targets.length
              ? "以下 VPS 将独立记录处理结果，其余继续执行："
              : "无法执行："}
            {blocked
              .map(
                (r) =>
                  `${r.name}（${
                    !connected(r, now)
                      ? "失联或数据过期"
                      : !r.snapshot?.maintenance
                        ? "尚未安装维护组件"
                        : action === "upgrade"
                          ? "请先使用推荐稳定版升级，以支持内核选择"
                          : "请先升级以支持整套重启"
                  }）`,
              )
              .join("、")}
            。请修复或调整选择；不会自动跳过。
          </p>
        )}
      </Modal>,
    );
  }

  function networkTuning(targets: NetworkTarget[]) {
    setDetail(undefined);
    setModal(
      <NetworkTuning
        targets={targets}
        localTarget={{ id: "local", name: "本机（主控）", snapshot: state }}
        initialIncludeLocal={includeLocal}
        close={close}
        submitted={async (id) => {
          await refresh();
          navigate("tasks");
          setTaskId(id);
        }}
      />,
    );
  }

  async function jump(r: Remote) {
    const target = window.open("about:blank", "_blank");
    try {
      const result = await api<{ url: string }>(
        `/fleet/${r.id}/login`,
        "POST",
        {},
      );
      if (target) {
        target.opener = null;
        target.location.href = result.url;
      } else location.assign(result.url);
    } catch (e) {
      target?.close();
      throw e;
    }
  }

  const activeDetail = remotes.find((r) => r.id === detail),
    activeTask = tasks.find((t) => t.id === taskId);

  const table = (label: string, children: ReactNode) => (
    <div
      className="overflow-x-auto fleet-responsive"
      tabIndex={0}
      role="region"
      aria-label={label}
    >
      {children}
    </div>
  );

  return (
    <div className="fleet-workspace">
      <header className="fleet-heading">
        <div className="flex items-center gap-2">
          <h1>多机总控</h1>
          <InfoTip>
            远端节点每 30 秒上报一次，总控界面每 5 秒同步；超过 90 秒未更新的采样不会计入实时速率。
          </InfoTip>
        </div>
        <Button
          variant="outline"
          disabled={busy}
          onClick={() => void run(() => refresh())}
        >
          <RefreshCw aria-hidden="true" />
          刷新总控
        </Button>
      </header>

      {connectionError && (
        <p
          role="alert"
          className="rounded-lg border border-destructive/25 bg-destructive/10 px-4 py-3 text-xs font-medium text-destructive"
        >
          总控连接中断：{connectionError}
          。下方展示最近一次同步快照，超时速率已自动隐藏。
        </p>
      )}

      {pendingApply
        .filter((id) => remotes.some((r) => r.id === id))
        .map((id) => (
          <div
            key={id}
            className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-xs"
          >
            <span className="font-medium text-foreground">
              {remotes.find((r) => r.id === id)!.name}：配置已保存，尚未应用
            </span>
            <Button
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={() =>
                void run(() => prepareApply(remotes.find((r) => r.id === id)!))
              }
            >
              继续应用
            </Button>
          </div>
        ))}

      {error && (
        <p
          role="alert"
          className="rounded-lg border border-destructive/25 bg-destructive/10 px-4 py-3 text-xs font-medium text-destructive"
        >
          {error}
        </p>
      )}

      {message && (
        <p
          role="status"
          className="rounded-lg border border-border bg-card px-4 py-3 text-xs text-foreground"
        >
          {message}
        </p>
      )}

      <nav aria-label="总控功能" className="fleet-navigation">
        {tabs.map(([id, label]) => {
          const Icon = tabIcons[id];
          return (
            <Button
              key={id}
              variant="ghost"
              aria-current={tab === id ? "page" : undefined}
              onClick={() => navigate(id)}
            >
              <Icon aria-hidden="true" />
              {label}
            </Button>
          );
        })}
      </nav>

      {running.length > 0 && (
        <div className="fleet-running-strip">
          <span className="font-medium text-foreground">
            {running.filter((t) => t.state === "running").length} 个任务执行中 ·{" "}
            {running.filter((t) => t.state !== "running").length} 个待处理
          </span>
          <Button size="sm" variant="outline" onClick={() => navigate("tasks")}>
            查看任务
          </Button>
        </div>
      )}

      {!data && (
        <p className="text-xs text-muted-foreground">正在加载总控数据…</p>
      )}

      {tab === "overview" && (
        <div className="fleet-overview">
          <div className="fleet-metrics">
            {(
              [
                {
                  label: "VPS 状态",
                  value: `${remotes.filter((r) => connected(r, now)).length} / ${remotes.length} 可同步`,
                  detail: `${remotes.filter((r) => health(r, now) !== "在线").length} 台需要关注 · ${remotes.filter(mismatch).length} 台版本不一致`,
                  icon: Server,
                  tone: "bg-blue-500/12 text-blue-600 dark:text-blue-400",
                },
                {
                  label: "VPS 网卡速率",
                  value: `↓ ${rate(stats.rx, stats.hosts > 0)}`,
                  detail: `↑ ${rate(stats.tx, stats.hosts > 0)} · 有效 ${stats.hosts}/${remotes.length} 台`,
                  icon: ArrowDownUp,
                  tone: "bg-emerald-500/12 text-emerald-600 dark:text-emerald-400",
                },
                {
                  label: "代理业务速率",
                  value: `↓ ${rate(stats.download, stats.sampled > 0)}`,
                  detail: `↑ ${rate(stats.upload, stats.sampled > 0)} · 有效 ${stats.sampled}/${stats.lines} 条`,
                  icon: Activity,
                  tone: "bg-indigo-500/12 text-indigo-600 dark:text-indigo-400",
                },
                {
                  label: "代理周期用量",
                  value: stats.usage ? bytes(stats.used) : "尚未采集",
                  detail: `包含 ${stats.usage}/${stats.lines} 条代理最近已知用量`,
                  icon: Database,
                  tone: "bg-amber-500/12 text-amber-600 dark:text-amber-400",
                  tip: "网卡速率包含机器全部进出流量；周期用量为各代理当前计费周期累加（跨机器链路不去重，不代表机房账单）。失联 VPS 仍保留最近一次已知用量。",
                },
              ] as const
            ).map((item) => {
              const Icon = item.icon;
              return (
                <Card key={item.label} className="fleet-metric">
                  <CardContent className="p-5">
                    <div className="fleet-metric-label">
                      <span className="inline-flex items-center gap-1.5">
                        <span>{item.label}</span>
                        {"tip" in item && item.tip && <InfoTip>{item.tip}</InfoTip>}
                      </span>
                      <span
                        className={`flex size-8 shrink-0 items-center justify-center rounded-lg ${item.tone}`}
                      >
                        <Icon className="size-4" aria-hidden="true" />
                      </span>
                    </div>
                    <p className="mt-3 mb-1.5 text-xl font-semibold tracking-tight text-foreground tabular-nums sm:text-2xl">
                      {item.value}
                    </p>
                    <p className="text-xs text-muted-foreground tabular-nums">
                      {item.detail}
                    </p>
                  </CardContent>
                </Card>
              );
            })}
          </div>

          <div className="fleet-proxy-summary">
            <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
              <span className="font-semibold text-foreground">代理概览</span>
              <span>
                {
                  allLines.filter(
                    ({ r, line }) =>
                      !["已启用", "已停用", "状态未知"].includes(
                        lineStatus(r, line, now),
                      ),
                  ).length
                }{" "}
                条有状态提示
              </span>
              <span>·</span>
              <span>
                {
                  allLines.filter(
                    ({ r, line }) => lineStatus(r, line, now) === "状态未知",
                  ).length
                }{" "}
                条因同步超时状态未知
              </span>
              <span>·</span>
              <span>
                {
                  allLines.filter(
                    ({ r, line }) => lineStatus(r, line, now) === "已停用",
                  ).length
                }{" "}
                条已停用
              </span>
            </div>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                navigate("lines");
                setFilter("attention");
              }}
            >
              查看代理状态
            </Button>
          </div>

          <div className="fleet-overview-panels">
            <Trend
              points={points.filter((p) => now - p.at <= 600)}
              now={now}
            />
            <Card className="fleet-attention">
              <CardHeader>
                <CardTitle>需要关注的 VPS</CardTitle>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => {
                    navigate("vps");
                    setFilter("attention");
                  }}
                >
                  查看全部异常（
                  {
                    remotes.filter(
                      (r) => health(r, now) !== "在线" || mismatch(r),
                    ).length
                  }
                  ）
                </Button>
              </CardHeader>
              <CardContent
                className="fleet-attention-scroll space-y-3"
                tabIndex={0}
                aria-label="待处理 VPS 列表"
              >
                {remotes
                  .filter((r) => health(r, now) !== "在线" || mismatch(r))
                  .sort((a, b) => priority(a, now) - priority(b, now))
                  .slice(0, 5)
                  .map((r) => (
                    <div key={r.id} className="fleet-attention-item">
                      <div>
                        <div className="flex flex-wrap items-center justify-between gap-2">
                          <p className="text-sm font-semibold text-foreground">
                            {r.name} · {health(r, now)}
                          </p>
                          <Status
                            tone={health(r, now) === "在线" ? "info" : "warn"}
                          >
                            {health(r, now) === "在线" ? "版本差异" : "需关注"}
                          </Status>
                        </div>
                        <div className="mt-1">
                          <Versions remote={r} />
                        </div>
                        <ul className="mt-2 max-w-3xl space-y-1.5">
                          {issues(r, now).map((issue, i) => (
                            <li key={i} className="break-words">
                              {issue.detail}
                              {issue.evidence && (
                                <details className="details mt-1.5">
                                  <summary>自动检测详情</summary>
                                  <p className="mt-1.5 whitespace-pre-wrap text-xs">
                                    {issue.evidence}
                                  </p>
                                </details>
                              )}
                            </li>
                          ))}
                        </ul>
                        <p className="mt-2 text-[11px] text-muted-foreground tabular-nums">
                          最近同步 {stamp(r.seen)}
                        </p>
                      </div>
                      <div className="flex flex-wrap gap-2">
                        <Button
                          size="sm"
                          variant="outline"
                          onClick={() => setDetail(r.id)}
                        >
                          查看 VPS
                        </Button>
                        <Button
                          size="sm"
                          variant="outline"
                          onClick={() =>
                            setModal(
                              <Diagnostics
                                nodes={r.snapshot?.nodes || []}
                                request={remoteApi(r.id)}
                                close={() => setModal(null)}
                              />,
                            )
                          }
                        >
                          日志与诊断
                        </Button>
                      </div>
                    </div>
                  ))}
                {!remotes.length ? (
                  <div className="flex flex-col items-center justify-center gap-3 py-8 text-center">
                    <p className="text-xs text-muted-foreground">尚未接入远端 VPS。</p>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => navigate("settings")}
                    >
                      接入 VPS
                    </Button>
                  </div>
                ) : (
                  remotes.every(
                    (r) => health(r, now) === "在线" && !mismatch(r),
                  ) && (
                    <div className="flex flex-col items-center justify-center gap-2 py-8 text-center">
                      <span className="flex size-10 items-center justify-center rounded-full bg-emerald-500/12 text-emerald-600 dark:text-emerald-400">
                        <CheckCircle2 className="size-5" />
                      </span>
                      <p className="text-sm font-medium text-foreground">
                        所有 VPS 在线且版本一致
                      </p>
                      <p className="max-w-xs text-xs leading-relaxed text-muted-foreground">
                        当前无心跳或组件版本异常；单条代理的出网连通性以探测结果为准。
                      </p>
                    </div>
                  )
                )}
              </CardContent>
            </Card>
          </div>
        </div>
      )}

      {["vps", "lines", "externals"].includes(tab) && (
        <Card className="fleet-list-card">
          <CardHeader>
            <CardHeading className="flex items-center gap-2 space-y-0">
              <CardTitle>
                {tab === "vps"
                  ? "VPS 列表"
                  : tab === "lines"
                    ? "所有代理"
                    : "外部代理"}
              </CardTitle>
              {tab === "lines" && (
                <InfoTip>
                  代理速率反映实时业务吞吐，非带宽测速；空闲时速率较低属正常现象。
                </InfoTip>
              )}
              {tab === "externals" && (
                <InfoTip>
                  外部代理作为链式出网节点使用，不单独采集网卡速率；先筛选指定 VPS 即可为其导入。
                </InfoTip>
              )}
            </CardHeading>
            <span className="fleet-record-count">{total} 条记录</span>
          </CardHeader>

          <div
            className={
              tab === "vps"
                ? "fleet-filterbar fleet-filterbar-vps"
                : "fleet-filterbar"
            }
            aria-label={tab === "vps" ? "搜索与筛选 VPS" : undefined}
          >
            <Input
              className={tab === "vps" ? "w-full" : "sm:max-w-72"}
              aria-label="搜索总控列表"
              placeholder={
                tab === "vps"
                  ? "搜索 VPS 名称或地址"
                  : "搜索 VPS、地址或代理名称"
              }
              value={search}
              onChange={(e) => {
                setSearch(e.target.value);
                setPage(1);
              }}
            />
            {tab !== "externals" && (
              <select
                aria-label="列表排序"
                className={
                  tab === "vps" ? "select w-full sm:w-auto" : "select w-auto"
                }
                value={sort}
                onChange={(e) => {
                  setSort(e.target.value);
                  setPage(1);
                }}
              >
                <option value="default">默认顺序</option>
                <option value="attention">异常优先</option>
                <option value="rate">当前吞吐从高到低</option>
              </select>
            )}
            {host && tab !== "vps" && (
              <Button size="sm" variant="ghost" onClick={() => navigate("vps")}>
                返回 VPS 列表
              </Button>
            )}
            {tab !== "vps" && (
              <select
                className="select w-auto max-w-full"
                aria-label="筛选 VPS"
                value={host}
                onChange={(e) => {
                  setHost(e.target.value);
                  setPage(1);
                }}
              >
                <option value="">所有 VPS</option>
                {remotes.map((r) => (
                  <option key={r.id} value={r.id}>
                    {r.name}
                  </option>
                ))}
              </select>
            )}
            {tab !== "externals" && (
              <select
                className={
                  tab === "vps" ? "select w-full sm:w-auto" : "select w-auto"
                }
                aria-label="筛选总控状态"
                value={filter}
                onChange={(e) => {
                  setFilter(e.target.value);
                  setPage(1);
                }}
              >
                <option value="">所有状态</option>
                <option value="attention">需要关注</option>
                {tab === "vps" ? (
                  <>
                    <option value="offline">失联 / 过期</option>
                    <option value="mismatch">版本不一致</option>
                  </>
                ) : (
                  <>
                    <option value="enabled">已启用</option>
                    <option value="disabled">已停用</option>
                  </>
                )}
              </select>
            )}
            {(search || host || filter) && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  setSearch("");
                  setHost("");
                  setFilter("");
                  setPage(1);
                }}
              >
                清除筛选
              </Button>
            )}
            {host && tab === "lines" && (
              <Button
                size="sm"
                disabled={busy}
                onClick={() =>
                  void run(() => openLine(remotes.find((r) => r.id === host)!))
                }
              >
                新建代理
              </Button>
            )}
            {host && tab === "externals" && (
              <Button
                size="sm"
                disabled={busy}
                onClick={() =>
                  void run(() =>
                    openExternal(remotes.find((r) => r.id === host)!),
                  )
                }
              >
                导入外部代理
              </Button>
            )}
          </div>

          {tab === "vps" && (
            <>
              <div className="fleet-selection" aria-label="VPS 批量操作">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <p className="text-xs sm:text-sm">
                    <strong className="font-semibold text-foreground">
                      已选 {chosen.length} 台 VPS
                    </strong>
                    <span className="ml-2 text-xs text-muted-foreground">
                      跨分页保留选择
                    </span>
                  </p>
                  <div className="flex flex-wrap gap-1">
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() =>
                        setSelected([
                          ...new Set([
                            ...selected,
                            ...slice(filteredVps).map((r) => r.id),
                          ]),
                        ])
                      }
                    >
                      选择本页
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() =>
                        setSelected([
                          ...new Set([
                            ...selected,
                            ...filteredVps.map((r) => r.id),
                          ]),
                        ])
                      }
                    >
                      选择全部筛选结果
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={!chosen.length}
                      onClick={() => setSelected([])}
                    >
                      清空选择
                    </Button>
                  </div>
                </div>
                <div className="flex flex-wrap gap-2">
                  {[
                    ["program", "程序更新"],
                    ["core", "代理内核"],
                    ["more", "更多维护"],
                  ].map(([k, label]) => (
                    <Button
                      key={k}
                      size="sm"
                      variant={maintenanceTab === k ? "secondary" : "outline"}
                      disabled={!chosen.length}
                      aria-expanded={maintenanceTab === k}
                      onClick={() =>
                        setMaintenanceTab(maintenanceTab === k ? "" : k)
                      }
                    >
                      {label}
                    </Button>
                  ))}
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={
                      busy ||
                      (!chosen.length && !includeLocal) ||
                      running.some((t) => t.state === "running")
                    }
                    onClick={() => networkTuning(chosen)}
                  >
                    网络设置 / BBR
                  </Button>
                </div>

                {chosen.length > 0 && maintenanceTab && (
                  <div className="space-y-3 rounded-lg border border-border bg-muted/25 p-3.5">
                    {maintenanceTab === "program" && (
                      <div className="flex flex-wrap items-center gap-2.5">
                        <Button
                          size="sm"
                          disabled={
                            busy ||
                            releaseLoading ||
                            !release ||
                            running.some((t) => t.state === "running")
                          }
                          onClick={() => batch("upgrade")}
                        >
                          批量升级
                        </Button>
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={busy || releaseLoading}
                          onClick={() => void fetchRelease()}
                        >
                          {releaseLoading
                            ? "正在获取最新版本…"
                            : "检查最新版本"}
                        </Button>
                        <span className="text-xs text-muted-foreground">
                          {release
                            ? `最新正式版 ${release.version}`
                            : "先检查版本，再升级所选 VPS"}
                        </span>
                      </div>
                    )}
                    {maintenanceTab === "core" && (
                      <>
                        <CoreSelector
                          value={coreChoice}
                          onChange={setCoreChoice}
                          disabled={
                            busy || running.some((t) => t.state === "running")
                          }
                        />
                        <Button
                          size="sm"
                          disabled={
                            busy ||
                            running.some((t) => t.state === "running") ||
                            !/^\d+\.\d+\.\d+$/.test(coreChoice.version)
                          }
                          onClick={() => batch("core-upgrade")}
                        >
                          并行升级所选 VPS 的 {coreChoice.core}
                        </Button>
                      </>
                    )}
                    {maintenanceTab === "more" && (
                      <div className="space-y-2.5 text-sm">
                        <div className="flex flex-wrap gap-2">
                          {[
                            "restart-all",
                            "restart-panel",
                            "restart-agent",
                          ].map((action) => (
                            <Button
                              key={action}
                              size="sm"
                              variant="outline"
                              disabled={
                                busy ||
                                running.some((t) => t.state === "running")
                              }
                              onClick={() => batch(action)}
                            >
                              {actions[action]}
                            </Button>
                          ))}
                        </div>
                        <p className="text-xs leading-relaxed text-muted-foreground">
                          最多 5 台并发滚动执行，单台异常不阻塞其余机器；远端需已部署 systemd 维护服务。
                          {release
                            ? ` 发行包组件：面板 / Agent ${release.version} · Xray ${release.xray || "未知"} · Mihomo ${release.mihomo || "未知"}`
                            : ""}
                        </p>
                      </div>
                    )}
                  </div>
                )}
              </div>

              {table(
                "VPS 列表",
                <table className="data-table [&_td]:align-top">
                  <thead>
                    <tr>
                      <th>VPS</th>
                      <th>状态</th>
                      <th>网卡速率</th>
                      <th>周期用量</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {slice(filteredVps).map((r) => {
                      const m = metrics([r], now);
                      return (
                        <tr key={r.id}>
                          <td data-label="VPS">
                            <label className="flex items-center gap-2 font-semibold text-foreground">
                              <input
                                type="checkbox"
                                aria-label={`选择 VPS ${r.name}`}
                                checked={selected.includes(r.id)}
                                onChange={() =>
                                  toggle(r.id, selected, setSelected)
                                }
                              />
                              <span>{r.name}</span>
                            </label>
                            <div className="ml-6 mt-1 space-y-0.5 text-xs text-muted-foreground">
                              <p className="mono max-w-56 break-all text-[11px]">
                                {r.url}
                              </p>
                              <Versions remote={r} />
                            </div>
                          </td>
                          <td data-label="状态" className="align-top">
                            <Status
                              tone={
                                health(r, now) === "在线" ? "good" : "warn"
                              }
                            >
                              {health(r, now)}
                            </Status>
                            {issues(r, now)[0]?.detail && (
                              <p className="mt-1.5 max-w-sm text-xs leading-relaxed text-muted-foreground">
                                {issues(r, now)[0]?.detail}
                              </p>
                            )}
                            <p className="mt-1 text-[11px] text-muted-foreground tabular-nums">
                              最近同步 {stamp(r.seen)}
                            </p>
                          </td>
                          <td
                            data-label="网卡速率"
                            className="whitespace-nowrap tabular-nums text-xs"
                          >
                            <span className="text-foreground">
                              ↓ {rate(m.rx, m.hosts > 0)}
                            </span>
                            <br />
                            <span className="text-muted-foreground">
                              ↑ {rate(m.tx, m.hosts > 0)}
                            </span>
                          </td>
                          <td data-label="周期用量" className="tabular-nums">
                            <span className="font-medium text-foreground">
                              {m.usage ? bytes(m.used) : "—"}
                            </span>
                            <p className="mt-0.5 text-[11px] text-muted-foreground">
                              {m.lines} 条代理累计
                            </p>
                          </td>
                          <td data-label="操作">
                            <Button
                              size="sm"
                              variant="outline"
                              onClick={() => setDetail(r.id)}
                            >
                              查看 VPS
                            </Button>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>,
              )}
            </>
          )}

          {tab === "lines" && (
            <>
              <div className="sticky top-0 z-10 flex flex-wrap items-center gap-2 border-b border-border bg-card px-5 py-3">
                <span className="mr-1 text-xs font-medium text-foreground">
                  已选 {chosenLines.length} 条代理 /{" "}
                  {new Set(chosenLines.map((v) => v.r.id)).size} 台
                  VPS（含隐藏项）
                </span>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() =>
                    setSelectedLines([
                      ...new Set([
                        ...selectedLines,
                        ...slice(filteredLines).map((v) => key(v.r, v.line)),
                      ]),
                    ])
                  }
                >
                  选择本页
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() =>
                    setSelectedLines([
                      ...new Set([
                        ...selectedLines,
                        ...filteredLines.map((v) => key(v.r, v.line)),
                      ]),
                    ])
                  }
                >
                  选择全部筛选结果
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={!chosenLines.length}
                  onClick={() => setSelectedLines([])}
                >
                  清空选择
                </Button>
                {["enable", "disable"].map((action) => (
                  <Button
                    key={action}
                    size="sm"
                    variant="outline"
                    disabled={
                      busy ||
                      !chosenLines.length ||
                      running.some((t) => t.state === "running")
                    }
                    onClick={() => batch(action)}
                  >
                    {actions[action]}
                  </Button>
                ))}
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy || !chosenLines.length}
                  onClick={() =>
                    setModal(
                      <FleetLineBatch
                        targets={chosenLines}
                        action="limits"
                        close={close}
                        saved={async () => {
                          await refresh();
                          setSelectedLines([]);
                        }}
                      />,
                    )
                  }
                >
                  额度 / 限速
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy || !chosenLines.length}
                  onClick={exportLines}
                >
                  导出
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={
                    busy ||
                    !chosenLines.length ||
                    chosenLines.some((t) => t.line.customer_plan_id)
                  }
                  onClick={() =>
                    setModal(
                      <FleetLineBatch
                        targets={chosenLines}
                        action="delete"
                        close={close}
                        saved={async () => {
                          await refresh();
                          setSelectedLines([]);
                        }}
                      />,
                    )
                  }
                >
                  删除
                </Button>
                {chosenLines.some((t) => t.line.customer_plan_id) && (
                  <span className="text-xs text-muted-foreground">
                    套餐线路不可删除，可停用并保留历史。
                  </span>
                )}
              </div>

              {table(
                "代理列表",
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>选择 / 代理</th>
                      <th>VPS</th>
                      <th>状态</th>
                      <th>代理速率 / 周期用量</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {slice(filteredLines).map(({ r, line }) => {
                      const t = line.traffic,
                        valid =
                          connected(r, now) &&
                          !!t &&
                          recent(t.sampled_at, now) &&
                          line.rental_status !== "offline" &&
                          line.enabled;
                      const statusText = lineStatus(r, line, now);
                      return (
                        <tr key={key(r, line)}>
                          <td data-label="代理">
                            <label className="flex items-center gap-2 font-medium text-foreground">
                              <input
                                type="checkbox"
                                aria-label={`选择代理 ${r.name} / ${line.name}`}
                                checked={selectedLines.includes(key(r, line))}
                                onChange={() =>
                                  toggle(
                                    key(r, line),
                                    selectedLines,
                                    setSelectedLines,
                                  )
                                }
                              />
                              <span>{line.name}</span>
                            </label>
                          </td>
                          <td data-label="所属 VPS" className="text-xs font-medium">
                            {r.name}
                          </td>
                          <td data-label="状态 / 探测">
                            <Status tone={lineStatusTone(statusText)}>
                              {statusText}
                            </Status>
                            <p className="mt-1 text-[11px] text-muted-foreground tabular-nums">
                              {latestCheck(r, line)
                                ? `最近探测 ${stamp(latestCheck(r, line)!.checked_at)}`
                                : "尚无探测记录"}
                            </p>
                            {latestCheck(r, line) && (
                              <details className="mt-1">
                                <summary className="cursor-pointer text-xs text-primary hover:underline">
                                  探测结果详情
                                </summary>
                                <p className="mt-1 max-w-64 whitespace-normal text-xs text-muted-foreground">
                                  {latestCheck(r, line)!.ok ? "通过" : "失败"} ·{" "}
                                  {latestCheck(r, line)!.latency_ms != null
                                    ? `${latestCheck(r, line)!.latency_ms} ms · `
                                    : ""}
                                  {latestCheck(r, line)!.detail}
                                </p>
                              </details>
                            )}
                            {line.enabled &&
                              line.sync_status === "synced" &&
                              (line.kind !== "forward" ||
                                line.network.includes("tcp")) && (
                                <Button
                                  type="button"
                                  variant="outline"
                                  size="sm"
                                  className="mt-2"
                                  disabled={
                                    busy ||
                                    !connected(r, now) ||
                                    line.rental_status !== "active"
                                  }
                                  onClick={() => void recheck(r, line.id)}
                                >
                                  立即复查
                                </Button>
                              )}
                            {probeFeedback?.key === key(r, line) && (
                              <p role="status" className="mt-1.5 text-xs text-muted-foreground">
                                {probeFeedback.text}
                              </p>
                            )}
                          </td>
                          <td
                            data-label="速率 / 用量"
                            className="whitespace-nowrap tabular-nums text-xs"
                          >
                            <span className="font-medium text-foreground">
                              ↓ {rate(t?.download_bps || 0, valid)} / ↑{" "}
                              {rate(t?.upload_bps || 0, valid)}
                            </span>
                            <p className="mt-0.5 text-[11px] text-muted-foreground">
                              {t ? bytes(t.upload + t.download) : "尚无用量"} ·{" "}
                              {line.limits.monthly_gib
                                ? `额度 ${line.limits.monthly_gib} GiB`
                                : "不限量"}
                            </p>
                            <p className="text-[11px] text-muted-foreground">
                              采样 {stamp(t?.sampled_at || 0)}
                              {!valid && " · 无有效速率"}
                            </p>
                          </td>
                          <td data-label="操作">
                            <div className="flex items-center gap-1.5">
                              <Button
                                size="sm"
                                variant="outline"
                                disabled={busy}
                                onClick={() =>
                                  void run(() => openLine(r, line))
                                }
                              >
                                编辑
                              </Button>
                              <DropdownMenu.Root>
                                <DropdownMenu.Trigger
                                  className="row-menu-trigger"
                                  aria-label={`${r.name} / ${line.name} 更多操作`}
                                  disabled={busy}
                                >
                                  <MoreHorizontal className="size-4" />
                                </DropdownMenu.Trigger>
                                <DropdownMenu.Portal>
                                  <DropdownMenu.Content
                                    className="row-menu-content"
                                    align="end"
                                    sideOffset={4}
                                    collisionPadding={8}
                                  >
                                    <DropdownMenu.Item
                                      onSelect={() =>
                                        void run(() =>
                                          lineAction(r, line, "toggle"),
                                        )
                                      }
                                    >
                                      <Power />
                                      {line.enabled
                                        ? "手动停用"
                                        : "解除手动停用"}
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Item
                                      onSelect={() =>
                                        void run(() =>
                                          lineAction(r, line, "limits"),
                                        )
                                      }
                                    >
                                      <Gauge />
                                      调整额度 / 限速
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Item
                                      disabled={!line.published || !line.enabled}
                                      onSelect={() =>
                                        void run(() =>
                                          lineAction(r, line, "share"),
                                        )
                                      }
                                    >
                                      <Share2 />
                                      分享配置
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Item
                                      onSelect={() =>
                                        void run(() =>
                                          lineAction(r, line, "access"),
                                        )
                                      }
                                    >
                                      <Shield />
                                      网站 / 端口 / 连接限制
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Separator className="my-1 h-px bg-border" />
                                    <DropdownMenu.Item
                                      disabled={!line.published || !line.enabled}
                                      onSelect={() =>
                                        void run(() =>
                                          lineAction(r, line, "probe"),
                                        )
                                      }
                                    >
                                      <Activity />
                                      检查连接
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Item
                                      onSelect={() =>
                                        void run(() =>
                                          lineAction(r, line, "diagnostics"),
                                        )
                                      }
                                    >
                                      <FileText />
                                      日志与诊断
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Item
                                      onSelect={() =>
                                        void run(() => prepareApply(r))
                                      }
                                    >
                                      <CheckCircle2 />
                                      应用配置修改
                                    </DropdownMenu.Item>
                                    {!line.customer_plan_id && (
                                      <>
                                        <DropdownMenu.Separator className="my-1 h-px bg-border" />
                                        <DropdownMenu.Item
                                          className="text-destructive"
                                          onSelect={() => remove(r, line)}
                                        >
                                          <Trash2 />
                                          删除代理
                                        </DropdownMenu.Item>
                                      </>
                                    )}
                                  </DropdownMenu.Content>
                                </DropdownMenu.Portal>
                              </DropdownMenu.Root>
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>,
              )}
            </>
          )}

          {tab === "externals" && (
            <>
              {table(
                "外部代理列表",
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>名称</th>
                      <th>协议 / 地址</th>
                      <th>所属 VPS</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {slice(externals).map(({ r, external: e, protocol, target }) => (
                      <tr key={`${r.id}:${e.id}`}>
                        <td data-label="外部代理" className="font-medium">
                          {e.name}
                        </td>
                        <td data-label="协议 / 地址">
                          <div className="flex flex-wrap items-center gap-2">
                            <Badge variant="primary" appearance="light" size="sm">
                              {protocol}
                            </Badge>
                            <span className="mono break-all text-xs text-muted-foreground">
                              {target}
                            </span>
                          </div>
                        </td>
                        <td data-label="所属 VPS">
                          <span className="text-xs font-medium">{r.name}</span>
                          {!connected(r, now) && (
                            <p className="text-[11px] text-muted-foreground">
                              数据可能已过期
                            </p>
                          )}
                        </td>
                        <td data-label="操作">
                          <div className="flex gap-1.5">
                            <Button
                              size="sm"
                              variant="outline"
                              disabled={busy}
                              onClick={() => void run(() => openExternal(r, e))}
                            >
                              <Pencil />
                              编辑
                            </Button>
                            <Button
                              size="sm"
                              variant="ghost"
                              disabled={busy}
                              onClick={() => remove(r, undefined, e)}
                            >
                              删除
                            </Button>
                          </div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>,
              )}
            </>
          )}

          {!total && (
            <div className="fleet-empty">
              <Network aria-hidden="true" />
              <p>没有符合条件的记录</p>
              <span>调整搜索或筛选条件后重试。</span>
            </div>
          )}

          <Pager total={total} page={currentPage} change={setPage} />
        </Card>
      )}

      {tab === "tasks" && (
        <Card className="fleet-tasks">
          <CardHeader>
            <CardTitle>维护任务</CardTitle>
            <label className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
              <input
                type="checkbox"
                checked={showTaskHistory}
                onChange={(e) => {
                  setShowTaskHistory(e.target.checked);
                  setPage(1);
                }}
              />
              显示历史任务
            </label>
          </CardHeader>
          <CardContent>
            {slice(visibleTasks).map((t) => (
              <div key={t.id} className="fleet-task-row">
                <div className="min-w-0 flex-1">
                  <div className="fleet-task-title">
                    <span className="fleet-task-icon">
                      <Wrench aria-hidden="true" />
                    </span>
                    <strong className="font-semibold text-foreground">
                      {actions[t.action] || t.action}
                    </strong>
                    <Status
                      tone={
                        t.state === "succeeded"
                          ? taskStatus(t) === "成功"
                            ? "good"
                            : "warn"
                          : ["paused", "failed", "completed"].includes(t.state)
                            ? "warn"
                            : "muted"
                      }
                    >
                      {taskStatus(t)}
                    </Status>
                    {t.archived && <Status>已归档</Status>}
                  </div>
                  <p className="text-xs text-muted-foreground tabular-nums">
                    {stamp(t.at || 0)} · {t.targets.length} 台 VPS · 维护完成{" "}
                    {t.targets.filter(maintenanceSucceeded).length} 台 ·
                    验证未通过 {t.targets.filter(verificationFailed).length} 台
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setTaskId(t.id)}
                  >
                    查看任务
                  </Button>
                  {!t.archived && t.state !== "running" && (
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy}
                      onClick={() => archiveTask(t)}
                    >
                      移入历史
                    </Button>
                  )}
                </div>
              </div>
            ))}
            {!visibleTasks.length && (
              <div className="fleet-empty">
                <ShieldCheck aria-hidden="true" />
                <p>暂无待处理维护任务</p>
                <span>已完成或归档的记录可勾选「显示历史任务」查看。</span>
              </div>
            )}
          </CardContent>
          <Pager
            total={visibleTasks.length}
            page={currentPage}
            change={setPage}
          />
        </Card>
      )}

      {tab === "settings" && (
        <div className="fleet-settings">
          <Card className="fleet-access-settings">
            <CardHeader>
              <CardHeading className="flex items-center gap-1.5 space-y-0">
                <CardTitle>接入与授权</CardTitle>
                <InfoTip>
                  每台 VPS 仍保留本地独立面板；接入后总控可统一编排代理、执行批量升级与免密跳转。
                </InfoTip>
              </CardHeading>
            </CardHeader>
            <CardContent className="space-y-4">
              <section className="fleet-setting-section">
                <h4>
                  <ShieldCheck aria-hidden="true" />
                  允许上级总控纳管本机
                </h4>
                {data?.controller ? (
                  <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-card px-3.5 py-2.5 text-xs">
                    <span>
                      已授权总控：<strong className="mono">{data.controller}</strong>
                    </span>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={busy}
                      onClick={() => {
                        if (window.confirm("解除总控授权并恢复本地监控策略？"))
                          void run(async () => {
                            await api("/fleet/unbind", "POST", {});
                            await refresh();
                          });
                      }}
                    >
                      解除授权
                    </Button>
                  </div>
                ) : (
                  <div>
                    <Button
                      variant="outline"
                      disabled={busy}
                      onClick={() =>
                        void run(async () => {
                          const result = await api<{ code: string }>(
                            "/fleet/invite",
                            "POST",
                            {},
                          );
                          setInvite(result.code);
                        })
                      }
                    >
                      生成本机接入码（10 分钟有效）
                    </Button>
                  </div>
                )}
                {invite && (
                  <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-dashed border-input bg-card p-3">
                    <code className="mono break-all text-xs">{invite}</code>
                    <CopyButton size="sm" value={invite} />
                  </div>
                )}
              </section>

              <section className="fleet-setting-section">
                <h4>
                  <Server aria-hidden="true" />
                  接入远端 VPS
                </h4>
                <p className="text-xs leading-relaxed text-muted-foreground">
                  先在目标 VPS 的「多机总控 → 总控设置」生成一次性接入码，再回到本页填写。
                </p>
                <form
                  className="grid gap-3.5 md:grid-cols-2"
                  onSubmit={(e) => {
                    e.preventDefault();
                    const form = e.currentTarget,
                      values = new FormData(form);
                    void run(async () => {
                      await api(
                        "/fleet/connect",
                        "POST",
                        Object.fromEntries(values),
                      );
                      form.reset();
                      await refresh();
                      setMessage("VPS 已接入。");
                    });
                  }}
                >
                  <Field label="VPS 名称">
                    <Input
                      name="name"
                      required
                      maxLength={120}
                      placeholder="例如：东京 CN2-01"
                    />
                  </Field>
                  <Field label="远端面板 HTTPS 地址">
                    <Input
                      name="url"
                      type="url"
                      required
                      placeholder="https://panel.example.com"
                    />
                  </Field>
                  <div className="md:col-span-2">
                    <Field label="远端一次性接入码">
                      <Input
                        name="code"
                        type="password"
                        required
                        autoComplete="off"
                        placeholder="粘贴目标 VPS 生成的 10 分钟有效接入码"
                      />
                    </Field>
                  </div>
                  <div className="md:col-span-2">
                    <Button disabled={busy}>接入面板</Button>
                  </div>
                </form>
              </section>
            </CardContent>
          </Card>

          <Card className="fleet-policy-settings">
            <CardHeader>
              <CardHeading className="flex items-center gap-1.5 space-y-0">
                <CardTitle>统一监控策略</CardTitle>
                <InfoTip>
                  启用后将统一覆盖所有已接入 VPS 的探测周期与阈值，但不会删除各机器原有的本地策略；关闭覆盖后自动恢复。通知渠道在本机「监控告警」中配置。
                </InfoTip>
              </CardHeading>
            </CardHeader>
            <CardContent className="space-y-4">
              <label className="flex items-center gap-2.5 rounded-lg border border-border bg-muted/25 px-3.5 py-2.5 text-xs font-medium text-foreground">
                <input
                  type="checkbox"
                  checked={override}
                  onChange={(e) => setOverride(e.target.checked)}
                />
                覆盖所有已接入 VPS 的本地策略
              </label>
              {override && (
                <div className="fleet-policy-fields">
                  <PolicyFields value={policy} change={setPolicy} />
                </div>
              )}
              <Button
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await api(
                      "/fleet/policy",
                      "POST",
                      override ? policy : null,
                    );
                    await refresh();
                    setMessage("总控策略已保存。");
                  })
                }
              >
                保存总控策略
              </Button>
            </CardContent>
          </Card>
        </div>
      )}

      {activeDetail && (
        <Modal
          drawer
          title={`VPS · ${activeDetail.name}`}
          close={() => setDetail(undefined)}
        >
          <div className="space-y-3 rounded-xl border border-border bg-muted/25 p-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <Status
                tone={health(activeDetail, now) === "在线" ? "good" : "warn"}
              >
                {health(activeDetail, now)}
              </Status>
              <CopyButton
                size="sm"
                value={activeDetail.url}
                label="复制面板地址"
              />
            </div>
            <p className="mono break-all text-xs text-foreground">
              {activeDetail.url}
            </p>
            <Versions remote={activeDetail} />
            <div className="grid gap-2 border-t border-border pt-2.5 text-[11px] text-muted-foreground sm:grid-cols-2">
              <span className="tabular-nums">
                最近同步：{stamp(activeDetail.seen)}
              </span>
              <span className="tabular-nums">
                Agent 上报：{stamp(activeDetail.snapshot?.agent_seen || 0)}
              </span>
            </div>
          </div>

          {issues(activeDetail, now).length > 0 && (
            <div className="space-y-3 rounded-xl border border-amber-500/30 bg-amber-500/10 p-4">
              {issues(activeDetail, now).map((issue, i) => (
                <div key={i}>
                  <p className="text-xs font-semibold text-foreground">
                    {issue.title}
                  </p>
                  <p className="mt-1 break-words text-xs leading-relaxed text-muted-foreground">
                    {issue.detail}
                  </p>
                  {issue.evidence && (
                    <details className="details mt-2">
                      <summary>自动检测详情</summary>
                      <p className="mt-1.5 whitespace-pre-wrap break-words text-xs">
                        {issue.evidence}
                      </p>
                    </details>
                  )}
                  {issue.probeLineId && (
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="mt-2"
                      disabled={busy}
                      onClick={() =>
                        void recheck(activeDetail, issue.probeLineId!)
                      }
                    >
                      立即复查
                    </Button>
                  )}
                </div>
              ))}
            </div>
          )}

          {probeFeedback?.key.startsWith(`${activeDetail.id}:`) && (
            <p
              role="status"
              className="rounded-lg border border-border bg-muted/25 px-3 py-2 text-xs"
            >
              {probeFeedback.text}
            </p>
          )}

          {localNode(activeDetail) && (
            <div className="space-y-3 rounded-xl border border-border bg-card p-4">
              <div className="flex items-center justify-between gap-2">
                <h4 className="text-xs font-semibold text-foreground">
                  系统资源状态
                </h4>
                {(() => {
                  const n = localNode(activeDetail)!,
                    s = n.report.system ?? {},
                    valid =
                      connected(activeDetail, now) &&
                      n.online &&
                      recent(n.seen, remoteNow(activeDetail, now)) &&
                      recent(s.sampled_at, remoteNow(activeDetail, now));
                  return (
                    <span className="text-[11px] text-muted-foreground tabular-nums">
                      采样 {stamp(s.sampled_at || 0)}
                      {!valid && " · 已过期或未知"}
                    </span>
                  );
                })()}
              </div>
              {(() => {
                const n = localNode(activeDetail)!,
                  s = n.report.system ?? {},
                  valid =
                    connected(activeDetail, now) &&
                    n.online &&
                    recent(n.seen, remoteNow(activeDetail, now)) &&
                    recent(s.sampled_at, remoteNow(activeDetail, now));
                const cpuPct =
                  valid && s.cpu_percent != null
                    ? Math.min(100, Math.max(0, s.cpu_percent))
                    : null;
                const memPct =
                  valid && s.memory_total && s.memory_used != null
                    ? Math.min(100, (s.memory_used / s.memory_total) * 100)
                    : null;
                const diskPct =
                  valid && s.disk_total && s.disk_used != null
                    ? Math.min(100, (s.disk_used / s.disk_total) * 100)
                    : null;
                return (
                  <div className="grid gap-3 sm:grid-cols-3">
                    <div className="rounded-lg border border-border bg-muted/20 p-3">
                      <p className="text-xs font-medium tabular-nums">
                        CPU：
                        {cpuPct != null ? `${s.cpu_percent!.toFixed(1)}%` : "—"}
                      </p>
                      {cpuPct != null && (
                        <Progress
                          aria-label="CPU"
                          value={cpuPct}
                          className="mt-2 h-1.5"
                        />
                      )}
                    </div>
                    <div className="rounded-lg border border-border bg-muted/20 p-3">
                      <p className="text-xs font-medium tabular-nums">
                        内存：
                        {memPct != null
                          ? `${bytes(s.memory_used!)} / ${bytes(s.memory_total!)}`
                          : "—"}
                      </p>
                      {memPct != null && (
                        <Progress
                          aria-label="内存"
                          value={memPct}
                          className="mt-2 h-1.5"
                        />
                      )}
                    </div>
                    <div className="rounded-lg border border-border bg-muted/20 p-3">
                      <p className="text-xs font-medium tabular-nums">
                        磁盘：
                        {diskPct != null
                          ? `${bytes(s.disk_used!)} / ${bytes(s.disk_total!)}`
                          : "—"}
                      </p>
                      {diskPct != null && (
                        <Progress
                          aria-label="磁盘"
                          value={diskPct}
                          className="mt-2 h-1.5"
                        />
                      )}
                    </div>
                    {n.report.error && (
                      <p
                        role="alert"
                        className="rounded-lg border border-destructive/25 bg-destructive/10 p-2.5 text-xs text-destructive sm:col-span-3"
                      >
                        {n.report.error}
                      </p>
                    )}
                  </div>
                );
              })()}
            </div>
          )}

          <div className="space-y-2.5">
            <h4 className="text-xs font-semibold text-muted-foreground">
              快捷运维与管理
            </h4>
            <div className="grid gap-2 sm:grid-cols-2">
              <Button
                type="button"
                className="justify-start"
                onClick={() => navigate("lines", activeDetail.id)}
              >
                <Network />
                管理代理
              </Button>
              <Button
                type="button"
                variant="outline"
                className="justify-start"
                onClick={() => navigate("externals", activeDetail.id)}
              >
                <Globe />
                管理外部代理
              </Button>
              <Button
                type="button"
                variant="outline"
                className="justify-start"
                onClick={() => {
                  const remote = activeDetail;
                  setDetail(undefined);
                  setModal(
                    <Diagnostics
                      nodes={remote.snapshot?.nodes || []}
                      request={remoteApi(remote.id)}
                      close={() => setModal(null)}
                    />,
                  );
                }}
              >
                <FileText />
                日志与诊断
              </Button>
              <Button
                type="button"
                variant="outline"
                className="justify-start"
                disabled={busy}
                onClick={() => networkTuning([activeDetail])}
              >
                <Sliders />
                网络算法 / BBR
              </Button>
              <Button
                type="button"
                variant="outline"
                className="justify-start"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await prepareApply(activeDetail);
                    setDetail(undefined);
                  })
                }
              >
                <CheckCircle2 />
                预览并应用配置
              </Button>
              <Button
                type="button"
                variant="outline"
                className="justify-start"
                disabled={busy}
                onClick={() => void run(() => jump(activeDetail))}
              >
                <ExternalLink />
                免密进入面板
              </Button>
            </div>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-destructive/25 bg-destructive/5 p-4">
            <div className="space-y-0.5">
              <p className="text-xs font-semibold text-foreground">
                解除总控纳管
              </p>
              <p className="text-[11px] text-muted-foreground">
                仅移除管理绑定与授权，不会停止或卸载远端正在运行的代理。
              </p>
            </div>
            <Button
              type="button"
              variant="outline"
              className="border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive"
              disabled={busy}
              onClick={() => {
                if (
                  window.confirm(
                    `从总控移除 ${activeDetail.name}？会尝试撤销远端授权；远端不可达时仍移除本地记录，但无法确认远端授权及已有免密会话已失效。此操作不会卸载或停止远端代理。`,
                  )
                )
                  void run(async () => {
                    const result = await api<{
                      remote_revoked?: boolean;
                      warning?: string;
                    }>(`/fleet/${activeDetail.id}`, "DELETE");
                    setMessage(
                      result.remote_revoked
                        ? `${activeDetail.name}：已从总控移除，远端授权已撤销。`
                        : `${activeDetail.name}：已从总控移除；远端授权及已有免密会话的撤销尚未确认。如远端仍在使用，请在该面板解除总控绑定。${result.warning ? `原因：${result.warning}` : ""}`,
                    );
                    setDetail(undefined);
                    await refresh();
                  });
              }}
            >
              从总控移除
            </Button>
          </div>
        </Modal>
      )}

      {taskId && (
        <Modal drawer title="批量任务进度" close={() => setTaskId(undefined)}>
          {error && (
            <p
              role="alert"
              className="rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-xs text-destructive"
            >
              {error}
            </p>
          )}
          {activeTask ? (
            <>
              <div className="space-y-2.5 rounded-xl border border-border bg-muted/25 p-4">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <h3 className="text-sm font-semibold text-foreground">
                    {actions[activeTask.action] || activeTask.action}
                  </h3>
                  <div className="flex items-center gap-1.5">
                    <Status
                      tone={
                        activeTask.state === "succeeded"
                          ? taskStatus(activeTask) === "成功"
                            ? "good"
                            : "warn"
                          : ["paused", "failed", "completed"].includes(
                                activeTask.state,
                              )
                            ? "warn"
                            : "muted"
                      }
                    >
                      {taskStatus(activeTask)}
                    </Status>
                    {activeTask.archived && <Status>已归档</Status>}
                  </div>
                </div>
                {activeTask.version && (
                  <p className="text-xs text-muted-foreground">
                    目标版本 <strong className="text-foreground">{activeTask.version}</strong>
                  </p>
                )}
                <Progress
                  value={
                    activeTask.targets.length
                      ? (activeTask.targets.filter((t) =>
                          activeTask.action === "core-upgrade"
                            ? t.state === "succeeded"
                            : maintenanceSucceeded(t),
                        ).length /
                          activeTask.targets.length) *
                        100
                      : 0
                  }
                  className="h-1.5"
                />
                <p className="text-xs text-muted-foreground tabular-nums">
                  维护完成{" "}
                  {
                    activeTask.targets.filter((t) =>
                      activeTask.action === "core-upgrade"
                        ? t.state === "succeeded"
                        : maintenanceSucceeded(t),
                    ).length
                  }{" "}
                  / {activeTask.targets.length}
                  {` · 验证未通过 ${activeTask.targets.filter(verificationFailed).length}`}
                  {` · 操作失败 ${activeTask.targets.filter((t) => t.state === "failed" && !verificationFailed(t)).length}`}
                </p>
              </div>

              <div className="space-y-3">
                {activeTask.targets.map((t) => {
                  const remote = remotes.find((r) => r.id === t.id);
                  const verifyFailed = verificationFailed(t);
                  const verifyPending =
                    t.id !== "local" &&
                    maintenanceSucceeded(t) &&
                    t.result?.verification?.state === "pending";
                  const maintenanceDone =
                    activeTask.action === "core-upgrade"
                      ? t.state === "succeeded"
                      : maintenanceSucceeded(t);
                  const issue = verifyFailed
                    ? t.result?.verification?.detail ||
                      (t.error || t.result?.error || "").replace(
                        /^组件维护已完成，但整链验证失败[：:]?\s*/,
                        "",
                      )
                    : t.error || t.result?.error;
                  const targetName =
                    t.id === "local" ? "本机（主控）" : remote?.name || t.id;
                  return (
                    <section
                      key={t.id}
                      className="min-w-0 space-y-3 rounded-xl border border-border bg-card p-4"
                      aria-label={`VPS 任务：${targetName}`}
                    >
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <h4 className="break-all text-sm font-semibold text-foreground">
                          {targetName}
                        </h4>
                        <Status
                          tone={
                            verifyFailed || verifyPending
                              ? "warn"
                              : t.state === "failed"
                                ? "bad"
                                : t.state === "succeeded"
                                  ? "good"
                                  : "muted"
                          }
                        >
                          {verifyFailed
                            ? activeTask.action === "core-upgrade"
                              ? t.auto_verify
                                ? "此 VPS 验收未通过"
                                : "连接未通过 · 后续已暂停"
                              : activeTask.archived
                                ? "维护完成 · 验证未通过"
                                : "维护完成 · 自动复查中"
                            : verifyPending
                              ? "维护完成 · 等待连接验证"
                              : statuses[t.state] || t.state}
                        </Status>
                      </div>

                      <div className="grid gap-2 rounded-lg bg-muted/30 p-3 text-xs sm:grid-cols-2">
                        <p>
                          组件维护：
                          <span
                            className={
                              maintenanceDone
                                ? "font-medium text-emerald-600 dark:text-emerald-400"
                                : ""
                            }
                          >
                            {maintenanceDone
                              ? "已完成"
                              : statuses[t.result?.maintenance_state || t.state] ||
                                t.state}
                          </span>
                        </p>
                        <p>
                          后续验证：
                          {t.id === "local"
                            ? "请查看本机网络设置结果"
                            : verifyFailed
                              ? "未通过"
                              : t.result?.verification?.state === "succeeded"
                                ? "已通过"
                                : activeTask.action === "core-upgrade"
                                  ? t.result?.verification?.state === "manual"
                                    ? "需人工核对"
                                    : "等待原链接验证"
                                  : "等待自动验证"}
                        </p>
                      </div>

                      {t.result?.after && (
                        <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                          {Object.entries(t.result.after).map(([k, value]) => (
                            <span key={k}>
                              {(
                                {
                                  panel: "控制台",
                                  agent: "Agent",
                                  chainctl: "管理命令",
                                } as Record<string, string>
                              )[k] || k}{" "}
                              <strong className="text-foreground">{value}</strong>
                            </span>
                          ))}
                        </div>
                      )}

                      {t.result?.network && (
                        <div className="space-y-1 text-xs text-muted-foreground">
                          <p>
                            系统默认：{t.result.network.after.congestion_control} +{" "}
                            {t.result.network.after.qdisc}
                          </p>
                          <p>
                            网卡实际队列：
                            {t.result.network.after.interfaces
                              .map((q) => `${q.dev} / ${q.parent}: ${q.kind}`)
                              .join("；") || "未知"}
                          </p>
                          <p>
                            {t.result.network.after.warning ||
                              "未重启服务器或服务；现有连接与监听器不会自动切换 TCP 算法。"}
                          </p>
                        </div>
                      )}

                      {remote &&
                        activeTask.action === "core-upgrade" &&
                        t.state === "awaiting_verification" &&
                        t.auto_verify &&
                        t.result?.verification?.state !== "manual" && (
                          <p className="text-xs text-muted-foreground">
                            自动验收中 ·{" "}
                            {t.result?.verification?.detail || "等待原任务回执"}
                          </p>
                        )}

                      {remote &&
                        activeTask.action === "core-upgrade" &&
                        t.state === "awaiting_verification" &&
                        (!t.auto_verify ||
                          t.result?.verification?.state === "manual") && (
                          <div className="space-y-2">
                            <p className="text-xs">
                              {t.result?.verification?.detail ||
                                "请用原链接确认实际连接、旧客户端及需要的 UDP 功能。"}
                            </p>
                            <div className="flex flex-wrap gap-2">
                              {(["core-confirm", "core-reject"] as const).map(
                                (action) => (
                                  <Button
                                    key={action}
                                    size="sm"
                                    variant={
                                      action === "core-confirm"
                                        ? "primary"
                                        : "outline"
                                    }
                                    disabled={busy}
                                    onClick={() => {
                                      if (
                                        window.confirm(
                                          action === "core-confirm"
                                            ? "确认已使用升级前的原链接完成实际传输验证？"
                                            : "将这台 VPS 的链接验收记为失败？Agent 和现有服务继续运行。",
                                        )
                                      )
                                        void run(async () => {
                                          const request = remoteApi(remote.id);
                                          try {
                                            const receipt = await request<{
                                              id: string;
                                              action?: string;
                                              state: string;
                                              error?: string;
                                            }>("/federation/task", "POST", {
                                              id: coreDecisionId(
                                                t.task_id!,
                                                action,
                                              ),
                                              action,
                                              task_id: t.task_id,
                                              core: activeTask.core,
                                              ...(action === "core-reject"
                                                ? { keep_agent_on_failure: true }
                                                : {}),
                                            });
                                            forgetFailedDecision(receipt);
                                            if (receipt.state === "failed")
                                              throw new Error(
                                                receipt.error || "内核处理失败",
                                              );
                                            if (
                                              ["queued", "running"].includes(
                                                receipt.state,
                                              )
                                            )
                                              setMessage(
                                                `${remote.name}：请求已受理，正在查询原任务。`,
                                              );
                                          } catch (err) {
                                            const original = await request<{
                                              state: string;
                                            }>("/federation/task", "POST", {
                                              action: "status",
                                              id: t.task_id,
                                            }).catch(() => undefined);
                                            if (
                                              original?.state !==
                                              (action === "core-confirm"
                                                ? "succeeded"
                                                : "failed")
                                            )
                                              throw err;
                                          }
                                          await refresh();
                                        });
                                    }}
                                  >
                                    {action === "core-confirm"
                                      ? "原链接已验证，继续"
                                      : "报告链接验证失败"}
                                  </Button>
                                ),
                              )}
                            </div>
                          </div>
                        )}

                      {issue ? (
                        <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-xs leading-relaxed text-amber-800 dark:text-amber-200">
                          <p className="font-semibold">
                            {verifyFailed
                              ? activeTask.action === "core-upgrade"
                                ? t.auto_verify
                                  ? "此 VPS 内核验证失败"
                                  : "内核验证失败，批次已暂停"
                                : "组件维护成功，后续连接检查未通过"
                              : "操作未完成"}
                          </p>
                          <p className="mt-1 whitespace-pre-wrap break-words">
                            {issue}
                          </p>
                          {verifyFailed &&
                            !activeTask.archived &&
                            activeTask.action !== "core-upgrade" && (
                              <p className="mt-2">
                                系统会自动复查连接，不会重复升级，也不阻塞其他 VPS。
                                {t.result?.verification?.retry_at
                                  ? `下次复查：${stamp(t.result.verification.retry_at)}`
                                  : ""}
                              </p>
                            )}
                        </div>
                      ) : (
                        (t.result?.verification?.state === "succeeded"
                          ? t.result.verification.detail
                          : t.result?.detail) && (
                          <p className="whitespace-pre-wrap break-words text-xs text-muted-foreground">
                            {t.result?.verification?.state === "succeeded"
                              ? t.result.verification.detail
                              : t.result?.detail}
                          </p>
                        )
                      )}

                      <div className="flex flex-wrap gap-2">
                        {remote &&
                          maintenanceDone &&
                          !["enable", "disable", "core-upgrade"].includes(
                            activeTask.action,
                          ) &&
                          !activeTask.archived && (
                            <Button
                              type="button"
                              size="sm"
                              variant="outline"
                              disabled={busy || verifyPending}
                              onClick={() =>
                                void run(async () => {
                                  await api(
                                    `/fleet/batch/${activeTask.id}/verify/${t.id}`,
                                    "POST",
                                    {},
                                  );
                                  await refresh();
                                })
                              }
                            >
                              {verifyPending ? "等待连接复查" : "立即复查连接"}
                            </Button>
                          )}
                        {remote && (
                          <Button
                            type="button"
                            size="sm"
                            variant="outline"
                            onClick={() => {
                              const returnTask = activeTask.id;
                              setTaskId(undefined);
                              setModal(
                                <Diagnostics
                                  nodes={remote.snapshot?.nodes || []}
                                  request={remoteApi(remote.id)}
                                  close={() => {
                                    setModal(null);
                                    setTaskId(returnTask);
                                  }}
                                />,
                              );
                            }}
                          >
                            查看此 VPS 日志
                          </Button>
                        )}
                      </div>
                    </section>
                  );
                })}
              </div>

              {!activeTask.archived &&
                ["paused", "completed"].includes(activeTask.state) && (
                  <div className="space-y-1.5 rounded-xl border border-amber-500/30 bg-amber-500/10 p-4 text-xs leading-relaxed text-amber-800 dark:text-amber-200">
                    <p className="font-semibold">
                      {activeTask.state === "paused"
                        ? activeTask.targets.some((t) => t.auto_verify)
                          ? "部分 VPS 等待验收结果"
                          : "批次已暂停，未执行的 VPS 仍等待继续"
                        : "维护已结束，请查看各 VPS 的独立结果"}
                    </p>
                    <p>
                      {activeTask.targets.some(verificationFailed)
                        ? "已完成的维护不会重做，连接会自动复查；继续任务只处理未完成的 VPS。"
                        : activeTask.targets.some((t) => t.state === "failed")
                          ? "先排查失败原因。核对原任务不会重新执行已失败操作；确需重试时选择失败对象新建任务。"
                          : "操作结果尚未确认。请先核对原任务，确认成功后继续后续 VPS。"}
                    </p>
                    <p>
                      等待执行：
                      {activeTask.targets
                        .filter((t) => t.state === "pending")
                        .map(
                          (t) =>
                            remotes.find((r) => r.id === t.id)?.name || t.id,
                        )
                        .join("、") || "无"}
                    </p>
                  </div>
                )}

              {activeTask.archived && (
                <p className="rounded-lg border border-border bg-muted/25 p-3 text-xs text-muted-foreground">
                  此任务已忽略并归档，保留原始结果，不再继续后续 VPS。归档不代表线路问题已修复。
                </p>
              )}

              <div className="flex flex-wrap gap-2">
                {!activeTask.archived &&
                  ["paused", "completed"].includes(activeTask.state) &&
                  activeTask.targets.some(
                    (t) => t.state === "failed" && !verificationFailed(t),
                  ) && (
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      onClick={() => {
                        const ids = activeTask.targets
                          .filter(
                            (t) =>
                              ["failed", "pending"].includes(t.state) &&
                              (t.auto_verify || !verificationFailed(t)),
                          )
                          .map((t) => t.id);
                        if (["enable", "disable"].includes(activeTask.action)) {
                          setSelectedLines(
                            ids.flatMap((id) =>
                              (activeTask.lines?.[id] || []).map(
                                (line) => `${id}:${line}`,
                              ),
                            ),
                          );
                          navigate("lines");
                        } else {
                          setSelected(ids.filter((id) => id !== "local"));
                          if (activeTask.action === "network-tune")
                            setIncludeLocal(ids.includes("local"));
                          navigate("vps");
                        }
                        setTaskId(undefined);
                        setMessage(
                          "已选择失败及未执行对象。请核对选择范围，再提交新任务；未知结果不会自动重试。",
                        );
                      }}
                    >
                      选择失败及未执行对象
                    </Button>
                  )}

                {!activeTask.archived &&
                  ["paused", "completed"].includes(activeTask.state) && (
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      disabled={busy}
                      onClick={() =>
                        void run(async () => {
                          await api(
                            `/fleet/batch/${activeTask.id}/resume`,
                            "POST",
                            {},
                          );
                          await refresh();
                        })
                      }
                    >
                      {activeTask.targets.some((t) => t.state === "pending")
                        ? "继续未执行任务"
                        : "重新核对原任务结果"}
                    </Button>
                  )}

                {!activeTask.archived && activeTask.state !== "running" && (
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    onClick={() => archiveTask(activeTask)}
                  >
                    忽略并归档
                  </Button>
                )}
              </div>
            </>
          ) : (
            <p className="text-xs text-muted-foreground">
              任务已提交，正在等待同步结果；关闭后可在维护任务中继续查看。
            </p>
          )}
        </Modal>
      )}

      {editor && (
        <LineEditor
          line={editor.line}
          kind={editor.line?.kind || "vless"}
          state={editor.state}
          remoteName={editor.remote.name}
          defaultHost={new URL(editor.remote.url).hostname.replace(
            /^\[|\]$/g,
            "",
          )}
          request={remoteApi(editor.remote.id)}
          close={() => setEditor(undefined)}
          ensureLocalServer={async () => {
            const node = editor.state.nodes.find(
              (n) => n.id === editor.state.local_node_id,
            );
            if (!node?.online)
              throw new Error("远端 Agent 尚未在线，请先修复服务。");
            return node;
          }}
          saved={async () => {
            const r = editor.remote;
            setEditor(undefined);
            markPending(r);
            setMessage(`${r.name}：配置已保存，尚未应用。`);
            try {
              await freshState(r);
              await prepareApply(r);
            } catch (e) {
              setError(
                `配置已保存，但读取应用预览失败：${errorText(e)}。请在 VPS 详情中继续应用。`,
              );
            }
          }}
        />
      )}

      {modal}
    </div>
  );
}
