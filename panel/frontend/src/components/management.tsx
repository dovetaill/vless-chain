import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Card, CardContent, CardHeader, CardTitle } from './ui/card';
import { Field, CopyButton, errorText, stamp } from './shared';

export type Policy = { enabled: boolean; interval: number; failures: number; recoveries: number; offline: number; reminder: number; url: string; quota: number[]; expiry_days: number[] };
type Channels = { telegram_token: string; telegram_chat: string; smtp_host: string; smtp_port: number; smtp_starttls: boolean; smtp_user: string; smtp_password: string; mail_from: string; mail_to: string };
type Notice = { id: string; at: number; message: string; error: string; telegram_done: boolean; mail_done: boolean };
type Monitor = { local: Policy; effective: Policy; override: Policy | null; controller: string; channels: Channels; telegram_set: boolean; password_set: boolean; notices: Notice[]; checks: Record<string,{ok:boolean;detail:string;checked_at:number}>; conditions: Record<string,{active:boolean;message:string}> };
export const defaultPolicy: Policy = { enabled: false, interval: 300, failures: 3, recoveries: 2, offline: 60, reminder: 86400, url: 'https://example.com/', quota: [80, 95, 100], expiry_days: [7, 3, 1, 0] };

export function PolicyFields({ value, change }: { value: Policy; change: (value: Policy) => void }) {
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
    <Card><CardHeader><CardTitle>最近自动检测</CardTitle></CardHeader><CardContent><p className="muted">{data?.effective.enabled ? '系统自动复查；连续通过后解除告警，无需手动检测。' : '自动检测未启用，以下为历史结果，不会自动复查。'}结果仅对应所列入口和检测目标。</p>{Object.entries(data?.checks || {}).map(([key,check]) => <div key={key} className="border-b py-3"><p>{data?.conditions[key]?.message || key} · {!data?.effective.enabled ? check.ok ? '历史结果：检查通过' : '历史结果：检查未通过' : check.ok ? data?.conditions[key]?.active ? '最近已通过，正在自动确认恢复' : '检查通过' : data?.conditions[key]?.active ? '检测持续未通过，自动复查中' : '检查失败，等待连续确认'}</p><p className="muted whitespace-pre-wrap break-words">{stamp(check.checked_at)} · {check.detail}</p></div>)}</CardContent></Card>
    <Card><CardHeader><CardTitle>最近通知</CardTitle></CardHeader><CardContent>{data?.notices.map(n => <div key={n.id} className="border-b py-3"><p>{n.message}</p><p className="muted">{stamp(n.at)} · Telegram {n.telegram_done ? '已发送' : '未发送'} · 邮件 {n.mail_done ? '已发送' : '未发送'}{n.error && ` · ${n.error}`}</p></div>)}</CardContent></Card>
  </div>;
}

export { Fleet } from './fleet';
