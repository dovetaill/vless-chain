import { useEffect, useRef, useState } from 'react';
import { api } from '../api';
import { Modal, errorText, stamp } from './shared';
import { Button } from './ui/button';
import { Input } from './ui/input';

type Logs = { logging_enabled?: boolean; retention_hours?: number; max_mib?: number; clock?: { detail?:string; ntp_enabled?:boolean; timezone?:string }; text: string; sampled_at: number; sources: string[]; detail: string };
export function Diagnostics({ nodes, lineId = '', close, request = api }: { nodes: {id:string;name:string}[]; lineId?:string; close:()=>void; request?:typeof api }) {
  const [node, setNode] = useState(nodes[0]?.id || '00000000-0000-0000-0000-000000000000');
  const [source, setSource] = useState(lineId && nodes.length ? 'xray' : 'panel');
  const [filter, setFilter] = useState(lineId);
  const [archive, setArchive] = useState(false);
  const [follow, setFollow] = useState(true);
  const [data, setData] = useState<Logs>();
  const [cleared, setCleared] = useState<{previous:string[]; lines:string[]} | null>(null);
  const [error, setError] = useState('');
  const [target, setTarget] = useState('https://example.com/');
  const [diagnosing, setDiagnosing] = useState(false);
  const [diagnostic, setDiagnostic] = useState('');
  const [until, setUntil] = useState(0);
  const [task, setTask] = useState<string>();
  const [taskMessage, setTaskMessage] = useState('');
  const [taskFailed, setTaskFailed] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [loading, setLoading] = useState(false);
  const [savingLogs, setSavingLogs] = useState(false);
  const [retentionValue, setRetentionValue] = useState<string>();
  const [retentionUnit, setRetentionUnit] = useState(1);
  const [sizeValue, setSizeValue] = useState<string>();
  useEffect(() => { setRetentionValue(undefined); setRetentionUnit(1); setSizeValue(undefined); }, [node, source]);
  const requester = useRef(request);
  requester.current = request;
  const inFlight = useRef<Promise<unknown>>(Promise.resolve());
  const [tick, setTick] = useState(0);
  const following = useRef(follow);
  following.current = follow;
  useEffect(() => { setCleared(null); }, [node, source, filter, archive]);
  useEffect(() => {
    if (!node || diagnosing || savingLogs) return;
    let stopped = false; let timer: ReturnType<typeof setTimeout>;
    setData(undefined); setError('');
    async function refresh() {
      // Let the remote request finish before switching sources or starting another read.
      await inFlight.current;
      if (stopped) return;
      setLoading(true);
      const pending = requester.current<Logs>(`/diagnostics/${node}/logs`, 'POST', {source,line_id:filter,archive});
      inFlight.current = pending.catch(() => undefined);
      try {
        const value = await pending;
        if (!stopped) {
          setData(value); setError('');
          setCleared(previous => {
            if (!previous) return null;
            const lines = value.text ? value.text.split('\n') : [];
            // shortcut: identical full snapshots cannot reveal new duplicates; add a server cursor if exact delivery is needed.
            let overlap = Math.min(previous.previous.length, lines.length);
            while (overlap && !previous.previous.slice(-overlap).every((line, i) => line === lines[i])) overlap--;
            return {previous:lines, lines:[...previous.lines, ...lines.slice(overlap)].slice(-500)};
          });
        }
      } catch (e) { if (!stopped) setError(errorText(e)); }
      finally { if (!stopped) setLoading(false); }
      if (!stopped && following.current) timer = setTimeout(() => { if (following.current) void refresh(); }, 3000);
    }
    void refresh();
    return () => { stopped = true; clearTimeout(timer); setLoading(false); };
  }, [node, source, filter, archive, tick, diagnosing, savingLogs]);
  useEffect(() => {
    if (!task) return;
    let stopped = false; let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const value = await requester.current<{state:string;detail?:string;error?:string}>('/federation/task','POST',{action:'status',id:task});
        if (stopped) return;
        setTaskFailed(value.state === 'failed');
        setTaskMessage([value.error, value.detail && !value.error?.includes(value.detail) ? value.detail : ''].filter(Boolean).join('\n') || ({queued:'校时任务排队中',running:'正在安装或启用时间服务并等待同步…',succeeded:'自动校时已完成',failed:'自动校时失败'}[value.state] || `校时任务：${value.state}`));
        if (['succeeded','failed'].includes(value.state)) { setTask(undefined); setTick(v=>v+1); }
        else timer = setTimeout(()=>void poll(), 3000);
      } catch(e) { if (!stopped) { setTaskFailed(true); setTaskMessage(`无法读取校时任务结果：${errorText(e)}；任务可能仍在运行，请在维护任务中查看。`); setTask(undefined); } }
    }
    void poll(); return () => { stopped = true; clearTimeout(timer); };
  }, [task]);
  useEffect(() => {
    if (!until) return;
    const timer = setTimeout(() => { setUntil(0); setDiagnostic('详细诊断窗口已到期。临时内核每次检查完成即退出。'); }, Math.max(0, until-Date.now()));
    return () => clearTimeout(timer);
  },[until]);
  async function diagnose() {
    setDiagnosing(true); setFollow(false); setDiagnostic('正在分段检查…');
    try {
      await inFlight.current;
      const result = await requester.current<{stage:string;result:{ok:boolean;detail:string};note:string}>(`/diagnostics/${node}/path`,'POST',{line_id:lineId,url:target});
      let text = `${result.stage}：${result.result.ok ? '通过' : '失败'}\n${result.result.detail}\n${result.note}`;
      if (result.result.ok) {
        const full = await requester.current<{ok:boolean;detail:string}>(`/lines/${lineId}/probe`,'POST',{mode:'http',url:target});
        text += `\n完整链路（面板发起）${full.ok ? '通过' : '失败'}：${full.detail}`;
        if (!full.ok) text += '\n出口已通过，故障范围缩小到入口、REALITY/TLS 握手或本机转发；请查看对应节点日志。';
      } else text += '\n入口尚未验证。先按出口日志检查证书时间、SNI、认证或后续节点。';
      setDiagnostic(text);
    } catch(e) { setDiagnostic(errorText(e)); } finally { setDiagnosing(false); }
  }
  async function saveLogging(enabled?: boolean) {
    if (data?.logging_enabled === undefined) return;
    const retention_hours = Number(retentionValue ?? String((data.retention_hours || 1) / retentionUnit)) * retentionUnit;
    const max_mib = Number(sizeValue ?? String(data.max_mib || 50));
    if (enabled === undefined && (!Number.isInteger(retention_hours) || retention_hours < 1 || retention_hours > 720 || !Number.isInteger(max_mib) || max_mib < 1 || max_mib > 1024)) {
      setError('保留时长须为 1 小时至 30 天，大小上限须为 1 至 1024 MiB 的整数'); return;
    }
    setSavingLogs(true);
    try {
      await inFlight.current;
      const value = await requester.current<Logs>(`/diagnostics/${node}/logs`, 'POST', {source,line_id:filter,archive,...(enabled === undefined ? {retention_hours,max_mib} : {enabled})});
      setData(value); setCleared(null); setRetentionValue(undefined); setSizeValue(undefined); setError(''); setTick(v=>v+1);
    } catch (e) { setError(errorText(e)); }
    finally { setSavingLogs(false); }
  }
  function download() {
    if (!data) return;
    const url = URL.createObjectURL(new Blob([data.text], {type:'text/plain;charset=utf-8'}));
    const a = document.createElement('a'); a.href = url; a.download = `chain-${source}-logs.txt`; a.click(); URL.revokeObjectURL(url);
  }
  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const sourceNames: Record<string,string> = {panel:'控制台应用',agent:'节点 Agent',xray:'Xray 内核',mihomo:'Mihomo 内核'};
  const visibleLogs = cleared ? cleared.lines.join('\n') : data?.text;
  const readableLogs = visibleLogs?.replace(/^(\d{10})\s+(.*)$/gm, (_, timestamp:string, body:string) => {
    if (source === 'xray') body = body.replace(/^\d{4}\/\d{2}\/\d{2} \d{2}:\d{2}:\d{2}(?:\.\d+)?\s+/, '');
    return `${stamp(Number(timestamp))} ${body}`;
  });
  return <Modal wide title="日志与诊断" close={close}>
    <div className="space-y-5 min-w-0">
      <section className="space-y-3 rounded-xl border bg-muted/20 p-4" aria-label="日志筛选">
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="space-y-1.5 text-sm font-medium">日志来源
            <select className="block h-10 w-full min-w-0 rounded-md border border-input bg-background px-3 text-sm" aria-label="日志来源" value={source} disabled={diagnosing || savingLogs} onChange={e=>setSource(e.target.value)}>
              {Array.from(new Set(['panel',...(nodes.length ? ['agent','xray','mihomo'] : []),...(data?.sources || [])])).map(s=><option key={s} value={s}>{sourceNames[s] || `旧版内核 · ${s.replace('legacy-', '')}`}</option>)}
            </select>
          </label>
          <label className="space-y-1.5 text-sm font-medium">节点
            <select className="block h-10 w-full min-w-0 rounded-md border border-input bg-background px-3 text-sm" aria-label="日志节点" value={node} disabled={source === 'panel' || diagnosing || savingLogs} onChange={e=>setNode(e.target.value)}>
              {!nodes.length && <option value={node}>当前 VPS 控制台</option>}{nodes.map(n=><option key={n.id} value={n.id}>{n.name}</option>)}
            </select>
          </label>
        </div>
        <div className="flex flex-wrap gap-x-5 gap-y-2 text-sm">
          {lineId && <label className="flex items-center gap-2"><input type="checkbox" disabled={diagnosing} checked={!!filter} onChange={e=>setFilter(e.target.checked ? lineId : '')}/>仅此代理</label>}
          <label className="flex items-center gap-2"><input type="checkbox" disabled={diagnosing} checked={archive} onChange={e=>setArchive(e.target.checked)}/>包含近期归档</label>
        </div>
        <div className="flex flex-wrap items-center gap-2 border-t pt-3">
          <Button type="button" variant="outline" disabled={diagnosing} onClick={()=>{ setFollow(!follow); if (!follow) setTick(v=>v+1); }}>{follow ? '暂停跟随' : '继续跟随'}</Button>
          <Button type="button" variant="outline" disabled={loading || diagnosing} onClick={()=>setTick(v=>v+1)}>{loading ? '读取中…' : '刷新日志'}</Button>
          <Button type="button" variant="outline" disabled={!data} onClick={()=>setCleared({previous:data?.text ? data.text.split('\n') : [],lines:[]})}>清屏</Button>
          {cleared && <Button type="button" variant="outline" onClick={()=>setCleared(null)}>显示历史</Button>}
          <Button type="button" variant="outline" disabled={!data} onClick={download}>下载脱敏日志</Button>
          <span className="text-xs text-muted-foreground">{follow ? '每 3 秒自动读取，关闭窗口停止' : '已暂停自动读取'}</span>
        </div>
      </section>
      <section className="space-y-2 rounded-xl border p-4" aria-label="日志存储设置">
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="space-y-1 text-sm">日志总大小上限（MiB）<Input type="number" min={1} max={1024} step={1} value={sizeValue ?? data?.max_mib ?? 50} disabled={savingLogs || data?.max_mib === undefined} onChange={e=>setSizeValue(e.target.value)} /></label>
          <div className="space-y-1 text-sm"><label htmlFor="log-retention">保留时长</label><div className="flex gap-2">
            <Input id="log-retention" type="number" min={1} max={720/retentionUnit} step={1} value={retentionValue ?? (data?.retention_hours || 1)/retentionUnit} disabled={savingLogs || data?.retention_hours === undefined} onChange={e=>setRetentionValue(e.target.value)} />
            <select className="select" aria-label="日志保留时长单位" value={retentionUnit} disabled={savingLogs || data?.retention_hours === undefined} onChange={e=>{setRetentionUnit(Number(e.target.value));setRetentionValue('1');}}><option value={1}>小时</option><option value={24}>天</option></select>
          </div></div>
        </div>
        <Button type="button" variant="outline" disabled={savingLogs || loading || diagnosing || data?.retention_hours === undefined} onClick={()=>void saveLogging()}>保存限制并清理过期日志</Button>
        <p className="muted">默认保留 1 小时、最多 50 MiB；按 UTC 日期轮转，单段达到 1 MiB 也会归档为 gzip。大小上限包含当前文件和压缩归档，超过任一限制先清理最旧整段，可能少于完整保留时长。</p>
        <Button type="button" variant="outline" disabled={savingLogs || loading || diagnosing || data?.logging_enabled === undefined} onClick={()=>void saveLogging(!data?.logging_enabled)}>{savingLogs ? '保存中…' : data?.logging_enabled === false ? '开启日志记录' : '关闭并清理日志'}</Button>
        <p className="muted">{source === 'panel' ? '当前控制台应用日志' : '所选节点的 Agent、Xray、Mihomo 及受管历史内核日志'}：{data?.logging_enabled === undefined ? '状态尚未取得（旧版 Agent 需升级）' : data.logging_enabled ? '开启' : '关闭'}。关闭立即删除这些日志及归档，重启后仍生效；不清理系统 journal 或自定义配置直接写入的日志。</p>
      </section>
      <section className="min-w-0 space-y-2" aria-label="日志内容">
        {error && <div role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive"><p className="font-medium">日志读取失败</p><p className="mt-1 break-words">{error}</p>{data && <p className="mt-1">下方保留上次成功读取的记录。</p>}</div>}
        <div className="flex flex-wrap items-center justify-between gap-1 text-xs text-muted-foreground">
          <span className="font-medium text-foreground">{sourceNames[source] || source}</span>
          <span role="status">{data ? `采样 ${stamp(data.sampled_at)} · 最多 500 行` : error ? '未取得日志' : '等待日志返回…'}</span>
        </div>
        <pre tabIndex={0} aria-label="日志记录" className="min-h-48 max-h-[45vh] overflow-auto whitespace-pre-wrap break-all rounded-xl bg-slate-950 p-4 text-xs leading-6 text-slate-100">{readableLogs || (cleared ? '已清屏，等待新日志。服务器日志未删除，可点击“显示历史”恢复。' : data ? '当前来源或筛选范围内没有日志。可取消代理筛选或切换来源。' : '正在读取…')}</pre>
        <p className="text-xs text-muted-foreground">采样时间及行首数字时间戳按浏览器时区 {localZone} 显示；下载保留原始脱敏内容。</p>
      </section>
      <section className="space-y-3 rounded-xl border p-4" aria-label="VPS 时间同步">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="space-y-1"><h3 className="text-sm font-semibold">VPS 时间同步</h3><p className="text-xs text-muted-foreground">作用于当前查看的控制台所在 VPS，保留系统时区；不会修改其他远程节点。</p></div>
          <Button type="button" variant="outline" disabled={!!task || submitting} onClick={async()=>{
            if (!window.confirm('在当前查看的控制台所在 VPS 启用并校准系统时间？必要时安装系统时间服务，可能改变整机时间；保持现有时区。此操作不修改其他远程 Agent。')) return;
            setSubmitting(true); setTaskFailed(false); setTaskMessage('正在提交校时任务…');
            const id = crypto.randomUUID();
            try { await requester.current('/federation/task','POST',{id,action:'sync-time'}); setTask(id); setTaskMessage('校时任务已提交，正在等待结果…'); }
            catch(e) { setTaskFailed(true); setTaskMessage(errorText(e)); }
            finally { setSubmitting(false); }
          }}>{task || submitting ? '正在校时…' : '启用本 VPS 自动校时'}</Button>
        </div>
        <p className="text-sm">{data?.clock ? `所选节点上报：${data.clock.detail || '时间状态未知'}` : '校时任务会检查此 VPS 的时间服务并返回结果。'}{data?.clock?.timezone && ` · 系统时区：${data.clock.timezone}`}{data?.clock?.ntp_enabled === false && ' · 自动时间同步未启用'}</p>
        {taskMessage && <div role={taskFailed ? 'alert' : 'status'} className={`rounded-lg border p-3 text-sm ${taskFailed ? 'border-destructive/30 bg-destructive/5 text-destructive' : 'bg-muted/30'}`}><p className="mb-1 font-medium">{taskFailed ? '自动校时未完成' : task || submitting ? '自动校时进行中' : '自动校时结果'}</p><pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words font-sans">{taskMessage}</pre></div>}
      </section>
      {lineId && <details className="space-y-3 rounded-xl border p-4">
        <summary className="cursor-pointer text-sm font-semibold">分段连接检查</summary>
        <p className="text-sm text-muted-foreground">先验证出口，再检查完整链路。会产生少量请求流量，不重启现有代理。</p>
        <label className="block space-y-1 text-sm">检查网址<Input type="url" value={target} onChange={e=>setTarget(e.target.value)} /></label>
        {!until ? <Button type="button" variant="outline" onClick={()=>setUntil(Date.now()+600000)}>开启 10 分钟详细诊断窗口</Button> : <Button type="button" disabled={diagnosing} onClick={()=>void diagnose()}>{diagnosing ? '正在分段检查…' : '执行详细检查'}</Button>}
        {!!until && <p className="text-xs text-muted-foreground">每次检查的临时内核完成即退出；窗口到期后停止接受新检查。</p>}
        {diagnostic && <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted p-3 text-xs">{diagnostic}</pre>}
      </details>}
      <details className="rounded-xl border p-4 text-sm text-muted-foreground">
        <summary className="cursor-pointer font-medium text-foreground">日志保留与排障说明</summary>
        <div className="mt-3 space-y-2"><p>控制台应用日志来自当前查看的 VPS；节点及内核日志来自所选节点。</p><p>控制台和每个节点分别保存日志限制。写入时及后台定期检查并清理；历史归档使用 gzip，当天活动日志用于持续写入。系统 journal 单独管理。</p><p>各跳 SNI 必须匹配该跳的 REALITY 目标或 TLS 证书。accepted 只表示已接收连接，不代表成功出网。</p></div>
      </details>
    </div>
  </Modal>;
}
