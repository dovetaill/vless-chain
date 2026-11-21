import type { Line, PaymentAdjustment, PublicLine, RentalStatus, SyncStatus } from "../types";
import { beijingTime, money, terms } from "../lib/business";
import { Progress } from "./ui/progress";
import { Badge } from "./ui/badge";
import { bytes, Status } from "./shared";

export const rentalStatusNames: Record<RentalStatus, string> = {
  draft: "未应用",
  not_started: "待收款开通",
  disabled: "手动停用",
  expired: "已到期",
  quota_exhausted: "额度用尽",
  offline: "服务器离线",
  active: "运行中",
};

export const syncStatusNames: Record<SyncStatus, string> = {
  unpublished: "尚未应用",
  pending: "等待同步",
  error: "同步异常",
  synced: "已同步",
};

export function adjustmentRights(change: PaymentAdjustment, lines: Line[]) {
  if (!change.disable_line) return "未调整权益";
  const ids = change.affected_line_ids;
  const synced = ids?.length && ids.every(id =>
    lines.some(line => line.id === id && line.enabled === false && line.sync_status === "synced"),
  );
  return synced ? "关联代理当前已同步停用" : "已提交停用请求，当前状态见代理列表";
}

export function RentalStatusBadge({ status }: { status: RentalStatus }) {
  return (
    <Status
      tone={
        status === "active"
          ? "good"
          : status === "expired" || status === "quota_exhausted"
            ? "bad"
            : status === "offline" || status === "not_started"
              ? "warn"
              : "muted"
      }
    >
      {rentalStatusNames[status]}
    </Status>
  );
}

export function SyncLabel({ status }: { status: SyncStatus }) {
  return (
    <span
      className={`text-[11px] font-medium ${
        status === "error"
          ? "text-destructive"
          : status === "pending"
            ? "text-amber-600 dark:text-amber-400"
            : "text-muted-foreground"
      }`}
    >
      {syncStatusNames[status]}
    </span>
  );
}

export function RentalUsage({ line }: { line: Line | PublicLine }) {
  const quota =
    line.limits.monthly_gib == null ? null : line.limits.monthly_gib * 1024 ** 3;
  const used = line.traffic
    ? (line.traffic.charged ?? line.traffic.upload + line.traffic.download)
    : null;
  const unlimited = line.customer_plan_id ? "共享套餐额度" : "不限量";
  const pct =
    quota != null && quota > 0 && used != null
      ? Math.min(100, (used / quota) * 100)
      : 0;
  const detailTitle = `实际 ↑${bytes(line.traffic?.upload || 0)} · ↓${bytes(line.traffic?.download || 0)} · 倍率 ×${line.multiplier || 1}`;

  return (
    <div className="min-w-[136px] max-w-[180px] space-y-1.5 text-xs tabular-nums" title={detailTitle}>
      <div className="flex items-center justify-between gap-2">
        <span className="font-medium text-foreground">
          {used == null ? "待采集" : bytes(used)}
        </span>
        <span className="text-[11px] text-muted-foreground">
          / {quota == null ? unlimited : bytes(quota)}
        </span>
      </div>
      {quota != null && used != null && (
        <Progress
          value={pct}
          indicatorClassName={
            pct >= 90 ? "bg-destructive" : pct >= 75 ? "bg-amber-500" : "bg-primary"
          }
          aria-label={`已用 ${bytes(used)}，额度 ${bytes(quota)}`}
        />
      )}
      <div className="flex items-center justify-between gap-1.5 text-[11px] text-muted-foreground">
        <span>
          {quota == null
            ? `↑${bytes(line.traffic?.upload || 0)} ↓${bytes(line.traffic?.download || 0)}`
            : used == null
              ? "等待上报"
              : `余 ${bytes(Math.max(0, quota - used))}`}
        </span>
        {(line.multiplier || 1) !== 1 && (
          <Badge variant="secondary" appearance="light" size="xs">
            ×{line.multiplier}
          </Badge>
        )}
      </div>
    </div>
  );
}

export function RentalSpeed({
  line,
  now,
}: {
  line: Line | PublicLine;
  now: number;
}) {
  const fresh =
    line.traffic &&
    now - line.traffic.sampled_at <= 30 &&
    !["offline", "draft"].includes(
      "status" in line ? line.status : line.rental_status,
    );
  return (
    <div className="space-y-0.5 whitespace-nowrap text-xs tabular-nums">
      <p className="font-medium text-foreground">
        ↓ {fresh ? `${bytes(line.traffic!.download_bps)}/s` : "—"}
      </p>
      <p className="text-[11px] text-muted-foreground">
        ↑ {fresh ? `${bytes(line.traffic!.upload_bps)}/s` : "—"}
        {fresh ? ` · ${line.traffic!.connections} 连接` : ""}
      </p>
    </div>
  );
}

export function RentalPrices({ line }: { line: Line | PublicLine }) {
  const configured = terms.filter((term) => line.rental?.prices[term.id] != null);
  if (!configured.length) {
    return <span className="text-xs text-muted-foreground">未设报价</span>;
  }
  return (
    <div className="space-y-0.5 whitespace-nowrap text-xs tabular-nums">
      {configured.map((term) => (
        <p key={term.id} className="text-foreground">
          <span className="text-muted-foreground">{term.name}</span>{" "}
          {money(line.rental?.prices[term.id])}
        </p>
      ))}
    </div>
  );
}

export function RentalDates({ line }: { line: Line | PublicLine }) {
  return (
    <div className="space-y-0.5 text-xs tabular-nums">
      <p className="font-medium text-foreground">
        {!line.rental || line.rental.permanent
          ? "无限期"
          : line.rental.expires_at
            ? beijingTime(line.rental.expires_at)
            : "尚未开通"}
      </p>
      {line.rental?.next_reset_at ? (
        <p className="text-[11px] text-muted-foreground">
          重置 {beijingTime(line.rental.next_reset_at)}
        </p>
      ) : null}
    </div>
  );
}
