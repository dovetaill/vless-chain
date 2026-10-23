import { useEffect, useState, type ReactNode } from 'react';
import { api } from '../api';
import type { AccessPolicy, External, Line, PanelState, Resource, Source } from '../types';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Card, CardHeader, CardTitle, CardContent } from './ui/card';
import { Modal, Field, bytes, errorText, field, number, stamp } from './shared';

type Allocation = { granted:number;used:number;rate:number;expires:number };
type ResourceState = {resources:Resource[];ledgers:Record<string,{cycle:number;end:number;allocations:Record<string,Allocation>}>};
const emptyResource:Resource={id:'',name:'',members:[],monthly_gib:null,mbps:null,reset_day:1,reset_hour:0,timezone:'Asia/Shanghai'};

export function Resources({state,saved}:{state:PanelState;saved:()=>Promise<void>}) {
  const [data,setData]=useState<ResourceState>({resources:[],ledgers:{}});
  const [error,setError]=useState(''); const [modal,setModal]=useState<ReactNode>(null);
  const load=async()=>{setData(await api<ResourceState>('/resources'));};
  useEffect(()=>{void load().catch(e=>setError(errorText(e)));const timer=setInterval(()=>void load().catch(e=>setError(errorText(e))),10000);return()=>clearInterval(timer);},[]);
  const members=[...state.nodes.map(n=>({id:`node:${n.id}`,name:`服务器 · ${n.name}`})),...state.externals.map(e=>({id:`external:${e.id}`,name:`外部代理 · ${e.name}`}))];
  function edit(resource:Resource){setModal(<ResourceEditor resource={resource} resources={data.resources} members={members} close={()=>setModal(null)} saved={async()=>{await load();await saved();}}/>);}
  return <Card><CardHeader><CardTitle>资源额度</CardTitle></CardHeader><CardContent className="space-y-5">
    <p className="muted">各资源的所有关联代理共享带宽和周期额度。公网转发的用户上传、下载都消耗服务器出站，资源带宽限制两者合计。统计范围为本系统代理业务，不包含其他程序或外部账号在别处的用量。租户链路倍率不重复乘入资源用量。</p>
    <Button onClick={()=>edit({...emptyResource})} disabled={state.busy}>新建资源组</Button>
    {error&&<p role="alert" className="text-red-600">{error}</p>}
    {data.resources.map(r=>{const ledger=data.ledgers[r.id];const allocations=Object.values(ledger?.allocations||{});const granted=allocations.reduce((n,a)=>n+a.granted,0);const used=allocations.reduce((n,a)=>n+a.used,0);return <div key={r.id} className="rounded-lg border p-4 space-y-2">
      <div className="flex flex-wrap justify-between gap-3"><strong>{r.name}</strong><div className="flex gap-2"><Button size="sm" variant="outline" onClick={()=>edit(r)} disabled={state.busy}>设置 / 成员</Button><Button size="sm" variant="ghost" disabled={state.busy||r.members.length>0} onClick={async()=>{try{await api(`/resources/${r.id}`,'DELETE');await load();await saved();}catch(e){setError(errorText(e));}}}>删除空组</Button></div></div>
      <p className="text-sm">额度 {r.monthly_gib==null?'不限':`${r.monthly_gib} GiB`} · 上下行合计 {r.mbps==null?'不限':`${r.mbps} Mbps`} · {r.members.length} 个成员</p>
      <p className="text-sm">已分配 {bytes(granted)} · 已上报使用 {bytes(used)} · 未分配 {r.monthly_gib==null?'不限':bytes(Math.max(0,r.monthly_gib*1024**3-granted))}</p>
      <p className="muted">每月 {r.reset_day} 日 {r.reset_hour}:00，{r.timezone}；下次重置 {ledger?.end?stamp(ledger.end):'待计算'}。已分配包括入口预留，使用量随申请更新，可能延迟。</p>
      <p className="text-xs text-slate-500">{r.members.map(id=>members.find(m=>m.id===id)?.name||id).join('、')||'尚未添加成员'}</p>
    </div>;})}
  </CardContent>{modal}</Card>;
}
function ResourceEditor({resource,resources,members,close,saved}:{resource:Resource;resources:Resource[];members:{id:string;name:string}[];close:()=>void;saved:()=>Promise<void>}){
  const [picked,setPicked]=useState(resource.members);
  return <Modal title={resource.id?'编辑资源额度':'新建资源额度'} close={close} submit="保存并下发" action={async data=>{
    await api('/resources','POST',{...resource,name:field(data,'name'),members:picked,monthly_gib:number(data,'quota'),mbps:number(data,'mbps'),reset_day:number(data,'day'),reset_hour:number(data,'hour'),timezone:field(data,'zone')});await saved();close();
  }}>
    <Field label="名称"><Input name="name" defaultValue={resource.name} required maxLength={120}/></Field>
    <div className="grid gap-4 sm:grid-cols-2"><Field label="周期总额度（GiB）" hint="留空不限制；1 TiB = 1024 GiB"><Input name="quota" type="number" min="0.000001" step="any" defaultValue={resource.monthly_gib??''}/></Field><Field label="上下行合计共享上限（Mbps）" hint="例如 1000 表示上传与下载合计 1Gbps；留空不限"><Input name="mbps" type="number" min="0.001" step="any" defaultValue={resource.mbps??''}/></Field></div>
    <div className="grid gap-4 sm:grid-cols-3"><Field label="每月重置日"><Input name="day" type="number" min={1} max={31} defaultValue={resource.reset_day} required/></Field><Field label="重置小时"><Input name="hour" type="number" min={0} max={23} defaultValue={resource.reset_hour} required/></Field><Field label="时区"><Input name="zone" defaultValue={resource.timezone} list="resource-timezones" required/></Field><datalist id="resource-timezones"><option>Asia/Shanghai</option><option>America/Los_Angeles</option><option>America/New_York</option><option>UTC</option></datalist></div>
    <p className="muted">月底没有该日期时使用最后一天。账期独立于客户租期；已有额度分配的周期不能直接修改账期。</p>
    <fieldset className="space-y-2"><legend className="mb-2 text-sm font-medium">共享成员</legend>{members.map(m=>{const other=resources.find(r=>r.id!==resource.id&&r.members.includes(m.id));return <label key={m.id} className="flex items-center gap-2 text-sm"><input type="checkbox" disabled={!!other} checked={picked.includes(m.id)} onChange={e=>setPicked(e.target.checked?[...picked,m.id]:picked.filter(id=>id!==m.id))}/>{m.name}{other&&`（已属于 ${other.name}）`}</label>;})}</fieldset>
  </Modal>;
}

export function RulesLibrary(){
  const [sources,setSources]=useState<Source[]>([]);const [filter,setFilter]=useState('');const [error,setError]=useState('');const [busy,setBusy]=useState('');const [modal,setModal]=useState<ReactNode>(null);
  const load=async()=>setSources(await api<Source[]>('/rules'));
  useEffect(()=>{void load().catch(e=>setError(errorText(e)));},[]);
  async function action(s:Source,operation:string){setBusy(s.id);setError('');try{await api(`/rules/${s.id}/${operation}`,'POST',{});await load();}catch(e){setError(errorText(e));}finally{setBusy('');}}
  function edit(s?:Source){setModal(<Modal title={s?'编辑规则源':'导入规则源'} close={()=>setModal(null)} submit="保存" action={async data=>{await api('/rules','POST',{id:s?.id||'',name:field(data,'name'),url:field(data,'url'),auto_apply:data.has('auto')});await load();setModal(null);}}>
    <Field label="名称"><Input name="name" defaultValue={s?.name} required/></Field><Field label="规则文件 HTTPS URL" hint="支持 Clash classical / domain 的 payload 列表；不导入完整客户端配置"><Input name="url" type="url" defaultValue={s?.url} required/></Field>
    <label className="flex gap-2 text-sm"><input name="auto" type="checkbox" defaultChecked={s?.auto_apply}/>每天下载后自动应用（允许范围随上游变化）</label>
  </Modal>);}
  return <Card><CardHeader><CardTitle>网站规则库</CardTitle></CardHeader><CardContent className="space-y-4">
    <p className="muted">外部规则默认每日下载新版本，手动应用；也可选择自动应用。内置规则随面板维护，需手动应用，可编辑为外部来源。严格白名单只采用完整域名和域名后缀，其他条目展示为未采用。规则包含公共服务时，允许范围可能超出单一网站。</p>
    <div className="flex flex-wrap gap-3"><Input aria-label="筛选网站规则" placeholder="搜索网站 / 分类" value={filter} onChange={e=>setFilter(e.target.value)} className="sm:max-w-xs"/><Button onClick={()=>edit()}>导入规则源</Button><Button variant="outline" disabled={!!busy} onClick={()=>void load().catch(e=>setError(errorText(e)))}>刷新状态</Button></div>
    {error&&<p role="alert" className="text-red-600">{error}</p>}
    {sources.filter(s=>`${s.name} ${s.category}`.toLowerCase().includes(filter.toLowerCase())).map(s=><div key={s.id} className="rounded-lg border p-4 space-y-2">
      <div className="flex flex-wrap justify-between gap-2"><strong>{s.name} <span className="text-xs font-normal text-slate-500">{s.category}</span></strong><div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" disabled={!!busy} onClick={()=>void action(s,'update')}>{busy===s.id?'处理中…':s.url?'下载更新':'载入内置规则'}</Button><Button size="sm" variant="outline" disabled={!!busy||!s.pending} onClick={()=>void action(s,'apply')}>应用新版本</Button><Button size="sm" variant="ghost" disabled={!!busy} onClick={()=>edit(s)}>编辑</Button></div></div>
      <p className="text-xs break-all">{s.url||'内置站点域名 · 随面板版本维护，可改用 HTTPS 规则源'}</p>
      <p className="text-sm">已应用 {s.active?.count??0} 条 · 待应用 {s.pending?.count??0} 条{s.pending&&`（新增 ${s.added} / 移除 ${s.removed}）`} · {s.url&&s.auto_apply?'自动应用':'手动应用'}</p>
      <p className="text-xs text-slate-500">检查：{s.checked_at?stamp(s.checked_at):'尚未下载'} · 应用：{s.applied_at?stamp(s.applied_at):'尚未应用'}</p>
      {s.error&&<p className="text-sm text-amber-700">{s.error}</p>}
      {(s.pending||s.active)?.excluded_count? <details className="text-xs"><summary>未采用 {(s.pending||s.active)!.excluded_count} 条（最多展示 100 条）</summary><pre className="mt-2 whitespace-pre-wrap break-all">{(s.pending||s.active)!.excluded.join('\n')}</pre></details>:null}
    </div>)}
  </CardContent>{modal}</Card>;
}

export function AccessPolicyEditor({line,close,saved,request=api}:{line:Line;close:()=>void;saved:()=>Promise<void>;request?:typeof api}){
  const [policy,setPolicy]=useState<AccessPolicy|null>(null);const [sources,setSources]=useState<Source[]>([]);const [filter,setFilter]=useState('');const [error,setError]=useState('');
  useEffect(()=>{void Promise.all([request<AccessPolicy>(`/access/${line.id}`),request<Source[]>('/rules')]).then(([p,s])=>{setPolicy(p);setSources(s);}).catch(e=>setError(errorText(e)));},[line.id,request]);
  const ports=(text:string)=>text.split(/[\s,，]+/).filter(Boolean).map(s=>{if(!/^\d+$/.test(s)||+s<1||+s>65535)throw Error('端口须为 1–65535，使用逗号或空格分隔');return +s;});
  return <Modal title={`访问限制 · ${line.name}`} close={close} wide submit={policy?'保存并下发':undefined} action={policy?async data=>{
    await request(`/access/${line.id}`,'POST',{...policy,domains:field(data,'domains').split(/\s+/).filter(Boolean),denied_ports:ports(field(data,'denied')),udp_ports:ports(field(data,'udp_ports')),max_connections:number(data,'connections')});await saved();close();
  }:undefined}>
    {error&&<p role="alert" className="text-red-600">{error}</p>}
    {!policy?<p>正在读取策略…</p>:<>
      <label className="flex gap-2 text-sm"><input type="checkbox" checked={policy.restricted} onChange={e=>setPolicy({...policy,restricted:e.target.checked})}/>仅允许所选网站及自定义域名，其余拒绝</label>
      {policy.restricted&&<><p className="muted">所有获准流量沿用当前代理出口。严格模式拒绝裸 IP 目标，客户端需向代理传递域名；本机改 hosts 或伪造 SNI 不会替换服务器解析的目标。未应用的规则源不授予访问权限。</p>
        <Input aria-label="搜索预设网站" placeholder="筛选预设网站…" value={filter} onChange={e=>setFilter(e.target.value)}/>
        <p className="text-sm">已选 {policy.sources.length} 组</p>
        {[...new Set(sources.map(s=>s.category))].map(category=><fieldset key={category} className="space-y-2"><legend className="text-sm font-medium">{category}</legend><div className="flex flex-wrap gap-2">{sources.filter(s=>s.category===category&&s.name.toLowerCase().includes(filter.toLowerCase())).map(s=><label key={s.id} className={`inline-flex items-center gap-2 rounded-md border p-2 text-sm ${policy.sources.includes(s.id)?'bg-blue-50 border-blue-300':''}`}><input type="checkbox" checked={policy.sources.includes(s.id)} onChange={e=>setPolicy({...policy,sources:e.target.checked?[...policy.sources,s.id]:policy.sources.filter(id=>id!==s.id)})}/>{s.name}{!s.active&&<small className="text-amber-700">尚无有效规则</small>}</label>)}</div></fieldset>)}
        <Field label="自定义域名（每行一个，包含子域名）"><textarea name="domains" className="textarea" rows={4} defaultValue={policy.domains.join('\n')}/></Field>
      </>}
      {!policy.restricted&&<input type="hidden" name="domains" value={policy.domains.join('\n')}/>}
      <Field label="禁止目标端口（逗号或空格分隔）"><Input name="denied" defaultValue={policy.denied_ports.join(', ')} placeholder="例如 25, 465, 587"/></Field>
      <Field label="UDP 策略"><select className="select" value={policy.udp||'allow'} onChange={e=>setPolicy({...policy,udp:e.target.value})}><option value="allow">允许（仍受网站和禁用端口限制）</option><option value="deny">全部禁止</option><option value="ports">仅允许指定目标端口</option></select></Field>
      <Field label="UDP 允许端口"><Input name="udp_ports" defaultValue={policy.udp_ports.join(', ')} placeholder="例如 53, 443, 123, 3478"/></Field>
      <Field label="并发业务连接 / UDP 关联上限" hint="留空沿用 1024 的资源保护上限；这不是设备数量"><Input name="connections" type="number" min={1} max={1024} defaultValue={policy.max_connections??''}/></Field>
    </>}
  </Modal>;
}

type Candidate={host:string;ids:string[];matching:{id:string;name:string}[]};
export function AutoGroups({state,ids=[],close,saved}:{state:PanelState;ids?:string[];close:()=>void;saved:()=>Promise<void>}){
  const [items,setItems]=useState<Candidate[]>([]);const [choices,setChoices]=useState<Record<string,string>>({});const [error,setError]=useState('');const [loaded,setLoaded]=useState(false);
  useEffect(()=>{void api<Candidate[]>('/resources/candidates','POST',{ids}).then(items=>{setItems(items);setChoices(Object.fromEntries(items.map(c=>[c.host,c.matching.length===1?c.matching[0].id:c.matching.length>1?'skip':'new'])));setLoaded(true);}).catch(e=>setError(errorText(e)));},[]);
  return <Modal title="自动分组 · 确认候选" description="只处理未分组链接；相同域名或直接填写的相同 IP 生成候选，不通过 DNS 解析合并。确认它们是否确实共享套餐。" close={close} submit={loaded&&items.length?'应用选择':undefined} action={loaded?async()=>{
    for(const item of items){const choice=choices[item.host];if(choice==='skip')continue;await api('/resources/groups','POST',{ids:item.ids,group:choice==='new'?null:choice,name:choice==='new'?item.host:'',automatic:true});}
    await saved();close();
  }:undefined}>
    {error&&<p role="alert" className="text-red-600">{error}</p>}{loaded&&!items.length&&<p>没有可处理的未分组链接。</p>}
    {items.map(c=><Field key={c.host} label={c.host} hint={c.ids.map(id=>state.externals.find(e=>e.id===id)?.name||id).join('、')}><select className="select" value={choices[c.host]} onChange={e=>setChoices({...choices,[c.host]:e.target.value})}><option value="new">新建共享资源组</option>{c.matching.map(r=><option key={r.id} value={r.id}>加入 {r.name}</option>)}<option value="skip">保持独立</option></select></Field>)}
  </Modal>;
}
export function ManualGroup({external,state,close,saved}:{external:External;state:PanelState;close:()=>void;saved:()=>Promise<void>}){
  const current=state.resources?.find(r=>r.members.includes(`external:${external.id}`));
  return <Modal title={`手动分组 · ${external.name}`} close={close} submit="保存分组" action={async data=>{const group=field(data,'group');await api('/resources/groups','POST',{ids:[external.id],group:group||null,name:field(data,'name'),automatic:false});await saved();close();}}>
    <Field label="已有资源组"><select name="group" className="select" defaultValue={current?.id||''}><option value="">移出分组 / 新建组</option>{state.resources?.map(r=><option key={r.id} value={r.id}>{r.name}</option>)}</select></Field><Field label="新建组名称" hint="未选择已有组时使用；留空则移出分组"><Input name="name"/></Field>
  </Modal>;
}
export function BatchImport({state,close,saved}:{state:PanelState;close:()=>void;saved:()=>Promise<void>}){
  const [groupIds,setGroupIds]=useState<string[]|null>(null);const [imported,setImported]=useState<External[]>([]);
  if(groupIds)return <AutoGroups state={{...state,externals:[...state.externals,...imported.filter(e=>!state.externals.some(old=>old.id===e.id))]}} ids={groupIds} close={close} saved={saved}/>;
  return <Modal title="批量导入外部代理" close={close} submit="导入" action={async data=>{
    const result=await api<External[]>('/externals/batch','POST',{urls:field(data,'urls').split(/\r?\n/).map(v=>v.trim()).filter(Boolean)});setImported(result);await saved();if(data.has('group'))setGroupIds(result.map(e=>e.id));else close();
  }}><Field label="分享链接（每行一条）"><textarea name="urls" className="textarea" rows={8} required/></Field><label className="flex gap-2 text-sm"><input type="checkbox" name="group"/>导入后自动识别分组候选</label></Modal>;
}
