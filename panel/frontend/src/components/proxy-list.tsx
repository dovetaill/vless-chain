import { AccessPolicyEditor } from "./sharing";
import { Diagnostics } from "./diagnostics";
import { useState, type ReactNode } from "react";
import {
  Activity,
  Ban,
  CheckCircle2,
  Download,
  FileText,
  Gauge,
  Infinity as InfinityIcon,
  Link2,
  MoreHorizontal,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  Search,
  Share2,
  Shield,
  Trash2,
} from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "./ui/dropdown-menu";
import { api } from "../api";
import type { Line, PanelState } from "../types";
import { expiringSoon, lineEndpoint, lowQuota, matchesLine } from "../lib/business";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { Card, CardFooter, CardHeader, CardHeading, CardTable, CardTitle } from "./ui/card";
import { Input } from "./ui/input";
import { LegacyConfigs } from "./legacy-configs";
import {
  CopyButton,
  Empty,
  Modal,
  PathView,
  download,
  errorText,
  protocolNames,
} from "./shared";
import {
  LimitsDialog,
  QueryLinkDialog,
  RentalEditor,
  RenewalDialog,
} from "./rental-editor";
import {
  RentalDates,
  RentalPrices,
  RentalSpeed,
  RentalStatusBadge,
  RentalUsage,
  SyncLabel,
  rentalStatusNames,
} from "./rental-shared";

export function ExportDialog({
  lines,
  close,
  requestForLine = () => api,
}: {
  lines: Line[];
  close: () => void;
  requestForLine?: (line: Line) => typeof api;
}) {
  const [result, setResult] = useState<{
    links: string;
    yaml: string;
    errors: string[];
    count: number;
  } | null>(null);
  return (
    <Modal
      title={`批量导出 · ${lines.length} 条`}
      description="导出已启用且已应用的代理链接与 Mihomo 节点列表。"
      submit={result ? undefined : "读取已应用配置"}
      close={close}
      action={async () => {
        const valid = lines.filter((line) => line.enabled && line.published);
        const errors = lines
          .filter((line) => !line.enabled || !line.published)
          .map((line) => `${line.name}：尚未启用或应用，已跳过。`);
        const exports = await Promise.allSettled(
          valid.map((line) =>
            requestForLine(line)<{ share: string; node: string }>(
              `/lines/${line.id}/export`,
            ),
          ),
        );
        const links: string[] = [],
          nodes: string[] = [];
        exports.forEach((value, i) => {
          if (value.status === "fulfilled") {
            links.push(value.value.share);
            nodes.push(value.value.node.trim());
          } else errors.push(`${valid[i].name}：${errorText(value.reason)}`);
        });
        setResult({
          links: links.join("\n"),
          yaml: `proxies:\n${nodes
            .map((node) =>
              node
                .split("\n")
                .map((row) => `  ${row}`)
                .join("\n"),
            )
            .join("\n")}\n`,
          errors,
          count: nodes.length,
        });
      }}
    >
      <div className="flex flex-wrap gap-1.5">
        {lines.map((line) => (
          <Badge key={line.id} variant="secondary" appearance="light" size="sm">
            {line.name}
          </Badge>
        ))}
      </div>
      {result && (
        <>
          <p role="status" className="text-xs font-medium text-foreground">
            已读取 {result.count} 条；未导出 {result.errors.length} 条。
          </p>
          {result.errors.length > 0 && (
            <ul className="space-y-1 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3.5 text-xs text-foreground">
              {result.errors.map((error, i) => (
                <li key={i}>{error}</li>
              ))}
            </ul>
          )}
          {result.count > 0 && (
            <div className="flex flex-wrap gap-2">
              <CopyButton value={result.links} label="复制分享链接" />
              <Button
                type="button"
                variant="outline"
                onClick={() => download("proxy-links.txt", result.links)}
              >
                <Download />
                下载链接
              </Button>
              <Button
                type="button"
                variant="outline"
                onClick={() => download("proxies.yaml", result.yaml)}
              >
                <Download />
                下载 Mihomo 节点列表
              </Button>
            </div>
          )}
        </>
      )}
    </Modal>
  );
}

export function ProxyList({
  state,
  saved,
  edit,
  create,
  share,
  probe,
  apply,
  configureImport,
  remote = false,
}: {
  state: PanelState;
  saved: () => Promise<void>;
  edit: (line: Line) => void;
  create: () => void;
  share: (line: Line) => void;
  probe: (line: Line) => void;
  apply: (line?: Line) => void;
  configureImport: (item: { id: string; name: string }) => void;
  remote?: boolean;
}) {
  const [search, setSearch] = useState("");
  const [protocol, setProtocol] = useState("");
  const [status, setStatus] = useState("");
  const [attention, setAttention] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [modal, setModal] = useState<ReactNode>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const close = () => setModal(null);

  const role = state.viewer?.role || "admin";
  const admin = role === "admin";
  const billing = (admin || role === "finance") && !remote;
  const operations = admin || role === "ops";
  const selectable = admin || billing;
  const completed = async () => {
    await saved();
    setSelected([]);
  };

  const all = state.lines;
  const lines = all.filter(
    (line) =>
      matchesLine(line, state, search) &&
      (!protocol || line.access.protocol === protocol) &&
      (!status || line.rental_status === status) &&
      (!attention ||
        (attention === "expiring"
          ? expiringSoon(line, state.time)
          : lowQuota(line))),
  );
  const chosen = all.filter((line) => selected.includes(line.id));
  const sharedPlanSelected = chosen.some((line) => line.customer_plan_id);
  const enabledPlanSelected = chosen.some(
    (line) => line.customer_plan_id && line.enabled,
  );
  const disabled = state.busy || pending;

  async function operate(action: "enable" | "disable", targets: Line[]) {
    setPending(true);
    setError("");
    try {
      await api("/lines/operate", "POST", {
        line_ids: targets.map((line) => line.id),
        action,
      });
      await completed();
    } catch (error) {
      setError(errorText(error));
    } finally {
      setPending(false);
    }
  }

  function remove(targets: Line[]) {
    setModal(
      <Modal
        title={`删除代理 · ${targets.length} 条`}
        description="删除所选代理并撤销其查询链接；同机其余代理可能短暂重连。"
        submit="确认删除并下发"
        close={close}
        action={async () => {
          await api("/lines/operate", "POST", {
            line_ids: targets.map((line) => line.id),
            action: "delete",
          });
          await completed();
          close();
        }}
      >
        <div className="flex flex-wrap gap-1.5">
          {targets.map((line) => (
            <Badge key={line.id} variant="destructive" appearance="light" size="sm">
              {line.name}
            </Badge>
          ))}
        </div>
      </Modal>,
    );
  }

  function unlimited(targets: Line[]) {
    setModal(
      <Modal
        title={`设为无限期 · ${targets.length} 条`}
        description="移除到期时间限制并同步，保持当前流量额度与限速设置。"
        submit="确认设为无限期并下发"
        close={close}
        action={async () => {
          await api("/lines/operate", "POST", {
            line_ids: targets.map((line) => line.id),
            action: "unlimited",
          });
          await completed();
          close();
        }}
      >
        <div className="flex flex-wrap gap-1.5">
          {targets.map((line) => (
            <Badge key={line.id} variant="primary" appearance="light" size="sm">
              {line.name}
            </Badge>
          ))}
        </div>
      </Modal>,
    );
  }

  function filtersChanged(set: (value: string) => void, value: string) {
    set(value);
    setSelected([]);
  }

  return (
    <Card>
      <CardHeader>
        <CardHeading className="flex items-center gap-2 space-y-0">
          <CardTitle>我的代理</CardTitle>
          <Badge variant="secondary" appearance="light" size="sm">
            {lines.length} / {all.length}
          </Badge>
        </CardHeading>
      </CardHeader>

      {admin && <LegacyConfigs state={state} saved={saved} apply={() => apply()} />}

      {admin &&
        (state.pending_imports || []).map((item) => (
          <div
            key={item.id}
            className="flex flex-wrap items-center justify-between gap-3 border-b border-border bg-amber-500/5 px-5 py-3.5"
          >
            <div>
              <p className="text-xs font-semibold text-foreground">
                {item.name} · 待补充配置
              </p>
              <p className="mt-0.5 text-xs text-amber-600 dark:text-amber-400">
                {item.reason}；当前未运行。
              </p>
            </div>
            <div className="flex gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={disabled}
                onClick={() => configureImport(item)}
              >
                配置代理
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={disabled}
                onClick={async () => {
                  setPending(true);
                  setError("");
                  try {
                    await api(`/legacy-import/${item.id}`, "DELETE");
                    await saved();
                  } catch (error) {
                    setError(errorText(error));
                  } finally {
                    setPending(false);
                  }
                }}
              >
                移除名称
              </Button>
            </div>
          </div>
        ))}

      <div
        className="flex flex-wrap items-center gap-2.5 border-b border-border bg-muted/15 px-5 py-4"
        role="group"
        aria-label="代理搜索与筛选"
      >
        <div className="relative w-full min-w-0 sm:w-64">
          <Search className="pointer-events-none absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="pl-8.5"
            aria-label="搜索代理名称、客户、服务器、地址或端口"
            placeholder="搜索名称、客户、地址或端口"
            value={search}
            onChange={(event) => filtersChanged(setSearch, event.target.value)}
          />
        </div>
        <select
          className="select w-auto min-w-0 max-w-full"
          aria-label="筛选协议"
          value={protocol}
          onChange={(event) => filtersChanged(setProtocol, event.target.value)}
        >
          <option value="">所有协议</option>
          {Object.entries(protocolNames).map(([value, name]) => (
            <option key={value} value={value}>
              {name}
            </option>
          ))}
        </select>
        <select
          className="select w-auto min-w-0 max-w-full"
          aria-label="筛选状态"
          value={status}
          onChange={(event) => filtersChanged(setStatus, event.target.value)}
        >
          <option value="">所有状态</option>
          {Object.entries(rentalStatusNames).map(([value, name]) => (
            <option key={value} value={value}>
              {name}
            </option>
          ))}
        </select>
        <select
          className="select w-auto min-w-0 max-w-full"
          aria-label="筛选到期与余量"
          value={attention}
          onChange={(event) => filtersChanged(setAttention, event.target.value)}
        >
          <option value="">到期与余量</option>
          <option value="expiring">7 天内到期</option>
          <option value="low">余量 ≤10%</option>
        </select>
        {(search || protocol || status || attention) && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setSearch("");
              setProtocol("");
              setStatus("");
              setAttention("");
              setSelected([]);
            }}
          >
            清除筛选
          </Button>
        )}
      </div>

      {chosen.length > 0 && (
        <div
          className="flex flex-wrap items-center gap-2 border-b border-primary/20 bg-primary/8 px-5 py-3"
          aria-label="批量操作"
        >
          <span className="mr-1 text-xs font-semibold text-primary">
            已选 {chosen.length} 条
          </span>
          {admin && (
            <>
              <Button
                variant="outline"
                size="sm"
                disabled={disabled}
                onClick={() => void operate("enable", chosen)}
              >
                启用
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={disabled}
                onClick={() => void operate("disable", chosen)}
              >
                停用
              </Button>
            </>
          )}
          {billing && !sharedPlanSelected && (
            <Button
              variant="outline"
              size="sm"
              disabled={disabled}
              onClick={() =>
                setModal(
                  <RenewalDialog
                    lines={chosen}
                    close={close}
                    saved={completed}
                  />,
                )
              }
            >
              收款 / 续费
            </Button>
          )}
          {billing && sharedPlanSelected && (
            <span className="text-xs text-muted-foreground">
              包含共享套餐线路，请到客户与收款按套餐收款。
            </span>
          )}
          {admin && enabledPlanSelected && (
            <span className="text-xs text-muted-foreground">
              启用中的套餐线路请先停用后再删除。
            </span>
          )}
          {admin && !remote && !sharedPlanSelected && (
            <Button
              variant="outline"
              size="sm"
              disabled={disabled}
              onClick={() => unlimited(chosen)}
            >
              设为无限期（自用）
            </Button>
          )}
          {admin && !remote && (
            <Button
              variant="outline"
              size="sm"
              disabled={disabled}
              onClick={() =>
                setModal(
                  <LimitsDialog lines={chosen} close={close} saved={completed} />,
                )
              }
            >
              额度 / 限速
            </Button>
          )}
          {admin && (
            <>
              <Button
                variant="outline"
                size="sm"
                disabled={disabled}
                onClick={() =>
                  setModal(<ExportDialog lines={chosen} close={close} />)
                }
              >
                导出
              </Button>
              {!enabledPlanSelected && (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={disabled}
                  onClick={() => remove(chosen)}
                >
                  删除
                </Button>
              )}
            </>
          )}
          <Button variant="ghost" size="sm" onClick={() => setSelected([])}>
            取消选择
          </Button>
        </div>
      )}

      {error && (
        <p
          role="alert"
          className="m-4 rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-xs text-destructive"
        >
          {error}
        </p>
      )}

      {lines.length ? (
        <CardTable>
          <div
            className={`overflow-x-auto fleet-responsive ${
              selectable ? "proxy-responsive" : ""
            }`}
            tabIndex={0}
            role="region"
            aria-label="代理列表"
          >
            <table className="data-table [&_th]:px-3 [&_td]:px-3">
              <thead>
                <tr>
                  {selectable && (
                    <th className="w-10">
                      <input
                        type="checkbox"
                        aria-label="选择全部筛选结果"
                        disabled={disabled}
                        checked={lines.every((line) => selected.includes(line.id))}
                        ref={(element) => {
                          if (element)
                            element.indeterminate =
                              lines.some((line) => selected.includes(line.id)) &&
                              !lines.every((line) => selected.includes(line.id));
                        }}
                        onChange={(event) =>
                          setSelected(
                            event.target.checked
                              ? lines.map((line) => line.id)
                              : [],
                          )
                        }
                      />
                    </th>
                  )}
                  <th>名称 / 归属 / 路径</th>
                  <th>接入入口</th>
                  <th>状态</th>
                  <th>周期用量</th>
                  <th>实时速率</th>
                  <th>到期 / 重置</th>
                  <th>报价</th>
                  <th className="text-right">操作</th>
                </tr>
              </thead>
              <tbody>
                {lines.map((line) => (
                  <tr key={line.id}>
                    {selectable && (
                      <td data-label="选择代理">
                        <input
                          type="checkbox"
                          aria-label={`选择 ${line.name}`}
                          disabled={disabled}
                          checked={selected.includes(line.id)}
                          onChange={(event) =>
                            setSelected((old) =>
                              event.target.checked
                                ? [...old, line.id]
                                : old.filter((id) => id !== line.id),
                            )
                          }
                        />
                      </td>
                    )}
                    <td data-label="名称 / 归属 / 路径" className="min-w-[170px] max-w-[245px]">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <p className="break-all text-[13px] font-semibold text-foreground">
                          {line.name}
                        </p>
                        <Badge variant="secondary" appearance="light" size="xs">
                          {billing
                            ? state.customers.find(
                                (customer) =>
                                  customer.id === line.rental?.customer_id,
                              )?.name || "自用"
                            : admin
                              ? "远端"
                              : "未授权"}
                        </Badge>
                      </div>
                      {admin && (
                        <div className="mt-1.5">
                          <PathView line={line} state={state} />
                        </div>
                      )}
                    </td>
                    <td data-label="接入入口" className="max-w-[185px]">
                      <Badge variant="primary" appearance="light" size="sm">
                        {protocolNames[line.access.protocol]}
                      </Badge>
                      <p className="mono mt-1.5 break-all text-[11px] leading-relaxed text-muted-foreground">
                        {lineEndpoint(line, state.nodes)}
                      </p>
                    </td>
                    <td data-label="状态" className="whitespace-nowrap">
                      <div className="flex flex-col items-start gap-1">
                        <RentalStatusBadge status={line.rental_status} />
                        <SyncLabel status={line.sync_status} />
                        {!line.published && (
                          <span className="text-[11px] font-medium text-amber-600 dark:text-amber-400">
                            有待应用的配置
                          </span>
                        )}
                        {state.checks[line.id]?.ok === false && (
                          <span className="text-[11px] font-medium text-destructive">
                            最近探测未通过
                          </span>
                        )}
                      </div>
                    </td>
                    <td data-label="周期用量">
                      <RentalUsage line={line} />
                    </td>
                    <td data-label="实时速率">
                      <RentalSpeed line={line} now={state.time} />
                    </td>
                    <td data-label="到期 / 重置" className="whitespace-nowrap">
                      {billing || admin ? <RentalDates line={line} /> : "未授权"}
                    </td>
                    <td data-label="报价" className="whitespace-nowrap">
                      {billing || admin ? <RentalPrices line={line} /> : "未授权"}
                    </td>
                    <td data-label="操作" className="whitespace-nowrap">
                      <div className="flex flex-wrap items-center justify-start gap-1 sm:justify-end">
                        {billing && line.customer_plan_id && (
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              location.hash = "customers";
                            }}
                          >
                            前往客户套餐
                          </Button>
                        )}
                        {billing && !line.customer_plan_id && (
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={disabled}
                            onClick={() =>
                              setModal(
                                <RentalEditor
                                  line={line}
                                  state={state}
                                  close={close}
                                  saved={saved}
                                />,
                              )
                            }
                          >
                            出租信息
                          </Button>
                        )}
                        {billing && !line.customer_plan_id && (
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={disabled || !line.rental}
                            onClick={() =>
                              setModal(
                                <RenewalDialog
                                  lines={[line]}
                                  close={close}
                                  saved={saved}
                                />,
                              )
                            }
                          >
                            收款 / 续费
                          </Button>
                        )}
                        {role !== "viewer" && (
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button
                                variant="ghost"
                                size="sm"
                                mode="icon"
                                aria-label={`${line.name} 更多操作`}
                                disabled={disabled}
                              >
                                <MoreHorizontal className="size-4" />
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent
                              align="end"
                              collisionPadding={8}
                              className="min-w-50"
                            >
                              {admin && (
                                <>
                                  <DropdownMenuItem
                                    onSelect={() =>
                                      void operate(
                                        line.enabled ? "disable" : "enable",
                                        [line],
                                      )
                                    }
                                  >
                                    {line.enabled ? <Ban /> : <Play />}
                                    {line.enabled ? "手动停用" : "解除手动停用"}
                                  </DropdownMenuItem>
                                  <DropdownMenuItem
                                    onSelect={() =>
                                      setModal(
                                        <LimitsDialog
                                          lines={[line]}
                                          close={close}
                                          saved={saved}
                                        />,
                                      )
                                    }
                                  >
                                    <Gauge />
                                    调整额度 / 限速
                                  </DropdownMenuItem>
                                  {!remote && !line.customer_plan_id && (
                                    <DropdownMenuItem
                                      onSelect={() => unlimited([line])}
                                    >
                                      <InfinityIcon />
                                      设为无限期（自用）
                                    </DropdownMenuItem>
                                  )}
                                  <DropdownMenuItem
                                    disabled={!line.published || !line.enabled}
                                    onSelect={() => share(line)}
                                  >
                                    <Share2 />
                                    分享配置
                                  </DropdownMenuItem>
                                </>
                              )}
                              {billing && !line.customer_plan_id && (
                                <DropdownMenuItem
                                  disabled={!line.rental || !line.published}
                                  onSelect={() =>
                                    setModal(
                                      <QueryLinkDialog
                                        line={line}
                                        close={close}
                                        saved={saved}
                                      />,
                                    )
                                  }
                                >
                                  <Link2 />
                                  客户私密查询链接
                                </DropdownMenuItem>
                              )}
                              {admin && (
                                <>
                                  <DropdownMenuSeparator />
                                  <DropdownMenuItem
                                    onSelect={() =>
                                      setModal(
                                        <AccessPolicyEditor
                                          line={line}
                                          close={close}
                                          saved={saved}
                                        />,
                                      )
                                    }
                                  >
                                    <Shield />
                                    网站 / 端口 / 连接限制
                                  </DropdownMenuItem>
                                  <DropdownMenuItem
                                    onSelect={() => edit(line)}
                                  >
                                    <Pencil />
                                    编辑代理配置
                                  </DropdownMenuItem>
                                </>
                              )}
                              {operations && (
                                <>
                                  <DropdownMenuSeparator />
                                  <DropdownMenuItem
                                    disabled={!line.published || !line.enabled}
                                    onSelect={() => probe(line)}
                                  >
                                    <Activity />
                                    检查连接
                                  </DropdownMenuItem>
                                  <DropdownMenuItem
                                    onSelect={() =>
                                      setModal(
                                        <Diagnostics
                                          nodes={state.nodes.filter((n) =>
                                            line.hops.some(
                                              (h) => h.node_id === n.id,
                                            ),
                                          )}
                                          lineId={line.id}
                                          canSyncTime={admin}
                                          close={close}
                                        />,
                                      )
                                    }
                                  >
                                    <FileText />
                                    日志与诊断
                                  </DropdownMenuItem>
                                  <DropdownMenuItem
                                    onSelect={() => apply(line)}
                                  >
                                    <RefreshCw />
                                    应用配置修改
                                  </DropdownMenuItem>
                                </>
                              )}
                              {admin && (!line.customer_plan_id || !line.enabled) && (
                                <>
                                  <DropdownMenuSeparator />
                                  <DropdownMenuItem
                                    variant="destructive"
                                    onSelect={() => remove([line])}
                                  >
                                    <Trash2 />
                                    删除代理
                                  </DropdownMenuItem>
                                </>
                              )}
                            </DropdownMenuContent>
                          </DropdownMenu>
                        )}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </CardTable>
      ) : (
        <Empty
          title={all.length ? "没有匹配结果" : "暂无代理"}
          action={
            admin &&
            !all.length && (
              <Button onClick={create}>
                <Plus />
                新建代理
              </Button>
            )
          }
        >
          {admin
            ? "创建代理后可一键导出分享链接，或在「出租信息」中分配客户与周期报价。"
            : "当前没有可查看的代理。"}
        </Empty>
      )}

      <CardFooter className="flex flex-wrap items-center justify-between gap-3 py-3 text-xs text-muted-foreground">
        <span>
          共 {lines.length} 条记录
          {operations &&
            state.lines.some((line) => !line.published) &&
            " · 存在未同步的草稿修改"}
        </span>
        {operations && state.lines.some((line) => !line.published) && (
          <Button
            variant="primary"
            size="sm"
            disabled={disabled}
            onClick={() => apply()}
          >
            <CheckCircle2 />
            应用待处理配置修改
          </Button>
        )}
      </CardFooter>

      {modal}
    </Card>
  );
}
