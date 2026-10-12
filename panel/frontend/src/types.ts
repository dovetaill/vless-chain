export interface NodeInfo {
  id: string; name: string; host: string; target: string; server_name: string;
  seen: number; online: boolean;
  report: { running: boolean; applied: boolean; error: string; revision: string };
}
export interface External { id: string; name: string; url: string }
export interface Hop { node_id: string; port: number; uuid?: string }
export interface Line {
  id: string; name: string; kind: 'vless' | 'forward'; hops: Hop[];
  enabled: boolean; published: boolean; external_id: string | null;
  target: string; network: string; share: string | null;
}
export interface Check { ok: boolean; latency_ms: number; detail: string; checked_at: number }
export interface Job { id: string; state: string; started: number; events: { at: number; message: string }[] }
export interface PanelState {
  nodes: NodeInfo[]; externals: External[]; lines: Line[]; checks: Record<string, Check>;
  jobs: Job[]; busy: boolean;
}
export interface Preview {
  message: string; affected_lines: string[];
  nodes: { id: string; name: string; online: boolean; ports: number[] }[];
}
