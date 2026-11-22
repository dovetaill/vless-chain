import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Gauge, Plus, RefreshCw, Search, ShieldCheck } from "lucide-react";
import { api } from "../api";
import type {
  AccessPolicy,
  External,
  Line,
  PanelState,
  Resource,
  Source,
} from "../types";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import {
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardHeading,
  CardTable,
  CardTitle,
  CardToolbar,
} from "./ui/card";
import { Input } from "./ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { Progress } from "./ui/progress";
import {
  Empty,
  Field,
  InfoTip,
  Modal,
  bytes,
  errorText,
  field,
  number,
  stamp,
} from "./shared";

type Allocation = { granted: number; used: number; rate: number; expires: number };
type ResourceState = {
  resources: Resource[];
  ledgers: Record<
    string,
    { cycle: number; end: number; allocations: Record<string, Allocation> }
  >;
};

const emptyResource: Resource = {
  id: "",
  name: "",
  members: [],
  monthly_gib: null,
  mbps: null,
  reset_day: 1,
  reset_hour: 0,
  timezone: "Asia/Shanghai",
};

export function Resources({
  state,
  saved,
}: {
  state: PanelState;
  saved: () => Promise<void>;
}) {
  const [data, setData] = useState<ResourceState>({ resources: [], ledgers: {} });
  const [error, setError] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);
  const load = async () => {
    setData(await api<ResourceState>("/resources"));
  };
  useEffect(() => {
    void load().catch((e) => setError(errorText(e)));
    const timer = setInterval(
      () => void load().catch((e) => setError(errorText(e))),
      10000,
    );
    return () => clearInterval(timer);
  }, []);

  const members = [
    ...state.nodes.map((n) => ({
      id: `node:${n.id}`,
      name: `服务器 · ${n.name}`,
    })),
    ...state.externals.map((e) => ({
      id: `external:${e.id}`,
      name: `外部代理 · ${e.name}`,
    })),
  ];

  function edit(resource: Resource) {
    setModal(
      <ResourceEditor
        resource={resource}
        resources={data.resources}
        members={members}
        close={() => setModal(null)}
        saved={async () => {
          await load();
          await saved();
        }}
      />,
    );
  }

  return (
    <Card>
      <CardHeader className="gap-3">
        <CardHeading className="flex items-center gap-1.5 space-y-0">
          <CardTitle>共享资源组</CardTitle>
          <Badge variant="secondary" appearance="light" size="sm">
            {data.resources.length}
          </Badge>
          <InfoTip>
            组内所有成员共享周期流量额度与上下行总带宽（公网转发的上传与下载合计受此上限约束，租户倍率不重复乘入底层资源）。
          </InfoTip>
        </CardHeading>
        <CardToolbar>
          <Button onClick={() => edit({ ...emptyResource })} disabled={state.busy}>
            <Plus />
            新建资源组
          </Button>
        </CardToolbar>
      </CardHeader>

      {error && (
        <p
          role="alert"
          className="m-4 rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-xs text-destructive"
        >
          {error}
        </p>
      )}

      {data.resources.length ? (
        <CardContent className="grid gap-4 lg:grid-cols-2">
          {data.resources.map((r) => {
            const ledger = data.ledgers[r.id];
            const allocations = Object.values(ledger?.allocations || {});
            const granted = allocations.reduce((n, a) => n + a.granted, 0);
            const used = allocations.reduce((n, a) => n + a.used, 0);
            const totalBytes = r.monthly_gib ? r.monthly_gib * 1024 ** 3 : null;
            const pct =
              totalBytes && totalBytes > 0
                ? Math.min(100, (used / totalBytes) * 100)
                : 0;

            return (
              <div
                key={r.id}
                className="space-y-3.5 rounded-xl border border-border bg-muted/20 p-4"
              >
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <div>
                    <strong className="text-sm font-semibold text-foreground">
                      {r.name}
                    </strong>
                    <p className="mt-0.5 text-xs text-muted-foreground">
                      额度 {r.monthly_gib == null ? "不限" : `${r.monthly_gib} GiB`} · 上下行合计{" "}
                      {r.mbps == null ? "不限" : `${r.mbps} Mbps`} · {r.members.length} 个成员
                    </p>
                  </div>
                  <div className="flex gap-1.5">
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => edit(r)}
                      disabled={state.busy}
                    >
                      设置 / 成员
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={state.busy || r.members.length > 0}
                      onClick={async () => {
                        try {
                          await api(`/resources/${r.id}`, "DELETE");
                          await load();
                          await saved();
                        } catch (e) {
                          setError(errorText(e));
                        }
                      }}
                    >
                      删除空组
                    </Button>
                  </div>
                </div>

                <div className="space-y-1.5 rounded-lg border border-border/70 bg-card p-3 text-xs tabular-nums">
                  <div className="flex flex-wrap justify-between gap-2">
                    <span className="font-medium text-foreground">
                      已用 {bytes(used)} · 已分配 {bytes(granted)}
                    </span>
                    <span className="text-muted-foreground">
                      未分配{" "}
                      {totalBytes == null
                        ? "不限"
                        : bytes(Math.max(0, totalBytes - granted))}
                    </span>
                  </div>
                  {totalBytes != null && <Progress value={pct} />}
                  <p className="pt-0.5 text-[11px] text-muted-foreground">
                    每月 {r.reset_day} 日 {r.reset_hour}:00（{r.timezone}）重置 · 下次{" "}
                    {ledger?.end ? stamp(ledger.end) : "待计算"}
                  </p>
                </div>

                <div className="flex flex-wrap gap-1.5">
                  {r.members.length ? (
                    r.members.map((id) => (
                      <Badge
                        key={id}
                        variant="secondary"
                        appearance="light"
                        size="xs"
                      >
                        {members.find((m) => m.id === id)?.name || id}
                      </Badge>
                    ))
                  ) : (
                    <span className="text-xs text-muted-foreground">
                      尚未添加成员节点
                    </span>
                  )}
                </div>
              </div>
            );
          })}
        </CardContent>
      ) : (
        <Empty
          icon={Gauge}
          title="尚未创建资源组"
          action={
            <Button onClick={() => edit({ ...emptyResource })} disabled={state.busy}>
              <Plus />
              新建资源组
            </Button>
          }
        >
          当多条外部代理或服务器共用同一台上游主机套餐时，可建立资源组共享总带宽与月流量上限。
        </Empty>
      )}
      {modal}
    </Card>
  );
}

function ResourceEditor({
  resource,
  resources,
  members,
  close,
  saved,
}: {
  resource: Resource;
  resources: Resource[];
  members: { id: string; name: string }[];
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [picked, setPicked] = useState(resource.members);
  return (
    <Modal
      title={resource.id ? "编辑资源额度" : "新建资源额度"}
      close={close}
      submit="保存并下发"
      action={async (data) => {
        await api("/resources", "POST", {
          ...resource,
          name: field(data, "name"),
          members: picked,
          monthly_gib: number(data, "quota"),
          mbps: number(data, "mbps"),
          reset_day: number(data, "day"),
          reset_hour: number(data, "hour"),
          timezone: field(data, "zone"),
        });
        await saved();
        close();
      }}
    >
      <Field label="名称">
        <Input
          name="name"
          defaultValue={resource.name}
          required
          maxLength={120}
        />
      </Field>
      <div className="grid gap-3.5 sm:grid-cols-2">
        <Field label="周期总额度（GiB）" hint="留空不限；1 TiB = 1024 GiB">
          <Input
            name="quota"
            type="number"
            min="0.000001"
            step="any"
            defaultValue={resource.monthly_gib ?? ""}
          />
        </Field>
        <Field
          label="上下行合计共享上限（Mbps）"
          hint="例如 1000 代表 1 Gbps；留空不限"
        >
          <Input
            name="mbps"
            type="number"
            min="0.001"
            step="any"
            defaultValue={resource.mbps ?? ""}
          />
        </Field>
      </div>
      <div className="grid gap-3.5 sm:grid-cols-3">
        <Field label="每月重置日" tooltip="当月无该日期时取月末最后一天。">
          <Input
            name="day"
            type="number"
            min={1}
            max={31}
            defaultValue={resource.reset_day}
            required
          />
        </Field>
        <Field label="重置小时">
          <Input
            name="hour"
            type="number"
            min={0}
            max={23}
            defaultValue={resource.reset_hour}
            required
          />
        </Field>
        <Field label="时区">
          <Input
            name="zone"
            defaultValue={resource.timezone}
            list="resource-timezones"
            required
          />
        </Field>
        <datalist id="resource-timezones">
          <option>Asia/Shanghai</option>
          <option>America/Los_Angeles</option>
          <option>America/New_York</option>
          <option>UTC</option>
        </datalist>
      </div>
      <fieldset className="rounded-xl border border-border bg-muted/20 p-3.5">
        <legend className="px-1.5 text-xs font-semibold text-foreground">
          共享成员
        </legend>
        {members.length ? (
          <div className="max-h-48 space-y-1.5 overflow-auto">
            {members.map((m) => {
              const other = resources.find(
                (r) => r.id !== resource.id && r.members.includes(m.id),
              );
              return (
                <label
                  key={m.id}
                  className="flex items-center gap-2 text-xs text-foreground"
                >
                  <input
                    type="checkbox"
                    disabled={!!other}
                    checked={picked.includes(m.id)}
                    onChange={(e) =>
                      setPicked(
                        e.target.checked
                          ? [...picked, m.id]
                          : picked.filter((id) => id !== m.id),
                      )
                    }
                  />
                  <span>
                    {m.name}
                    {other && (
                      <span className="ml-1 text-muted-foreground">
                        （已归属 {other.name}）
                      </span>
                    )}
                  </span>
                </label>
              );
            })}
          </div>
        ) : (
          <p className="muted">暂无可选节点或外部代理。</p>
        )}
      </fieldset>
    </Modal>
  );
}

const RULES_PAGE_SIZE = 15;

export function RulesLibrary() {
  const [sources, setSources] = useState<Source[]>([]);
  const [filter, setFilter] = useState("");
  const [category, setCategory] = useState("");
  const [statusFilter, setStatusFilter] = useState("");
  const [page, setPage] = useState(1);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);

  const load = async () => setSources(await api<Source[]>("/rules"));
  useEffect(() => {
    void load().catch((e) => setError(errorText(e)));
  }, []);

  async function action(s: Source, operation: string) {
    setBusy(s.id);
    setError("");
    try {
      await api(`/rules/${s.id}/${operation}`, "POST", {});
      await load();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy("");
    }
  }

  function edit(s?: Source) {
    setModal(
      <Modal
        title={s ? "编辑规则源" : "导入规则源"}
        close={() => setModal(null)}
        submit="保存"
        action={async (data) => {
          await api("/rules", "POST", {
            id: s?.id || "",
            name: field(data, "name"),
            url: field(data, "url"),
            auto_apply: data.has("auto"),
          });
          await load();
          setModal(null);
        }}
      >
        <Field label="名称">
          <Input name="name" defaultValue={s?.name} required />
        </Field>
        <Field
          label="规则文件 HTTPS URL"
          hint="支持 Clash classical / domain 格式的 payload 规则列表"
        >
          <Input name="url" type="url" defaultValue={s?.url} required />
        </Field>
        <label className="flex items-center gap-2 text-xs text-foreground">
          <input name="auto" type="checkbox" defaultChecked={s?.auto_apply} />
          每日更新后自动应用生效
        </label>
      </Modal>,
    );
  }

  const categories = useMemo(
    () => Array.from(new Set(sources.map((s) => s.category).filter(Boolean))),
    [sources],
  );

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return sources.filter((s) => {
      if (category && s.category !== category) return false;
      if (statusFilter === "pending" && !s.pending) return false;
      if (statusFilter === "active" && !s.active) return false;
      if (q && !`${s.name} ${s.category} ${s.url}`.toLowerCase().includes(q))
        return false;
      return true;
    });
  }, [sources, filter, category, statusFilter]);

  const totalPages = Math.max(1, Math.ceil(filtered.length / RULES_PAGE_SIZE));
  const currentPage = Math.min(page, totalPages);
  const visible = filtered.slice(
    (currentPage - 1) * RULES_PAGE_SIZE,
    currentPage * RULES_PAGE_SIZE,
  );

  return (
    <Card>
      <CardHeader className="gap-3">
        <CardHeading className="flex items-center gap-1.5 space-y-0">
          <CardTitle>网站规则库</CardTitle>
          <Badge variant="secondary" appearance="light" size="sm">
            {filtered.length} / {sources.length}
          </Badge>
          <InfoTip>
            外部规则默认每日拉取更新，支持手动或自动应用；白名单仅采纳完整域名与域名后缀，其余非域名条目将自动忽略。
          </InfoTip>
        </CardHeading>
        <CardToolbar className="flex-wrap">
          <Button
            variant="outline"
            size="sm"
            disabled={!!busy}
            onClick={() => void load().catch((e) => setError(errorText(e)))}
          >
            <RefreshCw className="size-3.5" />
            刷新状态
          </Button>
          <Button size="sm" onClick={() => edit()}>
            <Plus />
            导入规则源
          </Button>
        </CardToolbar>
      </CardHeader>

      <div className="space-y-3 border-b border-border bg-muted/15 px-5 py-3.5">
        <div className="flex flex-wrap items-center gap-2.5">
          <div className="relative w-full sm:w-64">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              aria-label="筛选网站规则"
              placeholder="搜索站点名称、分类或地址"
              value={filter}
              onChange={(e) => {
                setFilter(e.target.value);
                setPage(1);
              }}
              className="pl-8.5"
            />
          </div>
          <select
            className="select w-auto"
            aria-label="规则状态筛选"
            value={statusFilter}
            onChange={(e) => {
              setStatusFilter(e.target.value);
              setPage(1);
            }}
          >
            <option value="">全部状态</option>
            <option value="pending">有待应用更新</option>
            <option value="active">已应用生效</option>
          </select>
          {(filter || category || statusFilter) && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setFilter("");
                setCategory("");
                setStatusFilter("");
                setPage(1);
              }}
            >
              重置筛选
            </Button>
          )}
        </div>

        {categories.length > 1 && (
          <div className="flex flex-wrap gap-1.5">
            <button
              type="button"
              onClick={() => {
                setCategory("");
                setPage(1);
              }}
              className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                !category
                  ? "bg-primary text-primary-foreground"
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              全部
            </button>
            {categories.map((cat) => (
              <button
                key={cat}
                type="button"
                onClick={() => {
                  setCategory(cat === category ? "" : cat);
                  setPage(1);
                }}
                className={`cursor-pointer rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                  category === cat
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted text-muted-foreground hover:text-foreground"
                }`}
              >
                {cat}
              </button>
            ))}
          </div>
        )}
      </div>

      {error && (
        <p
          role="alert"
          className="m-4 rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-xs text-destructive"
        >
          {error}
        </p>
      )}

      {visible.length ? (
        <CardTable>
          <div className="overflow-x-auto fleet-responsive" role="region" aria-label="网站规则列表">
            <table className="data-table">
              <thead>
                <tr>
                  <th>站点 / 来源</th>
                  <th>规则条目状态</th>
                  <th>检查与应用时间</th>
                  <th className="text-right">操作</th>
                </tr>
              </thead>
              <tbody>
                {visible.map((s) => {
                  const excludedObj = s.pending || s.active;
                  return (
                    <tr key={s.id}>
                      <td data-label="站点 / 来源" className="max-w-xs">
                        <div className="flex flex-wrap items-center gap-1.5">
                          <strong className="text-[13px] font-semibold text-foreground">
                            {s.name}
                          </strong>
                          {s.category && (
                            <Badge variant="secondary" appearance="light" size="xs">
                              {s.category}
                            </Badge>
                          )}
                          <Badge
                            variant={s.url && s.auto_apply ? "primary" : "outline"}
                            appearance="light"
                            size="xs"
                          >
                            {!s.url ? "内置" : s.auto_apply ? "自动更新" : "手动应用"}
                          </Badge>
                        </div>
                        <p
                          className="mono mt-1 truncate text-[11px] text-muted-foreground"
                          title={s.url || "面板内置域名集"}
                        >
                          {s.url || "内置站点域名集"}
                        </p>
                        {s.error && (
                          <p className="mt-1 text-xs text-amber-600 dark:text-amber-400">
                            {s.error}
                          </p>
                        )}
                      </td>

                      <td data-label="规则条目状态">
                        <div className="flex flex-wrap items-center gap-1.5 text-xs tabular-nums">
                          <Badge
                            variant={s.active?.count ? "success" : "secondary"}
                            appearance="light"
                            size="sm"
                          >
                            已生效 {s.active?.count ?? 0} 条
                          </Badge>
                          {s.pending && (
                            <Badge variant="warning" appearance="light" size="sm">
                              待应用 {s.pending.count} 条 (+{s.added}/-{s.removed})
                            </Badge>
                          )}
                          {excludedObj?.excluded_count ? (
                            <Popover>
                              <PopoverTrigger asChild>
                                <button
                                  type="button"
                                  className="cursor-pointer text-[11px] text-muted-foreground underline decoration-dashed underline-offset-4 hover:text-foreground"
                                >
                                  忽略 {excludedObj.excluded_count} 条
                                </button>
                              </PopoverTrigger>
                              <PopoverContent className="w-80 p-3">
                                <p className="mb-1.5 text-xs font-semibold text-foreground">
                                  未采纳的非纯域名条目（最多展示 100 条）
                                </p>
                                <pre className="mono max-h-48 overflow-auto whitespace-pre-wrap break-all rounded bg-muted p-2 text-[11px] text-muted-foreground">
                                  {excludedObj.excluded.join("\n")}
                                </pre>
                              </PopoverContent>
                            </Popover>
                          ) : null}
                        </div>
                      </td>

                      <td
                        data-label="检查与应用时间"
                        className="whitespace-nowrap text-xs text-muted-foreground tabular-nums"
                      >
                        <p>检查：{s.checked_at ? stamp(s.checked_at) : "尚未下载"}</p>
                        <p className="mt-0.5">
                          应用：{s.applied_at ? stamp(s.applied_at) : "尚未应用"}
                        </p>
                      </td>

                      <td data-label="操作" className="whitespace-nowrap">
                        <div className="flex flex-wrap justify-start gap-1.5 md:justify-end">
                          <Button
                            size="sm"
                            variant="outline"
                            disabled={!!busy}
                            onClick={() => void action(s, "update")}
                          >
                            {busy === s.id
                              ? "处理中…"
                              : s.url
                                ? "下载更新"
                                : "载入内置规则"}
                          </Button>
                          <Button
                            size="sm"
                            variant={s.pending ? "primary" : "outline"}
                            disabled={!!busy || !s.pending}
                            onClick={() => void action(s, "apply")}
                          >
                            应用新版本
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={!!busy}
                            onClick={() => edit(s)}
                          >
                            编辑
                          </Button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </CardTable>
      ) : (
        <Empty icon={ShieldCheck} title="没有匹配的网站规则">
          调整筛选条件或导入自定义 Clash 规则源。
        </Empty>
      )}

      {filtered.length > RULES_PAGE_SIZE && (
        <CardFooter className="flex flex-wrap items-center justify-between gap-2 py-3 text-xs text-muted-foreground">
          <span className="tabular-nums">
            共 {filtered.length} 条 · 第 {currentPage} / {totalPages} 页
          </span>
          <div className="flex gap-1.5">
            <Button
              size="sm"
              variant="outline"
              disabled={currentPage <= 1}
              onClick={() => setPage((p) => p - 1)}
            >
              上一页
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={currentPage >= totalPages}
              onClick={() => setPage((p) => p + 1)}
            >
              下一页
            </Button>
          </div>
        </CardFooter>
      )}

      {modal}
    </Card>
  );
}

export function AccessPolicyEditor({
  line,
  close,
  saved,
  request = api,
}: {
  line: Line;
  close: () => void;
  saved: () => Promise<void>;
  request?: typeof api;
}) {
  const [policy, setPolicy] = useState<AccessPolicy | null>(null);
  const [sources, setSources] = useState<Source[]>([]);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    void Promise.all([
      request<AccessPolicy>(`/access/${line.id}`),
      request<Source[]>("/rules"),
    ])
      .then(([p, s]) => {
        setPolicy(p);
        setSources(s);
      })
      .catch((e) => setError(errorText(e)));
  }, [line.id, request]);

  const ports = (text: string) =>
    text
      .split(/[\s,，]+/)
      .filter(Boolean)
      .map((s) => {
        if (!/^\d+$/.test(s) || +s < 1 || +s > 65535)
          throw Error("端口须为 1–65535，使用逗号或空格分隔");
        return +s;
      });

  return (
    <Modal
      title={`访问限制 · ${line.name}`}
      close={close}
      wide
      submit={policy ? "保存并下发" : undefined}
      action={
        policy
          ? async (data) => {
              await request(`/access/${line.id}`, "POST", {
                ...policy,
                domains: field(data, "domains").split(/\s+/).filter(Boolean),
                denied_ports: ports(field(data, "denied")),
                udp_ports: ports(field(data, "udp_ports")),
                max_connections: number(data, "connections"),
              });
              await saved();
              close();
            }
          : undefined
      }
    >
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
      {!policy ? (
        <p className="muted">正在读取访问策略…</p>
      ) : (
        <>
          <div className="flex items-center gap-2 rounded-xl border border-border bg-muted/25 p-3.5 text-xs font-medium text-foreground">
            <label className="flex cursor-pointer items-center gap-2">
              <input
                type="checkbox"
                checked={policy.restricted}
                onChange={(e) =>
                  setPolicy({ ...policy, restricted: e.target.checked })
                }
              />
              <span>仅放行所选网站与自定义域名（白名单模式）</span>
            </label>
            <InfoTip>
              开启后拒绝裸 IP 请求，客户端需传递目标域名；未应用的规则源不会放行流量。
            </InfoTip>
          </div>

          {policy.restricted && (
            <div className="space-y-3 rounded-xl border border-border bg-muted/15 p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <Input
                  aria-label="搜索预设网站"
                  placeholder="搜索预设站点规则…"
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  className="sm:max-w-xs"
                />
                <Badge variant="primary" appearance="light" size="sm">
                  已选 {policy.sources.length} 组
                </Badge>
              </div>
              <div className="max-h-60 space-y-3 overflow-y-auto pr-1">
                {[...new Set(sources.map((s) => s.category))].map((category) => {
                  const items = sources.filter(
                    (s) =>
                      s.category === category &&
                      s.name.toLowerCase().includes(filter.toLowerCase()),
                  );
                  if (!items.length) return null;
                  return (
                    <fieldset key={category} className="space-y-1.5">
                      <legend className="text-xs font-semibold text-muted-foreground">
                        {category}
                      </legend>
                      <div className="flex flex-wrap gap-1.5">
                        {items.map((s) => {
                          const checked = policy.sources.includes(s.id);
                          return (
                            <label
                              key={s.id}
                              className={`inline-flex cursor-pointer items-center gap-1.5 rounded-md border px-2.5 py-1.5 text-xs transition-colors ${
                                checked
                                  ? "border-primary bg-primary/10 text-foreground"
                                  : "border-border bg-card text-foreground hover:bg-accent"
                              }`}
                            >
                              <input
                                type="checkbox"
                                checked={checked}
                                onChange={(e) =>
                                  setPolicy({
                                    ...policy,
                                    sources: e.target.checked
                                      ? [...policy.sources, s.id]
                                      : policy.sources.filter(
                                          (id) => id !== s.id,
                                        ),
                                  })
                                }
                              />
                              <span>{s.name}</span>
                              {!s.active && (
                                <small className="text-[10px] text-amber-600 dark:text-amber-400">
                                  未应用
                                </small>
                              )}
                            </label>
                          );
                        })}
                      </div>
                    </fieldset>
                  );
                })}
              </div>
              <Field label="自定义域名（每行一个，含子域名）">
                <textarea
                  name="domains"
                  className="textarea mono"
                  rows={3}
                  defaultValue={policy.domains.join("\n")}
                />
              </Field>
            </div>
          )}

          {!policy.restricted && (
            <input
              type="hidden"
              name="domains"
              value={policy.domains.join("\n")}
            />
          )}

          <div className="grid gap-3.5 sm:grid-cols-2">
            <Field label="禁止目标端口（逗号或空格分隔）">
              <Input
                name="denied"
                defaultValue={policy.denied_ports.join(", ")}
                placeholder="例如 25, 465, 587"
              />
            </Field>
            <Field
              label="并发业务连接 / UDP 关联上限"
              tooltip="留空默认 4,096，最大 20,480；单台设备可能同时建立多个并发连接。"
            >
              <Input
                name="connections"
                type="number"
                min={1}
                max={20480}
                placeholder="默认 4096"
                defaultValue={policy.max_connections ?? ""}
              />
            </Field>
          </div>

          <div className="grid gap-3.5 sm:grid-cols-2">
            <Field label="UDP 策略">
              <select
                className="select"
                value={policy.udp || "allow"}
                onChange={(e) => setPolicy({ ...policy, udp: e.target.value })}
              >
                <option value="allow">允许（遵循站点与禁端口规则）</option>
                <option value="deny">全部禁止</option>
                <option value="ports">仅允许指定目标端口</option>
              </select>
            </Field>
            <Field label="UDP 允许端口">
              <Input
                name="udp_ports"
                defaultValue={policy.udp_ports.join(", ")}
                placeholder="例如 53, 443, 123, 3478"
              />
            </Field>
          </div>
        </>
      )}
    </Modal>
  );
}

type Candidate = {
  host: string;
  ids: string[];
  matching: { id: string; name: string }[];
};

export function AutoGroups({
  state,
  ids = [],
  close,
  saved,
}: {
  state: PanelState;
  ids?: string[];
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [items, setItems] = useState<Candidate[]>([]);
  const [choices, setChoices] = useState<Record<string, string>>({});
  const [error, setError] = useState("");
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    void api<Candidate[]>("/resources/candidates", "POST", { ids })
      .then((items) => {
        setItems(items);
        setChoices(
          Object.fromEntries(
            items.map((c) => [
              c.host,
              c.matching.length === 1
                ? c.matching[0].id
                : c.matching.length > 1
                  ? "skip"
                  : "new",
            ]),
          ),
        );
        setLoaded(true);
      })
      .catch((e) => setError(errorText(e)));
  }, []);

  return (
    <Modal
      title="自动分组候选"
      description="按相同服务器地址匹配未分组的外部代理，确认是否共享上游额度。"
      close={close}
      submit={loaded && items.length ? "应用选择" : undefined}
      action={
        loaded
          ? async () => {
              for (const item of items) {
                const choice = choices[item.host];
                if (choice === "skip") continue;
                await api("/resources/groups", "POST", {
                  ids: item.ids,
                  group: choice === "new" ? null : choice,
                  name: choice === "new" ? item.host : "",
                  automatic: true,
                });
              }
              await saved();
              close();
            }
          : undefined
      }
    >
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
      {loaded && !items.length && (
        <p className="muted py-4 text-center">当前没有可自动归组的未分组节点。</p>
      )}
      {items.map((c) => (
        <Field
          key={c.host}
          label={c.host}
          hint={c.ids
            .map((id) => state.externals.find((e) => e.id === id)?.name || id)
            .join("、")}
        >
          <select
            className="select"
            value={choices[c.host]}
            onChange={(e) =>
              setChoices({ ...choices, [c.host]: e.target.value })
            }
          >
            <option value="new">新建共享资源组</option>
            {c.matching.map((r) => (
              <option key={r.id} value={r.id}>
                加入 {r.name}
              </option>
            ))}
            <option value="skip">保持独立</option>
          </select>
        </Field>
      ))}
    </Modal>
  );
}

export function ManualGroup({
  external,
  state,
  close,
  saved,
}: {
  external: External;
  state: PanelState;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const current = state.resources?.find((r) =>
    r.members.includes(`external:${external.id}`),
  );
  return (
    <Modal
      title={`设置资源组 · ${external.name}`}
      close={close}
      submit="保存分组"
      action={async (data) => {
        const group = field(data, "group");
        await api("/resources/groups", "POST", {
          ids: [external.id],
          group: group || null,
          name: field(data, "name"),
          automatic: false,
        });
        await saved();
        close();
      }}
    >
      <Field label="选择已有资源组">
        <select
          name="group"
          className="select"
          defaultValue={current?.id || ""}
        >
          <option value="">移出分组 / 新建组</option>
          {state.resources?.map((r) => (
            <option key={r.id} value={r.id}>
              {r.name}
            </option>
          ))}
        </select>
      </Field>
      <Field label="或新建组名称" hint="未选择已有组时生效；留空则移出当前分组">
        <Input name="name" placeholder="输入新资源组名称" />
      </Field>
    </Modal>
  );
}

export function BatchImport({
  state,
  close,
  saved,
}: {
  state: PanelState;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [groupIds, setGroupIds] = useState<string[] | null>(null);
  const [imported, setImported] = useState<External[]>([]);
  if (groupIds)
    return (
      <AutoGroups
        state={{
          ...state,
          externals: [
            ...state.externals,
            ...imported.filter(
              (e) => !state.externals.some((old) => old.id === e.id),
            ),
          ],
        }}
        ids={groupIds}
        close={close}
        saved={saved}
      />
    );
  return (
    <Modal
      title="批量导入外部代理"
      description="每行粘贴一条节点分享链接。"
      close={close}
      submit="导入"
      action={async (data) => {
        const result = await api<External[]>("/externals/batch", "POST", {
          urls: field(data, "urls")
            .split(/\r?\n/)
            .map((v) => v.trim())
            .filter(Boolean),
        });
        setImported(result);
        await saved();
        if (data.has("group")) setGroupIds(result.map((e) => e.id));
        else close();
      }}
    >
      <Field label="分享链接（每行一条）">
        <textarea name="urls" className="textarea mono" rows={7} required />
      </Field>
      <label className="flex items-center gap-2 text-xs text-muted-foreground">
        <input type="checkbox" name="group" />
        导入后自动识别相同地址归组
      </label>
    </Modal>
  );
}
