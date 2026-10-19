import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';
import type { PanelState } from '../types';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Card, CardContent, CardHeader, CardTitle } from './ui/card';
import { Field, CopyButton, errorText, stamp } from './shared';

type Policy = { enabled: boolean; interval: number; failures: number; recoveries: number; offline: number; reminder: number; url: string; quota: number[]; expiry_days: number[] };
type Channels = { telegram_token: string; telegram_chat: string; smtp_host: string; smtp_port: number; smtp_starttls: boolean; smtp_user: string; smtp_password: string; mail_from: string; mail_to: string };
type Notice = { id: string; at: number; message: string; error: string; telegram_done: boolean; mail_done: boolean };
type Monitor = { local: Policy; effective: Policy; override: Policy | null; controller: string; channels: Channels; telegram_set: boolean; password_set: boolean; notices: Notice[]; checks: Record<string,{ok:boolean;detail:string;checked_at:number}>; conditions: Record<string,{active:boolean;message:string}> };
const defaultPolicy: Policy = { enabled: false, interval: 300, failures: 3, recoveries: 2, offline: 60, reminder: 86400, url: 'https://example.com/', quota: [80, 95, 100], expiry_days: [7, 3, 1, 0] };
function requestId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}

function PolicyFields({ value, change }: { value: Policy; change: (value: Policy) => void }) {
  return <div className="grid gap-4 md:grid-cols-2">
    <label className="flex items-center gap-2"><input type="checkbox" checked={value.enabled} onChange={e => change({ ...value, enabled: e.target.checked })} />启用定时检测和状态提醒</label>
    <Field label="检测目标（HTTP / HTTPS）"><Input value={value.url} onChange={e => change({ ...value, url: e.target.value })} required type="url" /></Field>
    {([['interval', '检测间隔（秒）', 60, 86400], ['failures', '连续失败次数', 1, 20], ['recoveries', '连续恢复次数', 1, 20], ['offline', '失联阈值（秒）', 30, 3600], ['reminder', '持续故障提醒间隔（秒）', 3600, 604800]] as const).map(([key, label, min, max]) =>
      <Field label={label} key={key}><Input type="number" min={min} max={max} required value={value[key]} onChange={e => change({ ...value, [key]: Number(e.target.value) })} /></Field>)}
    <Field label="流量提醒百分比（逗号分隔）"><Input defaultValue={value.quota.join(',')} onChange={e => change({ ...value, quota: e.target.value.split(',').filter(v => v.trim()).map(Number) })} /></Field>
    <Field label="到期提醒天数（0 表示到期当天）"><Input defaultValue={value.expiry_days.join(',')} onChange={e => change({ ...value, expiry_days: e.target.value.split(',').filter(v => v.trim()).map(Number) })} /></Field>
  </div>;
}

export function Monitoring() {
  const [data, setData] = useState<Monitor>();
  const [policy, setPolicy] = useState<Policy>(defaultPolicy);
  const [channels, setChannels] = useState<Channels>();
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  const [clearTelegram, setClearTelegram] = useState(false);
  const [clearPassword, setClearPassword] = useState(false);
  const refresh = useCallback(async (initial = false) => {
    const value = await api<Monitor>('/monitoring'); setData(value);
    if (initial) { setPolicy(value.local); setChannels(value.channels); }
  }, []);
  useEffect(() => { void refresh(true).catch(e => setError(errorText(e))); const timer = setInterval(() => void refresh().catch(e => setError(errorText(e))), 10000); return () => clearInterval(timer); }, [refresh]);
  return <div className="space-y-6">
    {error && <p role="alert" className="text-red-600">{error}</p>}{message && <p role="status">{message}</p>}
    <Card><CardHeader><CardTitle>监控与通知</CardTitle></CardHeader><CardContent>
      {data && channels ? <form className="space-y-6" onSubmit={async e => { e.preventDefault(); setBusy(true); setError(''); try {
        await api('/monitoring', 'POST', { local: policy, channels, clear_telegram: clearTelegram, clear_password: clearPassword });
        await refresh(true); setClearTelegram(false); setClearPassword(false); setMessage('配置已保存');
      } catch (e) { setError(errorText(e)); } finally { setBusy(false); } }}>
        <p className="muted">{data.override ? '当前执行总控策略；下方本地配置保留，解除覆盖后生效。' : '当前执行本地策略。'}{data.controller && ' 通知由总控统一发送；总控失联期间继续检测并保存记录。'}</p>
        {data.override && <p className="muted">总控：每 {data.effective.interval} 秒检测，失败 {data.effective.failures} 次报警，成功 {data.effective.recoveries} 次恢复。</p>}
        <PolicyFields value={policy} change={setPolicy} />
        <p className="muted">仅检测已应用且可用的代理；手动停用、到期、额度耗尽时跳过。HTTPS 检测成功不代表 UDP 正常。探测会产生少量计费流量。</p>
        <div className="grid gap-4 md:grid-cols-2">
          {([['telegram_token', 'Telegram Bot Token', true], ['telegram_chat', 'Telegram Chat ID', false], ['smtp_host', 'SMTP 主机', false], ['smtp_user', 'SMTP 用户名', false], ['smtp_password', 'SMTP 密码', true], ['mail_from', '发件邮箱', false], ['mail_to', '收件邮箱', false]] as const).map(([key,label,secret]) =>
            <Field key={key} label={label}><Input type={secret ? 'password' : 'text'} autoComplete={secret ? 'new-password' : 'off'} value={channels[key]} placeholder={secret ? '留空保留已保存的值' : ''} onChange={e => setChannels({ ...channels, [key]: e.target.value })} /></Field>)}
          <Field label="SMTP 端口"><Input type="number" min={1} max={65535} value={channels.smtp_port || ''} onChange={e => setChannels({ ...channels, smtp_port: Number(e.target.value) })} /></Field>
          <label><input type="checkbox" checked={channels.smtp_starttls} onChange={e => setChannels({ ...channels, smtp_starttls: e.target.checked })} /> STARTTLS（通常 587；不勾选为直接 TLS，通常 465）</label>
          <label><input type="checkbox" checked={clearTelegram} onChange={e => setClearTelegram(e.target.checked)} />清除 Telegram Token{data.telegram_set ? '（已配置）' : ''}</label>
          <label><input type="checkbox" checked={clearPassword} onChange={e => setClearPassword(e.target.checked)} />清除 SMTP 密码{data.password_set ? '（已配置）' : ''}</label>
        </div>
        <div className="flex gap-3"><Button disabled={busy} type="submit">保存配置</Button><Button type="button" variant="outline" disabled={busy || !!data.controller} onClick={() => void api('/monitoring/test','POST',{}).then(() => { setMessage('测试通知已排队，发送结果见下方记录'); return refresh(); }).catch(e => setError(errorText(e)))}>发送测试通知</Button></div>
      </form> : <p>正在加载…</p>}
    </CardContent></Card>
    <Card><CardHeader><CardTitle>最近整链检测</CardTitle></CardHeader><CardContent>{Object.entries(data?.checks || {}).map(([key,check]) => <div key={key} className="border-b py-3"><p>{data?.conditions[key]?.message || key} · {check.ok ? '检查通过' : data?.conditions[key]?.active ? '故障已确认' : '检查失败，等待连续确认'}</p><p className="muted">{stamp(check.checked_at)} · {check.detail}</p></div>)}</CardContent></Card>
    <Card><CardHeader><CardTitle>最近通知</CardTitle></CardHeader><CardContent>{data?.notices.map(n => <div key={n.id} className="border-b py-3"><p>{n.message}</p><p className="muted">{stamp(n.at)} · Telegram {n.telegram_done ? '已发送' : '未发送'} · 邮件 {n.mail_done ? '已发送' : '未发送'}{n.error && ` · ${n.error}`}</p></div>)}</CardContent></Card>
  </div>;
}

type Remote = { id: string; name: string; url: string; seen: number; error: string; snapshot: PanelState & { version?: string; maintenance?: boolean; cores?: { xray?: string; mihomo?: string } } };
type Task = { id: string; action: string; state: string; targets: { id: string; state: string; error?: string; result?: { detail?: string; error?: string; before?: Record<string,string>; after?: Record<string,string> } }[] };
type FleetState = { remotes: Remote[]; controller: string; policy: Policy | null; tasks: Record<string,Task> };
const statuses: Record<string,string> = { pending:'等待执行',queued:'已排队',running:'执行中',succeeded:'成功',failed:'失败',unknown:'结果待确认',paused:'已暂停' };
export function Fleet() {
  const [data, setData] = useState<FleetState>();
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [invite, setInvite] = useState('');
  const [busy,setBusy] = useState(false);
  const [selected, setSelected] = useState<string[]>([]);
  const [lines, setLines] = useState<Record<string,string[]>>({});
  const [version,setVersion] = useState('');
  const [release,setRelease] = useState<{version:string; xray:string; mihomo:string}>();
  const [policy,setPolicy] = useState<Policy>(defaultPolicy);
  const [override,setOverride] = useState(false);
  const [search,setSearch] = useState('');
  const refresh = useCallback(async (initial = false) => {
    const value = await api<FleetState>('/fleet'); setData(value);
    if (initial) { setOverride(!!value.policy); setPolicy(value.policy || defaultPolicy); }
  }, []);
  useEffect(() => { void refresh(true).catch(e => setError(errorText(e))); const timer = setInterval(() => void refresh().catch(e => setError(errorText(e))),5000); return () => clearInterval(timer); },[refresh]);
  async function run(action: () => Promise<unknown>) { setBusy(true); setError(''); try { await action(); await refresh(); } catch(e) { setError(errorText(e)); } finally { setBusy(false); } }
  function toggle(id: string) { setSelected(current => current.includes(id) ? current.filter(v => v !== id) : [...current,id]); }
  async function batch(action: string) {
    if (action === 'upgrade' && release?.version !== version) throw new Error('请先核对目标发行版本');
    const targets = ['enable','disable'].includes(action) ? Object.keys(lines).filter(id => lines[id].length) : selected;
    if (!targets.length) throw new Error('请先选择 VPS 或代理');
    if (!window.confirm(`确认对 ${targets.length} 台 VPS 执行${action === 'upgrade' ? `升级到 ${version}` : action === 'enable' ? '启用所选代理' : action === 'disable' ? '停用所选代理' : '服务重启'}？升级或重启 Agent 会中断连接。`)) return;
    await api('/fleet/batch','POST',{ id: requestId(), targets, action, version, lines });
    setMessage('任务已提交；请查看逐台结果。暂停后先确认实际状态，再选择失败机器重新提交。');
  }
  async function jump(id: string) {
    const target = window.open('about:blank','_blank');
    try { const result = await api<{url:string}>(`/fleet/${id}/login`,'POST',{}); if (target) { target.opener = null; target.location.href = result.url; } else location.assign(result.url); }
    catch(e) { target?.close(); throw e; }
  }
  return <div className="space-y-6">
    {error && <p role="alert" className="text-red-600">{error}</p>}{message && <p role="status">{message}</p>}
    <Card><CardHeader><CardTitle>接入与授权</CardTitle></CardHeader><CardContent className="space-y-4">
      <p className="muted">每台 VPS 保留独立面板。接入后总控可管理代理、执行维护任务及免密登录；请仅授权你信任的总控。</p>
      {data?.controller ? <div>已授权：{data.controller} <Button variant="outline" disabled={busy} onClick={() => { if(window.confirm('解除总控授权并恢复本地监控策略？')) void run(() => api('/fleet/unbind','POST',{})); }}>解除授权</Button></div> : <Button variant="outline" disabled={busy} onClick={() => void run(async () => { const result = await api<{code:string}>('/fleet/invite','POST',{}); setInvite(result.code); })}>生成本机接入码（10 分钟有效）</Button>}
      {invite && <div className="flex flex-wrap gap-2"><code className="break-all">{invite}</code><CopyButton value={invite} /></div>}
      <form className="grid items-end gap-3 md:grid-cols-4" onSubmit={e => { e.preventDefault(); const form = e.currentTarget; const values = new FormData(form); void run(async () => { await api('/fleet/connect','POST',Object.fromEntries(values)); form.reset(); }); }}>
        <Field label="VPS 名称"><Input name="name" required maxLength={120} /></Field><Field label="远端面板 HTTPS 地址"><Input name="url" type="url" required placeholder="https://panel.example.com" /></Field><Field label="远端一次性接入码"><Input name="code" type="password" required autoComplete="off" /></Field><Button disabled={busy}>接入面板</Button>
      </form>
    </CardContent></Card>
    <Card><CardHeader><CardTitle>统一监控策略</CardTitle></CardHeader><CardContent className="space-y-4">
      <label><input type="checkbox" checked={override} onChange={e => setOverride(e.target.checked)} />覆盖所有已接入 VPS 的本地策略</label>
      {override && <PolicyFields value={policy} change={setPolicy} />}
      <p className="muted">整套覆盖并保留各 VPS 本地设置；取消后恢复本地策略。通知渠道在本机“监控告警”配置。</p>
      <Button disabled={busy} onClick={() => void run(() => api('/fleet/policy','POST',override ? policy : null))}>保存总控策略</Button>
    </CardContent></Card>
    <Card><CardHeader><CardTitle>VPS 与批量维护</CardTitle></CardHeader><CardContent className="space-y-4">
      <div className="flex flex-wrap items-center gap-2"><Input aria-label="目标正式版本" className="max-w-48" placeholder="目标版本，例如 0.2.29" value={version} onChange={e => { setVersion(e.target.value); setRelease(undefined); }} />
        <Button variant="outline" disabled={busy || !/^\d+\.\d+\.\d+$/.test(version)} onClick={() => void run(async () => setRelease(await api(`/fleet/release/${version}`)))}>核对发行版本</Button>
        <Button disabled={busy || release?.version !== version} onClick={() => void run(() => batch('upgrade'))}>逐台升级</Button>
        <Button variant="outline" disabled={busy} onClick={() => void run(() => batch('restart-panel'))}>重启面板</Button><Button variant="outline" disabled={busy} onClick={() => void run(() => batch('restart-agent'))}>重启 Agent</Button>
      </div>
      {release && <p>目标：面板／Agent {release.version} · Xray {release.xray || '版本信息未提供'} · Mihomo {release.mihomo || '版本信息未提供'}</p>}
      <p className="muted">升级整套发行版本，包括面板、Agent、Xray 和 Mihomo；首版维护支持 Linux systemd，需先在远端手工更新以安装维护组件。失败暂停后续机器。</p>
      {data?.remotes.map(r => <div key={r.id} className="flex flex-wrap items-center justify-between gap-3 border-b py-3">
        <label className="flex gap-3"><input type="checkbox" checked={selected.includes(r.id)} onChange={() => toggle(r.id)} /><span>{r.name}<span className="muted block">{r.url} · {r.snapshot?.version || '尚未同步'} · {r.error || `最近同步 ${r.seen ? stamp(r.seen) : '等待同步'}`}</span></span></label>
        <div className="flex flex-wrap gap-2"><a className="underline" href={`/admin?remote=${r.id}#lines`}>管理代理</a><a className="underline" href={`/admin?remote=${r.id}#nodes`}>管理外部代理</a><Button variant="outline" disabled={busy} onClick={() => void run(() => jump(r.id))}>免密进入面板</Button><Button variant="ghost" disabled={busy} onClick={() => { if(window.confirm(`解除 ${r.name} 的接入授权？远端需在线确认撤销。`)) void run(() => api(`/fleet/${r.id}`,'DELETE')); }}>解除接入</Button></div>
      </div>)}
    </CardContent></Card>
    <Card><CardHeader><CardTitle>所有代理与外部代理</CardTitle></CardHeader><CardContent className="space-y-3">
      <Input aria-label="搜索汇总代理" placeholder="搜索主机或代理名称" value={search} onChange={e => setSearch(e.target.value)} />
      <div className="flex gap-2"><Button disabled={busy} onClick={() => void run(() => batch('enable'))}>启用所选代理</Button><Button variant="outline" disabled={busy} onClick={() => void run(() => batch('disable'))}>停用所选代理</Button></div>
      {data?.remotes.flatMap(r => [
        ...(r.snapshot?.lines || []).filter(l => `${r.name} ${l.name}`.toLowerCase().includes(search.toLowerCase())).map(l => <div key={`${r.id}:line:${l.id}`} className="flex justify-between gap-3 border-b py-2"><label className="flex gap-2"><input type="checkbox" checked={(lines[r.id] || []).includes(l.id)} onChange={() => setLines(current => ({...current,[r.id]:(current[r.id] || []).includes(l.id) ? current[r.id].filter(id => id !== l.id) : [...(current[r.id] || []),l.id]}))} /><span>{r.name} / {l.name} <span className="muted">{l.enabled ? '启用' : '停用'}{r.error && ' · 数据可能已过期'}</span></span></label><a className="underline" href={`/admin?remote=${r.id}#lines`}>编辑与查看</a></div>),
        ...(r.snapshot?.externals || []).filter(e => `${r.name} ${e.name}`.toLowerCase().includes(search.toLowerCase())).map(e => <div key={`${r.id}:external:${e.id}`} className="flex justify-between gap-3 border-b py-2"><span>{r.name} / {e.name} <span className="muted">外部代理</span></span><a className="underline" href={`/admin?remote=${r.id}#nodes`}>编辑与查看</a></div>)
      ])}
    </CardContent></Card>
    <Card><CardHeader><CardTitle>批量任务结果</CardTitle></CardHeader><CardContent>{Object.values(data?.tasks || {}).reverse().map(task => <div key={task.id} className="border-b py-3"><p>{task.action} · {statuses[task.state] || task.state}</p>{task.state === 'paused' && <Button variant="outline" disabled={busy} onClick={() => void run(() => api(`/fleet/batch/${task.id}/resume`, 'POST', {}))}>重新查询原任务，成功后继续</Button>}{task.targets.map(target => <p key={target.id} className="muted">{data?.remotes.find(r => r.id === target.id)?.name || target.id}：{statuses[target.state] || target.state} {target.error || target.result?.error || target.result?.detail}{target.result?.after && ` · ${Object.entries(target.result.after).map(([k,v]) => `${k} ${v}`).join('，')}`}</p>)}</div>)}</CardContent></Card>
  </div>;
}
