import {
  ArrowDownLeft,
  ArrowUpRight,
  ArrowRight,
  ArrowLeftRight,
  Globe2,
  Network,
  Server,
  Activity,
  Bell,
} from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Button } from "./ui/button";
import { bytes, Status, stamp } from "./shared";
import type { Line, PanelState } from "../types";
export function Dashboard({
  state,
  create,
  navigate,
}: {
  state: PanelState;
  create: (kind: Line["kind"]) => void;
  navigate: (page: string) => void;
}) {
  const traffic = state.lines.map((l) => l.traffic).filter((t) => t !== null);
  const active = state.lines.filter((l) => l.enabled && l.published);
  const live = state.lines.filter(
    (l) =>
      l.traffic && state.nodes.find((n) => n.id === l.hops[0]?.node_id)?.online,
  );
  const sum = (key: "upload_bps" | "download_bps") =>
    live.reduce((n, l) => n + (l.traffic?.[key] || 0), 0);
  const days = Array.from({ length: 7 }, (_, i) =>
    new Date(Date.now() - (6 - i) * 86400000).toISOString().slice(0, 10),
  );
  const points = days.map((day) =>
    traffic.reduce(
      (sum, t) => sum + (t.days[day]?.[0] || 0) + (t.days[day]?.[1] || 0),
      0,
    ),
  );
  const max = Math.max(...points, 1);
  return (
    <div className="space-y-6">
      <section className="welcome-panel">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.16em] text-blue-200">
            YOUR NETWORK, AT A GLANCE
          </p>
          <h2 className="mt-3 text-2xl font-semibold tracking-tight sm:text-3xl">
            让每一条连接，都清晰可见。
          </h2>
          <p className="mt-3 max-w-xl text-sm leading-7 text-blue-100">
            从一个端口开始，管理代理接入、服务器中转和网站访问。
          </p>
          <div className="mt-5 flex items-center gap-2 text-xs text-blue-100">
            <span className="size-2 rounded-full bg-emerald-400" />
            {state.nodes.filter((n) => n.online).length} 台服务器在线 ·{" "}
            {active.length} 条规则已启用
          </div>
        </div>
        <div aria-hidden className="network-art">
          <div className="network-orbit" />
          <span className="network-core">
            <Network className="size-10" />
          </span>
          <span className="network-satellite one">
            <Server />
          </span>
          <span className="network-satellite two">
            <Globe2 />
          </span>
          <span className="network-satellite three">
            <Activity />
          </span>
        </div>
      </section>
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        {[
          {
            label: "在线服务器",
            value: `${state.nodes.filter((n) => n.online).length} / ${state.nodes.length}`,
            hint: "最近 15 秒内收到心跳",
            icon: Server,
            color: "text-blue-600 bg-blue-50",
          },
          {
            label: "已启用规则",
            value: String(active.length),
            hint: `${state.lines.filter((l) => !l.published).length} 条有待应用的修改`,
            icon: Network,
            color: "text-violet-600 bg-violet-50",
          },
          {
            label: "当前下载",
            value: live.length ? `${bytes(sum("download_bps"))}/s` : "尚未采集",
            hint: live.length
              ? `上传 ${bytes(sum("upload_bps"))}/s · 在线入口合计`
              : "创建并应用规则后开始统计",
            icon: ArrowDownLeft,
            color: "text-emerald-600 bg-emerald-50",
          },
          {
            label: "本月流量",
            value: traffic.length
              ? bytes(traffic.reduce((n, t) => n + t.upload + t.download, 0))
              : "尚未采集",
            hint: "入口上传＋下载 · UTC 月周期",
            icon: Activity,
            color: "text-amber-600 bg-amber-50",
          },
        ].map((m) => (
          <Card key={m.label}>
            <CardContent className="p-5">
              <div className="flex items-center justify-between">
                <p className="text-sm text-slate-500">{m.label}</p>
                <span className={`rounded-lg p-2 ${m.color}`}>
                  <m.icon className="size-4" />
                </span>
              </div>
              <p className="mt-3 text-2xl font-semibold tracking-tight">
                {m.value}
              </p>
              <p className="mt-2 text-xs text-slate-400">{m.hint}</p>
            </CardContent>
          </Card>
        ))}
      </div>
      <div className="grid gap-4 md:grid-cols-3">
        {[
          {
            kind: "vless" as const,
            title: "给客户端建代理",
            text: "创建后扫码，或导入 Mihomo 配置。",
            icon: Network,
          },
          {
            kind: "forward" as const,
            title: "转发一个端口",
            text: "指定入口和目标，支持 TCP / UDP。",
            icon: ArrowLeftRight,
          },
          {
            kind: "website" as const,
            title: "发布一个网站",
            text: "用域名访问后端，自动管理 HTTPS。",
            icon: Globe2,
          },
        ].map((item) => (
          <button
            key={item.kind}
            className="quick-task group"
            onClick={() => create(item.kind)}
          >
            <span className="rounded-xl border border-border bg-slate-50 p-3 text-blue-600">
              <item.icon className="size-5" />
            </span>
            <span className="text-left">
              <strong className="text-sm font-semibold">{item.title}</strong>
              <span className="mt-1.5 block text-xs leading-5 text-slate-500">
                {item.text}
              </span>
            </span>
            <ArrowUpRight className="ms-auto size-4 shrink-0 text-slate-300 transition group-hover:text-blue-600" />
          </button>
        ))}
      </div>
      <div className="grid gap-6 xl:grid-cols-[1.5fr_1fr]">
        <Card>
          <CardHeader>
            <CardTitle>近 7 天流量</CardTitle>
            <span className="text-xs text-slate-400">UTC · 入口合计</span>
          </CardHeader>
          <CardContent>
            {traffic.length ? (
              <div className="space-y-3">
                <div className="flex h-48 items-end justify-around gap-3 border-b border-slate-100 px-2">
                  {points.map((n, i) => (
                    <div
                      key={days[i]}
                      className="flex h-full flex-1 flex-col items-center justify-end gap-2"
                    >
                      <span className="text-[10px] text-slate-500">
                        {bytes(n)}
                      </span>
                      <div
                        title={`${days[i]} ${bytes(n)}`}
                        className="w-full max-w-14 rounded-t-md bg-linear-to-t from-blue-600 to-blue-400"
                        style={{ height: `${Math.max(2, (n / max) * 80)}%` }}
                      />
                    </div>
                  ))}
                </div>
                <div className="flex justify-around text-[11px] text-slate-400">
                  {days.map((day) => (
                    <span key={day}>{day.slice(5)}</span>
                  ))}
                </div>
              </div>
            ) : (
              <div className="flex h-56 flex-col items-center justify-center gap-3 text-slate-400">
                <Activity className="size-8" />
                <p className="text-sm">
                  应用第一条规则后，这里会展示真实流量趋势。
                </p>
              </div>
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Bell className="size-4" />
              需要关注
            </CardTitle>
            <Status tone={state.alerts.length ? "warn" : "good"}>
              {state.alerts.length || "正常"}
            </Status>
          </CardHeader>
          <CardContent className="space-y-3">
            {state.alerts.length ? (
              state.alerts.slice(0, 5).map((a) => (
                <div
                  key={a.id}
                  className={`rounded-lg border p-3 text-sm leading-6 ${a.level === "error" ? "border-red-100 bg-red-50 text-red-800" : "border-amber-100 bg-amber-50/60 text-amber-800"}`}
                >
                  {a.message}
                </div>
              ))
            ) : (
              <div className="rounded-lg bg-emerald-50 p-5 text-sm leading-7 text-emerald-800">
                目前没有异常提醒。你可以随时在线路详情中检查连接。
              </div>
            )}
            <Button
              variant="ghost"
              className="w-full"
              onClick={() => navigate("monitor")}
            >
              查看监控与告警
              <ArrowRight />
            </Button>
          </CardContent>
        </Card>
      </div>
      <Card>
        <CardHeader>
          <CardTitle>服务器状态</CardTitle>
          <Button variant="ghost" size="sm" onClick={() => navigate("nodes")}>
            管理服务器
            <ArrowRight />
          </Button>
        </CardHeader>
        <CardContent>
          {state.nodes.length ? (
            <div className="divide-y divide-border">
              {state.nodes.slice(0, 5).map((n) => (
                <div
                  key={n.id}
                  className="flex flex-wrap items-center gap-4 py-4 first:pt-0"
                >
                  <span className="rounded-lg bg-slate-100 p-2.5 text-slate-500">
                    <Server className="size-5" />
                  </span>
                  <div className="min-w-32 flex-1">
                    <p className="text-sm font-medium">{n.name}</p>
                    <p className="mono mt-1 text-xs text-slate-400">{n.host}</p>
                  </div>
                  <Status tone={n.online ? "good" : "warn"}>
                    {n.online ? "在线" : "离线"}
                  </Status>
                  <span className="text-xs text-slate-400">
                    {n.online && n.report.system?.memory_total
                      ? `内存 ${bytes(n.report.system.memory_used || 0)} / ${bytes(n.report.system.memory_total)}`
                      : stamp(n.seen)}
                  </span>
                </div>
              ))}
            </div>
          ) : (
            <p className="muted py-8 text-center">
              先接入自己的服务器，即可开始创建。
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
