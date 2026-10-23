import { useState, type ReactNode } from "react";
import { Download, Plus } from "lucide-react";
import { api } from "../api";
import type { Customer, PanelState } from "../types";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Input } from "./ui/input";
import { Empty, Field, Modal, download, field } from "./shared";
import { OrderDialog, PaymentAdjustmentDialog, PaymentTable } from "./rental-editor";
import { beijingTime, money, terms } from "../lib/business";
import { CustomerPlans } from "./customer-plans";

export function Customers({ state, saved }: { state: PanelState; saved: () => Promise<void> }) {
  const [search, setSearch] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);
  const [customerId, setCustomerId] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const close = () => setModal(null);
  function edit(customer?: Customer) {
    setModal(<Modal title={customer ? "编辑客户" : "新建客户"} submit="保存客户" close={close} action={async data => {
      await api("/customers", "POST", { id: customer?.id, name: field(data, "name"), contact: field(data, "contact"), notes: field(data, "notes") });
      await saved();
      close();
    }}>
      <Field label="客户名称"><Input name="name" required maxLength={80} defaultValue={customer?.name || ""} /></Field>
      <Field label="联系方式"><Input name="contact" maxLength={300} defaultValue={customer?.contact || ""} placeholder="微信、邮箱或其他联系方式" /></Field>
      <Field label="内部备注" hint="仅运营后台可见"><textarea className="textarea" name="notes" rows={3} maxLength={2000} defaultValue={customer?.notes || ""} /></Field>
    </Modal>);
  }
  const customers = state.customers.filter(customer => `${customer.name} ${customer.contact}`.toLowerCase().includes(search.trim().toLowerCase()));
  const orders = state.rental_orders || [];
  const adjustments = state.payment_adjustments || [];
  const inPeriod = (at: number) => {
    const day = new Date((at + 8 * 3600) * 1000).toISOString().slice(0, 10);
    return (!from || day >= from) && (!to || day <= to);
  };
  const matchingPayments = state.payments.filter(p => !customerId || p.customer_id === customerId);
  const payments = matchingPayments.filter(p => inPeriod(p.created));
  const changes = adjustments.filter(a => inPeriod(a.created) && matchingPayments.some(p => p.id === a.payment_id));
  const paid = payments.reduce((sum, p) => sum + p.amount_fen, 0);
  const refunds = -changes.filter(a => a.kind === "refund").reduce((sum, a) => sum + a.delta_fen, 0);
  const corrections = changes.filter(a => a.kind === "correction").reduce((sum, a) => sum + a.delta_fen, 0);
  const billCustomers = new Map([...state.payments.map(p => [p.customer_id, p.customer_name] as const), ...state.customers.map(c => [c.id, c.name] as const)]);
  const selectedOrders = orders.filter(o => inPeriod(o.created) && (!customerId || o.items.some(i => i.customer_id === customerId)));
  function exportLedger() {
    const rows = [
      ...payments.map(p => ({ at: p.created, type: "原始收款", id: p.id, payment: p, delta: p.amount_fen, note: p.note, rights: "按原记录开通/续费", session: orders.find(o => o.request_id === p.request_id)?.confirmed_session || "未记录", operator: orders.find(o => o.request_id === p.request_id)?.confirmed_by })),
      ...changes.map(a => ({ at: a.created, type: a.kind === "refund" ? "人工退款" : "录入纠错（非新增收款）", id: a.id, payment: state.payments.find(p => p.id === a.payment_id)!, delta: a.delta_fen, note: a.note, rights: a.disable_line ? "手动停用关联代理" : "未调整权益", session: a.session, operator: a.operator })),
    ].sort((a, b) => a.at - b.at);
    const cells = [["时间（北京时间）", "类型", "流水编号", "原收款编号", "订单编号", "客户编号", "客户", "线路编号", "线路", "金额变动（元）", "权益处理", "备注", "操作会话", "操作成员编号", "操作成员"], ...rows.map(row => [beijingTime(row.at), row.type, row.id, row.payment.id, orders.find(o => o.request_id === row.payment.request_id)?.id || "历史未记录", row.payment.customer_id, row.payment.customer_name, row.payment.line_id, row.payment.line_name, (row.delta / 100).toFixed(2), row.rights, row.note, row.session, row.operator?.id || "未记录", row.operator?.name || "未记录"])];
    const csv = cells.map(row => row.map((value, index) => `"${(index !== 9 && /^[\s]*[=+\-@\t\r]/.test(value) ? "'" + value : value).replaceAll('"', '""')}"`).join(",")).join("\r\n");
    download("收款与调整流水.csv", "\ufeff" + csv);
  }
  if (state.viewer && !["admin", "finance"].includes(state.viewer.role)) return <p role="alert" className="text-sm text-slate-500">当前角色没有客户与账务访问权限。</p>;
  return <div className="space-y-6">
    <Card><CardHeader className="flex flex-wrap items-center justify-between gap-3"><CardTitle>客户档案 · {state.customers.length}</CardTitle><div className="flex flex-wrap gap-2"><Input className="w-48" aria-label="搜索客户" placeholder="客户名称 / 联系方式" value={search} onChange={event => setSearch(event.target.value)} /><Button onClick={() => edit()}><Plus />新建客户</Button></div></CardHeader>
      {customers.length ? <div className="overflow-x-auto" tabIndex={0} role="region" aria-label="客户档案表"><table className="data-table"><thead><tr><th>客户</th><th>联系方式</th><th>代理数</th><th>内部备注</th><th>操作</th></tr></thead><tbody>{customers.map(customer => {
        const count = state.lines.filter(line => line.rental?.customer_id === customer.id).length;
        return <tr key={customer.id}><td className="text-sm font-medium">{customer.name}</td><td className="max-w-64 whitespace-pre-wrap break-words text-sm">{customer.contact || "—"}</td><td className="text-sm">{count}</td><td className="max-w-72 whitespace-pre-wrap break-words text-xs text-slate-500">{customer.notes || "—"}</td><td><div className="flex gap-2"><Button variant="outline" size="sm" onClick={() => edit(customer)}>编辑</Button><Button variant="ghost" size="sm" disabled={count > 0} title={count ? "请先处理客户关联的代理" : undefined} onClick={() => setModal(<Modal title="删除客户档案" description={`删除「${customer.name}」的档案。收款记录仍会保留。`} submit="确认删除" close={close} action={async () => {
          await api(`/customers/${customer.id}`, "DELETE");
          await saved();
          close();
        }} />)}>删除</Button></div></td></tr>;
      })}</tbody></table></div> : <Empty title={search ? "没有匹配的客户" : "尚无客户档案"}>{!search && "先新建客户，再为代理选择归属。"}</Empty>}
    </Card>
    <CustomerPlans state={state} saved={saved} />
    <Card><CardHeader><CardTitle>客户续费联系入口</CardTitle></CardHeader><CardContent>
      <p className="whitespace-pre-wrap break-words text-sm">{state.renewal_contact || "尚未设置"}</p><p className="muted mt-2">所有客户查询页显示此统一入口。可以填写联系说明、微信或完整网址。</p>
      <Button className="mt-4" variant="outline" onClick={() => setModal(<Modal title="设置续费联系入口" submit="保存" close={close} action={async data => {
        await api("/rentals/contact", "POST", { renewal_contact: field(data, "contact") });
        await saved();
        close();
      }}><Field label="统一续费联系方式"><textarea className="textarea" name="contact" rows={3} maxLength={2000} defaultValue={state.renewal_contact} placeholder="例如：微信 example，联系时请附上查询页的续费信息。" /></Field></Modal>)}>编辑联系入口</Button>
    </CardContent></Card>
    <Card><CardHeader><CardTitle>订单与对账</CardTitle></CardHeader><CardContent className="space-y-4">
      <div className="grid gap-3 sm:grid-cols-3"><Field label="客户"><select className="select" value={customerId} onChange={event => setCustomerId(event.target.value)}><option value="">全部客户（含已删除档案）</option>{[...billCustomers].map(([id, name]) => <option key={id} value={id}>{name}</option>)}</select></Field><Field label="起始日期 · 北京时间"><Input type="date" value={from} onChange={event => setFrom(event.target.value)} /></Field><Field label="结束日期 · 含当天"><Input type="date" value={to} onChange={event => setTo(event.target.value)} min={from || undefined} /></Field></div>
      {from && to && from > to && <p role="alert" className="text-sm text-red-600">起始日期不能晚于结束日期。</p>}
      <div className="flex flex-wrap gap-4 text-sm"><span>原始收款 {money(paid)}</span><span>实际退款 {money(refunds)}</span><span>录入纠错 {money(corrections)}</span><span>账面净变动 {money(paid + corrections - refunds)}</span></div>
      <p className="muted">按每笔收款或调整的发生日期筛选；纠错不是新增收款。新操作记录当前登录成员；历史收款不补造订单和操作人。</p>
      <Button variant="outline" disabled={Boolean(from && to && from > to)} onClick={exportLedger}><Download />导出筛选后的流水 CSV</Button>
      <h3 className="text-sm font-medium">开通 / 续费订单 · {selectedOrders.length}</h3>
      {!selectedOrders.length ? <p className="muted">暂无匹配订单。请从代理的「收款 / 续费」创建；旧历史保留为收款记录。</p> : <div className="max-h-96 space-y-3 overflow-auto">{[...selectedOrders].reverse().map(order => <div key={order.id} className="rounded-lg border p-3"><div className="flex flex-wrap items-start justify-between gap-3"><div className="min-w-0"><p className="text-sm">{order.confirmed_at ? "已确认收款" : order.cancelled_at ? "已取消" : "待收款"} · {terms.find(t => t.id === order.term)?.name} · {money(order.items.reduce((sum, item) => sum + item.amount_fen, 0))}</p><p className="muted mt-1">{beijingTime(order.created)} · {order.items.map(item => `${item.customer_name} / ${item.line_name}`).join("、")}</p><p className="mt-1 break-all text-xs text-slate-400">{order.id}</p></div><div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" onClick={() => setModal(<OrderDialog order={order} close={close} saved={saved} />)}>{order.confirmed_at || order.cancelled_at ? "查看订单" : "核对并确认收款"}</Button>{!order.confirmed_at && !order.cancelled_at && <Button size="sm" variant="ghost" onClick={() => setModal(<Modal title="取消待收款订单" description={`订单 ${order.id}。取消不会退款或改变代理权益。`} submit="确认尚未收款并取消" close={close} action={async () => { await api(`/rentals/orders/${order.id}/cancel`, "POST", {}); await saved(); close(); }} />)}>取消订单</Button>}</div></div></div>)}</div>}
    </CardContent></Card>
    <Card><CardHeader><div><CardTitle>筛选期间的原始收款</CardTitle><p className="muted mt-2">关联调整显示完整历史；当前净额不等于筛选期间的账面变动。</p></div></CardHeader><PaymentTable payments={payments} adjustments={adjustments} orders={orders} adjust={payment => {
      const plan = state.customer_plans?.find(plan => `plan:${plan.id}` === payment.line_id);
      const ids = plan ? plan.members.flatMap(member => member.line_ids) : payment.line_id.startsWith("plan:") ? [] : [payment.line_id];
      const affectedLines = ids.map(id => ({ id, name: state.lines.find(line => line.id === id)?.name || id }));
      setModal(<PaymentAdjustmentDialog payment={payment} adjustments={adjustments} affectedLines={affectedLines} close={close} saved={saved} />);
    }} /></Card>
    <Card><CardHeader><CardTitle>筛选期间的退款与纠错</CardTitle></CardHeader><CardContent className="space-y-3">{!changes.length ? <p className="muted">暂无匹配调整。</p> : [...changes].reverse().map(change => { const payment = state.payments.find(p => p.id === change.payment_id)!; return <div key={change.id} className="rounded-lg border p-3 text-sm"><p>{beijingTime(change.created)} · {payment.customer_name} / {payment.line_name}</p><p className="mt-1">{change.kind === "refund" ? "人工退款" : "录入纠错"} {money(change.delta_fen)} · {change.disable_line ? "已手动停用关联代理" : "未调整权益"}</p><p className="mt-1 whitespace-pre-wrap break-words">{change.note}</p><p className="mt-1 break-all text-xs text-slate-400">流水 {change.id} · 原收款 {payment.id} · 会话 {change.session}</p></div>; })}</CardContent></Card>
    {modal}
  </div>;
}
