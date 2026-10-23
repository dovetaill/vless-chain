export const beijingTime = (timestamp: number | null | undefined) => timestamp
  ? new Date(timestamp * 1000).toLocaleString("zh-CN", { timeZone: "Asia/Shanghai", hour12: false })
  : "尚未开通";

export const money = (cents: number | null | undefined) => cents == null
  ? "未开放"
  : new Intl.NumberFormat("zh-CN", { style: "currency", currency: "CNY" }).format(cents / 100);

export function parseMoney(value: string): number | null {
  const text = value.trim();
  if (!text) return null;
  if (!/^\d+(?:\.\d{1,2})?$/.test(text)) throw new Error("金额请填写非负数字，最多两位小数。");
  const [whole, fraction = ""] = text.split(".");
  const cents = Number(whole) * 100 + Number(fraction.padEnd(2, "0"));
  if (!Number.isSafeInteger(cents)) throw new Error("金额超出允许范围。");
  return cents;
}

export function recentDays(now: number, values: Record<string, [number, number]>) {
  const today = new Date((now + 8 * 3600) * 1000).toISOString().slice(0, 10);
  const midnight = Date.parse(`${today}T00:00:00Z`);
  return Array.from({ length: 30 }, (_, i) => {
    const date = new Date(midnight - (29 - i) * 86400000).toISOString().slice(0, 10);
    const [upload, download] = values[date] || [0, 0];
    return { date, upload, download };
  });
}
import type { Line, PanelState, Term } from "../types";

export const terms: { id: Term; name: string; months: number }[] = [
  { id: "monthly", name: "月付", months: 1 },
  { id: "quarterly", name: "季付", months: 3 },
  { id: "yearly", name: "年付", months: 12 },
];


export function matchesLine(line: Line, state: PanelState, search: string) {
  const customer = state.customers.find(customer => customer.id === line.rental?.customer_id);
  const nodes = line.hops.map(hop => {
    const node = state.nodes.find(node => node.id === hop.node_id);
    return `${node?.name || ""} ${node?.host || ""} ${hop.port}`;
  });
  const fields = [line.name, customer?.name, ...nodes, line.access.host, line.access.public_host,
    line.access.public_port, line.access.tls?.domain].join(" ").toLowerCase();
  return search.toLowerCase().trim().split(/\s+/).every(word => fields.includes(word));
}

export const expiringSoon = (line: Line, now: number) => Boolean(line.rental?.expires_at
  && line.rental.expires_at > now && line.rental.expires_at <= now + 7 * 86400);

export const lowQuota = (line: Line) => Boolean(line.traffic && line.limits.monthly_gib
  && (line.traffic.charged ?? line.traffic.upload + line.traffic.download) >= line.limits.monthly_gib * 1024 ** 3 * 0.9);
