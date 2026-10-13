export interface Traffic {
  epoch: string;
  month: string;
  upload: number;
  download: number;
  total_upload: number;
  total_download: number;
  upload_bps: number;
  download_bps: number;
  connections: number;
  sampled_at: number;
  blocked: boolean;
  days: Record<string, [number, number]>;
}
export interface NodeInfo {
  id: string;
  name: string;
  host: string;
  target: string;
  server_name: string;
  seen: number;
  online: boolean;
  report: {
    running: boolean;
    applied: boolean;
    error: string;
    revision: string;
    traffic: Record<string, Traffic>;
    system: {
      memory_total?: number;
      memory_used?: number;
      load_1m?: number;
      network_rx?: number;
      network_tx?: number;
      sampled_at?: number;
    };
  };
}
export interface External {
  id: string;
  name: string;
  url: string;
}
export interface Hop {
  node_id: string;
  port: number;
  uuid?: string;
}
export type Protocol = "vless" | "http" | "socks" | "shadowsocks" | "trojan";
export interface TLS {
  mode: string;
  domain: string;
  cert: string;
  key: string;
}
export interface Access {
  protocol: Protocol;
  username: string;
  password: string;
  method: string;
  public_host: string;
  public_port: number | null;
  tls: TLS;
}
export interface Limits {
  upload_mbps: number | null;
  download_mbps: number | null;
  monthly_gib: number | null;
}
export interface Line {
  id: string;
  name: string;
  kind: "vless" | "forward" | "website";
  hops: Hop[];
  enabled: boolean;
  published: boolean;
  external_id: string | null;
  target: string;
  network: string;
  share: string | null;
  access: Access;
  limits: Limits;
  traffic: Traffic | null;
}
export interface Check {
  ok: boolean;
  latency_ms?: number;
  detail: string;
  checked_at: number;
}
export interface Job {
  id: string;
  state: string;
  started: number;
  events: { at: number; message: string }[];
}
export interface Security {
  totp_enabled: boolean;
  recovery_remaining: number;
  origin: string;
  passkeys: { id: string; name: string; created: number }[];
}
export interface PanelState {
  nodes: NodeInfo[];
  externals: External[];
  lines: Line[];
  checks: Record<string, Check>;
  jobs: Job[];
  busy: boolean;
  time: number;
  subscriptions: {
    id: string;
    name: string;
    line_ids: string[];
    created: number;
  }[];
  security: Security;
  alerts: { id: string; level: string; message: string }[];
}
export interface Preview {
  message: string;
  affected_lines: string[];
  affected_line_ids: string[];
  nodes: { id: string; name: string; online: boolean; ports: number[] }[];
}
