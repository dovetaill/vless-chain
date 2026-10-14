import { useEffect, useState } from 'react';
import { Copy } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { api } from '@/api';
import type { PanelState } from '@/types';

const installCommand = 'curl -fsSL https://raw.githubusercontent.com/dovetaill/vless-chain/main/scripts/install-server.sh | sudo bash -s -- install --component agent';
const isLoopback = (host: string) => host === 'localhost' || host === '[::1]' || /^127\./.test(host);

export function AgentSetup({ id, token, host, copy }: { id: string; token: string; host?: string; copy: (value: string) => Promise<void> }) {
  const [address, setAddress] = useState(isLoopback(location.hostname) ? '' : location.origin);
  const [online, setOnline] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const [method, setMethod] = useState<'web' | 'script'>('script');
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
    setBusy(true); setMessage('正在连接服务器并安装 Agent，请稍候…');
    try {
      await api(`/nodes/${id}/install`, 'POST', {
        port: Number(port), fingerprint, token, panel_url: new URL(address).origin, allow_http: allowHttp,
        password: auth === 'password' ? password : null, private_key: auth === 'key' ? privateKey : null,
      });
      setMessage('Agent 安装完成，等待心跳确认。');
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
        if (!stopped) { setOnline(Boolean(state.nodes.find(node => node.id === id)?.online)); setUnavailable(false); }
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
  return <div className="space-y-5">
    <section className="space-y-3">
      <h3 className="text-sm font-semibold">1. 选择如何安装到目标服务器{host ? ` ${host}` : ''}</h3>
      <p className="muted">任选一种即可。脚本在目标服务器 B 执行；SSH 方式由控制台代你连接 B 安装。</p>
      <div className="flex flex-wrap gap-2">
        <Button type="button" disabled={busy} variant={method === 'script' ? 'primary' : 'outline'} onClick={() => setMethod('script')}>在 B 的终端安装</Button>
        <Button type="button" disabled={busy} variant={method === 'web' ? 'primary' : 'outline'} onClick={() => setMethod('web')}>通过 SSH 远程安装到 B</Button>
      </div>
    </section>
    <section className="space-y-3 border-t border-border pt-4">
      <h3 className="text-sm font-semibold">2. 确认控制台地址</h3>
      <p className="muted">这是 B 安装后回连的控制台地址，不是 B 的服务器地址。</p>
      <label className="field">节点可以访问的控制台地址
        <Input disabled={busy} value={address} onChange={event => { setAddress(event.target.value); setAllowHttp(false); }} placeholder="https://panel.example.com" />
      </label>
      {!code && <p className="muted">填写节点能够访问的完整 HTTP/HTTPS 地址；SSH 隧道的本机地址不能用于远端节点。</p>}
      {plainHttp && <p className="muted">当前为 HTTP 地址，安装向导会确认是否为可信私网；公网接入请使用 HTTPS。</p>}
      {method === 'script' && <>
        {code && <label className="field">节点接入码<textarea className="textarea mono" aria-label="节点接入码" readOnly value={code} rows={3} onFocus={event => event.target.select()} /></label>}
        <Button type="button" disabled={!code} onClick={() => void copy(code)}><Copy />复制接入码</Button>
        <p className="muted">接入码包含此节点的凭据，仅在本次显示，请私下保存。遗失后可在服务器卡片中重置接入码。</p>
      </>}
    </section>
    <section className="space-y-3 border-t border-border pt-4">
      <h3 className="text-sm font-semibold">3. {method === 'script' ? '在 B 的终端执行' : '填写 B 的 SSH 登录信息'}</h3>
      {method === 'script' ? <>
        <pre className="secret">{installCommand}</pre>
        <Button type="button" variant="outline" onClick={() => void copy(installCommand)}><Copy />复制安装命令</Button>
        <p className="muted">登录 B 的终端后执行上述命令，直接进入节点安装向导，按提示粘贴上面的接入码。B 已安装过时，直接输入 <code>chainctl</code>，选择「接入 / 重新接入节点」，无需重复下载脚本。</p>
      </> : <div className="space-y-3">
        <p className="muted">连接此节点的 SSH，以 root 安装。控制台直接传送自己的已校验安装包；节点需与控制台架构兼容。SSH 凭据仅用于本次操作，不保存到数据库。</p>
        <label className="field">SSH 端口<Input type="number" min={1} max={65535} value={port} disabled={busy} onChange={event => { setPort(event.target.value); setKeys([]); setConfirmed(false); }} /></label>
        <Button type="button" variant="outline" disabled={busy || !Number(port) || Number(port) > 65535} onClick={() => void inspect()}>检测 SSH 指纹</Button>
        {keys.length > 0 && <>
          <label className="field">服务器 SSH 指纹<select className="select" value={fingerprint} disabled={busy} onChange={event => { setFingerprint(event.target.value); setConfirmed(false); }}>
            {keys.map(key => <option key={key.fingerprint} value={key.fingerprint}>{key.algorithm} · {key.fingerprint}</option>)}
          </select></label>
          <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={confirmed} disabled={busy} onChange={event => setConfirmed(event.target.checked)} />已与服务器核对 SSH 指纹</label>
        </>}
        <label className="field">root 登录方式<select className="select" value={auth} disabled={busy} onChange={event => { setAuth(event.target.value); setPassword(''); setPrivateKey(''); }}><option value="password">SSH 密码</option><option value="key">SSH 私钥（无口令）</option></select></label>
        {auth === 'password' ? <label className="field">root SSH 密码<Input type="password" autoComplete="off" value={password} disabled={busy} onChange={event => setPassword(event.target.value)} /></label>
          : <label className="field">SSH 私钥<textarea className="textarea mono" autoComplete="off" value={privateKey} disabled={busy} onChange={event => setPrivateKey(event.target.value)} rows={4} /></label>}
        {plainHttp && <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={allowHttp} disabled={busy} onChange={event => setAllowHttp(event.target.checked)} />此控制台地址位于可信私网，允许 HTTP 接入</label>}
        <Button type="button" disabled={busy || !code || !confirmed || !(auth === 'password' ? password : privateKey) || (plainHttp && !allowHttp)} onClick={() => void install()}>{busy ? '正在处理…' : '安装并接入 Agent'}</Button>
        {message && <p role="status" className="whitespace-pre-wrap break-all text-sm">{message}</p>}
      </div>}
    </section>
    <section aria-live="polite" className={`rounded-lg border p-4 text-sm ${online ? 'border-green-200 bg-green-50 text-green-800' : 'border-border bg-slate-50'}`}>
      <strong>{online ? '节点已在线，可以创建线路了' : unavailable ? '暂时无法查询，请检查控制台连接' : '4. 等待 B 连接控制台…'}</strong>
      {!online && <p className="mt-2 text-xs">安装完成后这里会自动更新。连接异常时，在节点的 chainctl 菜单选择「查看日志」。</p>}
    </section>
  </div>;
}
