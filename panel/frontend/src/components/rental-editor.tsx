import { useRef, useState } from "react";
import { api } from "../api";
import type { Line, PanelState, Payment, Prices, Term } from "../types";
import { beijingTime, money, parseMoney, terms } from "../lib/business";
import { CopyButton, Field, Modal, field, number } from "./shared";
import { Button } from "./ui/button";
import { Input } from "./ui/input";

export function PaymentTable({ payments }: { payments: Payment[] }) {
  if (!payments.length) return <p className="muted py-3">尚无收款记录。</p>;
  return <div className="max-h-96 overflow-auto" tabIndex={0} role="region" aria-label="收款记录"><table className="data-table"><thead><tr><th>收款时间 / 客户</th><th>代理 / 周期</th><th>实收</th><th>续后到期</th><th>备注</th></tr></thead><tbody>{payments.map(payment => <tr key={payment.id}>
    <td><p className="text-xs">{beijingTime(payment.created)}</p><p className="muted mt-1">{payment.customer_name}</p></td><td><p className="text-sm">{payment.line_name}</p><p className="muted mt-1">{payment.previous_expires_at ? payment.previous_expires_at <= payment.created ? "重新开通" : "续费" : "首次开通"} · {terms.find(term => term.id === payment.term)?.name}</p></td><td className="text-sm tabular-nums">{money(payment.amount_fen)}</td><td className="text-xs">{beijingTime(payment.expires_at)}</td><td className="max-w-64 whitespace-pre-wrap break-words text-xs">{payment.note || "—"}</td>
  </tr>)}</tbody></table></div>;
}

export function RentalEditor({ line, state, close, saved }: { line: Line; state: PanelState; close: () => void; saved: () => Promise<void> }) {
  const assigned = Boolean(line.rental?.started_at && line.rental.customer_id);
  return <Modal title="出租信息" description={line.name} submit="保存归属与报价" close={close} action={async data => {
    const prices = Object.fromEntries(terms.map(term => [term.id, parseMoney(field(data, term.id))])) as Prices;
    await api(`/rentals/${line.id}`, "POST", { customer_id: assigned ? line.rental!.customer_id : field(data, "customer"), prices });
    await saved();
    close();
  }}>
    <Field label="所属客户" hint={assigned ? "已开通的代理保留原客户归属；为新客户新建代理。" : state.customers.length ? "每条代理分别计额、到期和收款。" : "请先在「客户与收款」页面新建客户档案。"}><select className="select" name="customer" defaultValue={line.rental?.customer_id || ""} disabled={assigned} required><option value="" disabled>选择客户</option>{state.customers.map(customer => <option value={customer.id} key={customer.id}>{customer.name}{customer.contact ? ` · ${customer.contact}` : ""}</option>)}</select></Field>
    <div className="grid gap-4 sm:grid-cols-3">{terms.map(term => <Field key={term.id} label={`${term.name}报价 · 元`} hint="留空关闭该周期"><Input name={term.id} type="number" min="0" step="0.01" defaultValue={line.rental?.prices[term.id] == null ? "" : line.rental.prices[term.id]! / 100} placeholder="未开放" /></Field>)}</div>
    <p className="muted">保存后通过「收款开通 / 续费」登记实收。首次收款前不能使用代理；改价不改变已有到期时间或流量。</p>
    {line.rental && <div className="rounded-lg bg-slate-50 p-4 text-xs leading-7"><p>当前周期起点：{beijingTime(line.rental.started_at)}</p><p>当前到期：{line.rental.permanent ? "无限期" : beijingTime(line.rental.expires_at)}</p><p>下次额度重置：{beijingTime(line.rental.next_reset_at)}</p><p>以上时间均为北京时间。</p></div>}
    <details><summary className="cursor-pointer text-sm">本代理收款记录</summary><PaymentTable payments={state.payments.filter(payment => payment.line_id === line.id)} /></details>
  </Modal>;
}

export function RenewalDialog({ lines, close, saved }: { lines: Line[]; close: () => void; saved: () => Promise<void> }) {
  const available = terms.filter(term => lines.every(line => line.rental?.customer_id && line.rental.prices[term.id] != null));
  const [term, setTerm] = useState<Term>(available[0]?.id || "monthly");
  const request = useRef<{ signature: string; id: string } | null>(null);
  return <Modal wide title={lines.length === 1 ? "收款开通 / 续费" : `批量登记收款 · ${lines.length} 条`} description="请确认已收到款项。提交后保存权益并下发，离线节点显示等待同步。" submit={available.length ? "确认已收款并下发" : undefined} close={close} action={async data => {
    const amounts = Object.fromEntries(lines.map(line => {
      const amount = parseMoney(field(data, `amount:${line.id}`));
      if (amount == null) throw new Error(`请填写「${line.name}」的实收金额。`);
      return [line.id, amount];
    }));
    const payload = { line_ids: lines.map(line => line.id), term, amounts, note: field(data, "note") };
    const signature = JSON.stringify(payload);
    if (request.current?.signature !== signature) {
      const bytes = crypto.getRandomValues(new Uint8Array(16));
      bytes[6] = (bytes[6] & 0x0f) | 0x40;
      bytes[8] = (bytes[8] & 0x3f) | 0x80;
      const text = Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
      request.current = { signature, id: `${text.slice(0, 8)}-${text.slice(8, 12)}-${text.slice(12, 16)}-${text.slice(16, 20)}-${text.slice(20)}` };
    }
    await api("/rentals/renew", "POST", { ...payload, request_id: request.current.id });
    await saved();
    close();
  }}>
    {!available.length ? <p role="alert" className="rounded-lg bg-amber-50 p-4 text-sm text-amber-800">所选代理必须已设置客户和报价，并至少开放一个共同的付费周期。请分别编辑出租信息后重试。</p> : <>
      <Field label="付费周期"><select className="select" value={term} onChange={event => setTerm(event.target.value as Term)}>{available.map(term => <option key={term.id} value={term.id}>{term.name} · {term.months} 个自然月</option>)}</select></Field>
      <div className="space-y-3">{lines.map(line => <div key={`${line.id}:${term}`} className="grid gap-3 rounded-lg border border-border p-3 sm:grid-cols-[1fr_160px]"><div><p className="text-sm font-medium">{line.name}</p><p className="muted mt-1">当前到期 {line.rental?.permanent ? "无限期" : beijingTime(line.rental?.expires_at)} · 报价 {money(line.rental?.prices[term])}</p>{line.rental?.permanent && <p className="mt-1 text-xs text-amber-700">本次收款将改为有限期并重新开通，流量恢复满额。</p>}</div><Field label="实收 · 元"><Input name={`amount:${line.id}`} type="number" min="0" step="0.01" required defaultValue={line.rental!.prices[term]! / 100} /></Field></div>)}</div>
      <Field label="收款备注"><textarea className="textarea" name="note" rows={2} maxLength={2000} placeholder="例如：微信转账，付款凭据编号" /></Field>
      <p className="muted">首次开通从本次收款起算；有效期内续费从原到期日顺延，不清零流量。过期重开从现在起算并恢复满额。按北京时间逐自然月计算，月末截到月底并保留原锚日。手动停用的代理不会因续费自动启用。</p>
    </>}
  </Modal>;
}

export function LimitsDialog({ lines, close, saved }: { lines: Line[]; close: () => void; saved: () => Promise<void> }) {
  const initial = lines.length === 1 ? lines[0].limits : null;
  return <Modal title={lines.length === 1 ? "调整额度与限速" : `批量调整额度与限速 · ${lines.length} 条`} description="以下三个值将整体替换所选代理的限制；留空表示不限。修改立即下发，不清零当前已用流量；离线节点等待同步。" submit="确认调整并下发" close={close} action={async data => {
    await api("/lines/operate", "POST", { line_ids: lines.map(line => line.id), action: "limits", limits: { monthly_gib: number(data, "quota"), upload_mbps: number(data, "upload"), download_mbps: number(data, "download") } });
    await saved();
    close();
  }}>
    <p className="text-sm">{lines.map(line => line.name).join("、")}</p>
    <div className="grid gap-4 sm:grid-cols-3">{[
      { key: "quota", label: "月额度 · GiB", value: initial?.monthly_gib },
      { key: "upload", label: "上传上限 · Mbps", value: initial?.upload_mbps },
      { key: "download", label: "下载上限 · Mbps", value: initial?.download_mbps },
    ].map(input => <Field key={input.key} label={input.label}><Input name={input.key} type="number" min="0.001" step="any" placeholder="不限" defaultValue={input.value ?? ""} /></Field>)}</div>
  </Modal>;
}

export function QueryLinkDialog({ line, close, saved }: { line: Line; close: () => void; saved: () => Promise<void> }) {
  const [url, setUrl] = useState("");
  return <Modal title="客户私密查询链接" description={line.name} submit={url ? undefined : line.rental?.query_link_exists ? "重新生成并使旧链接失效" : "生成查询链接"} close={close} action={async () => {
    const result = await api<{ path: string }>(`/rentals/${line.id}/query-link`, "POST");
    setUrl(new URL(result.path, location.origin).href);
    await saved();
  }}>
    <p className="muted">持有链接即可查看这一条代理的余量、到期与报价，无需登录。重新生成会使旧查询链接失效，不改变代理凭据。</p>
    {url && <><Field label="查询链接"><textarea className="textarea mono" readOnly rows={3} value={url} /></Field><CopyButton value={url} label="复制客户查询链接" /><Button type="button" variant="outline" onClick={() => window.open(url, "_blank", "noopener,noreferrer")}>打开查询页</Button></>}
  </Modal>;
}
