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
  cycle_start: number;
  cycle_end: number;
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
      cpu_percent?: number | null;
      disk_total?: number;
      disk_used?: number;
      network_rx_bps?: number | null;
      network_tx_bps?: number | null;
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
export type Protocol = "vless" | "http" | "socks" | "shadowsocks" | "trojan" | "vmess" | "hysteria2" | "tuic";
export interface TLS {
  mode: string;
  domain: string;
  cert: string;
  key: string;
}
export interface Access {
  transport: { network: "tcp" | "xhttp"; path: string; host: string; mode: string };
  reality: { target: string; server_name: string } | null;
  shared_port: boolean;
  fallback: string;
  host: string;
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
export type Term = "monthly" | "quarterly" | "yearly";
export type Prices = Record<Term, number | null>;
export type RentalStatus = "draft" | "not_started" | "disabled" | "expired" | "quota_exhausted" | "offline" | "active";
export type SyncStatus = "unpublished" | "pending" | "error" | "synced";
export interface Customer {
  id: string;
  name: string;
  contact: string;
  notes: string;
}
export interface Rental {
  customer_id: string;
  prices: Prices;
  started_at: number;
  expires_at: number;
  paid_months: number;
  next_reset_at: number;
  query_link_exists: boolean;
  permanent: boolean;
}
export interface Payment {
  id: string;
  request_id: string;
  line_id: string;
  customer_id: string;
  line_name: string;
  customer_name: string;
  term: Term;
  months: number;
  amount_fen: number;
  created: number;
  previous_expires_at: number;
  expires_at: number;
  note: string;
}
export interface Line {
  id: string;
  name: string;
  kind: "vless" | "forward" | "website";
  hops: Hop[];
  enabled: boolean;
  published: boolean;
  external_id: string | null;
  chain: string[];
  target: string;
  network: string;
  share: string | null;
  access: Access;
  limits: Limits;
  traffic: Traffic | null;
  rental: Rental | null;
  rental_status: RentalStatus;
  sync_status: SyncStatus;
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
  local_node_id: string | null;
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
  customers: Customer[];
  payments: Payment[];
  renewal_contact: string;
}
export interface PublicLine {
  id: string;
  name: string;
  status: RentalStatus;
  sync_status: SyncStatus;
  traffic: Traffic | null;
  limits: Limits;
  rental: Pick<Rental, "prices" | "started_at" | "expires_at" | "next_reset_at" | "permanent">;
}
export interface PublicLookup {
  lines: PublicLine[];
  unmatched: string[];
  renewal_contact: string;
  time: number;
}
export interface Preview {
  message: string;
  affected_lines: string[];
  affected_line_ids: string[];
  nodes: { id: string; name: string; online: boolean; ports: number[] }[];
}
