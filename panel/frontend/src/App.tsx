import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from 'react';
import { Activity, ArrowDown, ArrowLeftRight, ArrowRight, ArrowUp, Check, ChevronRight, Copy, Globe2, Layers3, LogOut, Menu, Network, Plus, Radio, Search, Server, ShieldCheck, Trash2, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Input } from '@/components/ui/input';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { api, ApiError } from './api';
import { AgentSetup } from './components/agent-setup';
import type { PanelState, NodeInfo, Line, Preview, Check as CheckResult } from './types';

const pages = [
  { id: 'lines', label: '代理线路', icon: Network, description: '从入口到出口，管理每条线路的完整路径。' },
  { id: 'forward', label: '端口转发', icon: ArrowLeftRight, description: '通过 TCP / UDP，将入口流量送达指定目标。' },
  { id: 'nodes', label: '服务器', icon: Server, description: '接入自己的服务器，或导入外部 VLESS 落地。' },
  { id: 'diagnostics', label: '诊断与发布', icon: Activity, description: '查看整链探测结果与每一次配置发布。' },
] as const;
type Page = typeof pages[number]['id'];
const pageFromHash = (): Page => pages.find(p => p.id === location.hash.slice(1))?.id || 'lines';
const stamp = (time: number) => time ? new Date(time * 1000).toLocaleString('zh-CN', { hour12: false }) : '尚未连接';
const jobNames: Record<string, string> = { publishing: '正在发布', rolling_back: '正在恢复', applied: '配置已应用', rolled_back: '已恢复旧配置', degraded: '部分节点尚未确认恢复' };
const errorText = (error: unknown) => error instanceof Error ? error.message : '操作失败，请重试';
const field = (data: FormData, key: string) => String(data.get(key) || '');

function Status({ children, tone = 'muted' }: { children: ReactNode; tone?: 'good' | 'bad' | 'warn' | 'muted' }) {
  return <Badge variant={({ good: 'success', bad: 'destructive', warn: 'warning', muted: 'secondary' } as const)[tone]} appearance="light" size="sm">{children}</Badge>;
}
function Field({ label, children }: { label: string; children: ReactNode }) {
  return <label className="field">{label}{children}</label>;
}
function Modal({ title, description, children, submit, action, close }: {
  title: string; description: string; children: ReactNode; submit?: string;
  action?: (data: FormData) => Promise<void>; close: () => void;
}) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!action || pending) return;
    const data = new FormData(event.currentTarget);
    setPending(true); setError('');
    try { await action(data); } catch (e) { setError(errorText(e)); } finally { setPending(false); }
  }
  return <Dialog open onOpenChange={open => { if (!open && !pending) close(); }}>
    <DialogContent tabIndex={-1} onOpenAutoFocus={event => { event.preventDefault(); (event.target as HTMLElement).focus({ preventScroll: true }); }} className="max-h-[90dvh] w-[calc(100%-2rem)] max-w-2xl overflow-y-auto rounded-xl p-6 sm:p-7">
      <DialogHeader><DialogTitle className="pe-6">{title}</DialogTitle><DialogDescription className="pt-2 leading-relaxed">{description}</DialogDescription></DialogHeader>
      <form onSubmit={save} className="space-y-5">
        {children}
        {error && <p role="alert" className="rounded-md bg-red-50 p-3 text-sm text-red-700">{error}</p>}
        <DialogFooter className="gap-2 border-t border-border">
          <Button type="button" variant="outline" disabled={pending} onClick={close}>关闭</Button>
          {submit && <Button type="submit" disabled={pending}>{pending ? '正在处理…' : submit}</Button>}
        </DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}
function PathView({ line, state }: { line: Line; state: PanelState }) {
  const names = line.hops.map(h => state.nodes.find(n => n.id === h.node_id)?.name || '已移除节点');
  names.push(line.external_id ? state.externals.find(e => e.id === line.external_id)?.name || '外部落地' : line.kind === 'forward' ? line.target : 'Internet');
  return <div className="flex min-w-44 flex-wrap items-center gap-1.5">{names.map((name, i) => <span key={i} className="inline-flex items-center gap-1.5">
    {i > 0 && <ChevronRight className="size-3 text-slate-400" />}
    <span title={i < line.hops.length ? `端口 ${line.hops[i].port}` : undefined} className={`rounded border px-2 py-1 text-xs ${i === 0 ? 'border-blue-100 bg-blue-50 text-blue-700' : 'border-border bg-white text-slate-600'}`}>{name}</span>
  </span>)}</div>;
}
function LineStatus({ line, state }: { line: Line; state: PanelState }) {
  if (!line.published) return <Status tone="warn">待发布{!line.enabled && ' · 停用'}</Status>;
  if (!line.enabled) return <Status>已停用</Status>;
  const check = state.checks[line.id];
  if (check) return <Status tone={check.ok ? 'good' : 'bad'}>{check.ok ? '探测通过' : '探测未通过'}</Status>;
  return <Status>已应用 · 待探测</Status>;
}
function Empty({ title, children, action }: { title: string; children: ReactNode; action?: ReactNode }) {
  return <div className="flex flex-col items-center px-6 py-16 text-center">
    <span className="mb-5 flex size-14 items-center justify-center rounded-2xl border border-blue-100 bg-blue-50 text-blue-600"><Layers3 className="size-6" /></span>
    <h3 className="text-base font-semibold">{title}</h3><p className="muted mt-2 max-w-md">{children}</p><div className="mt-6">{action}</div>
  </div>;
}

function LineEditor({ line, kind, state, close, saved }: {
  line?: Line; kind: Line['kind']; state: PanelState; close: () => void; saved: () => Promise<void>;
}) {
  const [hops, setHops] = useState(line?.hops.map(h => ({ node_id: h.node_id, port: String(h.port) })) || [{ node_id: state.nodes[0].id, port: '' }]);
  const update = (index: number, key: 'node_id' | 'port', value: string) => setHops(old => old.map((h, i) => i === index ? { ...h, [key]: value } : h));
  function move(i: number, delta: number) { setHops(old => { const next = [...old]; [next[i], next[i + delta]] = [next[i + delta], next[i]]; return next; }); }
  const available = state.nodes.find(n => !hops.some(h => h.node_id === n.id));
  return <Modal title={line ? '编辑线路' : kind === 'vless' ? '新建代理线路' : '新建端口转发'} description="从上到下依次经过入口、中转与出口。每条线路可以有独立的路径。" submit="保存草稿" close={close} action={async data => {
    await api('/lines', 'POST', {
      id: line?.id, kind, name: field(data, 'name'), enabled: data.has('enabled'),
      external_id: field(data, 'external_id') || null, target: field(data, 'target'), network: field(data, 'network') || 'tcp',
      hops: hops.map(h => ({ node_id: h.node_id, port: h.port ? Number(h.port) : null })),
    }); await saved(); close();
  }}>
    <Field label="线路名称"><Input name="name" defaultValue={line?.name} placeholder="例如：香港 → 日本" maxLength={80} required /></Field>
    <div className="space-y-3"><p className="text-sm font-medium">路径节点 <span className="muted">/ 最多 8 跳</span></p>
      {hops.map((hop, i) => <div key={i} className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-slate-50/60 p-3">
        <span className="mono flex size-7 shrink-0 items-center justify-center rounded-full bg-white text-xs text-blue-600">{i + 1}</span>
        <select className="select min-w-28 flex-1" aria-label={`第 ${i + 1} 跳服务器`} value={hop.node_id} onChange={e => update(i, 'node_id', e.target.value)}>
          {state.nodes.map(n => <option key={n.id} value={n.id} disabled={hops.some((h, j) => j !== i && h.node_id === n.id)}>{n.name}{i === 0 ? ' · 入口' : ''}</option>)}
        </select>
        <Input className="w-28" type="number" min={1} max={65535} aria-label={`第 ${i + 1} 跳端口`} placeholder="自动端口" value={hop.port} onChange={e => update(i, 'port', e.target.value)} />
        <div className="flex gap-1">
          <Button type="button" variant="ghost" size="icon" aria-label={`上移第 ${i + 1} 跳`} disabled={i === 0} onClick={() => move(i, -1)}><ArrowUp /></Button>
          <Button type="button" variant="ghost" size="icon" aria-label={`下移第 ${i + 1} 跳`} disabled={i === hops.length - 1} onClick={() => move(i, 1)}><ArrowDown /></Button>
          <Button type="button" variant="ghost" size="icon" aria-label={`移除第 ${i + 1} 跳`} disabled={hops.length === 1} onClick={() => setHops(old => old.filter((_, j) => j !== i))}><X /></Button>
        </div>
      </div>)}
      <Button type="button" variant="dashed" className="w-full" disabled={!available || hops.length >= 8} onClick={() => available && setHops([...hops, { node_id: available.id, port: '' }])}><Plus />添加下一跳</Button>
    </div>
    <Field label="最后一跳如何出网"><select name="external_id" className="select" defaultValue={line?.external_id || ''}>
      <option value="">由最后一个受管节点直接出网</option>{state.externals.map(e => <option key={e.id} value={e.id}>外部落地 · {e.name}</option>)}
    </select></Field>
    {kind === 'forward' && <div className="grid gap-4 sm:grid-cols-2">
      <Field label="最终转发目标"><Input name="target" defaultValue={line?.target} placeholder="target.example.com:443" required /></Field>
      <Field label="转发网络"><select name="network" className="select" defaultValue={line?.network || 'tcp,udp'}><option value="tcp">TCP</option><option value="udp">UDP</option><option value="tcp,udp">TCP + UDP</option></select></Field>
    </div>}
    <label className="flex items-center gap-2 text-sm"><input name="enabled" type="checkbox" defaultChecked={line?.enabled !== false} />启用这条线路</label>
    <p className="muted">端口留空会自动分配。仅修改中转和出口时，已有入口端口、UUID 保持不变。对应端口需要在服务器防火墙中放行。</p>
  </Modal>;
}
function ProbeEditor({ line, state, close, saved }: { line: Line; state: PanelState; close: () => void; saved: () => Promise<void> }) {
  const [result, setResult] = useState<CheckResult | null>(null);
  const [mode, setMode] = useState('http');
  return <Modal title="检查实际链路" description={line.name} submit="开始检查" close={close} action={async data => {
    const value = await api<CheckResult>(`/lines/${line.id}/probe`, 'POST', { mode, url: field(data, 'url') }); setResult(value); await saved();
  }}>
    <PathView line={line} state={state} />
    <Field label="检查方式"><select className="select" value={mode} onChange={e => { setMode(e.target.value); setResult(null); }}>
      <option value="http">HTTP / HTTPS 请求</option>{line.kind === 'forward' && <><option value="tcp_echo">TCP 回显</option><option value="udp_echo">UDP 回显</option></>}
    </select></Field>
    {mode === 'http' && <Field label="HTTP / HTTPS 探测地址"><Input name="url" defaultValue={line.kind === 'vless' ? 'https://example.com' : ''} placeholder="https://你的实际目标/" required /></Field>}
    <p className="muted">{mode === 'http' ? '请求经过已发布线路的真实入口。端口转发请填写与最终目标匹配的网址；HTTP 成功不代表 UDP 已通过。' : '最终目标必须运行回显服务，原样返回收到的数据。探测会通过实际转发入口。'}</p>
    {result && <div role="status" className={`rounded-lg border p-4 ${result.ok ? 'border-green-200 bg-green-50' : 'border-red-200 bg-red-50'}`}>
      <strong className="text-sm">{result.ok ? `检查通过 · ${result.latency_ms} ms` : '检查未通过'}</strong><p className="mt-2 whitespace-pre-wrap break-all text-xs leading-relaxed">{result.detail}</p>
    </div>}
  </Modal>;
}

export function App() {
  const [state, setState] = useState<PanelState | null>(null);
  const [loading, setLoading] = useState(true);
  const [connected, setConnected] = useState(true);
  const [page, setPage] = useState<Page>(pageFromHash);
  const [mobile, setMobile] = useState(false);
  const [search, setSearch] = useState('');
  const [modal, setModal] = useState<ReactNode>(null);
  const [toast, setToast] = useState('');
  const [loginError, setLoginError] = useState('');
  const [loggingIn, setLoggingIn] = useState(false);
  const close = () => setModal(null);
  const refresh = useCallback(async () => {
    try { setState(await api<PanelState>('/state')); setConnected(true); }
    catch (e) { if (e instanceof ApiError && e.status === 401) setState(null); else setConnected(false); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => {
    void refresh(); let fetching = false;
    const timer = setInterval(async () => { if (fetching) return; fetching = true; await refresh(); fetching = false; }, 3000);
    const expired = () => { setState(null); setModal(null); };
    const changed = () => { setPage(pageFromHash()); setSearch(''); setMobile(false); };
    window.addEventListener('session-expired', expired); window.addEventListener('hashchange', changed);
    return () => { clearInterval(timer); window.removeEventListener('session-expired', expired); window.removeEventListener('hashchange', changed); };
  }, [refresh]);
  useEffect(() => { if (!toast) return; const timer = setTimeout(() => setToast(''), 4500); return () => clearTimeout(timer); }, [toast]);
  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value); setToast('已复制'); }
    catch { setToast('浏览器未允许复制，请在文本框中手动复制'); }
  }
  function tokenModal(id: string, token: string) {
    setModal(<Modal key={token} title="接入服务器" description="在 Web 填写 SSH 信息安装，或使用脚本接入。完成后自动显示在线状态。" close={close}>
      <AgentSetup id={id} token={token} copy={copy} />
    </Modal>);
  }
  function editNode(node?: NodeInfo) {
    setModal(<Modal key={node?.id || 'new-node'} title={node ? '编辑服务器' : '添加服务器'} description="填写其他节点和控制台能够访问的地址。" submit={node ? '保存修改' : '创建服务器'} close={close} action={async data => {
      const result = await api<{ id: string; token: string }>(node ? `/nodes/${node.id}` : '/nodes', 'POST', Object.fromEntries(data));
      await refresh(); if (node) { close(); setToast('节点参数已保存，发布后生效'); } else tokenModal(result.id, result.token);
    }}>
      <Field label="服务器名称"><Input name="name" defaultValue={node?.name} placeholder="例如：香港入口" maxLength={80} required /></Field>
      <Field label="服务器 IP / 域名"><Input name="host" defaultValue={node?.host} placeholder="edge.example.com" required /></Field>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="REALITY 目标"><Input name="target" defaultValue={node?.target} placeholder="你的 TLS 目标:443" required /></Field>
        <Field label="REALITY Server Name"><Input name="server_name" defaultValue={node?.server_name} placeholder="TLS 目标的域名" required /></Field>
      </div>
      <p className="muted">填写该服务器可以访问的 TLS 目标及匹配域名。REALITY 密钥由控制台生成；修改后需重新发布。</p>
    </Modal>);
  }
  function editExternal() {
    setModal(<Modal key="external" title="导入外部落地" description="支持 VLESS + TLS / REALITY，传输方式为 TCP / raw 或 XHTTP。" submit="导入节点" close={close} action={async data => {
      await api('/externals', 'POST', Object.fromEntries(data)); await refresh(); close(); setToast('外部落地已导入');
    }}>
      <Field label="名称"><Input name="name" placeholder="例如：日本落地" maxLength={80} required /></Field>
      <Field label="VLESS 链接"><textarea className="textarea mono" name="url" placeholder="vless://…" required /></Field>
    </Modal>);
  }
  function editLine(line?: Line, kind: Line['kind'] = 'vless') {
    if (!state?.nodes.length) { editNode(); return; }
    setModal(<LineEditor key={line?.id || kind} line={line} kind={line?.kind || kind} state={state} close={close} saved={async () => { await refresh(); setToast('草稿已保存，发布后生效'); }} />);
  }
  function remove(resource: 'nodes' | 'externals' | 'lines', id: string, name: string) {
    setModal(<Modal key={`remove-${id}`} title={`删除「${name}」`} description={resource === 'lines' ? '删除草稿后，发布配置才会停止这条线路。' : '仍被草稿或已发布线路使用的节点无法删除。'} submit="确认删除" close={close} action={async () => {
      await api(`/${resource}/${id}`, 'DELETE'); await refresh(); close(); setToast('已删除');
    }}><p className="text-sm">确认删除此项目？</p></Modal>);
  }
  async function previewPublish() {
    try {
      const plan = await api<Preview>('/preview', 'POST', {});
      setModal(<Modal key="publish" title="预览配置发布" description="确认本次变更涉及的服务器和线路。" submit="发布此版本" close={close} action={async () => {
        await api('/publish', 'POST', {}); close(); location.hash = 'diagnostics'; await refresh(); setToast('发布任务已开始');
      }}>
        <p className="muted">{plan.message}</p>
        <h3 className="text-sm font-semibold">受影响节点</h3>
        {plan.nodes.length ? <div className="space-y-3">{plan.nodes.map(n => <div key={n.id} className="rounded-lg border border-border p-3">
          <div className="flex justify-between gap-3 text-sm"><strong>{n.name}</strong><Status tone={n.online ? 'good' : 'bad'}>{n.online ? '在线' : '离线'}</Status></div>
          <p className="muted mt-2">新配置监听端口：{n.ports.join(', ') || '无（清除已删除线路）'}</p>
        </div>)}</div> : <p className="muted">无需重启节点；将保存当前线路版本。</p>}
        <h3 className="text-sm font-semibold">可能短暂重连的线路</h3><p className="muted">{plan.affected_lines.join('、') || '无'}</p>
        <p className="rounded-lg bg-amber-50 p-3 text-xs leading-relaxed text-amber-800">失败时会尝试恢复旧配置；离线节点无法立即确认恢复，发布记录会保留具体状态。</p>
      </Modal>);
    } catch (e) { setToast(errorText(e)); }
  }
  function share(line: Line) {
    if (!state) return;
    const host = state.nodes.find(n => n.id === line.hops[0].node_id)?.host || '';
    const address = line.share || `${host.includes(':') ? `[${host}]` : host}:${line.hops[0].port}`;
    setModal(<Modal key={`share-${line.id}`} title="线路接入" description={line.name} close={close}>
      <PathView line={line} state={state} /><p className="muted">{line.published ? '当前配置已发布。' : '这是草稿接入信息，请先发布再使用。'}</p>
      <pre className="secret">{address}</pre><Button type="button" variant="outline" onClick={() => void copy(address)}><Copy />复制{line.share ? '链接' : '入口地址'}</Button>
      <p className="muted">{line.share ? '将链接导入支持 VLESS / REALITY 的客户端。' : '使用此规则配置的 TCP / UDP 协议连接入口地址。'}</p>
    </Modal>);
  }
  function probe(line: Line) { if (state) setModal(<ProbeEditor key={line.id} line={line} state={state} close={close} saved={refresh} />); }

  const toastView = toast && <div role="status" className="fixed bottom-6 left-1/2 z-[100] max-w-[90vw] -translate-x-1/2 rounded-lg bg-slate-900 px-5 py-3 text-sm text-white shadow-xl">{toast}</div>;
  if (loading) return <div className="grid min-h-dvh place-items-center text-sm text-slate-500">正在连接控制台…</div>;
  if (!state) return <main className="grid min-h-dvh place-items-center bg-slate-50 p-5">
    <Card className="w-full max-w-md"><CardContent className="p-8 sm:p-10">
      <div className="mb-8 flex items-center gap-3"><span className="flex size-10 items-center justify-center rounded-xl bg-blue-600 text-white"><Network className="size-6" /></span><span className="text-xl font-semibold tracking-tight">Chain</span></div>
      <h1 className="text-2xl font-semibold">登录网络控制台</h1><p className="muted mt-3">管理你的服务器、代理链与转发线路。</p>
      <form className="mt-8 space-y-5" onSubmit={async event => {
        event.preventDefault(); const form = event.currentTarget; setLoggingIn(true); setLoginError('');
        try { await api('/login', 'POST', { token: field(new FormData(form), 'token') }); form.reset(); await refresh(); }
        catch (e) { setLoginError(errorText(e)); } finally { setLoggingIn(false); }
      }}>
        <Field label="管理令牌"><Input type="password" name="token" autoComplete="current-password" placeholder="输入管理令牌" variant="lg" required /></Field>
        {loginError && <p role="alert" className="text-sm text-red-600">{loginError}</p>}
        {!connected && <p className="text-sm text-amber-700">暂时无法连接服务器，正在重试。</p>}
        <Button className="w-full" size="lg" disabled={loggingIn}>{loggingIn ? '正在登录…' : '进入控制台'}<ArrowRight /></Button>
      </form><p className="muted mt-6">在控制台服务器输入 chainctl，选择「查看登录信息」获取管理令牌。</p>
    </CardContent></Card>{toastView}
  </main>;

  const current = pages.find(p => p.id === page)!;
  const query = search.trim().toLocaleLowerCase();
  const matches = (name: string) => name.toLocaleLowerCase().includes(query);
  const kind = page === 'forward' ? 'forward' : 'vless';
  const lines = state.lines.filter(l => l.kind === kind && matches(l.name));
  const publishButton = <Button variant="outline" disabled={state.busy} onClick={() => void previewPublish()}><Layers3 />{state.busy ? '正在发布…' : '预览并发布'}</Button>;
  const nav = <>
    <a href="#lines" className="flex h-[76px] items-center gap-3 border-b border-border px-7"><span className="flex size-9 items-center justify-center rounded-lg bg-blue-600 text-white"><Network className="size-5" /></span><span className="text-xl font-semibold tracking-tight">Chain</span><Badge size="sm" variant="outline" className="ms-auto">个人版</Badge></a>
    <div className="px-5 py-8"><p className="mb-3 px-3 text-[11px] font-medium tracking-widest text-slate-400">网络管理</p>
      <nav className="space-y-1.5" aria-label="主导航">{pages.map(p => <a key={p.id} href={`#${p.id}`} aria-current={page === p.id ? 'page' : undefined} onClick={() => setMobile(false)} className={`flex items-center gap-3 rounded-lg px-3 py-3 text-sm transition-colors ${page === p.id ? 'bg-blue-50 font-medium text-blue-600' : 'text-slate-500 hover:bg-slate-50 hover:text-slate-800'}`}><p.icon className="size-[18px]" />{p.label}{p.id === 'lines' && <span className={`ms-auto rounded px-1.5 py-0.5 text-[11px] ${page === 'lines' ? 'bg-blue-100/70' : 'bg-slate-100'}`}>{state.lines.filter(l => l.kind === 'vless').length}</span>}</a>)}</nav>
    </div>
    <div className="mx-5 mt-auto mb-6 rounded-xl border border-border bg-slate-50 p-4"><div className="mb-2 flex items-center gap-2 text-xs font-medium"><ShieldCheck className="size-4 text-blue-600" />我的专属网络</div><p className="muted">独立管理每条路径，让服务器各司其职。</p></div>
  </>;
  return <div className="min-h-dvh">
    <aside className="fixed inset-y-0 left-0 z-30 hidden w-64 flex-col border-r border-border bg-white lg:flex">{nav}</aside>
    {mobile && <div className="fixed inset-0 z-40 lg:hidden"><button aria-label="关闭导航遮罩" className="absolute inset-0 bg-slate-900/30" onClick={() => setMobile(false)} /><aside className="relative flex h-full w-72 max-w-[85vw] flex-col bg-white">{nav}<Button variant="ghost" size="icon" aria-label="关闭导航" className="absolute bottom-32 right-5" onClick={() => setMobile(false)}><X /></Button></aside></div>}
    <div className="lg:ms-64">
      <header className="flex h-[76px] items-center justify-between gap-3 border-b border-border bg-white px-5 sm:px-8 xl:px-10">
        <div className="flex items-center gap-3"><Button variant="ghost" size="icon" className="lg:hidden" aria-label="打开导航" onClick={() => setMobile(true)}><Menu /></Button><span className="hidden text-xs text-slate-400 sm:inline">网络控制台</span><ChevronRight className="hidden size-3 text-slate-300 sm:block" /><span className="text-xs font-medium">{current.label}</span></div>
        <div className="flex items-center gap-4"><span className="hidden items-center gap-2 text-xs text-slate-500 sm:flex"><span className={`size-1.5 rounded-full ${connected ? 'bg-emerald-500' : 'bg-amber-500'}`} />{connected ? '控制台已连接' : '连接中断，正在重试'}</span><span className="flex size-8 items-center justify-center rounded-full bg-blue-50 text-xs font-semibold text-blue-600">ME</span><Button variant="ghost" size="icon" aria-label="退出登录" onClick={async () => { try { await api('/logout', 'POST', {}); setState(null); setModal(null); } catch (e) { setToast(errorText(e)); } }}><LogOut /></Button></div>
      </header>
      <main className="mx-auto max-w-[1600px] px-5 py-8 sm:px-8 xl:px-10">
        <div className="mb-7 flex flex-wrap items-center justify-between gap-5"><div><h1 className="text-[25px] font-semibold tracking-tight">{current.label}</h1><p className="mt-2 text-sm text-slate-500">{current.description}</p></div><div className="flex flex-wrap gap-2">
          {page === 'nodes' ? <><Button variant="outline" onClick={editExternal}><Globe2 />导入外部节点</Button><Button onClick={() => editNode()}><Plus />添加服务器</Button></> : <>{publishButton}{page !== 'diagnostics' && <Button onClick={() => editLine(undefined, kind)}><Plus />新建{kind === 'vless' ? '线路' : '转发'}</Button>}</>}
        </div></div>
        {(page === 'lines' || page === 'forward') && <>
          <div className="mb-7 grid grid-cols-2 gap-3 xl:grid-cols-4">{[
            { title: '线路总数', value: state.lines.length, hint: '代理链与端口转发', icon: Network },
            { title: '在线节点', value: `${state.nodes.filter(n => n.online).length} / ${state.nodes.length}`, hint: '最近 15 秒收到心跳', icon: Server },
            { title: '待发布线路', value: state.lines.filter(l => !l.published).length, hint: '保存后需发布才能应用', icon: Layers3 },
            { title: '探测通过', value: Object.values(state.checks).filter(c => c.ok).length, hint: '最近一次整链检查', icon: Activity },
          ].map(item => <Card key={item.title}><CardContent className="p-5"><div className="flex items-center justify-between gap-2 text-xs text-slate-500">{item.title}<item.icon className="size-4 text-slate-400" /></div><div className="my-3 text-3xl font-semibold tracking-tight">{item.value}</div><p className="muted">{item.hint}</p></CardContent></Card>)}</div>
          <Card><CardHeader><CardTitle>{kind === 'vless' ? '我的代理线路' : '我的转发规则'} <span className="ms-2 text-xs font-normal text-slate-400">{lines.length} 条</span></CardTitle><div className="relative"><Search className="absolute left-3 top-2.5 size-3.5 text-slate-400" /><Input className="w-48 ps-8" aria-label="搜索线路" placeholder="搜索线路…" value={search} onChange={e => setSearch(e.target.value)} /></div></CardHeader>
            {!lines.length ? <Empty title={query ? '没有匹配的线路' : `从第一条${kind === 'vless' ? '线路' : '转发规则'}开始`} action={query ? <Button variant="outline" onClick={() => setSearch('')}>清除搜索</Button> : <Button onClick={() => state.nodes.length ? editLine(undefined, kind) : editNode()}><Plus />{state.nodes.length ? '创建线路' : '添加服务器'}</Button>}>{query ? '试试其他名称。' : state.nodes.length ? '选择入口与沿途节点，构建完整路径。' : '先添加一台服务器并连接 Agent，再创建线路。'}</Empty>
              : <div className="overflow-x-auto"><table className="data-table"><thead><tr><th>线路 / 入口</th><th>完整路径</th><th>状态</th><th>操作</th></tr></thead><tbody>{lines.map(line => <tr key={line.id}>
                <td><div className="flex min-w-40 items-center gap-3"><span className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-border bg-slate-50 text-blue-600">{kind === 'vless' ? <Network className="size-4" /> : <ArrowLeftRight className="size-4" />}</span><div><strong className="font-medium">{line.name}</strong><p className="mono mt-1 text-[11px] text-slate-400">{state.nodes.find(n => n.id === line.hops[0].node_id)?.host}:{line.hops[0].port}</p><p className="mt-1 text-[10px] text-slate-400">{kind === 'vless' ? 'VLESS / REALITY' : line.network.toUpperCase()}</p></div></div></td>
                <td><PathView line={line} state={state} /></td><td className="whitespace-nowrap"><LineStatus line={line} state={state} /></td>
                <td><div className="flex gap-1"><Button variant="ghost" size="sm" aria-label={`编辑 ${line.name}`} onClick={() => editLine(line)}>编辑</Button><Button variant="ghost" size="sm" onClick={() => share(line)}>接入</Button><Button variant="ghost" size="sm" onClick={() => probe(line)}>诊断</Button><Button variant="ghost" size="icon" aria-label={`删除 ${line.name}`} onClick={() => remove('lines', line.id, line.name)}><Trash2 className="size-3.5 text-slate-400" /></Button></div></td>
              </tr>)}</tbody></table></div>}
            <div className="border-t border-border px-5 py-4 text-xs text-slate-400">{state.busy ? '正在发布配置，可在「诊断与发布」查看进度。' : '修改先保存为草稿，发布前会列出受影响的节点和线路。'}</div>
          </Card>
          <div className="mt-6 flex items-start gap-3 rounded-lg border border-blue-100 bg-blue-50/40 p-4"><Radio className="mt-0.5 size-4 shrink-0 text-blue-600" /><div><h3 className="text-xs font-medium">配置应用后，检查实际连通</h3><p className="mt-1 text-xs leading-relaxed text-slate-500">探测经过真实入口。HTTP 请求与 UDP 回显分别检查，便于定位路径上的问题。</p></div></div>
        </>}
        {page === 'nodes' && <>
          {state.nodes.length ? <div className="grid gap-5 md:grid-cols-2 xl:grid-cols-3">{state.nodes.map(node => <Card key={node.id}><CardHeader><div className="flex items-center gap-3"><Server className="size-4 text-blue-600" /><CardTitle>{node.name}</CardTitle></div><Status tone={node.online ? 'good' : 'muted'}>{node.online ? '在线' : '离线'}</Status></CardHeader><CardContent>
            <dl className="grid grid-cols-[auto_1fr] gap-x-5 gap-y-3 text-xs"><dt className="text-slate-400">连接地址</dt><dd className="mono break-all text-right">{node.host}</dd><dt className="text-slate-400">REALITY SNI</dt><dd className="break-all text-right">{node.server_name}</dd><dt className="text-slate-400">管理进程</dt><dd className="text-right">{node.report.running ? '正在运行' : '未运行'}</dd><dt className="text-slate-400">最后心跳</dt><dd className="text-right">{stamp(node.seen)}</dd></dl>
            {node.report.error && <p className="mt-4 break-all rounded bg-red-50 p-2 text-xs text-red-600">{node.report.error}</p>}
            <div className="mt-6 flex gap-2"><Button variant="outline" size="sm" onClick={() => editNode(node)}>编辑</Button><Button variant="ghost" size="sm" onClick={() => setModal(<Modal key={`rotate-${node.id}`} title="重置节点接入码" description="旧接入码立即失效。在节点的 chainctl 菜单选择重新接入，粘贴新接入码即可。" submit="重置接入码" close={close} action={async () => { const r = await api<{ token: string }>(`/nodes/${node.id}/token`, 'POST', {}); tokenModal(node.id, r.token); }}><p className="muted">当前线路不会因控制连接断开而自动停止。Agent 重启时，管理的连接会短暂重连。</p></Modal>)}>重置接入码</Button><Button className="ms-auto" variant="ghost" size="icon" aria-label={`删除 ${node.name}`} onClick={() => remove('nodes', node.id, node.name)}><Trash2 className="size-3.5 text-slate-400" /></Button></div>
          </CardContent></Card>)}</div> : <Card><Empty title="还没有受管服务器" action={<Button onClick={() => editNode()}><Plus />添加第一台服务器</Button>}>添加服务器后，按向导复制接入码并连接节点。</Empty></Card>}
          <div className="mt-9 mb-5 flex items-center justify-between"><div><h2 className="font-semibold">外部落地</h2><p className="muted mt-1">只保存连接参数，不修改对方服务器配置。</p></div><Button variant="outline" onClick={editExternal}><Plus />导入</Button></div>
          <Card>{state.externals.length ? <div className="overflow-x-auto"><table className="data-table"><thead><tr><th>名称</th><th>类型</th><th>操作</th></tr></thead><tbody>{state.externals.map(e => <tr key={e.id}><td>{e.name}</td><td><Status>外部 VLESS</Status></td><td><Button variant="ghost" size="sm" onClick={() => remove('externals', e.id, e.name)}>删除</Button></td></tr>)}</tbody></table></div> : <p className="muted p-7 text-center">没有外部落地。出口也可以直接使用受管服务器。</p>}</Card>
        </>}
        {page === 'diagnostics' && <>
          <Card><CardHeader><CardTitle>线路检查</CardTitle><span className="muted">使用已发布版本探测</span></CardHeader>{state.lines.length ? <div className="overflow-x-auto"><table className="data-table"><thead><tr><th>线路</th><th>最近结果</th><th>时间</th><th>操作</th></tr></thead><tbody>{state.lines.map(line => { const c = state.checks[line.id]; return <tr key={line.id}><td className="min-w-32 font-medium">{line.name}</td><td>{c ? <><Status tone={c.ok ? 'good' : 'bad'}>{c.ok ? `通过 · ${c.latency_ms} ms` : '未通过'}</Status><p className="muted mt-2 max-w-lg whitespace-pre-wrap break-all">{c.detail}</p></> : <Status>尚未检查</Status>}</td><td className="whitespace-nowrap text-xs text-slate-400">{c ? stamp(c.checked_at) : '—'}</td><td><Button variant="outline" size="sm" onClick={() => probe(line)}>检查线路</Button></td></tr>; })}</tbody></table></div> : <Empty title="还没有线路检查">创建并发布线路后，可在这里发起检查。</Empty>}</Card>
          <div className="mt-9 mb-5 flex items-center justify-between"><h2 className="font-semibold">发布记录</h2><span className="muted">最近 20 次</span></div>
          <div className="space-y-4">{state.jobs.length ? state.jobs.map(job => <Card key={job.id}><CardHeader><Status tone={job.state === 'applied' ? 'good' : job.state === 'degraded' ? 'bad' : 'warn'}>{jobNames[job.state] || job.state}</Status><span className="muted">{stamp(job.started)}</span></CardHeader><CardContent className="space-y-4">{job.events.map((event, i) => <div key={i} className="flex items-start gap-4 text-xs"><span className="mt-1 size-1.5 shrink-0 rounded-full bg-blue-300" /><time className="shrink-0 text-slate-400">{new Date(event.at * 1000).toLocaleTimeString('zh-CN', { hour12: false })}</time><p className="break-all leading-relaxed text-slate-600">{event.message}</p></div>)}</CardContent></Card>) : <Card><p className="muted p-7 text-center">还没有发布记录。保存线路后，点击「预览并发布」。</p></Card>}</div>
        </>}
        <footer className="mt-10 flex flex-wrap items-center justify-between gap-3 border-t border-border pt-5 text-[11px] text-slate-400"><span>Chain · 私有网络控制台</span><span className="flex items-center gap-1.5"><Check className="size-3" />独立线路 · 多跳编排</span></footer>
      </main>
    </div>{modal}{toastView}
  </div>;
}
