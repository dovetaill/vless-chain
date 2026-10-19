import { Diagnostics } from "./diagnostics";
import { useState, type ReactNode } from "react";
import { Download, MoreHorizontal, Plus, Search } from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { api } from "../api";
import type { Line, PanelState } from "../types";
import { expiringSoon, lowQuota, matchesLine } from "../lib/business";
import { Button } from "./ui/button";
import { Card, CardHeader, CardTitle } from "./ui/card";
import { Input } from "./ui/input";
import { LegacyConfigs } from "./legacy-configs";
import { CopyButton, Empty, Modal, PathView, download, errorText, protocolNames } from "./shared";
import { LimitsDialog, QueryLinkDialog, RentalEditor, RenewalDialog } from "./rental-editor";
import { RentalDates, RentalPrices, RentalSpeed, RentalStatusBadge, RentalUsage, SyncLabel, rentalStatusNames } from "./rental-shared";

function ExportDialog({ lines, close }: { lines: Line[]; close: () => void }) {
  const [result, setResult] = useState<{ links: string; yaml: string; errors: string[]; count: number } | null>(null);
  return <Modal title={`批量导出 · ${lines.length} 条`} submit={result ? undefined : "读取已应用配置"} close={close} action={async () => {
    const valid = lines.filter(line => line.enabled && line.published);
    const errors = lines.filter(line => !line.enabled || !line.published).map(line => `${line.name}：尚未启用或应用，已跳过。`);
    const exports = await Promise.allSettled(valid.map(line => api<{ share: string; node: string }>(`/lines/${line.id}/export`)));
    const links: string[] = [], nodes: string[] = [];
    exports.forEach((value, i) => {
      if (value.status === "fulfilled") { links.push(value.value.share); nodes.push(value.value.node.trim()); }
      else errors.push(`${valid[i].name}：${errorText(value.reason)}`);
    });
    setResult({ links: links.join("\n"), yaml: `proxies:\n${nodes.map(node => node.split("\n").map(row => `  ${row}`).join("\n")).join("\n")}\n`, errors, count: nodes.length });
  }}>
    <p className="muted">仅导出已经启用并应用的代理。Mihomo 导出为节点列表，可合并到客户端配置中的 proxies。</p>
    <p className="text-sm">{lines.map(line => line.name).join("、")}</p>
    {result && <><p role="status" className="text-sm">已读取 {result.count} 条；未导出 {result.errors.length} 条。</p>{result.errors.length > 0 && <ul className="space-y-1 rounded-lg bg-amber-50 p-4 text-xs text-amber-800">{result.errors.map((error, i) => <li key={i}>{error}</li>)}</ul>}{result.count > 0 && <div className="flex flex-wrap gap-2"><CopyButton value={result.links} label="复制分享链接" /><Button type="button" variant="outline" onClick={() => download("proxy-links.txt", result.links)}><Download />下载链接</Button><Button type="button" variant="outline" onClick={() => download("proxies.yaml", result.yaml)}><Download />下载 Mihomo 节点列表</Button></div>}</>}
  </Modal>;
}

export function ProxyList({ state, saved, edit, create, share, probe, apply, configureImport, remote = false }: {
  state: PanelState; saved: () => Promise<void>; edit: (line: Line) => void; create: () => void;
  share: (line: Line) => void; probe: (line: Line) => void; apply: (line?: Line) => void;
  configureImport: (item: { id: string; name: string }) => void;
  remote?: boolean;
}) {
  const [search, setSearch] = useState("");
  const [protocol, setProtocol] = useState("");
  const [status, setStatus] = useState("");
  const [attention, setAttention] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [modal, setModal] = useState<ReactNode>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);
  const close = () => setModal(null);
  const completed = async () => { await saved(); setSelected([]); };
  const all = state.lines.filter(line => line.kind === "vless");
  const lines = all.filter(line => matchesLine(line, state, search)
    && (!protocol || line.access.protocol === protocol) && (!status || line.rental_status === status)
    && (!attention || (attention === "expiring" ? expiringSoon(line, state.time) : lowQuota(line))));
  const chosen = all.filter(line => selected.includes(line.id));
  const disabled = state.busy || pending;
  async function operate(action: "enable" | "disable", targets: Line[]) {
    setPending(true);
    setError("");
    try { await api("/lines/operate", "POST", { line_ids: targets.map(line => line.id), action }); await completed(); }
    catch (error) { setError(errorText(error)); }
    finally { setPending(false); }
  }
  function remove(targets: Line[]) {
    setModal(<Modal title={`删除代理 · ${targets.length} 条`} description="删除代理并撤销其私密查询链接。配置重新下发时，同一服务器上的连接可能短暂重连；离线服务器等待同步。" submit="确认删除并下发" close={close} action={async () => {
      await api("/lines/operate", "POST", { line_ids: targets.map(line => line.id), action: "delete" });
      await completed();
      close();
    }}><p className="text-sm">{targets.map(line => line.name).join("、")}</p></Modal>);
  }
  function unlimited(targets: Line[]) {
    setModal(<Modal title={`设为无限期 · ${targets.length} 条`} description="取消到期限制并下发，不记录收款，不改变现有额度、限速或手动停用状态。流量仍按原周期重置。" submit="确认设为无限期并下发" close={close} action={async () => {
      await api("/lines/operate", "POST", { line_ids: targets.map(line => line.id), action: "unlimited" });
      await completed();
      close();
    }}><p className="text-sm">{targets.map(line => line.name).join("、")}</p></Modal>);
  }
  function filtersChanged(set: (value: string) => void, value: string) { set(value); setSelected([]); }
  return <Card>
    <CardHeader><CardTitle>我的代理 <span className="ml-2 text-sm font-normal text-slate-400">{lines.length} / {all.length}</span></CardTitle>
    </CardHeader>
    <LegacyConfigs state={state} saved={saved} apply={() => apply()} />
    {(state.pending_imports || []).map(item => <div key={item.id} className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4">
      <div><p className="text-sm font-medium">{item.name} · 待手动配置</p><p className="mt-1 text-xs text-amber-700">{item.reason}；当前未运行。</p></div>
      <div className="flex gap-2"><Button variant="outline" size="sm" disabled={disabled} onClick={() => configureImport(item)}>配置代理</Button>
        <Button variant="ghost" size="sm" disabled={disabled} onClick={async () => {
          setPending(true); setError("");
          try { await api(`/legacy-import/${item.id}`, "DELETE"); await saved(); }
          catch (error) { setError(errorText(error)); } finally { setPending(false); }
        }}>移除名称</Button></div>
    </div>)}
    <div className="flex flex-wrap items-center gap-2.5 border-b border-border px-5 py-4" role="group" aria-label="代理搜索与筛选">
      <div className="relative w-full min-w-0 sm:w-72"><Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-slate-400" /><Input className="pl-9" aria-label="搜索代理名称、客户、服务器、地址或端口" placeholder="名称 / 客户 / 服务器 / 地址 / 端口" value={search} onChange={event => filtersChanged(setSearch, event.target.value)} /></div>
      <select className="select h-8.5 w-auto min-w-0 max-w-full py-0" aria-label="筛选协议" value={protocol} onChange={event => filtersChanged(setProtocol, event.target.value)}><option value="">所有协议</option>{Object.entries(protocolNames).map(([value, name]) => <option key={value} value={value}>{name}</option>)}</select>
      <select className="select h-8.5 w-auto min-w-0 max-w-full py-0" aria-label="筛选状态" value={status} onChange={event => filtersChanged(setStatus, event.target.value)}><option value="">所有状态</option>{Object.entries(rentalStatusNames).map(([value, name]) => <option key={value} value={value}>{name}</option>)}</select>
      <select className="select h-8.5 w-auto min-w-0 max-w-full py-0" aria-label="筛选到期与余量" value={attention} onChange={event => filtersChanged(setAttention, event.target.value)}><option value="">所有到期 / 余量</option><option value="expiring">7 天内到期</option><option value="low">剩余 ≤10%</option></select>
      {(search || protocol || status || attention) && <Button variant="ghost" size="sm" onClick={() => { setSearch(""); setProtocol(""); setStatus(""); setAttention(""); setSelected([]); }}>清除筛选</Button>}
    </div>
    {chosen.length > 0 && <div className="flex flex-wrap items-center gap-2 border-b border-blue-100 bg-blue-50 px-5 py-3" aria-label="批量操作"><span className="mr-2 text-sm font-medium">已选 {chosen.length} 条</span>
      <Button variant="outline" size="sm" disabled={disabled} onClick={() => void operate("enable", chosen)}>启用</Button><Button variant="outline" size="sm" disabled={disabled} onClick={() => void operate("disable", chosen)}>停用</Button>
      {!remote && <Button variant="outline" size="sm" disabled={disabled} onClick={() => setModal(<RenewalDialog lines={chosen} close={close} saved={completed} />)}>收款 / 续费</Button>}
      {!remote && <Button variant="outline" size="sm" disabled={disabled} onClick={() => unlimited(chosen)}>设为无限期（自用）</Button>}
      {!remote && <Button variant="outline" size="sm" disabled={disabled} onClick={() => setModal(<LimitsDialog lines={chosen} close={close} saved={completed} />)}>额度 / 限速</Button>}
      <Button variant="outline" size="sm" disabled={disabled} onClick={() => setModal(<ExportDialog lines={chosen} close={close} />)}>导出</Button>
      <Button variant="outline" size="sm" disabled={disabled} onClick={() => remove(chosen)}>删除</Button><Button variant="ghost" size="sm" onClick={() => setSelected([])}>取消选择</Button>
    </div>}
    {error && <p role="alert" className="m-4 rounded-lg bg-red-50 p-3 text-sm text-red-700">{error}</p>}
    {lines.length ? <div className="overflow-x-auto" tabIndex={0} role="region" aria-label="代理列表"><table className="data-table"><thead><tr>
      <th><input type="checkbox" aria-label="选择全部筛选结果" disabled={disabled} checked={lines.every(line => selected.includes(line.id))} ref={element => { if (element) element.indeterminate = lines.some(line => selected.includes(line.id)) && !lines.every(line => selected.includes(line.id)); }} onChange={event => setSelected(event.target.checked ? lines.map(line => line.id) : [])} /></th>
      <th>名称 / 客户 / 路径</th><th>协议 / 入口</th><th>状态 / 同步</th><th>周期用量 / 余量</th><th>实时网速</th><th>到期 / 重置</th><th>报价</th><th className="text-right">操作</th>
    </tr></thead><tbody>{lines.map(line => <tr key={line.id}>
      <td><input type="checkbox" aria-label={`选择 ${line.name}`} disabled={disabled} checked={selected.includes(line.id)} onChange={event => setSelected(old => event.target.checked ? [...old, line.id] : old.filter(id => id !== line.id))} /></td>
      <td><p className="mb-1 text-sm font-semibold">{line.name}</p><p className="mb-2 text-xs text-slate-500">{state.customers.find(customer => customer.id === line.rental?.customer_id)?.name || "自用"}</p><PathView line={line} state={state} /></td>
      <td><p className="text-xs font-medium">{protocolNames[line.access.protocol]}</p><p className="mono mt-2 text-xs text-slate-400">{line.access.public_host || line.access.host || state.nodes.find(node => node.id === line.hops[0]?.node_id)?.host}<br />端口 {line.access.public_port || line.hops[0]?.port}</p></td>
      <td><RentalStatusBadge status={line.rental_status} /><p className="mt-2"><SyncLabel status={line.sync_status} /></p>{!line.published && <p className="mt-1 text-xs text-amber-600">有待应用的配置</p>}{state.checks[line.id]?.ok === false && <p className="mt-1 text-xs text-amber-600">最近连接检查未通过</p>}</td>
      <td><RentalUsage line={line} /></td><td><RentalSpeed line={line} now={state.time} /></td><td><RentalDates line={line} /></td><td><RentalPrices line={line} /></td>
      <td><div className="flex items-center justify-end gap-1">
        {!remote && <Button variant="outline" size="sm" disabled={disabled} onClick={() => setModal(<RentalEditor line={line} state={state} close={close} saved={saved} />)}>出租信息</Button>}
        {!remote && <Button variant="ghost" size="sm" disabled={disabled || !line.rental} onClick={() => setModal(<RenewalDialog lines={[line]} close={close} saved={saved} />)}>收款 / 续费</Button>}
        <DropdownMenu.Root><DropdownMenu.Trigger className="row-menu-trigger" aria-label={`${line.name} 更多操作`} disabled={disabled}><MoreHorizontal className="size-4" /></DropdownMenu.Trigger><DropdownMenu.Portal><DropdownMenu.Content className="row-menu-content" align="end" sideOffset={4} collisionPadding={8}>
          <DropdownMenu.Item onSelect={() => void operate(line.enabled ? "disable" : "enable", [line])}>{line.enabled ? "手动停用" : "解除手动停用"}</DropdownMenu.Item>
          <DropdownMenu.Item onSelect={() => setModal(<LimitsDialog lines={[line]} close={close} saved={saved} />)}>调整额度 / 限速</DropdownMenu.Item>
          {!remote && <DropdownMenu.Item onSelect={() => unlimited([line])}>设为无限期（自用）</DropdownMenu.Item>}
          <DropdownMenu.Item disabled={!line.published || !line.enabled} onSelect={() => share(line)}>分享配置</DropdownMenu.Item>
          {!remote && <DropdownMenu.Item disabled={!line.rental || !line.published} onSelect={() => setModal(<QueryLinkDialog line={line} close={close} saved={saved} />)}>客户私密查询链接</DropdownMenu.Item>}
          <DropdownMenu.Item onSelect={() => edit(line)}>编辑代理配置</DropdownMenu.Item>
          <DropdownMenu.Item disabled={!line.published || !line.enabled} onSelect={() => probe(line)}>检查连接</DropdownMenu.Item>
          <DropdownMenu.Item onSelect={() => setModal(<Diagnostics nodes={state.nodes.filter(n => line.hops.some(h => h.node_id === n.id))} lineId={line.id} close={close} />)}>日志与诊断</DropdownMenu.Item>
          <DropdownMenu.Item onSelect={() => apply(line)}>应用配置修改</DropdownMenu.Item>
          <DropdownMenu.Item className="text-red-600" onSelect={() => remove([line])}>删除代理</DropdownMenu.Item>
        </DropdownMenu.Content></DropdownMenu.Portal></DropdownMenu.Root>
      </div></td>
    </tr>)}</tbody></table></div> : <Empty title={all.length ? "没有匹配结果" : "暂无代理"} action={!all.length && <Button onClick={create}><Plus />新建代理</Button>}>新建代理后，在「出租信息」分配客户并设置报价。</Empty>}
    <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border px-5 py-4 text-xs text-slate-400"><span>{lines.length} 条 · 切换筛选会清除选择</span>{state.lines.some(line => !line.published) && <Button variant="outline" size="sm" disabled={disabled} onClick={() => apply()}>应用待处理配置修改</Button>}</div>
    {modal}
  </Card>;
}
