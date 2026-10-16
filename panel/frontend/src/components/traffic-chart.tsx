import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { bytes } from "./shared";
import { recentDays } from "../lib/business";

export function TrafficChart({ days, now }: { days: Record<string, [number, number]>; now: number }) {
  const rows = recentDays(now, days);
  const largest = Math.max(1, ...rows.map(row => row.upload + row.download));
  const total = rows.reduce((sum, row) => sum + row.upload + row.download, 0);
  return <Card className="min-w-0">
    <CardHeader className="gap-3 px-4 py-4">
      <div><CardTitle className="leading-5">近 30 天代理流量</CardTitle><p className="muted mt-1">北京时间 · 入口上传＋下载</p></div>
      <strong className="text-lg tabular-nums">{bytes(total)}</strong>
    </CardHeader>
    <CardContent className="p-4">
      <div className="flex h-32 items-end gap-1 border-b border-border" role="img" aria-label={`近 30 天代理流量合计 ${bytes(total)}，逐日数据见下方表格。`}>
        {rows.map(row => <div key={row.date} className="flex h-full min-w-0 flex-1 items-end" title={`${row.date} · 上传 ${bytes(row.upload)} · 下载 ${bytes(row.download)}`}>
          <div className="flex w-full flex-col-reverse overflow-hidden rounded-t-sm bg-slate-100" style={{ height: `${Math.max(1, (row.upload + row.download) / largest * 100)}%` }}>
            <div className="bg-blue-500" style={{ flex: row.upload }} />
            <div className="bg-emerald-400" style={{ flex: row.download }} />
          </div>
        </div>)}
      </div>
      <div className="mt-2 flex justify-between text-xs text-slate-400"><span>{rows[0].date}</span><span>{rows.at(-1)?.date}</span></div>
      <div className="mt-3 flex gap-4 text-xs text-slate-500"><span><span className="mr-2 inline-block size-2 rounded-sm bg-blue-500" />上传</span><span><span className="mr-2 inline-block size-2 rounded-sm bg-emerald-400" />下载</span></div>
      <details className="mt-3 text-sm"><summary className="cursor-pointer text-slate-500">查看逐日数据</summary>
        <div className="mt-3 max-h-64 overflow-auto"><table className="data-table"><thead><tr><th>日期（北京时间）</th><th>上传</th><th>下载</th><th>合计</th></tr></thead>
          <tbody>{[...rows].reverse().map(row => <tr key={row.date}><td>{row.date}</td><td>{bytes(row.upload)}</td><td>{bytes(row.download)}</td><td>{bytes(row.upload + row.download)}</td></tr>)}</tbody></table></div>
      </details>
    </CardContent>
  </Card>;
}
