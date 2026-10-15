import type { Line, PublicLine, RentalStatus, SyncStatus } from "../types";
import { beijingTime, money, terms } from "../lib/business";
import { bytes, Status } from "./shared";

export const rentalStatusNames: Record<RentalStatus, string> = {
  draft: "未应用", not_started: "待收款开通", disabled: "手动停用", expired: "已到期",
  quota_exhausted: "额度用尽", offline: "服务器离线", active: "运行中",
};
export const syncStatusNames: Record<SyncStatus, string> = {
  unpublished: "尚未应用", pending: "等待同步", error: "同步异常", synced: "已同步",
};
export function RentalStatusBadge({ status }: { status: RentalStatus }) {
  return <Status tone={status === "active" ? "good" : status === "expired" || status === "quota_exhausted" ? "bad" : status === "offline" || status === "not_started" ? "warn" : "muted"}>{rentalStatusNames[status]}</Status>;
}
export function SyncLabel({ status }: { status: SyncStatus }) {
  return <span className={`text-xs ${status === "error" ? "text-red-600" : status === "pending" ? "text-amber-600" : "text-slate-400"}`}>{syncStatusNames[status]}</span>;
}
export function RentalUsage({ line }: { line: Line | PublicLine }) {
  const quota = line.limits.monthly_gib == null ? null : line.limits.monthly_gib * 1024 ** 3;
  const used = line.traffic ? line.traffic.upload + line.traffic.download : null;
  return <div className="space-y-1.5 text-xs tabular-nums">
    <p>已用 {used == null ? "尚未采集" : bytes(used)}</p>
    <p className="text-slate-500">剩余 {quota == null ? "不限量" : used == null ? "尚未采集" : bytes(Math.max(0, quota - used))}</p>
    {quota != null && used != null && <progress className="h-1.5 w-full max-w-40 accent-blue-500" value={Math.min(used, quota)} max={quota} aria-label={`已用 ${bytes(used)}，额度 ${bytes(quota)}`} />}
    <p className="text-slate-400">周期额度 {quota == null ? "不限量" : bytes(quota)}</p>
  </div>;
}
export function RentalSpeed({ line, now }: { line: Line | PublicLine; now: number }) {
  const fresh = line.traffic && now - line.traffic.sampled_at <= 30 && !["offline", "draft"].includes("status" in line ? line.status : line.rental_status);
  return <div className="space-y-1 text-xs tabular-nums">
    <p>↑ {fresh ? `${bytes(line.traffic!.upload_bps)}/s` : "—"}</p>
    <p>↓ {fresh ? `${bytes(line.traffic!.download_bps)}/s` : "—"}</p>
    <p className="text-slate-400">{fresh ? `${line.traffic!.connections} 个连接` : "暂无实时采样"}</p>
  </div>;
}
export function RentalPrices({ line }: { line: Line | PublicLine }) {
  return <div className="space-y-1 text-xs tabular-nums">{terms.map(term => <p key={term.id} className={line.rental?.prices[term.id] == null ? "text-slate-400" : "text-slate-600"}>{term.name} {money(line.rental?.prices[term.id])}</p>)}</div>;
}
export function RentalDates({ line }: { line: Line | PublicLine }) {
  return <div className="space-y-1.5 text-xs"><p>{!line.rental || line.rental.permanent ? "无限期" : line.rental.expires_at ? beijingTime(line.rental.expires_at) : "尚未开通"}</p>
    {line.rental?.next_reset_at ? <p className="text-slate-400">重置 {beijingTime(line.rental.next_reset_at)}</p> : null}</div>;
}
