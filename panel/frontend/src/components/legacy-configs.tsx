import { useState } from "react";
import { api } from "../api";
import type { PanelState } from "../types";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Modal, errorText } from "./shared";

type Config = { id: string; node_id: string; name: string; enabled: boolean; config: unknown; source_inbounds?: unknown[] };

function Editor({ value, close, saved }: { value: Config; close: () => void; saved: () => Promise<void> }) {
  const [name, setName] = useState(value.name);
  const [enabled, setEnabled] = useState(value.enabled);
  const [config, setConfig] = useState(JSON.stringify(value.config, null, 2));
  return <Modal title="编辑迁移配置" description="保留原服务的全部用户、路由、DNS 和出口。配置包含连接凭据，请勿公开分享。" close={close} submit="校验并保存草稿" action={async () => {
    await api(`/legacy-configs/${value.id}`, "PUT", { ...value, name, enabled, config: JSON.parse(config) });
    await saved(); close();
  }}>
    <label className="grid gap-2">名称<Input value={name} onChange={e => setName(e.target.value)} /></label>
    <label className="flex items-center gap-2"><input type="checkbox" checked={enabled} onChange={e => setEnabled(e.target.checked)} />启用这组代理</label>
    <label className="grid gap-2">完整 Xray 配置<textarea className="min-h-80 w-full rounded-md border p-3 font-mono text-xs" spellCheck={false} value={config} onChange={e => setConfig(e.target.value)} /></label>
    {!!value.source_inbounds?.length && <details><summary>原面板入口配置备份（含未启用入口）</summary><p className="muted">只作为迁移参考保留，不自动启用原来未运行的入口或用户，也不迁移原面板的到期、配额管理。</p><textarea aria-label="原面板入口配置备份" readOnly className="mt-2 min-h-48 w-full rounded-md border p-3 font-mono text-xs" value={JSON.stringify(value.source_inbounds, null, 2)} /></details>}
    <p className="muted">保存后需要下发才会生效。已有 UUID、密码和端口保持配置中的值；不要删除仍需使用的路由或用户。</p>
  </Modal>;
}

export function LegacyConfigs({ state, saved, apply }: { state: PanelState; saved: () => Promise<void>; apply: () => void }) {
  const [editing, setEditing] = useState<Config | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const profiles = state.legacy_configs || [];
  if (!profiles.length) return null;
  return <section aria-label="已迁移的完整代理配置" className="space-y-3 border-b p-5">
    <div className="flex flex-wrap items-center justify-between gap-2"><h3 className="font-semibold">已迁移的代理配置</h3><Button variant="outline" disabled={state.busy || loading} onClick={apply}>下发配置</Button></div>
    <p className="muted">每组配置独立运行，保留原面板的多用户、传输设置及路由。通过编辑配置管理；不与下方单条代理表单混用。</p>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    {profiles.map(p => <div key={p.id} className="flex flex-wrap items-center justify-between gap-2 border-t py-3">
      <div><p>{p.name} · {p.pending_delete ? "待下发删除" : `${p.enabled ? "启用" : "停用"} · ${p.published ? "已下发" : "待下发"}`}</p>{p.pending_delete && <p className="muted">删除尚未下发，运行中的配置仍然保留。</p>}<p className="muted">{state.nodes.find(n => n.id === p.node_id)?.name || "未知节点"} · 端口 {p.ports.slice(0, 12).join("、")}{p.ports.length > 12 ? ` 等 ${p.ports.length} 个` : ""}</p></div>
      {!p.pending_delete && <div className="flex gap-2"><Button variant="outline" disabled={state.busy || loading} onClick={async () => {
        setLoading(true); setError("");
        try { setEditing(await api<Config>(`/legacy-configs/${p.id}`)); }
        catch (e) { setError(errorText(e)); } finally { setLoading(false); }
      }}>查看与编辑</Button><Button variant="ghost" disabled={state.busy || loading} onClick={() => setRemoving(p.id)}>删除</Button></div>}
    </div>)}
    {editing && <Editor value={editing} close={() => setEditing(null)} saved={saved} />}
    {removing && <Modal title="删除整组迁移配置" description="这会删除该组的所有入口、用户和路由草稿。下发后停止这组代理；原服务备份不会删除。" submit="删除草稿" close={() => setRemoving(null)} action={async () => {
      await api(`/legacy-configs/${removing}`, "DELETE"); await saved(); setRemoving(null);
    }}><p>{profiles.find(p => p.id === removing)?.name}</p></Modal>}
  </section>;
}
