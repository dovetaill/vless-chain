import { useState } from "react";
import { TableProperties } from "lucide-react";
import { Button } from "./ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardHeading,
  CardTitle,
  CardToolbar,
} from "./ui/card";
import { bytes } from "./shared";
import { recentDays } from "../lib/business";

export function TrafficChart({
  days,
  now,
}: {
  days: Record<string, [number, number]>;
  now: number;
}) {
  const [showTable, setShowTable] = useState(false);
  const [hovered, setHovered] = useState<number | null>(null);
  const rows = recentDays(now, days);
  const largest = Math.max(1, ...rows.map((row) => row.upload + row.download));
  const total = rows.reduce((sum, row) => sum + row.upload + row.download, 0);
  const activeRow = hovered != null ? rows[hovered] : rows.at(-1);

  return (
    <Card className="min-w-0">
      <CardHeader className="gap-2">
        <CardHeading>
          <CardTitle>近 30 天代理流量</CardTitle>
          <p className="text-xs text-muted-foreground">北京时间 · 入口上传与下载合计</p>
        </CardHeading>
        <CardToolbar>
          <strong className="text-base font-semibold tabular-nums text-foreground">
            {bytes(total)}
          </strong>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => setShowTable((v) => !v)}
          >
            <TableProperties className="size-3.5" />
            {showTable ? "收起明细" : "每日明细"}
          </Button>
        </CardToolbar>
      </CardHeader>
      <CardContent className="space-y-3">
        <div
          className="relative flex h-40 items-end gap-1 border-b border-border pt-4"
          role="img"
          aria-label={`近 30 天代理流量合计 ${bytes(total)}`}
          onMouseLeave={() => setHovered(null)}
        >
          <div className="pointer-events-none absolute inset-x-0 top-1/2 border-t border-dashed border-border/70" />
          {rows.map((row, index) => {
            const sum = row.upload + row.download;
            const heightPct = sum > 0 ? Math.max(3, (sum / largest) * 100) : 1;
            return (
              <div
                key={row.date}
                onMouseEnter={() => setHovered(index)}
                className="group flex h-full min-w-0 flex-1 cursor-pointer items-end"
                title={`${row.date} · 上传 ${bytes(row.upload)} · 下载 ${bytes(row.download)}`}
              >
                <div
                  className="flex w-full flex-col-reverse overflow-hidden rounded-t-[3px] bg-muted/60 transition-opacity group-hover:opacity-85"
                  style={{ height: `${heightPct}%` }}
                >
                  {sum > 0 && (
                    <>
                      <div className="bg-blue-500" style={{ flex: row.upload }} />
                      <div className="bg-emerald-500" style={{ flex: row.download }} />
                    </>
                  )}
                </div>
              </div>
            );
          })}
        </div>

        <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground tabular-nums">
          <span>{rows[0].date}</span>
          {activeRow && (
            <span className="font-medium text-foreground">
              {activeRow.date} · ↑{bytes(activeRow.upload)} · ↓{bytes(activeRow.download)}
            </span>
          )}
          <span>{rows.at(-1)?.date}</span>
        </div>

        <div className="flex items-center gap-4 text-xs text-muted-foreground">
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2 rounded-xs bg-blue-500" />
            上传
          </span>
          <span className="inline-flex items-center gap-1.5">
            <span className="size-2 rounded-xs bg-emerald-500" />
            下载
          </span>
        </div>

        {showTable && (
          <div className="max-h-56 overflow-auto rounded-lg border border-border">
            <table className="data-table">
              <thead>
                <tr>
                  <th>日期</th>
                  <th>上传</th>
                  <th>下载</th>
                  <th>合计</th>
                </tr>
              </thead>
              <tbody>
                {[...rows].reverse().map((row) => (
                  <tr key={row.date}>
                    <td className="tabular-nums">{row.date}</td>
                    <td className="tabular-nums">{bytes(row.upload)}</td>
                    <td className="tabular-nums">{bytes(row.download)}</td>
                    <td className="font-medium tabular-nums">
                      {bytes(row.upload + row.download)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
