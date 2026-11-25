import type { Line, PanelState, Term } from "../types";

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

export const terms: { id: Term; name: string; months: number }[] = [
  { id: "monthly", name: "月付", months: 1 },
  { id: "quarterly", name: "季付", months: 3 },
  { id: "yearly", name: "年付", months: 12 },
];

export const endpoint = (host: string, port: number | string) =>
  `${host.includes(":") && !host.startsWith("[") ? `[${host}]` : host}:${port}`;

export function lineEndpoint({ access, hops }: Line, nodes: PanelState["nodes"]) {
  const entry = hops[0];
  const tls = access.tls;
  const host = access.host ||
    (tls?.mode === "auto" || tls?.mode === "manual"
      ? tls.domain
      : nodes?.find(node => node.id === entry?.node_id)?.host);
  const port = entry?.port;
  if (!port) return host || "—";
  return host ? endpoint(host, port) : `端口 ${port}`;
}

export function parseExternalSummary(raw: string): { protocol: string; target: string } {
  const link = raw.trim();
  const scheme = /^(vmess|mihomo|ssr?|vless|trojan|socks5?|https?|hy2|hysteria2|tuic):\/\//i.exec(link)?.[1].toLowerCase();
  const unknown = "地址未知";
  let protocol = scheme?.toUpperCase() || "NODE";
  const address = (host: unknown, port: unknown) => {
    if (typeof host !== "string" || !/^(?:[\w.-]+|\[?[\da-f:.]+\]?)$/i.test(host)) return "";
    if (!/^\d{1,5}$/.test(String(port)) || Number(port) < 1 || Number(port) > 65535) return "";
    return endpoint(host.replace(/^\[|\]$/g, ""), Number(port));
  };
  const decode = (payload: string) => {
    const clean = payload.split(/[?#]/, 1)[0].replace(/\/$/, "").replace(/-/g, "+").replace(/_/g, "/");
    const padded = clean + "=".repeat((4 - (clean.length % 4)) % 4);
    return new TextDecoder().decode(Uint8Array.from(atob(padded), c => c.charCodeAt(0)));
  };
  const nativeProtocol = (type: unknown) =>
    typeof type === "string" && /^(vless|vmess|trojan|ss|ssr|snell|wireguard|shadowquic|http|socks5|hysteria2?|tuic|anytls)$/.test(type)
      ? type.toUpperCase() : "MIHOMO";
  try {
    const payload = link.slice(link.indexOf("://") + 3);
    if (scheme === "vmess" || scheme === "mihomo") {
      const node = JSON.parse(decode(payload));
      if (scheme === "vmess") return { protocol, target: address(node.add, node.port) || unknown };
      protocol = nativeProtocol(node.type);
      const servers: { server?: unknown; port?: unknown }[] =
        Array.isArray(node.peers) && node.peers.length ? node.peers : [node];
      const target = servers.map(peer => address(peer?.server, peer?.port)).filter(Boolean).join("、");
      return { protocol, target: target || unknown };
    }
    if (scheme === "ssr") {
      const decoded = decode(payload);
      const lastColon = decoded.lastIndexOf(":");
      const portSep = lastColon > 0 ? decoded.lastIndexOf(":", lastColon - 1) : -1;
      const parts = decoded.split(":");
      if (parts.length >= 6) {
        const host = parts.slice(0, parts.length - 5).join(":");
        const port = parts[parts.length - 5];
        return { protocol, target: address(host, port) || unknown };
      }
      if (portSep > 0) return { protocol, target: unknown };
      return { protocol, target: unknown };
    }
    if (scheme) {
      const mainPart = payload.split(/[?#]/, 1)[0].replace(/\/$/, "");
      const legacySS = scheme === "ss" && !mainPart.includes("@");
      const url = new URL(legacySS ? `ss://${decode(mainPart)}` : link);
      const port = url.port || (scheme === "http" ? 80 : /^(https|hy2|hysteria2|tuic)$/.test(scheme) ? 443 : "");
      return { protocol, target: address(url.hostname, port) || unknown };
    }
    if (/^[-{]/.test(link)) {
      protocol = nativeProtocol(link.match(/\btype["']?\s*:\s*["']?([\w-]+)/i)?.[1]);
      const host = link.match(/\bserver["']?\s*:\s*["']?([^"',\s}]+)/i)?.[1];
      const port = link.match(/\bport["']?\s*:\s*["']?(\d+)/i)?.[1];
      return { protocol, target: address(host, port) || unknown };
    }
  } catch {
    return { protocol, target: unknown };
  }
  return { protocol, target: unknown };
}

export function matchesLine(
  line: Line,
  state: Pick<PanelState, "customers" | "nodes">,
  search: string,
  extra = "",
) {
  const customer = state.customers?.find(customer => customer.id === line.rental?.customer_id);
  const nodes = line.hops.map(hop => {
    const node = state.nodes?.find(node => node.id === hop.node_id);
    return `${node?.name || ""} ${node?.host || ""} ${hop.port}`;
  });
  const fields = [extra, line.name, customer?.name, ...nodes, lineEndpoint(line, state.nodes), line.access.host,
    line.access.tls?.domain].join(" ").toLowerCase();
  return search.toLowerCase().trim().split(/\s+/).every(word => fields.includes(word));
}

export const expiringSoon = (line: Line, now: number) => Boolean(line.rental?.expires_at
  && line.rental.expires_at > now && line.rental.expires_at <= now + 7 * 86400);

export const lowQuota = (line: Line) => Boolean(line.traffic && line.limits.monthly_gib
  && (line.traffic.charged ?? line.traffic.upload + line.traffic.download) >= line.limits.monthly_gib * 1024 ** 3 * 0.9);
