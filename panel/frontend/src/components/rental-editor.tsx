import { useEffect, useRef, useState } from "react";
import { ApiError, api } from "../api";
import type {
  Line,
  PanelState,
  Payment,
  PaymentAdjustment,
  Prices,
  Rental,
  RentalOrder,
  Term,
} from "../types";
import { beijingTime, money, parseMoney, terms } from "../lib/business";
import { adjustmentRights } from "./rental-shared";
import { CopyButton, Field, InfoTip, Modal, errorText, field, number } from "./shared";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { Input } from "./ui/input";

export function PaymentTable({
  payments,
  lines,
  adjustments = [],
  orders = [],
  adjust,
}: {
  payments: Payment[];
  lines: Line[];
  adjustments?: PaymentAdjustment[];
  orders?: RentalOrder[];
  adjust?: (payment: Payment) => void;
}) {
  if (!payments.length)
    return <p className="muted p-5">尚无收款记录。</p>;
  return (
    <div
      className="max-h-96 overflow-auto"
      tabIndex={0}
      role="region"
      aria-label="收款记录"
    >
      <table className="data-table">
        <thead>
          <tr>
            <th>时间 / 客户</th>
            <th>线路 / 周期</th>
            <th>实收 / 当前净额</th>
            <th>续后到期</th>
            <th>备注与调整</th>
          </tr>
        </thead>
        <tbody>
          {[...payments].reverse().map((payment) => {
            const changes = adjustments.filter(
              (a) => a.payment_id === payment.id,
            );
            const order = orders.find(
              (o) => o.request_id === payment.request_id,
            );
            return (
              <tr key={payment.id}>
                <td>
                  <p className="text-xs font-medium tabular-nums text-foreground">
                    {beijingTime(payment.created)}
                  </p>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {payment.customer_name}
                  </p>
                  <p className="mono mt-0.5 break-all text-[11px] text-muted-foreground/75">
                    {order
                      ? `订单 ${order.id} · 流水 ${payment.id}`
                      : `流水 ${payment.id}`}
                  </p>
                </td>
                <td>
                  <p className="text-xs font-semibold text-foreground">
                    {payment.line_name}
                  </p>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {payment.previous_expires_at
                      ? payment.previous_expires_at <= payment.created
                        ? "重新开通"
                        : "续费"
                      : "首次开通"}{" "}
                    · {terms.find((term) => term.id === payment.term)?.name}
                  </p>
                </td>
                <td className="text-xs tabular-nums">
                  <p className="font-semibold text-foreground">
                    {money(payment.amount_fen)}
                  </p>
                  <p className="mt-0.5 text-muted-foreground">
                    净额{" "}
                    {money(
                      payment.amount_fen +
                        changes.reduce((sum, a) => sum + a.delta_fen, 0),
                    )}
                  </p>
                </td>
                <td className="text-xs tabular-nums text-muted-foreground">
                  {beijingTime(payment.expires_at)}
                </td>
                <td className="max-w-72 whitespace-pre-wrap break-words text-xs">
                  <p className="text-foreground">{payment.note || "—"}</p>
                  {changes.map((a) => (
                    <p key={a.id} className="mt-1.5 text-muted-foreground">
                      {beijingTime(a.created)} ·{" "}
                      {a.kind === "refund" ? "人工退款" : "录入纠错"}{" "}
                      {money(a.delta_fen)} · 调整流水 {a.id} ·{" "}
                      {adjustmentRights(a, lines)}
                      <br />
                      {a.note}
                    </p>
                  ))}
                  {adjust && (
                    <Button
                      type="button"
                      className="mt-2"
                      size="sm"
                      variant="outline"
                      onClick={() => adjust(payment)}
                    >
                      退款 / 纠错
                    </Button>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

export function RentalEditor({
  line,
  state,
  close,
  saved,
}: {
  line: Line;
  state: PanelState;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const assigned = Boolean(line.rental?.started_at && line.rental.customer_id);
  return (
    <Modal
      title="出租信息"
      description={line.name}
      submit="保存归属与报价"
      close={close}
      action={async (data) => {
        const prices = Object.fromEntries(
          terms.map((term) => [term.id, parseMoney(field(data, term.id))]),
        ) as Prices;
        await api(`/rentals/${line.id}`, "POST", {
          customer_id: assigned
            ? line.rental!.customer_id
            : field(data, "customer"),
          prices,
        });
        await saved();
        close();
      }}
    >
      <Field
        label="所属客户"
        hint={
          assigned
            ? "已开通线路保持原客户归属"
            : !state.customers.length
              ? "请先在「客户与收款」页面新建客户档案"
              : undefined
        }
      >
        <select
          className="select"
          name="customer"
          defaultValue={line.rental?.customer_id || ""}
          disabled={assigned}
          required
        >
          <option value="" disabled>
            选择客户
          </option>
          {state.customers.map((customer) => (
            <option value={customer.id} key={customer.id}>
              {customer.name}
              {customer.contact ? ` · ${customer.contact}` : ""}
            </option>
          ))}
        </select>
      </Field>

      <div className="grid gap-3.5 sm:grid-cols-3">
        {terms.map((term) => (
          <Field key={term.id} label={`${term.name}报价 · 元`}>
            <Input
              name={term.id}
              type="number"
              min="0"
              step="0.01"
              defaultValue={
                line.rental?.prices[term.id] == null
                  ? ""
                  : line.rental.prices[term.id]! / 100
              }
              placeholder="留空不开放"
            />
          </Field>
        ))}
      </div>

      {line.rental && (
        <div className="grid gap-2 rounded-xl border border-border bg-muted/25 p-3.5 text-xs sm:grid-cols-3">
          <div>
            <span className="text-muted-foreground">周期起点</span>
            <p className="mt-0.5 font-medium tabular-nums">
              {beijingTime(line.rental.started_at)}
            </p>
          </div>
          <div>
            <span className="text-muted-foreground">当前到期</span>
            <p className="mt-0.5 font-medium tabular-nums">
              {line.rental.permanent
                ? "无限期"
                : beijingTime(line.rental.expires_at)}
            </p>
          </div>
          <div>
            <span className="text-muted-foreground">下次重置</span>
            <p className="mt-0.5 font-medium tabular-nums">
              {beijingTime(line.rental.next_reset_at)}
            </p>
          </div>
        </div>
      )}

      <details className="details">
        <summary>查看本线路收款记录</summary>
        <div className="mt-3">
          <PaymentTable
            lines={state.lines}
            payments={state.payments.filter(
              (payment) => payment.line_id === line.id,
            )}
            adjustments={state.payment_adjustments}
            orders={state.rental_orders}
          />
        </div>
      </details>
    </Modal>
  );
}

export type BillingTarget = { id: string; name: string; rental: Rental | null };

export function RenewalDialog({
  lines,
  close,
  saved,
}: {
  lines: BillingTarget[];
  close: () => void;
  saved: () => Promise<void>;
}) {
  const available = terms.filter((term) =>
    lines.every(
      (line) => line.rental?.customer_id && line.rental.prices[term.id] != null,
    ),
  );
  const [term, setTerm] = useState<Term>(available[0]?.id || "monthly");
  const [storageKey, setStorageKey] = useState("");
  const [pendingId, setPendingId] = useState("");
  const [order, setOrder] = useState<RentalOrder | null>(null);
  const [notice, setNotice] = useState("");
  const [missing, setMissing] = useState(false);

  async function restore(requestId: string) {
    setMissing(false);
    try {
      setOrder(
        await api<RentalOrder>(
          `/rentals/orders/by-request/${encodeURIComponent(requestId)}`,
        ),
      );
      setNotice("");
    } catch (error) {
      setNotice(errorText(error));
      setMissing(error instanceof ApiError && error.status === 404);
    }
  }

  useEffect(() => {
    let alive = true;
    void api<{ scope: string }>("/rentals/context")
      .then(async ({ scope }) => {
        if (!alive) return;
        const key = `chain-order:${scope}`;
        const requestId = localStorage.getItem(key) || "";
        setStorageKey(key);
        setPendingId(requestId);
        if (requestId) {
          try {
            const found = await api<RentalOrder>(
              `/rentals/orders/by-request/${encodeURIComponent(requestId)}`,
            );
            if (alive) setOrder(found);
          } catch (error) {
            if (alive) {
              setNotice(errorText(error));
              setMissing(error instanceof ApiError && error.status === 404);
            }
          }
        }
      })
      .catch((error) => {
        if (alive) setNotice(errorText(error));
      });
    return () => {
      alive = false;
    };
  }, []);

  const clear = () => {
    const targetId = order?.request_id || pendingId;
    if (storageKey && targetId && localStorage.getItem(storageKey) === targetId) {
      localStorage.removeItem(storageKey);
    }
    setPendingId("");
    setNotice("");
    setMissing(false);
  };

  if (order)
    return (
      <OrderDialog
        order={order}
        expectedLines={lines}
        close={close}
        saved={async () => {
          clear();
          await saved();
        }}
      />
    );

  return (
    <Modal
      wide
      title={
        lines.length === 1
          ? "开通 / 续费订单"
          : `批量开通 / 续费 · ${lines.length} 条`
      }
      description="先建立待收款订单，再核对线下到账并下发租期权益。"
      submit={
        storageKey && !pendingId && available.length
          ? "保存待收款订单"
          : undefined
      }
      close={close}
      action={async (data) => {
        const previous = localStorage.getItem(storageKey);
        if (previous) {
          setPendingId(previous);
          await restore(previous);
          return;
        }
        const amounts = Object.fromEntries(
          lines.map((line) => {
            const amount = parseMoney(field(data, `amount:${line.id}`));
            if (amount == null)
              throw new Error(`请填写「${line.name}」的实收金额。`);
            return [line.id, amount];
          }),
        );
        const payload = {
          line_ids: lines.map((line) => line.id),
          term,
          amounts,
          note: field(data, "note"),
        };
        const requestId = billingRequestId();
        localStorage.setItem(storageKey, requestId);
        setPendingId(requestId);
        setOrder(
          await api<RentalOrder>("/rentals/orders", "POST", {
            ...payload,
            request_id: requestId,
          }),
        );
        await saved();
      }}
    >
      {notice && (
        <p role="alert" className="text-xs text-amber-600 dark:text-amber-400">
          {notice}
        </p>
      )}
      {!storageKey && <p className="muted">正在核对当前会话状态…</p>}
      {pendingId ? (
        <div className="space-y-3 rounded-xl border border-amber-500/30 bg-amber-500/10 p-4">
          <p className="text-xs font-medium text-foreground">
            存在一笔结果待核对的订单请求，不会重复下单。
          </p>
          <p className="mono break-all text-[11px] text-muted-foreground">
            请求编号：{pendingId}
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => void restore(pendingId)}
            >
              查询原订单
            </Button>
            {missing && (
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => {
                  if (
                    window.confirm(
                      "服务器未找到原订单。请先在客户与收款中核对；清除此标记仅允许重新创建待收款订单，不会自动确认收款。继续？",
                    )
                  )
                    clear();
                }}
              >
                已核对，清除待确认标记
              </Button>
            )}
          </div>
        </div>
      ) : !available.length ? (
        <p
          role="alert"
          className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3.5 text-xs text-foreground"
        >
          所选代理或套餐必须已设置客户与报价，并至少开放一个共同付费周期。
        </p>
      ) : (
        <>
          <Field
            label="付费周期"
            tooltip="有效期内续费顺延租期且不清零当月已用流量；已过期重开恢复满额。"
          >
            <select
              className="select"
              value={term}
              onChange={(event) => setTerm(event.target.value as Term)}
            >
              {available.map((term) => (
                <option key={term.id} value={term.id}>
                  {term.name} · {term.months} 个自然月
                </option>
              ))}
            </select>
          </Field>

          <div className="space-y-2.5">
            {lines.map((line) => (
              <div
                key={`${line.id}:${term}`}
                className="grid gap-3 rounded-xl border border-border bg-muted/20 p-3.5 sm:grid-cols-[1fr_160px]"
              >
                <div>
                  <p className="text-xs font-semibold text-foreground">
                    {line.name}
                  </p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    当前到期{" "}
                    {line.rental?.permanent
                      ? "无限期"
                      : beijingTime(line.rental?.expires_at)}{" "}
                    · 报价 {money(line.rental?.prices[term])}
                  </p>
                  {line.rental?.permanent && (
                    <p className="mt-1 text-xs text-amber-600 dark:text-amber-400">
                      本次收款将改为有限期并重新开通，流量恢复满额。
                    </p>
                  )}
                </div>
                <Field label="实收 · 元">
                  <Input
                    name={`amount:${line.id}`}
                    type="number"
                    min="0"
                    step="0.01"
                    required
                    defaultValue={line.rental!.prices[term]! / 100}
                  />
                </Field>
              </div>
            ))}
          </div>

          <Field label="收款备注">
            <textarea
              className="textarea"
              name="note"
              rows={2}
              maxLength={2000}
              placeholder="如微信/支付宝转账单号"
            />
          </Field>
        </>
      )}
    </Modal>
  );
}

function billingRequestId() {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

export function OrderDialog({
  order,
  expectedLines,
  close,
  saved,
}: {
  order: RentalOrder;
  expectedLines?: { id: string; name: string }[];
  close: () => void;
  saved: () => Promise<void>;
}) {
  const complete = order.confirmed_at > 0 || order.cancelled_at > 0;
  const orderLineIds = new Set(order.items.map((item) => item.line_id));
  const matchedCount = expectedLines?.filter((line) => orderLineIds.has(line.id)).length ?? 0;
  const mismatched = expectedLines && expectedLines.length > 0 &&
    (matchedCount !== expectedLines.length || matchedCount !== orderLineIds.size);
  const orderTargetNames = order.items.map((item) => item.line_name).join("、");
  return (
    <Modal
      wide
      title="核对开通 / 续费订单"
      description={`订单 ${order.id}`}
      submit={complete ? undefined : "确认已实际收款并下发权益"}
      close={close}
      action={async () => {
        await api(`/rentals/orders/${order.id}/confirm`, "POST", {});
        await saved();
        close();
      }}
    >
      {mismatched && (
        <div
          role="alert"
          className="rounded-xl border border-amber-500/30 bg-amber-500/10 p-3.5 text-xs leading-relaxed text-foreground"
        >
          <p className="font-semibold">
            当前打开的「{expectedLines.map((line) => line.name).join("、")}」
            {matchedCount ? "与这笔订单的线路范围不完全一致。" : "与这笔订单无关。"}
          </p>
          <p className="mt-1 text-muted-foreground">
            下方展示的是上一次待核对的订单（关联：{orderTargetNames}），请核对该订单状态后再继续，避免串单。
          </p>
        </div>
      )}

      <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-muted/25 px-3.5 py-2.5 text-xs">
        <span className="font-medium text-foreground">
          {order.confirmed_at
            ? `已确认收款 · ${beijingTime(order.confirmed_at)}`
            : order.cancelled_at
              ? `已取消 · ${beijingTime(order.cancelled_at)}`
              : "待人工确认收款"}
        </span>
        <Badge variant="primary" appearance="light" size="sm">
          {terms.find((t) => t.id === order.term)?.name}
        </Badge>
      </div>

      <div className="space-y-2">
        {order.items.map((item) => (
          <div
            key={item.line_id}
            className="rounded-lg border border-border bg-card p-3 text-xs"
          >
            <div className="flex flex-wrap justify-between gap-2">
              <span className="font-semibold text-foreground">
                {item.customer_name} · {item.line_name}
              </span>
              <span className="font-semibold tabular-nums text-foreground">
                实收 {money(item.amount_fen)}
                <span className="ml-1.5 font-normal text-muted-foreground">
                  （报价 {money(item.quoted_fen)}）
                </span>
              </span>
            </div>
            <p className="mt-1 text-muted-foreground">
              原到期：
              {item.started_at && !item.expires_at
                ? "无限期"
                : beijingTime(item.expires_at)}
            </p>
          </div>
        ))}
      </div>

      <div className="flex items-center justify-between border-t border-border pt-3 text-sm font-semibold">
        <span>订单合计</span>
        <span className="tabular-nums">
          {money(order.items.reduce((sum, item) => sum + item.amount_fen, 0))}
        </span>
      </div>

      {order.note && (
        <p className="whitespace-pre-wrap break-words rounded-lg bg-muted/30 p-3 text-xs text-muted-foreground">
          备注：{order.note}
        </p>
      )}

      {!complete && (
        <label className="flex items-start gap-2 rounded-lg border border-primary/25 bg-primary/8 p-3 text-xs leading-relaxed text-foreground">
          <input type="checkbox" required className="mt-0.5" />
          <span>
            {mismatched
              ? `已逐项核对「${orderTargetNames}」的客户、周期和金额，确认线下款项实际到账；确认后立即为该订单关联线路开通或顺延租期。`
              : "已逐项核对客户、周期和金额，确认线下款项实际到账；确认后立即开通或顺延租期。"}
          </span>
        </label>
      )}

      {complete && (
        <Button
          type="button"
          variant="outline"
          onClick={() => void saved().then(close)}
        >
          已核对，关闭订单
        </Button>
      )}

      <div className="text-[11px] text-muted-foreground">
        创建人 {order.created_by?.name || "未记录"} · 确认人{" "}
        {order.confirmed_by?.name || "未记录"}
        {order.cancelled_by && ` · 取消人 ${order.cancelled_by.name}`}
      </div>
    </Modal>
  );
}

export function PaymentAdjustmentDialog({
  payment,
  adjustments,
  affectedLines,
  close,
  saved,
}: {
  payment: Payment;
  adjustments: PaymentAdjustment[];
  affectedLines?: { id: string; name: string }[];
  close: () => void;
  saved: () => Promise<void>;
}) {
  const changes = adjustments.filter((a) => a.payment_id === payment.id);
  const gross =
    payment.amount_fen +
    changes
      .filter((a) => a.kind === "correction")
      .reduce((sum, a) => sum + a.delta_fen, 0);
  const net =
    payment.amount_fen + changes.reduce((sum, a) => sum + a.delta_fen, 0);
  const [kind, setKind] = useState<"refund" | "correction">("refund");
  const [disableLine, setDisableLine] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const [storageKey, setStorageKey] = useState("");
  const [recovery, setRecovery] = useState<{
    id: string;
    record?: PaymentAdjustment;
    missing?: boolean;
  } | null>(null);
  const [notice, setNotice] = useState("");
  const request = useRef<{ id: string; payload: object } | null>(null);

  async function recover(requestId: string) {
    setRecovery({ id: requestId });
    setNotice("");
    try {
      setRecovery({
        id: requestId,
        record: await api<PaymentAdjustment>(
          `/rentals/adjustments/by-request/${encodeURIComponent(requestId)}`,
        ),
      });
    } catch (error) {
      setRecovery({
        id: requestId,
        missing: error instanceof ApiError && error.status === 404,
      });
      setNotice(errorText(error));
    }
  }

  useEffect(() => {
    let alive = true;
    void api<{ scope: string }>("/rentals/context")
      .then(async ({ scope }) => {
        if (!alive) return;
        const key = `chain-adjustment:${scope}`;
        const requestId = localStorage.getItem(key);
        setStorageKey(key);
        if (requestId) {
          setRecovery({ id: requestId });
          try {
            const record = await api<PaymentAdjustment>(
              `/rentals/adjustments/by-request/${encodeURIComponent(requestId)}`,
            );
            if (alive) setRecovery({ id: requestId, record });
          } catch (error) {
            if (alive) {
              setRecovery({
                id: requestId,
                missing: error instanceof ApiError && error.status === 404,
              });
              setNotice(errorText(error));
            }
          }
        }
      })
      .catch((error) => {
        if (alive) setNotice(errorText(error));
      });
    return () => {
      alive = false;
    };
  }, []);

  const clear = () => {
    if (localStorage.getItem(storageKey) === (recovery?.id ?? request.current?.id))
      localStorage.removeItem(storageKey);
    setRecovery(null);
    setNotice("");
    request.current = null;
    setSubmitted(false);
  };

  return (
    <Modal
      title="人工退款 / 金额纠错"
      description={recovery ? "核对上一次退款或纠错的登记结果" : `${payment.customer_name} · ${payment.line_name}`}
      submit={
        !storageKey || recovery
          ? undefined
          : submitted
            ? "使用原请求核对并重试"
            : "确认追加记录"
      }
      close={close}
      action={async (data) => {
        if (!request.current) {
          const previous = localStorage.getItem(storageKey);
          if (previous) {
            await recover(previous);
            return;
          }
          const amount = parseMoney(field(data, "amount"));
          if (amount == null) throw new Error("请填写金额。");
          const requestId = billingRequestId();
          localStorage.setItem(storageKey, requestId);
          request.current = {
            id: requestId,
            payload: {
              payment_id: payment.id,
              kind,
              amount_fen: amount,
              expected_net_fen: net,
              note: field(data, "note"),
              disable_line:
                kind === "refund" && field(data, "rights") === "disable",
              expected_line_ids: affectedLines?.map((line) => line.id),
            },
          };
          setSubmitted(true);
        }
        try {
          await api("/rentals/adjustments", "POST", {
            ...request.current.payload,
            request_id: request.current.id,
          });
        } catch (error) {
          if (
            error instanceof ApiError &&
            [400, 401, 403, 404, 409, 422].includes(error.status)
          )
            clear();
          throw error;
        }
        clear();
        await saved();
        close();
      }}
    >
      {notice && (
        <p role="alert" className="text-xs text-amber-600 dark:text-amber-400">
          {notice}
        </p>
      )}
      {!storageKey && <p className="muted">正在核对当前会话状态…</p>}
      {recovery && (
        <div className="space-y-2.5 rounded-xl border border-amber-500/30 bg-amber-500/10 p-3.5">
          <p className="text-xs font-medium text-foreground">
            存在一笔待核对的退款 / 纠错请求，不会重复落账。
          </p>
          {recovery.record ? (
            <>
              {recovery.record.payment_id !== payment.id && (
                <p className="text-xs font-medium">当前打开的收款 {payment.id} 与这笔回执无关。</p>
              )}
              <p className="break-all text-xs">原收款流水 {recovery.record.payment_id}</p>
              <p className="break-all text-xs">调整流水 {recovery.record.id}</p>
              <p className="text-xs text-foreground">
                已登记{" "}
                {recovery.record.kind === "refund" ? "人工退款" : "录入纠错"}{" "}
                {money(recovery.record.delta_fen)} ·{" "}
                {beijingTime(recovery.record.created)}
              </p>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => {
                  clear();
                  void saved().then(close);
                }}
              >
                已核对原操作，关闭
              </Button>
            </>
          ) : (
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => void recover(recovery.id)}
              >
                查询原操作结果
              </Button>
              {recovery.missing && (
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => {
                    if (
                      window.confirm(
                        "请核对原收款和调整流水，确认原请求未落账且已结束后再清除此标记。不得重复实际转账。继续？",
                      )
                    )
                      clear();
                  }}
                >
                  已核对，重新填写
                </Button>
              )}
            </div>
          )}
        </div>
      )}

      <div hidden={Boolean(recovery)} className="grid grid-cols-3 gap-2 rounded-lg border border-border bg-muted/25 p-3 text-xs tabular-nums">
        <div>
          <span className="text-muted-foreground">原始实收</span>
          <p className="mt-0.5 font-semibold">{money(payment.amount_fen)}</p>
        </div>
        <div>
          <span className="text-muted-foreground">修正后实收</span>
          <p className="mt-0.5 font-semibold">{money(gross)}</p>
        </div>
        <div>
          <span className="text-muted-foreground">当前可退</span>
          <p className="mt-0.5 font-semibold text-primary">{money(net)}</p>
        </div>
      </div>

      <fieldset
        hidden={Boolean(recovery)}
        disabled={submitted || !storageKey || Boolean(recovery)}
        className="space-y-3.5"
      >
        <Field label="记录类型">
          <select
            className="select"
            value={kind}
            onChange={(event) => {
              setKind(event.target.value as typeof kind);
              setDisableLine(false);
            }}
          >
            <option value="refund">登记线下退款</option>
            <option value="correction">更正原收款金额误差</option>
          </select>
        </Field>
        <Field
          label={kind === "refund" ? "本次实际退款 · 元" : "正确的原始实收总额 · 元"}
        >
          <Input
            key={kind}
            name="amount"
            type="number"
            min={kind === "refund" ? "0.01" : "0"}
            step="0.01"
            required
            defaultValue={kind === "correction" ? gross / 100 : ""}
          />
        </Field>
        {kind === "refund" && (
          <>
            <Field label="代理权益处理">
              <select className="select" name="rights" value={disableLine ? "disable" : "keep"}
                onChange={(event) => setDisableLine(event.target.value === "disable")}>
                <option value="keep">保留当前租期与启停状态</option>
                <option
                  value="disable"
                  disabled={
                    payment.line_id.startsWith("plan:") && !affectedLines?.length
                  }
                >
                  {payment.line_id.startsWith("plan:")
                    ? "停用此套餐全部成员的关联线路"
                    : "申请停用关联代理"}
                </option>
              </select>
            </Field>
            {payment.line_id.startsWith("plan:") && (
              <div className="rounded-lg border border-border bg-muted/20 p-3 text-xs">
                <p className="text-muted-foreground">
                  选择停用将影响以下套餐成员线路：
                </p>
                <ul className="mt-1.5 list-inside list-disc text-foreground">
                  {affectedLines?.map((line) => (
                    <li key={line.id}>
                      {line.name} · {line.id}
                    </li>
                  ))}
                </ul>
                {!affectedLines?.length && (
                  <p className="text-muted-foreground">
                    当前无法确定成员范围，仅支持保留权益。
                  </p>
                )}
              </div>
            )}
          </>
        )}
        <Field label="原因 / 凭据">
          <textarea
            className="textarea"
            name="note"
            required
            maxLength={600}
            rows={3}
            placeholder="填写退款凭据编号或金额更正说明"
          />
        </Field>
        <label className="flex items-start gap-2 text-xs leading-relaxed text-foreground">
          <input key={`${kind}-${disableLine}`} type="checkbox" required className="mt-0.5" />
          <span>
            {kind === "refund"
              ? disableLine
                ? "确认已经实际完成线下退款，并提交停用所选关联线路的请求。"
                : "确认已经实际完成线下退款，保留原代理权益。"
              : "确认仅更正原录入金额误差，不改变既有租期。"}
          </span>
        </label>
      </fieldset>

      {submitted && (
        <p role="alert" className="text-xs text-amber-600 dark:text-amber-400">
          该请求结果尚需核对，内容已锁定。重试使用同一请求；关闭重开后先查询原操作。
        </p>
      )}
    </Modal>
  );
}

export function LimitsDialog({
  lines,
  close,
  saved,
  request = api,
}: {
  lines: Line[];
  close: () => void;
  saved: () => Promise<void>;
  request?: typeof api;
}) {
  const initial = lines.length === 1 ? lines[0].limits : null;
  return (
    <Modal
      title={
        lines.length === 1
          ? "调整额度与限速"
          : `批量调整额度与限速 · ${lines.length} 条`
      }
      description="留空表示不限制；修改立即同步下发，不清零当前周期已用流量。"
      submit="确认调整并下发"
      close={close}
      action={async (data) => {
        await request("/lines/operate", "POST", {
          line_ids: lines.map((line) => line.id),
          action: "limits",
          limits: {
            monthly_gib: number(data, "quota"),
            upload_mbps: number(data, "upload"),
            download_mbps: number(data, "download"),
          },
        });
        await saved();
        close();
      }}
    >
      <div className="flex flex-wrap gap-1.5">
        {lines.map((line) => (
          <Badge key={line.id} variant="secondary" appearance="light" size="sm">
            {line.name}
          </Badge>
        ))}
      </div>
      <div className="grid gap-3.5 sm:grid-cols-3">
        {[
          { key: "quota", label: "月额度 · GiB", value: initial?.monthly_gib },
          { key: "upload", label: "上传上限 · Mbps", value: initial?.upload_mbps },
          {
            key: "download",
            label: "下载上限 · Mbps",
            value: initial?.download_mbps,
          },
        ].map((input) => (
          <Field key={input.key} label={input.label}>
            <Input
              name={input.key}
              type="number"
              min="0.001"
              step="any"
              placeholder="不限"
              defaultValue={input.value ?? ""}
            />
          </Field>
        ))}
      </div>
    </Modal>
  );
}

export function QueryLinkDialog({
  line,
  close,
  saved,
}: {
  line: Line;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [url, setUrl] = useState("");
  return (
    <Modal
      title="客户私密查询链接"
      description={line.name}
      submit={
        url
          ? undefined
          : line.rental?.query_link_exists
            ? "重新生成并使旧链接失效"
            : "生成查询链接"
      }
      close={close}
      action={async () => {
        const result = await api<{ path: string }>(
          `/rentals/${line.id}/query-link`,
          "POST",
        );
        setUrl(new URL(result.path, location.origin).href);
        await saved();
      }}
    >
      <p className="text-xs leading-relaxed text-muted-foreground">
        客户凭此链接免登录查询该线路余量、到期日与续费报价；重新生成将使旧查询链接失效。
      </p>
      {url && (
        <>
          <Field label="查询链接">
            <textarea className="textarea mono" readOnly rows={3} value={url} />
          </Field>
          <div className="flex flex-wrap gap-2">
            <CopyButton value={url} label="复制客户查询链接" />
            <Button
              type="button"
              variant="outline"
              onClick={() => window.open(url, "_blank", "noopener,noreferrer")}
            >
              打开查询页
            </Button>
          </div>
        </>
      )}
    </Modal>
  );
}
