import type { Line, PanelState } from "../types";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { bytes, Empty, Status } from "./shared";
import { TrafficChart } from "./traffic-chart";
import { RentalDates, RentalPrices, RentalSpeed, RentalStatusBadge, RentalUsage, SyncLabel } from "./rental-shared";
import { beijingTime, expiringSoon, lowQuota } from "../lib/business";

export function Dashboard({ state, manage }: { state: PanelState; manage: (line: Line) => void }) {
  const lines = state.lines.filter(line => line.kind === "vless");
  const days: Record<string, [number, number]> = {};
  for (const line of lines) for (const [day, traffic] of Object.entries(line.traffic?.days || {})) {
    const total = days[day] ||= [0, 0];
    total[0] += traffic[0];
    total[1] += traffic[1];
  }
  const used = lines.reduce((sum, line) => sum + (line.traffic ? line.traffic.upload + line.traffic.download : 0), 0);
  const fresh = lines.filter(line => line.traffic && state.time - line.traffic.sampled_at <= 30 && line.rental_status !== "offline");
  const upload = fresh.reduce((sum, line) => sum + line.traffic!.upload_bps, 0);
  const download = fresh.reduce((sum, line) => sum + line.traffic!.download_bps, 0);
  return <div className="space-y-4">
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
      {[
        { label: "代理 / 客户", value: `${lines.length} / ${state.customers.length}`, detail: `${lines.filter(line => line.rental_status === "active").length} 条运行中` },
        { label: "当前周期代理流量", value: lines.some(line => line.traffic) ? bytes(used) : "尚未采集", detail: "各代理当前计量周期合计" },
        { label: "代理实时网速", value: `↓ ${fresh.length ? `${bytes(download)}/s` : "—"}`, detail: `↑ ${fresh.length ? `${bytes(upload)}/s` : "暂无实时采样"}` },
        { label: "需要关注", value: `${lines.filter(line => expiringSoon(line, state.time)).length} 条即将到期`, detail: `${lines.filter(lowQuota).length} 条剩余 ≤10% · ${lines.filter(line => line.rental_status === "expired").length} 条已到期` },
      ].map(item => <Card key={item.label}><CardContent className="p-3 sm:p-4"><p className="text-xs text-slate-500">{item.label}</p><p className="mt-2 text-base font-semibold tabular-nums sm:text-lg">{item.value}</p><p className="mt-1 text-xs text-slate-400">{item.detail}</p></CardContent></Card>)}
    </div>
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
    <Card className="min-w-0"><CardHeader className="px-4 py-4"><div className="min-w-0"><CardTitle className="leading-5">服务器实时状态</CardTitle><p className="muted mt-1">主机网卡统计包含全部业务，与代理计费流量分开统计。</p></div></CardHeader><CardContent className="p-4">
      {!state.nodes.length ? <p className="muted">新建代理后自动接入本机，等待首次采样。</p> : <div className="space-y-3">{state.nodes.map(node => {
        const system = node.report.system;
        const fresh = node.online && Boolean(system.sampled_at && state.time - system.sampled_at <= 30);
        return <section key={node.id} className="min-w-0 rounded-lg border border-border bg-slate-50/60 p-3" aria-label={`${node.name} 实时状态`}>
          <div className="mb-3 flex items-start justify-between gap-3"><div className="min-w-0"><h3 className="break-all text-sm font-semibold">{node.name}</h3><p className="mt-1 break-all text-xs text-slate-500">{node.host}</p></div><Status tone={node.online ? "good" : "bad"}>{node.online ? "在线" : "离线"}</Status></div>
          <dl className="grid grid-cols-2 gap-3 text-sm sm:grid-cols-3">
            <div><dt className="text-xs text-slate-500">CPU</dt><dd className="mt-1 font-semibold tabular-nums">{fresh && system.cpu_percent != null ? `${system.cpu_percent.toFixed(1)}%` : "—"}</dd></div>
            <div><dt className="text-xs text-slate-500">内存</dt><dd className="mt-1 font-semibold tabular-nums">{fresh && system.memory_used != null && system.memory_total ? `${bytes(system.memory_used)} / ${bytes(system.memory_total)}` : "—"}</dd></div>
            <div className="col-span-2 sm:col-span-1"><dt className="text-xs text-slate-500">根目录所在磁盘</dt><dd className="mt-1 font-semibold tabular-nums">{fresh && system.disk_used != null && system.disk_total ? `${bytes(system.disk_used)} / ${bytes(system.disk_total)}` : "—"}</dd></div>
          </dl>
          <dl className="mt-3 grid gap-3 border-t border-border pt-3 text-sm sm:grid-cols-2">
            <div><dt className="text-xs text-slate-500">网卡接收 / 发送</dt><dd className="mt-1 flex flex-wrap gap-x-3 gap-y-1 font-medium tabular-nums"><span>↓ {fresh && system.network_rx_bps != null ? `${bytes(system.network_rx_bps)}/s` : "—"}</span><span>↑ {fresh && system.network_tx_bps != null ? `${bytes(system.network_tx_bps)}/s` : "—"}</span></dd></div>
            <div><dt className="text-xs text-slate-500">网卡累计接收 / 发送</dt><dd className="mt-1 flex flex-wrap gap-x-3 gap-y-1 font-medium tabular-nums"><span>↓ {fresh && system.network_rx != null ? bytes(system.network_rx) : "—"}</span><span>↑ {fresh && system.network_tx != null ? bytes(system.network_tx) : "—"}</span></dd></div>
          </dl>
          <p className="mt-3 text-xs leading-5 text-slate-400">{system.sampled_at ? `采样 ${beijingTime(system.sampled_at)}${fresh ? "" : " · 数据已过时"}` : "等待采样"}</p>
        </section>;
      })}</div>}
    </CardContent></Card>
    <TrafficChart days={days} now={state.time} />
    </div>
    <Card><CardHeader className="px-4 py-4"><div><CardTitle className="leading-5">代理经营总览</CardTitle><p className="muted mt-1">到期与额度重置时间均为北京时间；点击名称管理出租信息。</p></div></CardHeader>
      {lines.length ? <div className="overflow-x-auto" tabIndex={0} role="region" aria-label="代理经营总览表"><table className="data-table [&_td]:px-4! [&_td]:py-3! [&_th]:px-4! [&_th]:py-3!"><thead><tr><th>代理 / 客户</th><th>运行 / 同步</th><th>周期用量 / 余量</th><th>实时网速</th><th>到期 / 重置</th><th>续费报价</th></tr></thead><tbody>{lines.map(line => <tr key={line.id}>
        <td><button className="text-left text-sm font-semibold text-blue-700 underline-offset-4 hover:underline" onClick={() => manage(line)}>{line.name}</button><p className="mt-2 text-xs text-slate-500">{state.customers.find(customer => customer.id === line.rental?.customer_id)?.name || "自用"}</p></td>
        <td><RentalStatusBadge status={line.rental_status} /><p className="mt-2"><SyncLabel status={line.sync_status} /></p></td>
        <td><RentalUsage line={line} /></td><td><RentalSpeed line={line} now={state.time} /></td><td><RentalDates line={line} /></td><td><RentalPrices line={line} /></td>
      </tr>)}</tbody></table></div> : <Empty title="尚无代理">先新建代理，再分配客户、设置报价并登记首次收款。</Empty>}
    </Card>
  </div>;
}
