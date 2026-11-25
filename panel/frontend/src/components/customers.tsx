import { useState, type ReactNode } from "react";
import { ArrowDownLeft, ArrowUpRight, Download, Edit3, Plus, Receipt, Search, Wallet } from "lucide-react";
import { api } from "../api";
import type { Customer, PanelState } from "../types";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import {
  Card,
  CardContent,
  CardHeader,
  CardHeading,
  CardTable,
  CardTitle,
  CardToolbar,
} from "./ui/card";
import { Input } from "./ui/input";
import { Empty, Field, InfoTip, Modal, download, field } from "./shared";
import {
  OrderDialog,
  PaymentAdjustmentDialog,
  PaymentTable,
  clearOrderStorage,
} from "./rental-editor";
import { beijingTime, money, terms } from "../lib/business";
import { CustomerPlans } from "./customer-plans";
import { adjustmentRights } from "./rental-shared";

export function Customers({
  state,
  saved,
}: {
  state: PanelState;
  saved: () => Promise<void>;
}) {
  const [search, setSearch] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);
  const [customerId, setCustomerId] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const close = () => setModal(null);

  function edit(customer?: Customer) {
    setModal(
      <Modal
        title={customer ? "编辑客户" : "新建客户"}
        submit="保存客户"
        close={close}
        action={async (data) => {
          await api("/customers", "POST", {
            id: customer?.id,
            name: field(data, "name"),
            contact: field(data, "contact"),
            notes: field(data, "notes"),
          });
          await saved();
          close();
        }}
      >
        <Field label="客户名称">
          <Input
            name="name"
            required
            maxLength={120}
            defaultValue={customer?.name || ""}
          />
        </Field>
        <Field label="联系方式">
          <Input
            name="contact"
            maxLength={500}
            defaultValue={customer?.contact || ""}
            placeholder="微信、Telegram、邮箱等"
          />
        </Field>
        <Field label="内部备注" hint="仅后台可见">
          <textarea
            className="textarea"
            name="notes"
            rows={3}
            maxLength={4000}
            defaultValue={customer?.notes || ""}
          />
        </Field>
      </Modal>,
    );
  }

  const customers = state.customers.filter((customer) =>
    `${customer.name} ${customer.contact}`
      .toLowerCase()
      .includes(search.trim().toLowerCase()),
  );
  const orders = state.rental_orders || [];
  const adjustments = state.payment_adjustments || [];
  const inPeriod = (at: number) => {
    const day = new Date((at + 8 * 3600) * 1000).toISOString().slice(0, 10);
    return (!from || day >= from) && (!to || day <= to);
  };
  const matchingPayments = state.payments.filter(
    (p) => !customerId || p.customer_id === customerId,
  );
  const payments = matchingPayments.filter((p) => inPeriod(p.created));
  const changes = adjustments.filter(
    (a) =>
      inPeriod(a.created) && matchingPayments.some((p) => p.id === a.payment_id),
  );
  const paid = payments.reduce((sum, p) => sum + p.amount_fen, 0);
  const refundsRaw = changes
    .filter((a) => a.kind === "refund")
    .reduce((sum, a) => sum + Math.abs(a.delta_fen), 0);
  const corrections = changes
    .filter((a) => a.kind === "correction")
    .reduce((sum, a) => sum + a.delta_fen, 0);
  const netDelta = paid + corrections - refundsRaw;

  const billCustomers = new Map([
    ...state.payments.map((p) => [p.customer_id, p.customer_name] as const),
    ...state.customers.map((c) => [c.id, c.name] as const),
  ]);
  const selectedOrders = orders.filter(
    (o) =>
      inPeriod(o.created) &&
      (!customerId || o.items.some((i) => i.customer_id === customerId)),
  );

  function exportLedger() {
    const rows = [
      ...payments.map((p) => ({
        at: p.created,
        type: "原始收款",
        id: p.id,
        payment: p,
        delta: p.amount_fen,
        note: p.note,
        rights: "按原记录开通/续费",
        operator: orders.find((o) => o.request_id === p.request_id)
          ?.confirmed_by,
      })),
      ...changes.map((a) => ({
        at: a.created,
        type: a.kind === "refund" ? "人工退款" : "录入纠错（非新增收款）",
        id: a.id,
        payment: state.payments.find((p) => p.id === a.payment_id)!,
        delta: a.delta_fen,
        note: a.note,
        rights: a.disable_line ? "已提交停用关联代理请求" : "未调整权益",
        operator: a.operator,
      })),
    ].sort((a, b) => a.at - b.at);
    const cells = [
      [
        "时间（北京时间）",
        "类型",
        "流水编号",
        "原收款编号",
        "订单编号",
        "客户编号",
        "客户",
        "线路编号",
        "线路",
        "金额变动（元）",
        "权益处理",
        "备注",
        "操作成员编号",
        "操作成员",
      ],
      ...rows.map((row) => [
        beijingTime(row.at),
        row.type,
        row.id,
        row.payment.id,
        orders.find((o) => o.request_id === row.payment.request_id)?.id ||
          "历史未记录",
        row.payment.customer_id,
        row.payment.customer_name,
        row.payment.line_id,
        row.payment.line_name,
        (row.delta / 100).toFixed(2),
        row.rights,
        row.note,
        row.operator?.id || "未记录",
        row.operator?.name || "未记录",
      ]),
    ];
    const csv = cells
      .map((row) =>
        row
          .map(
            (value, index) =>
              `"${(index !== 9 && /^[\s]*[=+\-@\t\r]/.test(value) ? "'" + value : value).replaceAll('"', '""')}"`,
          )
          .join(","),
      )
      .join("\r\n");
    download("收款与调整流水.csv", "\ufeff" + csv);
  }

  if (state.viewer && !["admin", "finance"].includes(state.viewer.role))
    return (
      <p role="alert" className="text-sm text-muted-foreground">
        当前角色没有客户与账务访问权限。
      </p>
    );

  return (
    <div className="space-y-5">
      <div className="grid grid-cols-2 gap-3.5 lg:grid-cols-4">
        {[
          {
            label: "筛选期原始收款",
            value: money(paid),
            icon: ArrowDownLeft,
            tone: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
          },
          {
            label: "筛选期实际退款",
            value: refundsRaw ? `-${money(refundsRaw)}` : money(0),
            icon: ArrowUpRight,
            tone: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
          },
          {
            label: "筛选期录入纠错",
            value: money(corrections),
            icon: Receipt,
            tone: "bg-violet-500/10 text-violet-600 dark:text-violet-400",
          },
          {
            label: "账面净变动",
            value: money(netDelta),
            icon: Wallet,
            tone: "bg-primary/10 text-primary",
          },
        ].map((item) => {
          const Icon = item.icon;
          return (
            <Card key={item.label}>
              <CardContent className="p-4">
                <div className="flex items-center justify-between gap-2">
                  <span className="text-xs font-medium text-muted-foreground">
                    {item.label}
                  </span>
                  <span
                    className={`inline-flex size-7 items-center justify-center rounded-lg ${item.tone}`}
                  >
                    <Icon className="size-3.5" />
                  </span>
                </div>
                <p className="mt-2.5 text-lg font-semibold tabular-nums text-foreground">
                  {item.value}
                </p>
              </CardContent>
            </Card>
          );
        })}
      </div>

      <Card>
        <CardContent className="flex flex-wrap items-center justify-between gap-3 py-3.5">
          <div className="min-w-0">
            <div className="flex items-center gap-1.5">
              <span className="text-xs font-semibold text-foreground">
                客户查询页统一续费入口
              </span>
              <InfoTip>所有客户私密查询页底部展示的联系方式或客服网址。</InfoTip>
            </div>
            <p className="mt-0.5 truncate text-xs text-muted-foreground">
              {state.renewal_contact || "尚未配置联系方式"}
            </p>
          </div>
          <Button
            size="sm"
            variant="outline"
            onClick={() =>
              setModal(
                <Modal
                  title="设置续费联系入口"
                  submit="保存"
                  close={close}
                  action={async (data) => {
                    await api("/rentals/contact", "POST", {
                      renewal_contact: field(data, "contact"),
                    });
                    await saved();
                    close();
                  }}
                >
                  <Field label="统一续费联系方式">
                    <textarea
                      className="textarea"
                      name="contact"
                      rows={3}
                      maxLength={2000}
                      defaultValue={state.renewal_contact}
                      placeholder="例如：微信 example，续费时请附上查询页服务编号。"
                    />
                  </Field>
                </Modal>,
              )
            }
          >
            <Edit3 />
            编辑联系入口
          </Button>
        </CardContent>
      </Card>

      <div className="grid items-start gap-5 xl:grid-cols-[minmax(0,1.15fr)_minmax(0,1fr)]">
        <Card>
          <CardHeader className="gap-3">
            <CardHeading className="flex items-center gap-2 space-y-0">
              <CardTitle>客户档案</CardTitle>
              <Badge variant="secondary" appearance="light" size="sm">
                {state.customers.length}
              </Badge>
            </CardHeading>
            <CardToolbar className="flex-wrap">
              <div className="relative w-44 sm:w-52">
                <Search className="pointer-events-none absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
                <Input
                  className="pl-8.5"
                  aria-label="搜索客户"
                  placeholder="客户名称 / 联系方式"
                  value={search}
                  onChange={(event) => setSearch(event.target.value)}
                />
              </div>
              <Button onClick={() => edit()}>
                <Plus />
                新建客户
              </Button>
            </CardToolbar>
          </CardHeader>

          {customers.length ? (
            <CardTable>
              <div
                className="overflow-x-auto fleet-responsive"
                tabIndex={0}
                role="region"
                aria-label="客户档案表"
              >
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>客户</th>
                      <th>联系方式</th>
                      <th>线路数</th>
                      <th>备注</th>
                      <th className="text-right">操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {customers.map((customer) => {
                      const count = state.lines.filter(
                        (line) => line.rental?.customer_id === customer.id,
                      ).length;
                      const planCount = (state.customer_plans || []).filter(
                        (plan) => plan.customer_id === customer.id,
                      ).length;
                      const hasRelations = count > 0 || planCount > 0;
                      return (
                        <tr key={customer.id}>
                          <td data-label="客户" className="font-semibold">
                            {customer.name}
                          </td>
                          <td
                            data-label="联系方式"
                            className="max-w-52 whitespace-pre-wrap break-words text-xs text-muted-foreground"
                          >
                            {customer.contact || "—"}
                          </td>
                          <td data-label="线路数" className="tabular-nums">
                            <Badge variant="secondary" appearance="light" size="sm">
                              {count}
                            </Badge>
                          </td>
                          <td
                            data-label="备注"
                            className="max-w-56 whitespace-pre-wrap break-words text-xs text-muted-foreground"
                          >
                            {customer.notes || "—"}
                          </td>
                          <td data-label="操作">
                            <div className="flex justify-start gap-1.5 md:justify-end">
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={() => edit(customer)}
                              >
                                编辑
                              </Button>
                              <Button
                                variant="ghost"
                                size="sm"
                                disabled={hasRelations}
                                title={
                                  hasRelations
                                    ? "请先删除或解绑该客户关联的代理与共享套餐（启用中的套餐线路停用后可移除）"
                                    : undefined
                                }
                                onClick={() =>
                                  setModal(
                                    <Modal
                                      title="删除客户档案"
                                      description={`确定删除「${customer.name}」的客户档案吗？历史收款记录仍会保留。`}
                                      submit="确认删除"
                                      close={close}
                                      action={async () => {
                                        await api(
                                          `/customers/${customer.id}`,
                                          "DELETE",
                                        );
                                        await saved();
                                        close();
                                      }}
                                    />,
                                  )
                                }
                              >
                                删除
                              </Button>
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </CardTable>
          ) : (
            <Empty title={search ? "没有匹配的客户" : "尚无客户档案"}>
              {!search && "新建客户档案后即可在代理列表中绑定归属并登记收款。"}
            </Empty>
          )}
        </Card>

        <CustomerPlans state={state} saved={saved} />
      </div>

      <Card>
        <CardHeader className="gap-3">
          <CardHeading className="flex items-center gap-1.5 space-y-0">
            <CardTitle>订单与流水对账</CardTitle>
            <InfoTip>
              按每笔收款或调整的发生日期（北京时间）筛选；纠错仅修正录入差额，不计作新增订单。
            </InfoTip>
          </CardHeading>
          <CardToolbar>
            <Button
              variant="outline"
              size="sm"
              disabled={Boolean(from && to && from > to)}
              onClick={exportLedger}
            >
              <Download />
              导出筛选后的流水 CSV
            </Button>
          </CardToolbar>
        </CardHeader>

        <CardContent className="space-y-5">
          <div className="grid gap-3.5 rounded-xl border border-border bg-muted/20 p-4 sm:grid-cols-3">
            <Field label="客户筛选">
              <select
                className="select"
                value={customerId}
                onChange={(event) => setCustomerId(event.target.value)}
              >
                <option value="">全部客户（含已删档）</option>
                {[...billCustomers].map(([id, name]) => (
                  <option key={id} value={id}>
                    {name}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="起始日期 · 北京时间">
              <Input
                type="date"
                value={from}
                onChange={(event) => setFrom(event.target.value)}
              />
            </Field>
            <Field label="结束日期 · 含当天">
              <Input
                type="date"
                value={to}
                onChange={(event) => setTo(event.target.value)}
                min={from || undefined}
              />
            </Field>
          </div>

          {from && to && from > to && (
            <p role="alert" className="text-xs text-destructive">
              起始日期不能晚于结束日期。
            </p>
          )}

          {selectedOrders.length > 0 && (
            <div className="space-y-2.5">
              <div className="flex items-center gap-2">
                <h3 className="text-xs font-semibold text-foreground">
                  开通 / 续费订单
                </h3>
                <Badge variant="secondary" appearance="light" size="xs">
                  {selectedOrders.length}
                </Badge>
              </div>
              <div className="max-h-72 space-y-2 overflow-auto">
                {[...selectedOrders].reverse().map((order) => (
                  <div
                    key={order.id}
                    className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-card p-3.5"
                  >
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2 text-xs">
                        <Badge
                          variant={
                            order.confirmed_at
                              ? "success"
                              : order.cancelled_at
                                ? "secondary"
                                : "warning"
                          }
                          appearance="light"
                          size="sm"
                        >
                          {order.confirmed_at
                            ? "已确认收款"
                            : order.cancelled_at
                              ? "已取消"
                              : "待收款"}
                        </Badge>
                        <span className="font-semibold text-foreground">
                          {terms.find((t) => t.id === order.term)?.name} ·{" "}
                          {money(
                            order.items.reduce(
                              (sum, item) => sum + item.amount_fen,
                              0,
                            ),
                          )}
                        </span>
                      </div>
                      <p className="mt-1 text-xs text-muted-foreground">
                        {beijingTime(order.created)} ·{" "}
                        {order.items
                          .map(
                            (item) => `${item.customer_name} / ${item.line_name}`,
                          )
                          .join("、")}
                      </p>
                      <p className="mono mt-0.5 break-all text-[11px] text-muted-foreground/80">
                        {order.id}
                      </p>
                    </div>
                    <div className="flex flex-wrap gap-2">
                      <Button
                        size="sm"
                        variant={
                          order.confirmed_at || order.cancelled_at
                            ? "outline"
                            : "primary"
                        }
                        onClick={() =>
                          setModal(
                            <OrderDialog
                              order={order}
                              close={close}
                              saved={saved}
                            />,
                          )
                        }
                      >
                        {order.confirmed_at || order.cancelled_at
                          ? "查看订单"
                          : "核对并确认收款"}
                      </Button>
                      {!order.confirmed_at && !order.cancelled_at && (
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() =>
                            setModal(
                              <Modal
                                title="取消待收款订单"
                                description={`订单 ${order.id} 取消后不改变现有代理权益。`}
                                submit="确认尚未收款并取消"
                                close={close}
                                action={async () => {
                                  await api(
                                    `/rentals/orders/${order.id}/cancel`,
                                    "POST",
                                    {},
                                  );
                                  clearOrderStorage(order);
                                  await saved();
                                  close();
                                }}
                              />,
                            )
                          }
                        >
                          取消订单
                        </Button>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          <div className="space-y-2">
            <div className="flex items-center gap-1.5">
              <h3 className="text-xs font-semibold text-foreground">收款记录</h3>
              <InfoTip>每笔收款展示完整关联退款与纠错明细。</InfoTip>
            </div>
            <div className="overflow-hidden rounded-xl border border-border">
              <PaymentTable
                lines={state.lines}
                payments={payments}
                adjustments={adjustments}
                orders={orders}
                adjust={(payment) => {
                  const plan = state.customer_plans?.find(
                    (plan) => `plan:${plan.id}` === payment.line_id,
                  );
                  const ids = plan
                    ? plan.members.flatMap((member) => member.line_ids)
                    : payment.line_id.startsWith("plan:")
                      ? []
                      : [payment.line_id];
                  const affectedLines = ids.map((id) => ({
                    id,
                    name: state.lines.find((line) => line.id === id)?.name || id,
                  }));
                  setModal(
                    <PaymentAdjustmentDialog
                      payment={payment}
                      adjustments={adjustments}
                      affectedLines={affectedLines}
                      close={close}
                      saved={saved}
                    />,
                  );
                }}
              />
            </div>
          </div>

          {changes.length > 0 && (
            <div className="space-y-2">
              <h3 className="text-xs font-semibold text-foreground">
                退款与金额纠错记录
              </h3>
              <div className="space-y-2">
                {[...changes].reverse().map((change) => {
                  const payment = state.payments.find(
                    (p) => p.id === change.payment_id,
                  )!;
                  return (
                    <div
                      key={change.id}
                      className="rounded-lg border border-border bg-muted/20 p-3 text-xs"
                    >
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <span className="font-semibold text-foreground">
                          {payment.customer_name} / {payment.line_name}
                        </span>
                        <Badge
                          variant={change.kind === "refund" ? "warning" : "info"}
                          appearance="light"
                          size="xs"
                        >
                          {change.kind === "refund" ? "人工退款" : "录入纠错"}{" "}
                          {money(change.delta_fen)}
                        </Badge>
                      </div>
                      <p className="mt-1 text-muted-foreground">
                        {beijingTime(change.created)} ·{" "}
                        {adjustmentRights(change, state.lines)}
                      </p>
                      <p className="mono mt-0.5 break-all text-[11px] text-muted-foreground/75">
                        调整流水 {change.id} · 原收款流水 {change.payment_id}
                      </p>
                      <p className="mt-1 whitespace-pre-wrap break-words text-foreground">
                        {change.note}
                      </p>
                    </div>
                  );
                })}
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      {modal}
    </div>
  );
}
