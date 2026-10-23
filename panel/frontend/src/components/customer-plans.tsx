import { useState, type ReactNode } from "react";
import { Plus } from "lucide-react";
import { api } from "../api";
import type { CustomerPlan, PanelState, Prices } from "../types";
import { beijingTime, money, parseMoney, terms } from "../lib/business";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Input } from "./ui/input";
import { CopyButton, Field, Modal, bytes, field, number } from "./shared";
import { RenewalDialog } from "./rental-editor";

export function CustomerPlans({ state, saved }: { state: PanelState; saved: () => Promise<void> }) {
  const [modal, setModal] = useState<ReactNode>(null);
  const plans = state.customer_plans || [];
  const close = () => setModal(null);
  return <Card><CardHeader className="flex flex-wrap items-center justify-between gap-3"><CardTitle>客户共享套餐</CardTitle><Button disabled={state.busy || !state.customers.length} onClick={() => setModal(<PlanEditor state={state} close={close} saved={saved} />)}><Plus />新建共享套餐</Button></CardHeader><CardContent className="space-y-4">
    <p className="muted">同一套餐的成员共享总额度与租期，可给成员设置额外硬上限。独立线路的租期、用量和凭据保留原样，不会自动合并。套餐只覆盖本面板管理的线路。</p>
    {!state.customers.length && <p className="text-sm text-amber-700">请先创建客户档案。</p>}
    {!plans.length && <p className="muted">先新建代理并选择「仅保存草稿」，再绑定套餐、登记收款，最后应用线路。</p>}
    {plans.map(plan => {
      const ids = plan.members.flatMap(member => member.line_ids);
      const applied = ids.filter(id => state.lines.some(line => line.id === id && line.sync_status === "synced")).length;
      return <section key={plan.id} className="space-y-3 rounded-lg border p-4">
        <div className="flex flex-wrap items-start justify-between gap-3"><div><h3 className="font-medium">{plan.name}</h3><p className="muted mt-1">{state.customers.find(customer => customer.id === plan.customer_id)?.name || plan.customer_id} · {plan.members.length} 名客户团队成员 · {ids.length} 条线路</p></div><div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" disabled={state.busy} onClick={() => setModal(<PlanEditor plan={plan} state={state} close={close} saved={saved} />)}>套餐 / 成员设置</Button><Button size="sm" variant="outline" disabled={state.busy} onClick={() => setModal(<PlanQueryLink plan={plan} close={close} />)}>套餐查询链接</Button><Button size="sm" disabled={state.busy || !plan.rental} onClick={() => setModal(<RenewalDialog lines={[{ id: `plan:${plan.id}`, name: plan.name, rental: plan.rental }]} close={close} saved={saved} />)}>开通 / 续费订单</Button></div></div>
        <div className="grid gap-2 text-sm sm:grid-cols-2"><p>共享总额 {bytes(plan.monthly_gib * 1024 ** 3)} · 已上报扣量 {bytes(plan.usage.used)}</p><p>已授权 {bytes(plan.usage.reserved)} · 尚未授权 {bytes(Math.max(0, plan.monthly_gib * 1024 ** 3 - plan.usage.reserved))}</p><p>到期 {plan.rental?.permanent ? "无限期" : beijingTime(plan.rental?.expires_at)}</p><p>下次重置 {plan.rental?.next_reset_at ? beijingTime(plan.rental.next_reset_at) : "待开通"} · 北京时间</p></div>
        <p className="text-xs text-slate-500">{terms.map(term => `${term.name} ${money(plan.rental?.prices[term.id])}`).join(" · ")} · 已同步线路 {applied}/{ids.length}</p>
        <div className="space-y-2">{plan.members.map(member => <div key={member.id} className="rounded-lg bg-slate-50 p-3 text-sm"><div className="flex flex-wrap items-center justify-between gap-2"><p>{member.name} · {member.monthly_gib == null ? "不设额外成员硬上限" : `成员上限 ${bytes(member.monthly_gib * 1024 ** 3)}`} · 已上报扣量 {bytes(member.usage.used)}</p><Button size="sm" variant="outline" disabled={state.busy} onClick={() => setModal(<PlanQueryLink plan={plan} member={member} close={close} />)}>成员查询链接</Button></div><p className="muted mt-1">{member.line_ids.map(id => state.lines.find(line => line.id === id)?.name || id).join("、")}</p></div>)}</div>
        <p className="muted">额度在传输准入时预扣，转发失败也可能占用该块。已授权包含已扣和入口预留，扣量上报可能延迟；不能把已授权当作实际使用量或自动退回额度。套餐耗尽或到期影响全部成员。已有成员和线路保留历史归属，离队请停用对应线路。</p>
      </section>;
    })}
  </CardContent>{modal}</Card>;
}

function PlanQueryLink({ plan, member, close }: { plan: CustomerPlan; member?: CustomerPlan["members"][number]; close: () => void }) {
  const [url, setUrl] = useState("");
  return <Modal title={member ? "成员私密查询链接" : "套餐私密查询链接"} description={`${plan.name}${member ? ` / ${member.name}` : ""}`} submit={url ? undefined : "生成新链接并撤销原链接"} close={close} action={async () => {
    const result = await api<{ path: string }>(`/customer-plans/${plan.id}${member ? `/members/${member.id}` : ""}/query-link`, "POST", {});
    setUrl(new URL(result.path, location.origin).href);
  }}>
    <p className="text-sm">{member ? "持有链接可查询该成员及套餐共享总额，不会显示其他成员的明细。" : "持有链接可查看此套餐及全部成员的用量和租期，仅交给客户团队负责人。"}此链接不能登录管理后台；重新生成会撤销同一范围的旧查询链接。</p>
    {url && <><Field label="私密查询链接"><textarea className="textarea mono" readOnly rows={3} value={url} /></Field><CopyButton value={url} label="复制私密查询链接" /></>}
  </Modal>;
}

type DraftMember = Pick<CustomerPlan["members"][number], "id" | "name" | "line_ids" | "monthly_gib">;

function PlanEditor({ plan, state, close, saved }: { plan?: CustomerPlan; state: PanelState; close: () => void; saved: () => Promise<void> }) {
  const [members, setMembers] = useState<DraftMember[]>(() => plan ? plan.members.map(member => ({ id: member.id, name: member.name, line_ids: [...member.line_ids], monthly_gib: member.monthly_gib })) : [{ id: "", name: "成员 1", line_ids: [], monthly_gib: null }]);
  const [membersChanged, setMembersChanged] = useState(false);
  const eligible = state.lines.filter(line => line.plan_eligible === true);
  function update(index: number, patch: Partial<DraftMember>) { setMembers(old => old.map((member, i) => i === index ? { ...member, ...patch } : member)); }
  return <Modal wide protectChanges dirty={membersChanged} title={plan ? "编辑共享套餐与成员" : "新建客户共享套餐"} description="已有独立出租或已发布线路不能迁入；保存套餐不会自动收款。" submit="保存套餐与报价" close={close} action={async data => {
    if (members.some(member => !member.line_ids.length)) throw new Error("每名成员至少选择一条线路。");
    const prices = Object.fromEntries(terms.map(term => [term.id, parseMoney(field(data, term.id))])) as Prices;
    await api("/customer-plans", "POST", { id: plan?.id || "", name: field(data, "name"), customer_id: plan?.customer_id || field(data, "customer"), monthly_gib: number(data, "quota"), members, prices });
    await saved(); close();
  }}>
    <div className="grid gap-4 sm:grid-cols-2"><Field label="套餐名称"><Input name="name" required maxLength={40} defaultValue={plan?.name || ""} /></Field><Field label="所属客户" hint="客户团队成员只代表使用归属，不授予后台管理权限。"><select className="select" name="customer" required disabled={Boolean(plan)} defaultValue={plan?.customer_id || ""}><option value="" disabled>选择客户</option>{state.customers.map(customer => <option key={customer.id} value={customer.id}>{customer.name}</option>)}</select></Field></div>
    <Field label="每周期共享总额度 · GiB" hint="所有成员共同消费；成员上限不会为成员预留独占份额。"><Input name="quota" type="number" min="0.001" max="1000000000" step="any" required defaultValue={plan?.monthly_gib ?? ""} /></Field>
    <div className="grid gap-4 sm:grid-cols-3">{terms.map(term => <Field key={term.id} label={`${term.name}报价 · 元`} hint="留空关闭该档位"><Input name={term.id} type="number" min="0" step="0.01" defaultValue={plan?.rental?.prices[term.id] == null ? "" : plan.rental.prices[term.id]! / 100} /></Field>)}</div>
    {members.map((member, index) => {
      const original = plan?.members.find(old => old.id === member.id);
      const candidates = state.lines.filter(line => member.line_ids.includes(line.id) || eligible.some(candidate => candidate.id === line.id));
      return <fieldset key={index} className="space-y-3 rounded-lg border p-3"><legend className="px-1 text-sm font-medium">成员 {index + 1}</legend>
        <div className="grid gap-3 sm:grid-cols-2"><Field label={`成员 ${index + 1} 名称`}><Input required maxLength={40} value={member.name} onChange={event => update(index, { name: event.target.value })} /></Field><Field label={`成员 ${index + 1} 额外硬上限 · GiB`} hint="留空仅受套餐总额限制。"><Input type="number" min="0.001" step="any" value={member.monthly_gib ?? ""} onChange={event => update(index, { monthly_gib: event.target.value ? Number(event.target.value) : null })} /></Field></div>
        <p className="text-xs text-slate-500">绑定线路。已有归属不能移除；可继续加入尚未出租、未发布的新草稿。</p>
        {!candidates.length && <p className="text-sm text-amber-700">没有可绑定线路。请先新建代理并选择「仅保存草稿」，再返回绑定套餐。</p>}
        <div className="max-h-48 space-y-2 overflow-auto">{candidates.map(line => {
          const retained = Boolean(original?.line_ids.includes(line.id));
          const elsewhere = members.some((other, i) => i !== index && other.line_ids.includes(line.id));
          return <label key={line.id} className="flex items-start gap-2 text-sm"><input type="checkbox" className="mt-1" checked={member.line_ids.includes(line.id)} disabled={retained || elsewhere} onChange={event => update(index, { line_ids: event.target.checked ? [...member.line_ids, line.id] : member.line_ids.filter(id => id !== line.id) })} /><span>{line.name}{retained ? " · 已有归属" : elsewhere ? " · 已选给其他成员" : ""}</span></label>;
        })}</div>
        {!original && members.length > 1 && <Button type="button" size="sm" variant="ghost" onClick={() => { setMembers(old => old.filter((_, i) => i !== index)); setMembersChanged(true); }}>移除此新增成员</Button>}
      </fieldset>;
    })}
    <Button type="button" variant="outline" disabled={members.length >= 500} onClick={() => { setMembers(old => [...old, { id: "", name: `成员 ${old.length + 1}`, line_ids: [], monthly_gib: null }]); setMembersChanged(true); }}><Plus />添加成员</Button>
    <p className="muted">成员和线路加入后保留历史归属；不自动并入旧独立权益，不重置历史用量。修改已有套餐总额或成员上限会下发，不能低于本周期已授权额度。新套餐需通过订单确认开通，再应用线路。</p>
  </Modal>;
}
