import { useState, type ReactNode } from "react";
import { Plus, Users } from "lucide-react";
import { api } from "../api";
import type { CustomerPlan, PanelState, Prices } from "../types";
import { beijingTime, money, parseMoney, terms } from "../lib/business";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import {
  Card,
  CardContent,
  CardHeader,
  CardHeading,
  CardTitle,
  CardToolbar,
} from "./ui/card";
import { Progress } from "./ui/progress";
import { Input } from "./ui/input";
import {
  CopyButton,
  Empty,
  Field,
  InfoTip,
  Modal,
  bytes,
  field,
  number,
} from "./shared";
import { RenewalDialog } from "./rental-editor";

export function CustomerPlans({
  state,
  saved,
}: {
  state: PanelState;
  saved: () => Promise<void>;
}) {
  const [modal, setModal] = useState<ReactNode>(null);
  const plans = state.customer_plans || [];
  const close = () => setModal(null);

  return (
    <Card>
      <CardHeader className="gap-3">
        <CardHeading className="flex items-center gap-1.5 space-y-0">
          <CardTitle>客户共享套餐</CardTitle>
          <Badge variant="secondary" appearance="light" size="sm">
            {plans.length}
          </Badge>
          <InfoTip>
            同一套餐内成员共享总额度与租期，也可为单名成员设个人上限。先新建代理并选「仅保存草稿」，即可绑定至套餐。
          </InfoTip>
        </CardHeading>
        <CardToolbar>
          <Button
            disabled={state.busy || !state.customers.length}
            onClick={() =>
              setModal(<PlanEditor state={state} close={close} saved={saved} />)
            }
          >
            <Plus />
            新建共享套餐
          </Button>
        </CardToolbar>
      </CardHeader>

      <CardContent className="space-y-4">
        {!plans.length ? (
          <Empty icon={Users} title="尚无共享套餐">
            {!state.customers.length
              ? "请先在左侧建立客户档案，随后可将多条草稿线路组合为团队共享套餐。"
              : "可将未发布的草稿代理绑定为团队共享流量池并统一收款续期。"}
          </Empty>
        ) : (
          plans.map((plan) => {
            const ids = plan.members.flatMap((member) => member.line_ids);
            const applied = ids.filter((id) =>
              state.lines.some(
                (line) => line.id === id && line.sync_status === "synced",
              ),
            ).length;
            const totalBytes = plan.monthly_gib * 1024 ** 3;
            const usedPct =
              totalBytes > 0
                ? Math.min(100, (plan.usage.used / totalBytes) * 100)
                : 0;

            return (
              <section
                key={plan.id}
                className="space-y-3.5 rounded-xl border border-border bg-muted/20 p-4"
              >
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div>
                    <h3 className="text-sm font-semibold text-foreground">
                      {plan.name}
                    </h3>
                    <p className="mt-0.5 text-xs text-muted-foreground">
                      {state.customers.find(
                        (customer) => customer.id === plan.customer_id,
                      )?.name || plan.customer_id}{" "}
                      · {plan.members.length} 名成员 · {applied}/{ids.length} 条已同步
                    </p>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={state.busy}
                      onClick={() =>
                        setModal(
                          <PlanEditor
                            plan={plan}
                            state={state}
                            close={close}
                            saved={saved}
                          />,
                        )
                      }
                    >
                      套餐 / 成员设置
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={state.busy}
                      onClick={() =>
                        setModal(<PlanQueryLink plan={plan} close={close} />)
                      }
                    >
                      套餐查询链接
                    </Button>
                    <Button
                      size="sm"
                      disabled={state.busy || !plan.rental}
                      onClick={() =>
                        setModal(
                          <RenewalDialog
                            lines={[
                              {
                                id: `plan:${plan.id}`,
                                name: plan.name,
                                rental: plan.rental,
                              },
                            ]}
                            close={close}
                            saved={saved}
                          />,
                        )
                      }
                    >
                      开通 / 续费订单
                    </Button>
                  </div>
                </div>

                <div className="space-y-1.5 rounded-lg border border-border/70 bg-card p-3 text-xs tabular-nums">
                  <div className="flex flex-wrap justify-between gap-2">
                    <span className="font-medium text-foreground">
                      已用 {bytes(plan.usage.used)} / 总额 {bytes(totalBytes)}
                    </span>
                    <span className="text-muted-foreground">
                      已预留 {bytes(plan.usage.reserved)}
                    </span>
                  </div>
                  <Progress value={usedPct} />
                  <div className="flex flex-wrap justify-between gap-2 pt-1 text-[11px] text-muted-foreground">
                    <span>
                      到期：
                      {plan.rental?.permanent
                        ? "无限期"
                        : beijingTime(plan.rental?.expires_at)}
                    </span>
                    <span>
                      重置：
                      {plan.rental?.next_reset_at
                        ? beijingTime(plan.rental.next_reset_at)
                        : "待开通"}
                    </span>
                  </div>
                </div>

                <div className="space-y-2">
                  {plan.members.map((member) => (
                    <div
                      key={member.id}
                      className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border/70 bg-card px-3 py-2.5 text-xs"
                    >
                      <div className="min-w-0">
                        <p className="font-medium text-foreground">
                          {member.name} · 已用 {bytes(member.usage.used)}
                          {member.monthly_gib != null && (
                            <span className="ml-1.5 text-muted-foreground">
                              （上限 {bytes(member.monthly_gib * 1024 ** 3)}）
                            </span>
                          )}
                        </p>
                        <p className="mt-0.5 truncate text-[11px] text-muted-foreground">
                          {member.line_ids
                            .map(
                              (id) =>
                                state.lines.find((line) => line.id === id)
                                  ?.name || id,
                            )
                            .join("、")}
                        </p>
                      </div>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={state.busy}
                        onClick={() =>
                          setModal(
                            <PlanQueryLink
                              plan={plan}
                              member={member}
                              close={close}
                            />,
                          )
                        }
                      >
                        成员查询链接
                      </Button>
                    </div>
                  ))}
                </div>
              </section>
            );
          })
        )}
      </CardContent>
      {modal}
    </Card>
  );
}

function PlanQueryLink({
  plan,
  member,
  close,
}: {
  plan: CustomerPlan;
  member?: CustomerPlan["members"][number];
  close: () => void;
}) {
  const [url, setUrl] = useState("");
  return (
    <Modal
      title={member ? "成员私密查询链接" : "套餐私密查询链接"}
      description={`${plan.name}${member ? ` / ${member.name}` : ""}`}
      submit={url ? undefined : "生成新链接并撤销原链接"}
      close={close}
      action={async () => {
        const result = await api<{ path: string }>(
          `/customer-plans/${plan.id}${member ? `/members/${member.id}` : ""}/query-link`,
          "POST",
          {},
        );
        setUrl(new URL(result.path, location.origin).href);
      }}
    >
      <p className="text-xs leading-relaxed text-muted-foreground">
        {member
          ? "凭此链接仅可查看该成员自身及套餐共享总额，不展示其他成员明细。"
          : "凭此链接可查看整套共享套餐与全部成员用量，适合交付客户负责人。"}
        重新生成后旧链接立即作废。
      </p>
      {url && (
        <>
          <Field label="私密查询链接">
            <textarea className="textarea mono" readOnly rows={3} value={url} />
          </Field>
          <CopyButton value={url} label="复制私密查询链接" />
        </>
      )}
    </Modal>
  );
}

type DraftMember = Pick<
  CustomerPlan["members"][number],
  "id" | "name" | "line_ids" | "monthly_gib"
>;

function PlanEditor({
  plan,
  state,
  close,
  saved,
}: {
  plan?: CustomerPlan;
  state: PanelState;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [members, setMembers] = useState<DraftMember[]>(() =>
    plan
      ? plan.members.map((member) => ({
          id: member.id,
          name: member.name,
          line_ids: [...member.line_ids],
          monthly_gib: member.monthly_gib,
        }))
      : [{ id: "", name: "成员 1", line_ids: [], monthly_gib: null }],
  );
  const [membersChanged, setMembersChanged] = useState(false);
  const eligible = state.lines.filter((line) => line.plan_eligible === true);

  function update(index: number, patch: Partial<DraftMember>) {
    setMembers((old) =>
      old.map((member, i) => (i === index ? { ...member, ...patch } : member)),
    );
  }

  return (
    <Modal
      wide
      protectChanges
      dirty={membersChanged}
      title={plan ? "编辑共享套餐与成员" : "新建客户共享套餐"}
      description="保存套餐与报价后，请通过「开通 / 续费订单」确认收款并应用线路。"
      submit="保存套餐与报价"
      close={close}
      action={async (data) => {
        if (members.some((member) => !member.line_ids.length))
          throw new Error("每名成员至少选择一条线路。");
        const prices = Object.fromEntries(
          terms.map((term) => [term.id, parseMoney(field(data, term.id))]),
        ) as Prices;
        await api("/customer-plans", "POST", {
          id: plan?.id || "",
          name: field(data, "name"),
          customer_id: plan?.customer_id || field(data, "customer"),
          monthly_gib: number(data, "quota"),
          members,
          prices,
        });
        await saved();
        close();
      }}
    >
      <div className="grid gap-3.5 sm:grid-cols-2">
        <Field label="套餐名称">
          <Input
            name="name"
            required
            maxLength={40}
            defaultValue={plan?.name || ""}
          />
        </Field>
        <Field label="所属客户">
          <select
            className="select"
            name="customer"
            required
            disabled={Boolean(plan)}
            defaultValue={plan?.customer_id || ""}
          >
            <option value="" disabled>
              选择客户
            </option>
            {state.customers.map((customer) => (
              <option key={customer.id} value={customer.id}>
                {customer.name}
              </option>
            ))}
          </select>
        </Field>
      </div>

      <Field
        label="每周期共享总额度 · GiB"
        tooltip="全体成员共享该总额度；成员个人上限不独占预留配额。"
      >
        <Input
          name="quota"
          type="number"
          min="0.001"
          max="1000000000"
          step="any"
          required
          defaultValue={plan?.monthly_gib ?? ""}
        />
      </Field>

      <div className="grid gap-3.5 sm:grid-cols-3">
        {terms.map((term) => (
          <Field key={term.id} label={`${term.name}报价 · 元`}>
            <Input
              name={term.id}
              type="number"
              min="0"
              step="0.01"
              placeholder="留空不开放"
              defaultValue={
                plan?.rental?.prices[term.id] == null
                  ? ""
                  : plan.rental.prices[term.id]! / 100
              }
            />
          </Field>
        ))}
      </div>

      {members.map((member, index) => {
        const original = plan?.members.find((old) => old.id === member.id);
        const candidates = state.lines.filter(
          (line) =>
            member.line_ids.includes(line.id) ||
            eligible.some((candidate) => candidate.id === line.id),
        );
        return (
          <fieldset
            key={index}
            className="space-y-3 rounded-xl border border-border bg-muted/20 p-3.5"
          >
            <legend className="px-1.5 text-xs font-semibold text-foreground">
              成员 {index + 1}
            </legend>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label={`成员 ${index + 1} 名称`}>
                <Input
                  required
                  maxLength={40}
                  value={member.name}
                  onChange={(event) =>
                    update(index, { name: event.target.value })
                  }
                />
              </Field>
              <Field label={`成员 ${index + 1} 额外硬上限 · GiB`}>
                <Input
                  type="number"
                  min="0.001"
                  step="any"
                  placeholder="留空共用总额"
                  value={member.monthly_gib ?? ""}
                  onChange={(event) =>
                    update(index, {
                      monthly_gib: event.target.value
                        ? Number(event.target.value)
                        : null,
                    })
                  }
                />
              </Field>
            </div>
            {!candidates.length ? (
              <p className="text-xs text-amber-600 dark:text-amber-400">
                暂无可绑定的未发布草稿线路。请先在「代理」页面新建代理并选择「仅保存草稿」。
              </p>
            ) : (
              <div className="max-h-40 space-y-1.5 overflow-auto rounded-lg border border-border bg-card p-2.5">
                {candidates.map((line) => {
                  const retained = Boolean(
                    original?.line_ids.includes(line.id),
                  );
                  const elsewhere = members.some(
                    (other, i) =>
                      i !== index && other.line_ids.includes(line.id),
                  );
                  return (
                    <label
                      key={line.id}
                      className="flex items-center gap-2 text-xs text-foreground"
                    >
                      <input
                        type="checkbox"
                        checked={member.line_ids.includes(line.id)}
                        disabled={retained || elsewhere}
                        onChange={(event) =>
                          update(index, {
                            line_ids: event.target.checked
                              ? [...member.line_ids, line.id]
                              : member.line_ids.filter((id) => id !== line.id),
                          })
                        }
                      />
                      <span>
                        {line.name}
                        {retained
                          ? "（已绑定）"
                          : elsewhere
                            ? "（已分配其他成员）"
                            : ""}
                      </span>
                    </label>
                  );
                })}
              </div>
            )}
            {!original && members.length > 1 && (
              <Button
                type="button"
                size="sm"
                variant="ghost"
                onClick={() => {
                  setMembers((old) => old.filter((_, i) => i !== index));
                  setMembersChanged(true);
                }}
              >
                移除此成员
              </Button>
            )}
          </fieldset>
        );
      })}

      <Button
        type="button"
        size="sm"
        variant="outline"
        disabled={members.length >= 500}
        onClick={() => {
          setMembers((old) => [
            ...old,
            { id: "", name: `成员 ${old.length + 1}`, line_ids: [], monthly_gib: null },
          ]);
          setMembersChanged(true);
        }}
      >
        <Plus />
        添加成员
      </Button>
    </Modal>
  );
}
