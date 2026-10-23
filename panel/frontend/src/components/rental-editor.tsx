import { useEffect, useRef, useState } from "react";
import { ApiError, api } from "../api";
import type { Line, PanelState, Payment, PaymentAdjustment, Prices, Rental, RentalOrder, Term } from "../types";
import { beijingTime, money, parseMoney, terms } from "../lib/business";
import { CopyButton, Field, Modal, errorText, field, number } from "./shared";
import { Button } from "./ui/button";
import { Input } from "./ui/input";

export function PaymentTable({ payments, adjustments = [], orders = [], adjust }: { payments: Payment[]; adjustments?: PaymentAdjustment[]; orders?: RentalOrder[]; adjust?: (payment: Payment) => void }) {
  if (!payments.length) return <p className="muted p-5">尚无收款记录。</p>;
  return <div className="max-h-96 overflow-auto" tabIndex={0} role="region" aria-label="收款记录"><table className="data-table"><thead><tr><th>收款时间 / 客户</th><th>代理 / 周期</th><th>原始实收 / 当前净额</th><th>续后到期</th><th>备注 / 调整</th></tr></thead><tbody>{[...payments].reverse().map(payment => {
    const changes = adjustments.filter(a => a.payment_id === payment.id);
    const order = orders.find(o => o.request_id === payment.request_id);
    return <tr key={payment.id}>
      <td><p className="text-xs">{beijingTime(payment.created)}</p><p className="muted mt-1">{payment.customer_name}</p><p className="mt-1 break-all text-xs text-slate-400">收款 {payment.id}</p><p className="mt-1 break-all text-xs text-slate-400">{order ? `订单 ${order.id}` : "历史收款 · 未记录原始订单/操作人"}</p></td><td><p className="text-sm">{payment.line_name}</p><p className="muted mt-1">{payment.previous_expires_at ? payment.previous_expires_at <= payment.created ? "重新开通" : "续费" : "首次开通"} · {terms.find(term => term.id === payment.term)?.name}</p></td><td className="text-sm tabular-nums"><p>{money(payment.amount_fen)}</p><p className="muted mt-1">净额 {money(payment.amount_fen + changes.reduce((sum, a) => sum + a.delta_fen, 0))}</p></td><td className="text-xs">{beijingTime(payment.expires_at)}</td><td className="max-w-72 whitespace-pre-wrap break-words text-xs"><p>{payment.note || "—"}</p>{changes.map(a => <p key={a.id} className="mt-2">{beijingTime(a.created)} · {a.kind === "refund" ? "人工退款" : "录入纠错"} {money(a.delta_fen)} · {a.disable_line ? "已手动停用" : "未调整权益"}<br />{a.note}</p>)}{adjust && <Button type="button" className="mt-2" size="sm" variant="outline" onClick={() => adjust(payment)}>退款 / 纠错</Button>}</td>
    </tr>;
  })}</tbody></table></div>;
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
    <details><summary className="cursor-pointer text-sm">本代理收款记录</summary><PaymentTable payments={state.payments.filter(payment => payment.line_id === line.id)} adjustments={state.payment_adjustments} orders={state.rental_orders} /></details>
  </Modal>;
}

export type BillingTarget = { id: string; name: string; rental: Rental | null };
export function RenewalDialog({ lines, close, saved }: { lines: BillingTarget[]; close: () => void; saved: () => Promise<void> }) {
  const available = terms.filter(term => lines.every(line => line.rental?.customer_id && line.rental.prices[term.id] != null));
  const [term, setTerm] = useState<Term>(available[0]?.id || "monthly");
  const [storageKey, setStorageKey] = useState("");
  const [pendingId, setPendingId] = useState("");
  const [order, setOrder] = useState<RentalOrder | null>(null);
  const [notice, setNotice] = useState("");
  const [missing, setMissing] = useState(false);
  async function restore(requestId: string) {
    setMissing(false);
    try { setOrder(await api<RentalOrder>(`/rentals/orders/by-request/${encodeURIComponent(requestId)}`)); setNotice(""); }
    catch (error) { setNotice(errorText(error)); setMissing(error instanceof ApiError && error.status === 404); }
  }
  useEffect(() => {
    let alive = true;
    void api<{ scope: string }>("/rentals/context").then(async ({ scope }) => {
      if (!alive) return;
      const key = `chain-order:${scope}`;
      const requestId = localStorage.getItem(key) || "";
      setStorageKey(key); setPendingId(requestId);
      if (requestId) {
        try { const found = await api<RentalOrder>(`/rentals/orders/by-request/${encodeURIComponent(requestId)}`); if (alive) setOrder(found); }
        catch (error) { if (alive) { setNotice(errorText(error)); setMissing(error instanceof ApiError && error.status === 404); } }
      }
    }).catch(error => { if (alive) setNotice(errorText(error)); });
    return () => { alive = false; };
  }, []);
  const clear = () => { localStorage.removeItem(storageKey); setPendingId(""); setNotice(""); setMissing(false); };
  if (order) return <OrderDialog order={order} close={close} saved={async () => { clear(); await saved(); }} />;
  return <Modal wide title={lines.length === 1 ? "开通 / 续费订单" : `批量开通 / 续费 · ${lines.length} 条`} description="先保存订单，再核对实际收款。创建订单不会扣款或改变权益。" submit={storageKey && !pendingId && available.length ? "保存待收款订单" : undefined} close={close} action={async data => {
    const previous = localStorage.getItem(storageKey);
    if (previous) { setPendingId(previous); await restore(previous); return; }
    const amounts = Object.fromEntries(lines.map(line => {
      const amount = parseMoney(field(data, `amount:${line.id}`));
      if (amount == null) throw new Error(`请填写「${line.name}」的实收金额。`);
      return [line.id, amount];
    }));
    const payload = { line_ids: lines.map(line => line.id), term, amounts, note: field(data, "note") };
    const requestId = billingRequestId();
    localStorage.setItem(storageKey, requestId);
    setPendingId(requestId);
    setOrder(await api<RentalOrder>("/rentals/orders", "POST", { ...payload, request_id: requestId }));
    await saved();
  }}>
    {notice && <p role="alert" className="text-sm text-amber-700">{notice}</p>}
    {!storageKey && <p className="muted">正在核对本次登录的待确认操作。无法读取本地存储时，请先恢复存储权限。</p>}
    {pendingId ? <div className="space-y-3"><p className="text-sm">本次登录有一笔结果待核对的订单创建请求。不会自动重复登记收款。</p><p className="break-all text-xs">请求编号 {pendingId}</p><Button type="button" variant="outline" onClick={() => void restore(pendingId)}>查询原订单</Button>{missing && <Button type="button" variant="outline" onClick={() => { if (window.confirm("服务器未找到原订单。请先在客户与收款中核对；清除此标记仅允许重新创建待收款订单，不会自动确认收款。继续？")) clear(); }}>已核对，清除待确认标记</Button>}</div> : <>
    {!available.length ? <p role="alert" className="rounded-lg bg-amber-50 p-4 text-sm text-amber-800">所选代理或套餐必须已设置客户和报价，并至少开放一个共同的付费周期。</p> : <>
      <Field label="付费周期"><select className="select" value={term} onChange={event => setTerm(event.target.value as Term)}>{available.map(term => <option key={term.id} value={term.id}>{term.name} · {term.months} 个自然月</option>)}</select></Field>
      <div className="space-y-3">{lines.map(line => <div key={`${line.id}:${term}`} className="grid gap-3 rounded-lg border border-border p-3 sm:grid-cols-[1fr_160px]"><div><p className="text-sm font-medium">{line.name}</p><p className="muted mt-1">当前到期 {line.rental?.permanent ? "无限期" : beijingTime(line.rental?.expires_at)} · 报价 {money(line.rental?.prices[term])}</p>{line.rental?.permanent && <p className="mt-1 text-xs text-amber-700">本次收款将改为有限期并重新开通，流量恢复满额。</p>}</div><Field label="实收 · 元"><Input name={`amount:${line.id}`} type="number" min="0" step="0.01" required defaultValue={line.rental!.prices[term]! / 100} /></Field></div>)}</div>
      <Field label="收款备注"><textarea className="textarea" name="note" rows={2} maxLength={2000} placeholder="例如：微信转账，付款凭据编号" /></Field>
      <p className="muted">首次开通从确认收款起算；有效期内续费顺延，不清零流量。过期重开恢复满额。手动停用不会因续费自动启用。</p>
    </>}
    </>}
  </Modal>;
}

function billingRequestId() {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, "0")).join("");
}

export function OrderDialog({ order, close, saved }: { order: RentalOrder; close: () => void; saved: () => Promise<void> }) {
  const complete = order.confirmed_at > 0 || order.cancelled_at > 0;
  return <Modal wide title="核对开通 / 续费订单" description={`订单 ${order.id}`} submit={complete ? undefined : "确认已实际收款并下发权益"} close={close} action={async () => {
    await api(`/rentals/orders/${order.id}/confirm`, "POST", {});
    await saved(); close();
  }}>
    <p className="text-sm">{order.confirmed_at ? `已确认收款 · ${beijingTime(order.confirmed_at)}` : order.cancelled_at ? `已取消 · ${beijingTime(order.cancelled_at)}` : "待人工确认收款"} · {terms.find(t => t.id === order.term)?.name}</p>
    {order.items.map(item => <div key={item.line_id} className="rounded-lg border p-3 text-sm"><p>{item.customer_name} · {item.line_name}</p><p className="mt-1">约定实收 {money(item.amount_fen)} · 当时报价 {money(item.quoted_fen)}</p><p className="muted mt-1">原到期 {item.started_at && !item.expires_at ? "无限期" : beijingTime(item.expires_at)}</p></div>)}
    <p className="font-medium">合计 {money(order.items.reduce((sum, item) => sum + item.amount_fen, 0))}</p>
    <p className="whitespace-pre-wrap break-words text-sm">{order.note || "未填写备注"}</p>
    <p className="muted">本页不会发起在线支付。确认后才开始或延长租期并下发；离线节点等待同步。有效期内续费不清零用量，手动停用仍保留。客户归属、报价或租期发生变化时需先核对并重建订单。套餐额度与成员范围按当前配置生效，订单不锁定这些设置。</p>
    {!complete && <label className="flex items-start gap-2 text-sm"><input type="checkbox" required className="mt-1" />已逐项核对客户、周期和金额，确认线下款项实际到账；不是重试另一张未知结果订单。</label>}
    {complete && <Button type="button" variant="outline" onClick={() => void saved().then(close)}>已核对，关闭订单</Button>}
    <p className="text-xs text-slate-500">创建人 {order.created_by?.name || "未记录"} · 收款确认人 {order.confirmed_by?.name || "未记录"}{order.cancelled_by && ` · 取消人 ${order.cancelled_by.name}`}</p>
    <p className="break-all text-xs text-slate-400">创建会话 {order.created_session}；{order.confirmed_session ? `收款会话 ${order.confirmed_session}` : order.cancelled_session ? `取消会话 ${order.cancelled_session}` : "尚未记录收款会话"}</p>
  </Modal>;
}

export function PaymentAdjustmentDialog({ payment, adjustments, affectedLines, close, saved }: { payment: Payment; adjustments: PaymentAdjustment[]; affectedLines?: { id: string; name: string }[]; close: () => void; saved: () => Promise<void> }) {
  const changes = adjustments.filter(a => a.payment_id === payment.id);
  const gross = payment.amount_fen + changes.filter(a => a.kind === "correction").reduce((sum, a) => sum + a.delta_fen, 0);
  const net = payment.amount_fen + changes.reduce((sum, a) => sum + a.delta_fen, 0);
  const [kind, setKind] = useState<"refund" | "correction">("refund");
  const [submitted, setSubmitted] = useState(false);
  const [storageKey, setStorageKey] = useState("");
  const [recovery, setRecovery] = useState<{ id: string; record?: PaymentAdjustment; missing?: boolean } | null>(null);
  const [notice, setNotice] = useState("");
  const request = useRef<{ id: string; payload: object } | null>(null);
  async function recover(requestId: string) {
    setRecovery({ id: requestId }); setNotice("");
    try { setRecovery({ id: requestId, record: await api<PaymentAdjustment>(`/rentals/adjustments/by-request/${encodeURIComponent(requestId)}`) }); }
    catch (error) { setRecovery({ id: requestId, missing: error instanceof ApiError && error.status === 404 }); setNotice(errorText(error)); }
  }
  useEffect(() => {
    let alive = true;
    void api<{ scope: string }>("/rentals/context").then(async ({ scope }) => {
      if (!alive) return;
      const key = `chain-adjustment:${scope}`;
      const requestId = localStorage.getItem(key);
      setStorageKey(key);
      if (requestId) {
        setRecovery({ id: requestId });
        try { const record = await api<PaymentAdjustment>(`/rentals/adjustments/by-request/${encodeURIComponent(requestId)}`); if (alive) setRecovery({ id: requestId, record }); }
        catch (error) { if (alive) { setRecovery({ id: requestId, missing: error instanceof ApiError && error.status === 404 }); setNotice(errorText(error)); } }
      }
    }).catch(error => { if (alive) setNotice(errorText(error)); });
    return () => { alive = false; };
  }, []);
  const clear = () => { localStorage.removeItem(storageKey); setRecovery(null); setNotice(""); request.current = null; setSubmitted(false); };
  return <Modal title="人工退款 / 金额纠错" description={`${payment.customer_name} · ${payment.line_name}`} submit={!storageKey || recovery ? undefined : submitted ? "使用原请求核对并重试" : "确认追加记录"} close={close} action={async data => {
    if (!request.current) {
      const previous = localStorage.getItem(storageKey);
      if (previous) { await recover(previous); return; }
      const amount = parseMoney(field(data, "amount"));
      if (amount == null) throw new Error("请填写金额。");
      const requestId = billingRequestId();
      localStorage.setItem(storageKey, requestId);
      request.current = { id: requestId, payload: { payment_id: payment.id, kind, amount_fen: amount, expected_net_fen: net, note: field(data, "note"), disable_line: kind === "refund" && field(data, "rights") === "disable", expected_line_ids: affectedLines?.map(line => line.id) } };
      setSubmitted(true);
    }
    try { await api("/rentals/adjustments", "POST", { ...request.current.payload, request_id: request.current.id }); }
    catch (error) { if (error instanceof ApiError && [400, 401, 403, 404, 409, 422].includes(error.status)) clear(); throw error; }
    clear();
    await saved(); close();
  }}>
    {notice && <p role="alert" className="text-sm text-amber-700">{notice}</p>}
    {!storageKey && <p className="muted">正在核对本次登录的未确认操作。</p>}
    {recovery && <div className="space-y-3 rounded-lg border border-amber-300 p-3"><p className="text-sm">本次登录有一笔退款 / 纠错结果需要先核对，不会自动再登记。</p>{recovery.record ? <><p className="text-sm">已登记 {recovery.record.kind === "refund" ? "人工退款" : "录入纠错"} {money(recovery.record.delta_fen)} · {beijingTime(recovery.record.created)}</p><p className="break-all text-xs">原收款 {recovery.record.payment_id} · 调整 {recovery.record.id}</p><Button type="button" variant="outline" onClick={() => { clear(); void saved().then(close); }}>已核对原操作，关闭</Button></> : <><Button type="button" variant="outline" onClick={() => void recover(recovery.id)}>查询原操作结果</Button>{recovery.missing && <Button type="button" variant="outline" onClick={() => { if (window.confirm("请核对原收款和调整流水，确认原请求未落账且已结束后再清除此标记。不得重复实际转账。继续？")) clear(); }}>已核对，重新填写</Button>}</>}</div>}
    <p className="text-sm">原始实收 {money(payment.amount_fen)} · 纠错后实收 {money(gross)} · 可退 {money(net)}</p>
    <fieldset disabled={submitted || !storageKey || Boolean(recovery)} className="space-y-4">
      <Field label="记录类型"><select className="select" value={kind} onChange={event => setKind(event.target.value as typeof kind)}><option value="refund">已在线下实际退款</option><option value="correction">更正原收款录入错误（不是新增收款）</option></select></Field>
      <Field label={kind === "refund" ? "本次实际退款 · 元" : "正确的原始实收总额 · 元"}><Input key={kind} name="amount" type="number" min={kind === "refund" ? "0.01" : "0"} step="0.01" required defaultValue={kind === "correction" ? gross / 100 : ""} /></Field>
      {kind === "refund" && <><Field label="代理权益处理"><select className="select" name="rights" defaultValue="keep"><option value="keep">保留现有租期、额度和启停状态</option><option value="disable" disabled={payment.line_id.startsWith("plan:") && !affectedLines?.length}>{payment.line_id.startsWith("plan:") ? "停用此套餐全部成员的关联线路" : "另行手动停用关联代理"}（不改变租期和用量）</option></select></Field>{payment.line_id.startsWith("plan:") && <div className="rounded-lg border p-3 text-xs"><p>选择停用将影响以下全部线路；套餐共享租期、用量账本和历史权益保持原样。</p><ul className="mt-2 list-inside list-disc">{affectedLines?.map(line => <li key={line.id}>{line.name} · {line.id}</li>)}</ul>{!affectedLines?.length && <p>当前无法确定成员范围，只能选择保留权益。</p>}</div>}</>}
      <Field label="原因 / 凭据"><textarea className="textarea" name="note" required maxLength={600} rows={3} placeholder="填写退款凭据或原录入错误及正确金额的依据" /></Field>
      <label className="flex items-start gap-2 text-sm"><input type="checkbox" required className="mt-1" />{kind === "refund" ? "确认已经实际完成线下退款；登记不会自动转账。" : "确认只是原金额录错，正确金额已有实际收款依据；不会据此延长租期。"}</label>
    </fieldset>
    <p className="muted">原收款及历史租期保持原样，调整追加留痕。纠错不能低于已退款总额；退款不得超过可退金额。</p>
    {submitted && <p role="alert" className="text-sm text-amber-700">该请求结果尚需核对，内容已锁定。重试使用同一请求；关闭重开后先查询原操作。</p>}
  </Modal>;
}

export function LimitsDialog({ lines, close, saved, request = api }: { lines: Line[]; close: () => void; saved: () => Promise<void>; request?: typeof api }) {
  const initial = lines.length === 1 ? lines[0].limits : null;
  return <Modal title={lines.length === 1 ? "调整额度与限速" : `批量调整额度与限速 · ${lines.length} 条`} description="以下三个值将整体替换所选代理的限制；留空表示不限。修改立即下发，不清零当前已用流量；离线节点等待同步。" submit="确认调整并下发" close={close} action={async data => {
    await request("/lines/operate", "POST", { line_ids: lines.map(line => line.id), action: "limits", limits: { monthly_gib: number(data, "quota"), upload_mbps: number(data, "upload"), download_mbps: number(data, "download") } });
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
