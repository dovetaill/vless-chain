import { useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { Modal, errorText, stamp } from './shared';
import { Button } from './ui/button';
import { Input } from './ui/input';

type Logs = { clock?: { detail?:string; ntp_enabled?:boolean; timezone?:string }; text: string; sampled_at: number; sources: string[]; detail: string };
export function Diagnostics({ nodes, lineId = '', close, request = api }: { nodes: {id:string;name:string}[]; lineId?:string; close:()=>void; request?:typeof api }) {
  const [node, setNode] = useState(nodes[0]?.id || '00000000-0000-0000-0000-000000000000');
  const [source, setSource] = useState(nodes.length ? 'xray' : 'panel');
  const [filter, setFilter] = useState(lineId);
  const [archive, setArchive] = useState(false);
  const [follow, setFollow] = useState(true);
  const [data, setData] = useState<Logs>();
  const [error, setError] = useState('');
  const [target, setTarget] = useState('https://example.com/');
  const [diagnosing, setDiagnosing] = useState(false);
  const [diagnostic, setDiagnostic] = useState('');
  const [until, setUntil] = useState(0);
  const [task, setTask] = useState<string>();
  const [taskMessage, setTaskMessage] = useState('');
  const [tick, setTick] = useState(0);
  const previous = useRef('');
  useEffect(() => {
    if (!node || diagnosing) return;
    const controller = new AbortController(); let timer: ReturnType<typeof setTimeout>;
    const key = JSON.stringify([node, source, filter, archive, tick]);
    if (!follow && previous.current === key) return;
    if (previous.current !== key) { setData(undefined); setError(''); }
    previous.current = key;
    async function refresh() {
      try {
        const value = await request<Logs>(`/diagnostics/${node}/logs`, 'POST', {source,line_id:filter,archive}, controller.signal);
        if (!controller.signal.aborted) { setData(value); setError(''); }
      } catch (e) { if (!controller.signal.aborted) setError(errorText(e)); }
      if (!controller.signal.aborted && follow) timer = setTimeout(() => void refresh(), 3000);
    }
    void refresh();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [node, source, filter, archive, follow, tick, request, diagnosing]);
  useEffect(() => {
    if (!task) return;
    let stopped = false; let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const value = await request<{state:string;detail?:string;error?:string}>('/federation/task','POST',{action:'status',id:task});
        if (stopped) return;
        setTaskMessage(value.error || value.detail || `校时任务：${value.state}`);
        if (['succeeded','failed'].includes(value.state)) { setTask(undefined); setTick(v=>v+1); }
        else timer = setTimeout(()=>void poll(), 3000);
      } catch(e) { if (!stopped) { setTaskMessage(errorText(e)); setTask(undefined); } }
    }
    void poll(); return () => { stopped = true; clearTimeout(timer); };
  }, [task,request]);
  useEffect(() => {
    if (!until) return;
    const timer = setTimeout(() => { setUntil(0); setDiagnostic('详细诊断窗口已到期。临时内核每次检查完成即退出。'); }, Math.max(0, until-Date.now()));
    return () => clearTimeout(timer);
  },[until]);
  async function diagnose() {
    setDiagnosing(true); setFollow(false); setDiagnostic('正在分段检查…');
    try {
      const result = await request<{stage:string;result:{ok:boolean;detail:string};note:string}>(`/diagnostics/${node}/path`,'POST',{line_id:lineId,url:target});
      let text = `${result.stage}：${result.result.ok ? '通过' : '失败'}\n${result.result.detail}\n${result.note}`;
      if (result.result.ok) {
        const full = await request<{ok:boolean;detail:string}>(`/lines/${lineId}/probe`,'POST',{mode:'http',url:target});
        text += `\n完整链路（面板发起）${full.ok ? '通过' : '失败'}：${full.detail}`;
        if (!full.ok) text += '\n出口已通过，故障范围缩小到入口、REALITY/TLS 握手或本机转发；请查看对应节点日志。';
      } else text += '\n入口尚未验证。先按出口日志检查证书时间、SNI、认证或后续节点。';
      setDiagnostic(text);
    } catch(e) { setDiagnostic(errorText(e)); } finally { setDiagnosing(false); }
  }
  function download() {
    if (!data) return;
    const url = URL.createObjectURL(new Blob([data.text], {type:'text/plain;charset=utf-8'}));
    const a = document.createElement('a'); a.href = url; a.download = `chain-${source}-logs.txt`; a.click(); URL.revokeObjectURL(url);
  }
  return <Modal wide title="日志与诊断" close={close}>
    <div className="flex flex-wrap items-center gap-3">
      <label>节点 <select aria-label="日志节点" value={node} onChange={e=>setNode(e.target.value)}>{!nodes.length && <option value={node}>当前面板（无已接入节点）</option>}{nodes.map(n=><option key={n.id} value={n.id}>{n.name}</option>)}</select></label>
      <label>来源 <select aria-label="日志来源" value={source} onChange={e=>setSource(e.target.value)}>{Array.from(new Set(['panel','agent','xray','mihomo',...(data?.sources || [])])).map(s=><option key={s} value={s}>{s}</option>)}</select></label>
      {lineId && <label><input type="checkbox" checked={!!filter} onChange={e=>setFilter(e.target.checked ? lineId : '')}/>仅此代理（按线路标签过滤）</label>}
      <label><input type="checkbox" checked={archive} onChange={e=>setArchive(e.target.checked)}/>包含近期归档</label>
      <Button type="button" variant="outline" onClick={()=>setFollow(!follow)}>{follow ? '暂停跟随' : '继续跟随'}</Button>
      <Button type="button" variant="outline" onClick={()=>setTick(tick+1)}>刷新</Button>
      <Button type="button" variant="outline" disabled={!data} onClick={download}>下载脱敏片段</Button>
    </div>
    <p className="muted">日志保留 3 天，面板和 Agent 各最多 50 MiB，合计 100 MiB。达到任一限制先清理最旧记录。关闭窗口停止读取；系统 journal 单独管理。</p>
    <p className="muted">链路各跳的 SNI 可以不同，但每一跳必须匹配该跳的 REALITY 目标或 TLS 证书。accepted 只表示已接收连接，不代表已成功出网。</p>
    {lineId && <div className="space-y-3 rounded border p-3">
      <p>分段连接检查：先隔离验证出口，再检查完整链路。会产生少量实际请求流量；不重启现有代理。</p>
      <label>检查网址<Input type="url" value={target} onChange={e=>setTarget(e.target.value)} /></label>
      {!until ? <Button type="button" variant="outline" onClick={()=>setUntil(Date.now()+600000)}>开启 10 分钟详细诊断窗口</Button> : <Button type="button" disabled={diagnosing} onClick={()=>void diagnose()}>{diagnosing ? '正在分段检查…' : '执行详细检查'}</Button>}
      {!!until && <p className="muted">窗口 10 分钟后关闭；每次检查的临时 debug 内核完成即退出，无需重启恢复日志级别。</p>}
      {diagnostic && <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">{diagnostic}</pre>}
    </div>}
    <p className="muted">{data?.clock?.detail} {data?.clock?.timezone && `时区：${data.clock.timezone}`} {data?.clock?.ntp_enabled === false && '自动时间同步未启用'}</p>
    <Button type="button" variant="outline" disabled={!!task} onClick={async()=>{
      if (!window.confirm('在当前面板所在 VPS 启用并校准系统时间？必要时安装系统时间服务，可能改变整机时间；保持现有时区。此操作不修改其他远程 Agent。')) return;
      const id = crypto.randomUUID();
      try { await request('/federation/task','POST',{id,action:'sync-time'}); setTask(id); setTaskMessage('校时任务已提交'); }
      catch(e) { setTaskMessage(errorText(e)); }
    }}>启用本 VPS 自动校时</Button>
    {taskMessage && <p role="status">{taskMessage}</p>}
    {error && <p role="alert" className="text-red-600">{error}{data && '；下方为上次成功读取的记录。'}</p>}
    <p role="status" className="muted">{data ? `采样 ${stamp(data.sampled_at)} · 最多 500 行` : error ? '未取得日志' : '等待节点返回日志…'}</p>
    <pre className="max-h-[55vh] overflow-auto whitespace-pre-wrap break-all rounded bg-slate-950 p-4 text-xs text-slate-100">{data?.text || (data ? '当前来源 / 筛选范围内没有日志。可取消代理筛选或切换来源。' : '')}</pre>
  </Modal>;
}
