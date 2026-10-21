import { publishChecked } from "../api";
import { Diagnostics } from "./diagnostics";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api, remoteApi } from "../api";
import type { External, Line, PanelState, Preview } from "../types";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import {
  Field,
  CopyButton,
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
  requestId,
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
const key = (r: Remote, line: Line) => `${r.id}:${line.id}`;
const rate = (value: number, valid: boolean) =>
  valid ? `${bytes(value)}/s` : "—";
function Pager({
  total,
  page,
  change,
}: {
  total: number;
  page: number;
  change: (page: number) => void;
}) {
  const pages = Math.max(1, Math.ceil(total / PAGE_SIZE));
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 border-t p-4">
      <span className="muted">
        共 {total} 条 · 每页 {PAGE_SIZE} 条 · 第 {page} / {pages} 页
      </span>
      <div className="flex gap-2">
        <Button
          variant="outline"
          disabled={page <= 1}
          onClick={() => change(page - 1)}
        >
          上一页
        </Button>
        <Button
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
    <span className="text-xs">
      面板 {r.snapshot?.version || "未知"} · Agent{" "}
      {r.snapshot?.agent_version || "版本未知（尚未上报）"}
      {mismatch(r) && (
        <strong className="ml-2 text-amber-600">版本不一致</strong>
      )}
    </span>
  );
}
function Trend({ points, now }: { points: Point[]; now: number }) {
  const [selected, setSelected] = useState<number>();
  const active = points.find((p) => p.at === selected) || points.at(-1);
  const largest = Math.max(1, ...points.flatMap((p) => [p.rx || 0, p.tx || 0]));
  return (
    <Card>
      <CardHeader>
        <CardTitle>网卡速率 · 最近 10 分钟</CardTitle>
      </CardHeader>
      <CardContent>
        <p className="muted mb-3">
          ↓ 接收（蓝） / ↑ 发送（绿） · 当前页面会话采样；每个点显示当时有效 VPS
          数，缺失数据留空。
        </p>
        <div className="flex justify-between text-xs text-slate-500">
          <span>上限 {bytes(largest)}/s</span>
          <span>点击采样点查看数值</span>
        </div>
        <div
          className="relative my-2 h-32 border-y"
          aria-label={`网卡速率趋势，共 ${points.length} 个采样点`}
        >
          <div className="absolute inset-x-0 top-1/2 border-t border-dashed">
            <span className="bg-background text-xs text-slate-400">
              {bytes(largest / 2)}/s
            </span>
          </div>
          {points.map((p, i) => (
            <button
              type="button"
              key={`${p.at}:${i}`}
              aria-label={`查看采样 ${stamp(p.at)}`}
              onClick={() => setSelected(p.at)}
              className="absolute inset-y-0 flex w-3 items-end gap-px focus-visible:outline focus-visible:outline-blue-500"
              style={{
                left: `clamp(0px, ${((p.at - (now - 600)) / 600) * 100}%, calc(100% - 12px))`,
              }}
            >
              {p.rx != null && (
                <span
                  className="w-1.5 bg-blue-500"
                  style={{ height: `${Math.max(1, (p.rx / largest) * 100)}%` }}
                />
              )}
              {p.tx != null && (
                <span
                  className="w-1.5 bg-emerald-500"
                  style={{ height: `${Math.max(1, (p.tx / largest) * 100)}%` }}
                />
              )}
              {p.rx == null && (
                <span className="absolute bottom-0 size-2 rounded-full border border-slate-400" />
              )}
            </button>
          ))}
        </div>
        <div className="flex justify-between text-xs text-slate-500">
          <span>{stamp(now - 600)}</span>
          <span>现在 · 0 B/s 为基线</span>
        </div>
        <p role="status" className="mt-3 text-sm">
          {active
            ? `${stamp(active.at)} · 有效 ${active.hosts} 台 · ↓ ${rate(active.rx || 0, active.rx != null)} · ↑ ${rate(active.tx || 0, active.tx != null)}`
            : "等待采样"}
        </p>
        <details className="mt-3 text-sm">
          <summary>查看采样数值与覆盖范围</summary>
          <div className="max-h-48 overflow-auto">
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
                      <td>{stamp(p.at)}</td>
                      <td>{p.hosts}</td>
                      <td>
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

export function Fleet() {
  const [data, setData] = useState<FleetState>();
  const [error, setError] = useState(""),
    [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false),
    [tab, setTab] = useState<Tab>("overview");
  const [search, setSearch] = useState(""),
    [filter, setFilter] = useState(""),
    [host, setHost] = useState("");
  const [page, setPage] = useState(1);
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
  const serverClock = useRef({ at: Date.now() / 1000, received: performance.now() });
  const controllerNow = () => serverClock.current.at + (performance.now() - serverClock.current.received) / 1000;
  const [connectionError, setConnectionError] = useState("");
  const [pendingApply, setPendingApply] = useState<string[]>([]);
  const [selected, setSelected] = useState<string[]>([]),
    [selectedLines, setSelectedLines] = useState<string[]>([]);
  const [modal, setModal] = useState<ReactNode>(null),
    [detail, setDetail] = useState<string>();
  const [taskId, setTaskId] = useState<string>();
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
  const [invite, setInvite] = useState(""),
    [policy, setPolicy] = useState<Policy>(defaultPolicy),
    [override, setOverride] = useState(false);
  const [points, setPoints] = useState<Point[]>([]);
  const lastSample = useRef("");
  const refresh = useCallback(async (initial = false) => {
    const value = await api<FleetState>("/fleet");
    serverClock.current = { at: value.server_now ?? Date.now() / 1000, received: performance.now() };
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
          `${r.id}:${localNode(r)?.report.system.sampled_at}:${connected(r, now)}:${recent(localNode(r)?.seen, remoteNow(r, now))}:${recent(localNode(r)?.report.system.sampled_at, remoteNow(r, now))}`,
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
  const running = tasks.filter((t) => ["running", "paused"].includes(t.state));
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
      matches(`${r.name} ${line.name}`) &&
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
      (r.snapshot?.externals || []).map((external) => ({ r, external })),
    )
    .filter(
      ({ r, external }) =>
        (!host || r.id === host) && matches(`${r.name} ${external.name}`),
    );
  const total =
    tab === "vps"
      ? filteredVps.length
      : tab === "lines"
        ? filteredLines.length
        : tab === "externals"
          ? externals.length
          : tasks.length;
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
  const markPending = (r: Remote) =>
    setPendingApply((old) => [...new Set([...old, r.id])]);
  async function prepareApply(r: Remote) {
    const preview = await remoteApi(r.id)<Preview>("/preview", "POST", {});
    setModal(
      <Modal
        title={`应用修改 · ${r.name}`}
        description="配置已保存，尚未应用。以下代理可能短暂重连；关闭后可在 VPS 详情中继续应用。"
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
        <p>{preview.message}</p>
        <ul className="max-h-64 overflow-auto text-sm">
          {preview.affected_lines.map((name, i) => (
            <li key={i}>{name}</li>
          ))}
        </ul>
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
  async function openExternal(r: Remote, external?: External) {
    const state = await freshState(r),
      current = external && state.externals.find((e) => e.id === external.id);
    if (external && !current) throw new Error("该外部代理已不存在。");
    setDetail(undefined);
    setModal(
      <Modal
        title={`${current ? "编辑" : "导入"}外部代理 · ${r.name}`}
        description={`操作目标：${r.name}。修改保留引用关系，保存后预览受影响的代理。`}
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
        description="删除后无法撤回；应用配置可能中断连接。"
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
        <p>{line?.name || external?.name}</p>
      </Modal>,
    );
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
      version = release?.version || "";
    const blocked = targets.filter(
      (r) => !connected(r, now) || (!isLines && !r.snapshot?.maintenance)
        || (action === "restart-all" && !r.snapshot?.maintenance_actions?.includes(action)),
    );
    setModal(
      <Modal
        title={`确认${actions[action]}`}
        description={`${targets.length} 台 VPS${isLines ? ` / ${chosenLines.length} 条代理` : ""}${action === "upgrade" ? ` · 目标 ${version}，升级后自动重启相关服务` : ""}。操作可能中断连接，失败后暂停后续机器。`}
        submit={
          blocked.length || (action === "upgrade" && !release)
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
            lines: lineMap,
          });
          close();
          setTaskId(id);
          setMessage("任务已提交，可在维护任务中查看逐台结果。");
          try {
            await refresh();
          } catch (e) {
            setError(`任务已提交，状态刷新失败：${errorText(e)}`);
          }
        }}
      >
        <ul className="max-h-64 space-y-2 overflow-auto">
          {targets.map((r) => (
            <li key={r.id}>
              <strong>{r.name}</strong>
              {isLines && (
                <p className="muted">
                  {chosenLines
                    .filter((v) => v.r.id === r.id)
                    .map((v) => v.line.name)
                    .join("、")}
                </p>
              )}
            </li>
          ))}
        </ul>
        {blocked.length > 0 && (
          <p role="alert" className="text-amber-700">
            无法执行：
            {blocked
              .map(
                (r) =>
                  `${r.name}（${!connected(r, now) ? "失联或数据过期" : !r.snapshot?.maintenance ? "尚未安装维护组件" : "请先升级以支持整套重启"}）`,
              )
              .join("、")}
            。请修复或调整选择；不会自动跳过。
          </p>
        )}
      </Modal>,
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
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">运行与维护</h2>
          <p className="muted">
            远端约 30 秒同步，页面每 5 秒刷新；超过 90 秒的采样不作为当前速率。
          </p>
        </div>
        <Button
          variant="outline"
          disabled={busy}
          onClick={() => void run(() => refresh())}
        >
          刷新总控
        </Button>
      </div>
      {connectionError && (
        <p
          role="alert"
          className="rounded border border-red-200 p-3 text-red-700"
        >
          总控连接中断：{connectionError}
          。下方为最后已知数据，过期速率会自动隐藏。
        </p>
      )}
      {pendingApply
        .filter((id) => remotes.some((r) => r.id === id))
        .map((id) => (
          <div
            key={id}
            className="flex flex-wrap items-center justify-between gap-2 rounded border border-amber-200 p-3"
          >
            <span>
              {remotes.find((r) => r.id === id)!.name}：配置已保存，尚未应用
            </span>
            <Button
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
          className="rounded border border-red-200 p-3 text-red-700"
        >
          {error}
        </p>
      )}
      {message && (
        <p role="status" className="rounded border p-3 text-sm">
          {message}
        </p>
      )}
      <nav aria-label="总控功能" className="flex flex-wrap gap-2">
        {tabs.map(([id, label]) => (
          <Button
            key={id}
            variant={tab === id ? "primary" : "outline"}
            aria-current={tab === id ? "page" : undefined}
            onClick={() => navigate(id)}
          >
            {label}
          </Button>
        ))}
      </nav>
      {running.length > 0 && (
        <div className="flex flex-wrap items-center gap-2 rounded border border-blue-200 p-3 text-sm">
          <span>{running.length} 个运行中或暂停的任务</span>
          {running.slice(0, 3).map((t) => (
            <Button
              key={t.id}
              variant="outline"
              onClick={() => setTaskId(t.id)}
            >
              {actions[t.action] || t.action} · {statuses[t.state] || t.state}
            </Button>
          ))}
        </div>
      )}
      {!data && <p>正在加载总控…</p>}
      {tab === "overview" && (
        <>
          <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
            {[
              [
                "VPS 状态",
                `${remotes.filter((r) => connected(r, now)).length} / ${remotes.length} 可同步`,
                `${remotes.filter((r) => health(r, now) !== "在线").length} 台需要关注 · ${remotes.filter(mismatch).length} 台版本不一致`,
              ],
              [
                "VPS 网卡速率",
                `↓ ${rate(stats.rx, stats.hosts > 0)}`,
                `↑ ${rate(stats.tx, stats.hosts > 0)} · 有效 ${stats.hosts}/${remotes.length} 台`,
              ],
              [
                "代理业务速率",
                `↓ ${rate(stats.download, stats.sampled > 0)}`,
                `↑ ${rate(stats.upload, stats.sampled > 0)} · 有效 ${stats.sampled}/${stats.lines} 条`,
              ],
              [
                "代理周期用量",
                stats.usage ? bytes(stats.used) : "尚未采集",
                `包含 ${stats.usage}/${stats.lines} 条代理最近已知用量`,
              ],
            ].map(([label, value, detail]) => (
              <Card key={label}>
                <CardContent className="p-4">
                  <p className="muted">{label}</p>
                  <p className="my-2 text-xl font-semibold tabular-nums">
                    {value}
                  </p>
                  <p className="text-xs text-slate-500">{detail}</p>
                </CardContent>
              </Card>
            ))}
          </div>
          <p className="muted">
            网卡包含全部业务；代理用量为各自当前周期合计，周期可能不同，跨机器代理链不去重，不代表运营商账单。失联
            VPS 的历史用量仍保留。
          </p>
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border p-4">
            <p className="text-sm">
              代理状态：
              {
                allLines.filter(
                  ({ r, line }) =>
                    !["已启用", "已停用", "状态未知"].includes(lineStatus(r, line, now)),
                ).length
              }{" "}
              条有状态提示 · {allLines.filter(({ r, line }) => lineStatus(r, line, now) === "状态未知").length} 条因 VPS 同步异常而状态未知（不等于代理故障） · {allLines.filter(({ r, line }) => lineStatus(r, line, now) === "已停用").length}{" "}
              条已停用
            </p>
            <Button
              variant="outline"
              onClick={() => {
                navigate("lines");
                setFilter("attention");
              }}
            >
              查看代理状态
            </Button>
          </div>
          <Trend points={points.filter((p) => now - p.at <= 600)} now={now} />
          <Card>
            <CardHeader>
              <CardTitle>需要关注的 VPS</CardTitle>
              <Button
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
            <CardContent className="space-y-3">
              {remotes
                .filter((r) => health(r, now) !== "在线" || mismatch(r))
                .sort((a, b) => priority(a, now) - priority(b, now))
                .slice(0, 5)
                .map((r) => (
                  <div
                    key={r.id}
                    className="flex flex-wrap items-center justify-between gap-2 border-b pb-3"
                  >
                    <div>
                      <p className="font-medium">
                        {r.name} · {health(r, now)}
                      </p>
                      <Versions remote={r} />
                      <ul className="mt-2 max-w-3xl space-y-2 text-sm">
                        {issues(r, now).map((issue, i) => (
                          <li key={i} className="break-words">{issue.detail}</li>
                        ))}
                      </ul>
                      <p className="muted">
                        最近同步 {stamp(r.seen)}
                      </p>
                    </div>
                    <div className="flex flex-wrap gap-2">
                      <Button variant="outline" onClick={() => setDetail(r.id)}>
                        查看 VPS
                      </Button>
                      <Button variant="outline" onClick={() => setModal(
                        <Diagnostics nodes={r.snapshot?.nodes || []} request={remoteApi(r.id)} close={() => setModal(null)} />,
                      )}>
                        日志与诊断
                      </Button>
                    </div>
                  </div>
                ))}
              {!remotes.length ? (
                <p>
                  尚未接入 VPS。
                  <Button
                    variant="outline"
                    onClick={() => navigate("settings")}
                  >
                    接入 VPS
                  </Button>
                </p>
              ) : (
                remotes.every(
                  (r) => health(r, now) === "在线" && !mismatch(r),
                ) && (
                  <p className="muted">
                    当前没有 VPS
                    状态或版本异常；代理的实际出网状态需结合探测结果判断。
                  </p>
                )
              )}
            </CardContent>
          </Card>
        </>
      )}
      {["vps", "lines", "externals"].includes(tab) && (
        <Card>
          <CardHeader>
            <CardTitle>
              {tab === "vps"
                ? "VPS 列表"
                : tab === "lines"
                  ? "所有代理"
                  : "外部代理"}
            </CardTitle>
          </CardHeader>
          <div className={tab === "vps" ? "grid grid-cols-1 gap-3 border-b bg-muted/10 p-4 sm:grid-cols-[minmax(12rem,1fr)_auto_auto_auto] sm:items-center" : "flex flex-wrap gap-3 border-b p-4"} aria-label={tab === "vps" ? "搜索与筛选 VPS" : undefined}>
            <Input
              className={tab === "vps" ? "w-full" : "sm:max-w-72"}
              aria-label="搜索总控列表"
              placeholder={tab === "vps" ? "搜索 VPS 名称或地址" : "搜索 VPS、地址或代理名称"}
              value={search}
              onChange={(e) => {
                setSearch(e.target.value);
                setPage(1);
              }}
            />
            {tab !== "externals" && (
              <select
                aria-label="列表排序"
                className={tab === "vps" ? "select w-full sm:w-auto" : "select w-auto"}
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
              <Button variant="ghost" onClick={() => navigate("vps")}>
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
                className={tab === "vps" ? "select w-full sm:w-auto" : "select w-auto"}
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
              <div className="space-y-3 border-b p-4" aria-label="VPS 批量操作">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <p className="text-sm"><strong>已选 {chosen.length} 台 VPS</strong><span className="ml-2 text-xs text-muted-foreground">含其他页与隐藏项</span></p>
                  <div className="flex flex-wrap gap-1">
                    <Button size="sm" variant="ghost" onClick={() => setSelected([...new Set([...selected, ...slice(filteredVps).map(r => r.id)])])}>选择本页</Button>
                    <Button size="sm" variant="ghost" onClick={() => setSelected([...new Set([...selected, ...filteredVps.map(r => r.id)])])}>选择全部筛选结果</Button>
                    <Button size="sm" variant="ghost" disabled={!chosen.length} onClick={() => setSelected([])}>清空选择</Button>
                  </div>
                </div>
                {chosen.length > 0 && <div className="space-y-3 rounded-lg border bg-muted/20 p-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <Button disabled={busy || releaseLoading || !release || running.some(t => t.state === "running")} onClick={() => batch("upgrade")}>逐台升级</Button>
                    <Button variant="outline" disabled={busy || releaseLoading} onClick={() => void fetchRelease()}>{releaseLoading ? "正在获取最新版本…" : "检查最新版本"}</Button>
                    <span className="text-xs text-muted-foreground">{release ? `最新正式版 ${release.version}` : "先检查版本，再升级所选 VPS"}</span>
                  </div>
                  <details className="text-sm">
                    <summary className="cursor-pointer text-muted-foreground">重启与维护说明</summary>
                    <div className="mt-3 space-y-3 border-t pt-3">
                      <div className="flex flex-wrap gap-2">{["restart-all", "restart-panel", "restart-agent"].map(action => <Button key={action} variant="outline" disabled={busy || running.some(t => t.state === "running")} onClick={() => batch(action)}>{actions[action]}</Button>)}</div>
                      <p className="text-xs leading-5 text-muted-foreground">升级会更新整套发行版本并重启相关服务，失败后暂停后续机器。远端需已安装 Linux systemd 维护组件。</p>
                      {release && <p className="text-xs text-muted-foreground">版本包含：控制台／Agent {release.version} · Xray {release.xray || "未知"} · Mihomo {release.mihomo || "未知"}</p>}
                    </div>
                  </details>
                </div>}
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
                            <label className="flex items-center gap-2">
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
                            <div className="ml-6 mt-1 space-y-1 text-xs text-muted-foreground">
                              <p className="max-w-56 break-all">{r.url}</p>
                              <Versions remote={r} />
                            </div>
                          </td>
                          <td data-label="状态" className="align-top">
                            <Status
                              tone={health(r, now) === "在线" ? "good" : "warn"}
                            >
                              {health(r, now)}
                            </Status>
                            <p className="mt-2 max-w-sm text-xs leading-5 text-muted-foreground">{issues(r, now)[0]?.detail}</p>
                            <p className="muted">最近同步 {stamp(r.seen)}</p>
                          </td>
                          <td
                            data-label="网卡速率"
                            className="whitespace-nowrap"
                          >
                            ↓ {rate(m.rx, m.hosts > 0)}
                            <br />↑ {rate(m.tx, m.hosts > 0)}
                          </td>
                          <td data-label="周期用量">
                            {m.usage ? bytes(m.used) : "—"}
                            <p className="muted">
                              {m.lines} 条 · 最近已知周期用量
                            </p>
                          </td>
                          <td data-label="操作">
                            <Button
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
              <div className="sticky top-0 z-10 flex flex-wrap items-center gap-2 border-b bg-background p-4">
                <span className="text-sm">
                  已选 {chosenLines.length} 条代理 /{" "}
                  {new Set(chosenLines.map((v) => v.r.id)).size} 台
                  VPS（含隐藏项）
                </span>
                <Button
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
                  variant="ghost"
                  disabled={!chosenLines.length}
                  onClick={() => setSelectedLines([])}
                >
                  清空选择
                </Button>
                {["enable", "disable"].map((action) => (
                  <Button
                    key={action}
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
              </div>
              <p className="muted px-4 py-2">
                速率表示当前业务吞吐，不是带宽测速；低吞吐不代表节点变慢。探测详情以最近一次结果和时间为准。
              </p>
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
                      return (
                        <tr key={key(r, line)}>
                          <td data-label="代理">
                            <label className="flex gap-2">
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
                              {line.name}
                            </label>
                          </td>
                          <td data-label="所属 VPS">{r.name}</td>
                          <td data-label="状态 / 探测">
                            <span>{lineStatus(r, line, now)}</span>
                            <p className="muted">
                              {latestCheck(r, line)
                                ? `最近探测 ${stamp(latestCheck(r, line)!.checked_at)}`
                                : "尚无探测记录"}
                            </p>
                            {latestCheck(r, line) && (
                              <details>
                                <summary className="cursor-pointer text-xs">
                                  探测结果详情
                                </summary>
                                <p className="max-w-64 whitespace-normal text-xs">
                                  {latestCheck(r, line)!.ok ? "通过" : "失败"} ·{" "}
                                  {latestCheck(r, line)!.latency_ms != null
                                    ? `${latestCheck(r, line)!.latency_ms} ms · `
                                    : ""}
                                  {latestCheck(r, line)!.detail}
                                </p>
                              </details>
                            )}
                          </td>
                          <td
                            data-label="速率 / 用量"
                            className="whitespace-nowrap"
                          >
                            ↓ {rate(t?.download_bps || 0, valid)} / ↑{" "}
                            {rate(t?.upload_bps || 0, valid)}
                            <p className="muted">
                              {t ? bytes(t.upload + t.download) : "尚无用量"} ·{" "}
                              {line.limits.monthly_gib
                                ? `额度 ${line.limits.monthly_gib} GiB`
                                : "不限量"}
                            </p>
                            <p className="muted">
                              采样 {stamp(t?.sampled_at || 0)}
                              {!valid && " · 无有效速率"}
                            </p>
                          </td>
                          <td data-label="操作">
                            <div className="flex gap-2">
                              <Button
                                variant="outline"
                                disabled={busy}
                                onClick={() =>
                                  void run(() => openLine(r, line))
                                }
                              >
                                编辑
                              </Button>
                              <Button
                                variant="ghost"
                                disabled={busy}
                                onClick={() => remove(r, line)}
                              >
                                删除
                              </Button>
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
              <p className="muted p-4">
                外部代理是出网配置，不具有独立的速率采样。选择一台 VPS
                后可导入。
              </p>
              {table(
                "外部代理列表",
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>名称</th>
                      <th>所属 VPS</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {slice(externals).map(({ r, external: e }) => (
                      <tr key={`${r.id}:${e.id}`}>
                        <td data-label="外部代理">{e.name}</td>
                        <td data-label="所属 VPS">
                          {r.name}
                          {!connected(r, now) && (
                            <p className="muted">数据可能已过期</p>
                          )}
                        </td>
                        <td data-label="操作">
                          <div className="flex gap-2">
                            <Button
                              variant="outline"
                              disabled={busy}
                              onClick={() => void run(() => openExternal(r, e))}
                            >
                              编辑
                            </Button>
                            <Button
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
          {!total && <p className="p-5 muted">没有符合条件的记录。</p>}
          <Pager total={total} page={currentPage} change={setPage} />
        </Card>
      )}
      {tab === "tasks" && (
        <Card>
          <CardHeader>
            <CardTitle>维护任务</CardTitle>
          </CardHeader>
          <CardContent>
            {slice(tasks).map((t) => (
              <div
                key={t.id}
                className="flex flex-wrap items-center justify-between gap-3 border-b py-3"
              >
                <div>
                  <p>
                    {actions[t.action] || t.action} ·{" "}
                    {statuses[t.state] || t.state}
                  </p>
                  <p className="muted">
                    {stamp(t.at || 0)} · {t.targets.length} 台 VPS · 成功{" "}
                    {t.targets.filter((v) => v.state === "succeeded").length} 台
                  </p>
                </div>
                <Button variant="outline" onClick={() => setTaskId(t.id)}>
                  查看任务
                </Button>
              </div>
            ))}
            {!tasks.length && <p className="muted">暂无维护任务。</p>}
          </CardContent>
          <Pager total={tasks.length} page={currentPage} change={setPage} />
        </Card>
      )}
      {tab === "settings" && (
        <>
          <Card>
            <CardHeader>
              <CardTitle>接入与授权</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <p className="muted">
                每台 VPS
                保留独立面板；接入后总控可管理代理、执行维护及免密登录。
              </p>
              <h4 className="font-medium">允许另一台总控管理本机</h4>
              {data?.controller ? (
                <div>
                  已授权：{data.controller}{" "}
                  <Button
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
              )}
              {invite && (
                <div className="flex flex-wrap gap-2">
                  <code className="break-all">{invite}</code>
                  <CopyButton value={invite} />
                </div>
              )}
              <h4 className="font-medium">接入远端 VPS</h4>
              <p className="muted">
                先到目标 VPS
                的“总控设置”生成本机接入码，再回到这里填写。不要填写本页生成的接入码。
              </p>
              <form
                className="grid items-end gap-3 md:grid-cols-2"
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
                  <Input name="name" required maxLength={120} />
                </Field>
                <Field label="远端面板 HTTPS 地址">
                  <Input
                    name="url"
                    type="url"
                    required
                    placeholder="https://panel.example.com"
                  />
                </Field>
                <Field label="远端一次性接入码">
                  <Input
                    name="code"
                    type="password"
                    required
                    autoComplete="off"
                  />
                </Field>
                <Button disabled={busy}>接入面板</Button>
              </form>
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>统一监控策略</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <label>
                <input
                  type="checkbox"
                  checked={override}
                  onChange={(e) => setOverride(e.target.checked)}
                />
                覆盖所有已接入 VPS 的本地策略
              </label>
              {override && <PolicyFields value={policy} change={setPolicy} />}
              <p className="muted">
                整套覆盖并保留各 VPS
                本地设置；取消后恢复本地策略。通知渠道在本机“监控告警”配置。
              </p>
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
        </>
      )}
      {activeDetail && (
        <Modal
          drawer
          title={`VPS · ${activeDetail.name}`}
          close={() => setDetail(undefined)}
        >
          <p className="break-all">{activeDetail.url}</p>
          <p>{health(activeDetail, now)}</p>
          <Versions remote={activeDetail} />
          <p className="muted">
            最近同步 {stamp(activeDetail.seen)} · Agent 最近上报{" "}
            {stamp(activeDetail.snapshot?.agent_seen || 0)}
          </p>
          {issues(activeDetail, now).length > 0 && (
            <div className="space-y-3 rounded-lg border border-amber-300 bg-amber-50/30 p-4">
              {issues(activeDetail, now).map((issue, i) => (
                <div key={i}>
                  <p className="font-medium">{issue.title}</p>
                  <p className="mt-1 text-sm break-words">{issue.detail}</p>
                </div>
              ))}
            </div>
          )}
          {localNode(activeDetail) && (
            <div className="grid gap-3 sm:grid-cols-2">
              {(() => {
                const n = localNode(activeDetail)!,
                  s = n.report.system,
                  valid =
                    connected(activeDetail, now) &&
                    n.online &&
                    recent(n.seen, remoteNow(activeDetail, now)) &&
                    recent(s.sampled_at, remoteNow(activeDetail, now));
                return (
                  <>
                    <p>
                      CPU：
                      {valid && s.cpu_percent != null
                        ? `${s.cpu_percent.toFixed(1)}%`
                        : "—"}
                    </p>
                    <p>
                      内存：
                      {valid && s.memory_total && s.memory_used != null
                        ? `${bytes(s.memory_used)} / ${bytes(s.memory_total)}`
                        : "—"}
                    </p>
                    <p>
                      磁盘：
                      {valid && s.disk_total && s.disk_used != null
                        ? `${bytes(s.disk_used)} / ${bytes(s.disk_total)}`
                        : "—"}
                    </p>
                    <p className="muted">
                      采样 {stamp(s.sampled_at || 0)}
                      {!valid && " · 已过期或未知"}
                    </p>
                    {n.report.error && <p role="alert">{n.report.error}</p>}
                  </>
                );
              })()}
            </div>
          )}
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              onClick={() => navigate("lines", activeDetail.id)}
            >
              管理代理
            </Button>
            <Button
              type="button"
              variant="outline"
              onClick={() => navigate("externals", activeDetail.id)}
            >
              管理外部代理
            </Button>
            <Button type="button" variant="outline" onClick={() => {
              const remote = activeDetail;
              setDetail(undefined);
              setModal(<Diagnostics nodes={remote.snapshot?.nodes || []} request={remoteApi(remote.id)} close={() => setModal(null)} />);
            }}>日志与诊断</Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() =>
                void run(async () => {
                  await prepareApply(activeDetail);
                  setDetail(undefined);
                })
              }
            >
              预览并应用配置
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => void run(() => jump(activeDetail))}
            >
              免密进入面板
            </Button>
            <Button
              type="button"
              variant="ghost"
              disabled={busy}
              onClick={() => {
                if (
                  window.confirm(
                    `解除 ${activeDetail.name} 的接入授权？远端需在线确认撤销。`,
                  )
                )
                  void run(async () => {
                    await api(`/fleet/${activeDetail.id}`, "DELETE");
                    setDetail(undefined);
                    await refresh();
                  });
              }}
            >
              解除接入
            </Button>
          </div>
        </Modal>
      )}
      {taskId && (
        <Modal drawer title="逐台任务进度" close={() => setTaskId(undefined)}>
          {error && (
            <p role="alert" className="text-red-700">
              {error}
            </p>
          )}
          {activeTask ? (
            <>
              <div className="rounded-xl border bg-muted/30 p-4 space-y-2">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <h3 className="font-semibold">{actions[activeTask.action] || activeTask.action}</h3>
                  <Status tone={activeTask.state === "paused" ? "warn" : activeTask.state === "succeeded" ? "good" : "muted"}>{statuses[activeTask.state] || activeTask.state}</Status>
                </div>
                {activeTask.version && <p className="text-sm">目标版本 <strong>{activeTask.version}</strong></p>}
                <p className="text-sm text-muted-foreground">
                  已完成 {activeTask.targets.filter(t => t.state === "succeeded").length} / {activeTask.targets.length}
                  {` · 验证未通过 ${activeTask.targets.filter(verificationFailed).length}`}
                  {` · 操作失败 ${activeTask.targets.filter(t => t.state === "failed" && !verificationFailed(t)).length}`}
                </p>
              </div>
              <div className="space-y-3">
              {activeTask.targets.map((t) => {
                const remote = remotes.find(r => r.id === t.id);
                const verifyFailed = verificationFailed(t);
                const maintenanceDone = t.result?.maintenance_state === "succeeded" || verifyFailed || t.state === "succeeded";
                const issue = verifyFailed
                  ? t.result?.verification?.detail || (t.error || t.result?.error || "").replace(/^组件维护已完成，但整链验证失败[：:]?\s*/, "")
                  : t.error || t.result?.error;
                return <section key={t.id} className="min-w-0 space-y-3 rounded-xl border p-4" aria-label={`VPS 任务：${remote?.name || t.id}`}>
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <h4 className="font-semibold break-all">{remote?.name || t.id}</h4>
                    <Status tone={verifyFailed ? "warn" : t.state === "failed" ? "bad" : t.state === "succeeded" ? "good" : "muted"}>{verifyFailed ? "维护完成 · 验证待处理" : statuses[t.state] || t.state}</Status>
                  </div>
                  <div className="grid gap-2 rounded-lg bg-muted/30 p-3 text-sm sm:grid-cols-2">
                    <p>组件维护：<span className={maintenanceDone ? "text-emerald-700" : ""}>{maintenanceDone ? "已完成" : statuses[t.result?.maintenance_state || t.state] || t.state}</span></p>
                    <p>后续验证：{verifyFailed ? "未通过" : t.result?.verification?.state === "succeeded" || t.state === "succeeded" ? "已通过" : "等待验证"}</p>
                  </div>
                  {t.result?.after && <div className="flex flex-wrap gap-x-4 gap-y-1 text-sm">{Object.entries(t.result.after).map(([key, value]) => <span key={key}>{({panel:"控制台",agent:"Agent",chainctl:"管理命令"} as Record<string,string>)[key] || key} <strong>{value}</strong></span>)}</div>}
                  {issue ? <div className="rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-950"><p className="font-medium">{verifyFailed ? "组件维护成功，后续连接检查未通过" : "操作未完成"}</p><p className="mt-1 whitespace-pre-wrap break-words">{issue}</p>{verifyFailed && <p className="mt-2">先查看日志并排查连接，再重新验证；无需重复升级。</p>}</div> : t.result?.detail && <p className="whitespace-pre-wrap break-words text-sm text-muted-foreground">{t.result.detail}</p>}
                  {remote && <Button type="button" variant="outline" onClick={() => {
                    const returnTask = activeTask.id;
                    setTaskId(undefined);
                    setModal(<Diagnostics nodes={remote.snapshot?.nodes || []} request={remoteApi(remote.id)} close={() => { setModal(null); setTaskId(returnTask); }} />);
                  }}>查看此 VPS 日志</Button>}
                </section>;
              })}
              </div>
              {activeTask.state === "paused" && (
                <div className="space-y-2 rounded-xl border border-amber-200 bg-amber-50 p-4 text-sm text-amber-950">
                  <p className="font-medium">已暂停后续 VPS，避免继续影响服务</p>
                  <p>{activeTask.targets.some(verificationFailed)
                    ? "维护已经完成。重新验证只核对原任务并检查连接，不会重新安装组件；全部通过后继续后续 VPS。"
                    : activeTask.targets.some(t => t.state === "failed")
                      ? "先排查失败原因。核对原任务不会重新执行已失败操作；确需重试时选择失败对象新建任务。"
                      : "操作结果尚未确认。请先核对原任务，确认成功后继续后续 VPS。"}</p>
                  <p>等待执行：{activeTask.targets.filter(t => t.state === "pending").map(t => remotes.find(r => r.id === t.id)?.name || t.id).join("、") || "无"}</p>
                </div>
              )}
              <div className="flex flex-wrap gap-2">
              {activeTask.state === "paused" &&
                activeTask.targets.some((t) => t.state === "failed" && !verificationFailed(t)) && (
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => {
                      const ids = activeTask.targets
                        .filter((t) => ["failed", "pending"].includes(t.state) && !verificationFailed(t))
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
                        setSelected(ids);
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
              {activeTask.state === "paused" && (
                <Button
                  type="button"
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
                  {activeTask.targets.some(verificationFailed) ? "重新验证并继续" : "重新核对原任务结果"}
                </Button>
              )}
              </div>
            </>
          ) : (
            <p>任务已提交，正在等待同步结果；关闭后可在维护任务中继续查看。</p>
          )}
        </Modal>
      )}
      {editor && (
        <LineEditor
          line={editor.line}
          kind={editor.line?.kind || "vless"}
          state={editor.state}
          remoteName={editor.remote.name}
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
