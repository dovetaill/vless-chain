import { Activity, AlertTriangle, Database, Waypoints } from "lucide-react";
import type { Line, PanelState } from "../types";
import {
  Card,
  CardContent,
  CardHeader,
  CardHeading,
  CardTable,
  CardTitle,
} from "./ui/card";
import { Progress } from "./ui/progress";
import { bytes, Empty, InfoTip, Status } from "./shared";
import { TrafficChart } from "./traffic-chart";
import {
  RentalDates,
  RentalPrices,
  RentalSpeed,
  RentalStatusBadge,
  RentalUsage,
  SyncLabel,
} from "./rental-shared";
import { beijingTime, expiringSoon, lowQuota } from "../lib/business";

export function Dashboard({
  state,
  manage,
}: {
  state: PanelState;
  manage?: (line: Line) => void;
}) {
  const billing = !state.viewer || ["admin", "finance"].includes(state.viewer.role);
  const lines = state.lines;
  const days: Record<string, [number, number]> = {};
  for (const line of lines)
    for (const [day, traffic] of Object.entries(line.traffic?.days || {})) {
      const total = (days[day] ||= [0, 0]);
      total[0] += traffic[0];
      total[1] += traffic[1];
    }
  const used = lines.reduce(
    (sum, line) => sum + (line.traffic ? line.traffic.upload + line.traffic.download : 0),
    0,
  );
  const fresh = lines.filter(
    (line) =>
      line.traffic &&
      state.time - line.traffic.sampled_at <= 30 &&
      line.rental_status !== "offline",
  );
  const upload = fresh.reduce((sum, line) => sum + line.traffic!.upload_bps, 0);
  const download = fresh.reduce((sum, line) => sum + line.traffic!.download_bps, 0);

  const kpis = [
    {
      label: billing ? "代理 / 客户" : "代理总数",
      value: billing ? `${lines.length} / ${state.customers.length}` : `${lines.length}`,
      detail: `${lines.filter((line) => line.rental_status === "active").length} 条运行中`,
      icon: Waypoints,
      tone: "bg-primary/10 text-primary",
    },
    {
      label: "当前周期流量",
      value: lines.some((line) => line.traffic) ? bytes(used) : "待采集",
      detail: "活跃线路本周期累计",
      icon: Database,
      tone: "bg-violet-500/10 text-violet-600 dark:text-violet-400",
    },
    {
      label: "代理实时吞吐",
      value: `↓ ${fresh.length ? `${bytes(download)}/s` : "—"}`,
      detail: `↑ ${fresh.length ? `${bytes(upload)}/s` : "等待实时采样"}`,
      icon: Activity,
      tone: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
    },
    billing
      ? {
          label: "到期与余量预警",
          value: `${lines.filter((line) => expiringSoon(line, state.time)).length} 条临期`,
          detail: `${lines.filter(lowQuota).length} 条余量 ≤10% · ${lines.filter((line) => line.rental_status === "expired").length} 条已到期`,
          icon: AlertTriangle,
          tone: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
        }
      : {
          label: "运行关注项",
          value: `${lines.filter((line) => line.rental_status === "offline").length} 条离线`,
          detail: `${lines.filter((line) => line.sync_status === "pending").length} 条待同步 · ${lines.filter((line) => line.sync_status === "error").length} 条异常`,
          icon: AlertTriangle,
          tone: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
        },
  ];

  return (
    <div className="space-y-5">
      <div className="grid grid-cols-2 gap-3.5 lg:grid-cols-4">
        {kpis.map((item) => {
          const Icon = item.icon;
          return (
            <Card key={item.label}>
              <CardContent className="p-4 sm:p-5">
                <div className="flex items-center justify-between gap-2">
                  <p className="text-xs font-medium text-muted-foreground">{item.label}</p>
                  <span
                    className={`inline-flex size-8 shrink-0 items-center justify-center rounded-lg ${item.tone}`}
                  >
                    <Icon className="size-4" />
                  </span>
                </div>
                <p className="mt-3 text-lg font-semibold tracking-tight tabular-nums text-foreground sm:text-xl">
                  {item.value}
                </p>
                <p className="mt-1 text-xs text-muted-foreground">{item.detail}</p>
              </CardContent>
            </Card>
          );
        })}
      </div>

      <div className="grid items-start gap-5 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Card className="min-w-0">
          <CardHeader>
            <CardHeading className="flex items-center gap-1.5 space-y-0">
              <CardTitle>服务器实时状态</CardTitle>
              <InfoTip>
                主机网卡统计包含系统全部进出流量，与单条代理业务的计量流量独立统计。
              </InfoTip>
            </CardHeading>
          </CardHeader>
          <CardContent className="space-y-3.5">
            {!state.nodes.length ? (
              <p className="muted py-4 text-center">新建代理后自动接入本机节点并开始采样。</p>
            ) : (
              state.nodes.map((node) => {
                const system = node.report.system ?? {};
                const fresh =
                  node.online &&
                  Boolean(system.sampled_at && state.time - system.sampled_at <= 30);
                const cpuPct =
                  fresh && system.cpu_percent != null ? Math.min(100, system.cpu_percent) : null;
                const memPct =
                  fresh && system.memory_used != null && system.memory_total
                    ? Math.min(100, (system.memory_used / system.memory_total) * 100)
                    : null;
                const diskPct =
                  fresh && system.disk_used != null && system.disk_total
                    ? Math.min(100, (system.disk_used / system.disk_total) * 100)
                    : null;

                return (
                  <section
                    key={node.id}
                    className="min-w-0 space-y-3.5 rounded-xl border border-border bg-muted/25 p-4"
                    aria-label={`${node.name} 实时状态`}
                  >
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <h3 className="break-all text-sm font-semibold text-foreground">
                          {node.name}
                        </h3>
                        <p className="mono mt-0.5 break-all text-xs text-muted-foreground">
                          {node.host}
                        </p>
                      </div>
                      <Status tone={node.online ? "good" : "bad"}>
                        {node.online ? "在线" : "离线"}
                      </Status>
                    </div>

                    <dl className="grid grid-cols-1 gap-3 text-xs sm:grid-cols-3">
                      <div className="space-y-1.5 rounded-lg border border-border/70 bg-card p-3">
                        <div className="flex items-center justify-between">
                          <dt className="text-muted-foreground">CPU</dt>
                          <dd className="font-semibold tabular-nums text-foreground">
                            {fresh && system.cpu_percent != null
                              ? `${system.cpu_percent.toFixed(1)}%`
                              : "—"}
                          </dd>
                        </div>
                        <Progress aria-label="CPU" value={cpuPct} />
                      </div>
                      <div className="space-y-1.5 rounded-lg border border-border/70 bg-card p-3">
                        <div className="flex items-center justify-between gap-1">
                          <dt className="text-muted-foreground">内存</dt>
                          <dd className="truncate font-semibold tabular-nums text-foreground">
                            {fresh && system.memory_used != null && system.memory_total
                              ? `${bytes(system.memory_used)} / ${bytes(system.memory_total)}`
                              : "—"}
                          </dd>
                        </div>
                        <Progress aria-label="内存" value={memPct} />
                      </div>
                      <div className="space-y-1.5 rounded-lg border border-border/70 bg-card p-3">
                        <div className="flex items-center justify-between gap-1">
                          <dt className="text-muted-foreground">系统磁盘</dt>
                          <dd className="truncate font-semibold tabular-nums text-foreground">
                            {fresh && system.disk_used != null && system.disk_total
                              ? `${bytes(system.disk_used)} / ${bytes(system.disk_total)}`
                              : "—"}
                          </dd>
                        </div>
                        <Progress aria-label="系统磁盘" value={diskPct} />
                      </div>
                    </dl>

                    <dl className="grid gap-3 border-t border-border pt-3 text-xs sm:grid-cols-2">
                      <div>
                        <dt className="text-muted-foreground">网卡实时吞吐</dt>
                        <dd className="mt-1 flex flex-wrap gap-x-3 gap-y-1 font-semibold tabular-nums text-foreground">
                          <span>
                            ↓{" "}
                            {fresh && system.network_rx_bps != null
                              ? `${bytes(system.network_rx_bps)}/s`
                              : "—"}
                          </span>
                          <span>
                            ↑{" "}
                            {fresh && system.network_tx_bps != null
                              ? `${bytes(system.network_tx_bps)}/s`
                              : "—"}
                          </span>
                        </dd>
                      </div>
                      <div>
                        <dt className="text-muted-foreground">网卡累计收发</dt>
                        <dd className="mt-1 flex flex-wrap gap-x-3 gap-y-1 font-medium tabular-nums text-foreground">
                          <span>
                            ↓{" "}
                            {fresh && system.network_rx != null
                              ? bytes(system.network_rx)
                              : "—"}
                          </span>
                          <span>
                            ↑{" "}
                            {fresh && system.network_tx != null
                              ? bytes(system.network_tx)
                              : "—"}
                          </span>
                        </dd>
                      </div>
                    </dl>

                    <p className="text-[11px] text-muted-foreground tabular-nums">
                      {system.sampled_at
                        ? `采样 ${beijingTime(system.sampled_at)}${fresh ? "" : " · 数据已过期"}`
                        : "等待采样"}
                    </p>
                  </section>
                );
              })
            )}
          </CardContent>
        </Card>

        <TrafficChart days={days} now={state.time} />
      </div>

      <Card>
        <CardHeader>
          <CardHeading className="flex items-center gap-1.5 space-y-0">
            <CardTitle>{billing ? "代理经营总览" : "代理运行总览"}</CardTitle>
            {billing && (
              <InfoTip>
                到期与流量重置均以北京时间为准；点击代理名称可快速配置客户归属与报价。
              </InfoTip>
            )}
          </CardHeading>
        </CardHeader>

        {lines.length ? (
          <CardTable>
            <div
              className="overflow-x-auto fleet-responsive"
              tabIndex={0}
              role="region"
              aria-label={billing ? "代理经营总览表" : "代理运行总览表"}
            >
              <table className="data-table">
                <thead>
                  <tr>
                    <th>{billing ? "代理 / 客户" : "代理"}</th>
                    <th>状态</th>
                    <th>周期用量</th>
                    <th>实时速率</th>
                    {billing && (
                      <>
                        <th>到期 / 重置</th>
                        <th>续费报价</th>
                      </>
                    )}
                  </tr>
                </thead>
                <tbody>
                  {lines.map((line) => (
                    <tr key={line.id}>
                      <td data-label="代理">
                        {manage ? (
                          <button
                            className="cursor-pointer text-left text-sm font-semibold text-primary underline-offset-4 hover:underline"
                            onClick={() => manage(line)}
                          >
                            {line.name}
                          </button>
                        ) : (
                          <span className="text-sm font-semibold text-foreground">
                            {line.name}
                          </span>
                        )}
                        {billing && (
                          <p className="mt-1 text-xs text-muted-foreground">
                            {state.customers.find(
                              (customer) => customer.id === line.rental?.customer_id,
                            )?.name || "自用"}
                          </p>
                        )}
                      </td>
                      <td data-label="状态">
                        <div className="flex flex-col items-start gap-1">
                          <RentalStatusBadge status={line.rental_status} />
                          <SyncLabel status={line.sync_status} />
                        </div>
                      </td>
                      <td data-label="周期用量">
                        <RentalUsage line={line} />
                      </td>
                      <td data-label="实时速率">
                        <RentalSpeed line={line} now={state.time} />
                      </td>
                      {billing && (
                        <>
                          <td data-label="到期 / 重置">
                            <RentalDates line={line} />
                          </td>
                          <td data-label="续费报价">
                            <RentalPrices line={line} />
                          </td>
                        </>
                      )}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </CardTable>
        ) : (
          <Empty title="尚无代理">
            {!state.viewer || state.viewer.role === "admin"
              ? "新建代理后即可配置客户端分享链接或分配客户。"
              : "当前没有可查看的代理。"}
          </Empty>
        )}
      </Card>
    </div>
  );
}
