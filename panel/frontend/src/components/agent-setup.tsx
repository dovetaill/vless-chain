import { useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { api } from '@/api';
import type { NodeInfo, PanelState } from '@/types';

const isLoopback = (host: string) => host === 'localhost' || host === '[::1]' || /^127\./.test(host);

export function AgentSetup({ id, token, host, onOnline }: { id: string; token: string; host?: string; copy?: (value: string) => Promise<void>; onOnline?: (node: NodeInfo) => void }) {
  const [address, setAddress] = useState(isLoopback(location.hostname) ? '' : location.origin);
  const [online, setOnline] = useState<NodeInfo | null>(null);
  const notified = useRef(false);
  useEffect(() => {
    if (online && onOnline && !notified.current) {
      notified.current = true;
      onOnline(online);
    }
  }, [online, onOnline]);
  const [unavailable, setUnavailable] = useState(false);
  const [port, setPort] = useState('22');
  const [auth, setAuth] = useState('password');
  const [password, setPassword] = useState('');
  const [privateKey, setPrivateKey] = useState('');
  const [keys, setKeys] = useState<{ fingerprint: string; algorithm: string }[]>([]);
  const [fingerprint, setFingerprint] = useState('');
  const [confirmed, setConfirmed] = useState(false);
  const [allowHttp, setAllowHttp] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  async function inspect() {
    setBusy(true); setMessage(''); setConfirmed(false); setKeys([]); setFingerprint('');
    try {
      const result = await api<{ keys: { fingerprint: string; algorithm: string }[] }>(`/nodes/${id}/ssh-keys`, 'POST', { port: Number(port) });
      setKeys(result.keys); setFingerprint(result.keys[0]?.fingerprint || '');
    } catch (error) { setMessage(error instanceof Error ? error.message : 'SSH 检测失败'); }
    finally { setBusy(false); }
  }
  async function install() {
    setBusy(true); setMessage('正在安装…');
    try {
      await api(`/nodes/${id}/install`, 'POST', {
        port: Number(port), fingerprint, token, panel_url: new URL(address).origin, allow_http: allowHttp,
        password: auth === 'password' ? password : null, private_key: auth === 'key' ? privateKey : null,
      });
      setMessage('安装完成，正在连接…');
    } catch (error) { setMessage(error instanceof Error ? error.message : '安装失败'); }
    finally { setPassword(''); setPrivateKey(''); setBusy(false); }
  }
  useEffect(() => {
    let stopped = false;
    let pending = false;
    const check = async () => {
      if (pending) return;
      pending = true;
      try {
        const state = await api<PanelState>('/state');
        if (!stopped) { setOnline(state.nodes.find(node => node.id === id && node.online) || null); setUnavailable(false); }
      } catch { if (!stopped) setUnavailable(true); }
      finally { pending = false; }
    };
    void check();
    const timer = setInterval(() => void check(), 3000);
    return () => { stopped = true; clearInterval(timer); };
  }, [id]);
  let code = '';
  let plainHttp = false;
  try {
    const url = new URL(address);
    if (['http:', 'https:'].includes(url.protocol) && url.pathname === '/' && !url.search && !url.hash
      && !url.username && !url.password && !isLoopback(url.hostname)) {
      code = 'chain-agent-v1:' + btoa(JSON.stringify({ panel_url: url.origin, node_id: id, token }))
        .replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
      plainHttp = url.protocol === 'http:';
    }
  } catch { /* Keep the copy action disabled until a reachable URL is entered. */ }
  return <div className="space-y-4">
    {host && <p className="text-sm font-medium">{host}</p>}
    <div className="grid gap-4 sm:grid-cols-2">
      <label className="field">SSH 端口<Input type="number" min={1} max={65535} value={port} disabled={busy} onChange={event => { setPort(event.target.value); setKeys([]); setConfirmed(false); }} /></label>
      <label className="field">root 登录方式<select className="select" value={auth} disabled={busy} onChange={event => { setAuth(event.target.value); setPassword(''); setPrivateKey(''); }}><option value="password">密码</option><option value="key">私钥（无口令）</option></select></label>
    </div>
    {auth === 'password' ? <label className="field">SSH 密码<Input type="password" autoComplete="off" value={password} disabled={busy} onChange={event => setPassword(event.target.value)} /></label>
      : <label className="field">SSH 私钥<textarea className="textarea mono" autoComplete="off" value={privateKey} disabled={busy} onChange={event => setPrivateKey(event.target.value)} rows={4} /></label>}
    <Button type="button" variant="outline" disabled={busy || !Number(port) || Number(port) > 65535} onClick={() => void inspect()}>验证服务器</Button>
    {keys.length > 0 && <>
      <label className="field">SSH 指纹<select className="select" value={fingerprint} disabled={busy} onChange={event => { setFingerprint(event.target.value); setConfirmed(false); }}>
        {keys.map(key => <option key={key.fingerprint} value={key.fingerprint}>{key.algorithm} · {key.fingerprint}</option>)}
      </select></label>
      <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={confirmed} disabled={busy} onChange={event => setConfirmed(event.target.checked)} />信任此服务器指纹</label>
    </>}
    <details className="details" open={!code || plainHttp || undefined}>
      <summary>控制台连接地址</summary>
      <div className="space-y-3 pt-3">
        <Input aria-label="控制台连接地址" disabled={busy} value={address} onChange={event => { setAddress(event.target.value); setAllowHttp(false); }} placeholder="https://panel.example.com" />
        {!code && <p role="alert" className="text-sm text-red-600">请输入远程服务器可访问的控制台地址。</p>}
        {plainHttp && <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={allowHttp} disabled={busy} onChange={event => setAllowHttp(event.target.checked)} />使用可信私网 HTTP 地址</label>}
      </div>
    </details>
    <Button type="button" disabled={busy || Boolean(online) || !code || !confirmed || !(auth === 'password' ? password : privateKey) || (plainHttp && !allowHttp)} onClick={() => void install()}>{busy ? '正在安装…' : online ? '已连接' : '安装并连接'}</Button>
    {message && <p role="status" className="whitespace-pre-wrap break-all text-sm">{message}</p>}
    {unavailable && <p role="alert" className="text-sm text-red-600">无法获取连接状态，请重试。</p>}
  </div>;
}
