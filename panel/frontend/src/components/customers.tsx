import { useState, type ReactNode } from "react";
import { Plus } from "lucide-react";
import { api } from "../api";
import type { Customer, PanelState } from "../types";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Input } from "./ui/input";
import { Empty, Field, Modal, field } from "./shared";
import { PaymentTable } from "./rental-editor";

export function Customers({ state, saved }: { state: PanelState; saved: () => Promise<void> }) {
  const [search, setSearch] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);
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
    <Card><CardHeader><CardTitle>客户续费联系入口</CardTitle></CardHeader><CardContent>
      <p className="whitespace-pre-wrap break-words text-sm">{state.renewal_contact || "尚未设置"}</p><p className="muted mt-2">所有客户查询页显示此统一入口。可以填写联系说明、微信或完整网址。</p>
      <Button className="mt-4" variant="outline" onClick={() => setModal(<Modal title="设置续费联系入口" submit="保存" close={close} action={async data => {
        await api("/rentals/contact", "POST", { renewal_contact: field(data, "contact") });
        await saved();
        close();
      }}><Field label="统一续费联系方式"><textarea className="textarea" name="contact" rows={3} maxLength={2000} defaultValue={state.renewal_contact} placeholder="例如：微信 example，联系时请附上查询页的续费信息。" /></Field></Modal>)}>编辑联系入口</Button>
    </CardContent></Card>
    <Card><CardHeader><div><CardTitle>开通与续费收款记录</CardTitle><p className="muted mt-2">人工确认实收的历史记录 · 人民币 · 北京时间</p></div></CardHeader><PaymentTable payments={state.payments} /></Card>
    {modal}
  </div>;
}
