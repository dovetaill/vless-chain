import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Activity,
  Bell,
  CheckCircle2,
  Mail,
  Search,
  Send,
  Terminal,
  XCircle,
} from "lucide-react";
import { api } from "../api";
import type { Check } from "../types";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { Input } from "./ui/input";
import {
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardHeading,
  CardTable,
  CardTitle,
  CardToolbar,
} from "./ui/card";
import { Empty, Field, InfoTip, errorText, stamp } from "./shared";

export type Policy = {
  enabled: boolean;
  interval: number;
  failures: number;
  recoveries: number;
  offline: number;
  reminder: number;
  url: string;
  quota: number[];
  expiry_days: number[];
};

type Channels = {
  telegram_token: string;
  telegram_chat: string;
  smtp_host: string;
  smtp_port: number;
  smtp_starttls: boolean;
  smtp_user: string;
  smtp_password: string;
  mail_from: string;
  mail_to: string;
};

type Notice = {
  id: string;
  at: number;
  message: string;
  error: string;
  telegram_done: boolean;
  mail_done: boolean;
};

type Monitor = {
  local: Policy;
  effective: Policy;
  override: Policy | null;
  controller: string;
  channels: Channels;
  telegram_set: boolean;
  password_set: boolean;
  notices: Notice[];
  checks: Record<string, Check>;
  conditions: Record<string, { active: boolean; message: string; bad?: number }>;
};

export const defaultPolicy: Policy = {
  enabled: false,
  interval: 300,
  failures: 3,
  recoveries: 2,
  offline: 60,
  reminder: 86400,
  url: "https://example.com/",
  quota: [80, 95, 100],
  expiry_days: [7, 3, 1, 0],
};

export function PolicyFields({
  value,
  change,
}: {
  value: Policy;
  change: (value: Policy) => void;
}) {
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2.5 rounded-xl border border-border bg-muted/25 px-4 py-3 text-xs font-semibold text-foreground">
        <label className="flex cursor-pointer items-center gap-2.5">
          <input
            type="checkbox"
            checked={value.enabled}
            onChange={(e) => change({ ...value, enabled: e.target.checked })}
          />
          <span>启用定时检测和状态提醒</span>
        </label>
        <InfoTip>
          仅探测已应用且正常计费周期内的代理；HTTPS 探测不验证 UDP 转发，每次探测会产生极少量流量。
        </InfoTip>
      </div>

      <div className="grid gap-3.5 md:grid-cols-2 lg:grid-cols-3">
        <div className="md:col-span-2 lg:col-span-3">
          <Field label="检测目标（HTTP / HTTPS）">
            <Input
              value={value.url}
              onChange={(e) => change({ ...value, url: e.target.value })}
              required
              type="url"
            />
          </Field>
        </div>
        {(
          [
            ["interval", "检测间隔（秒）", 60, 86400],
            ["failures", "连续失败次数", 1, 20],
            ["recoveries", "连续恢复次数", 1, 20],
            ["offline", "失联阈值（秒）", 30, 3600],
            ["reminder", "持续故障提醒间隔（秒）", 3600, 604800],
          ] as const
        ).map(([key, label, min, max]) => (
          <Field label={label} key={key}>
            <Input
              type="number"
              min={min}
              max={max}
              required
              value={value[key]}
              onChange={(e) =>
                change({ ...value, [key]: Number(e.target.value) })
              }
            />
          </Field>
        ))}
        <Field label="流量提醒百分比（逗号分隔）">
          <Input
            defaultValue={value.quota.join(",")}
            onChange={(e) =>
              change({
                ...value,
                quota: e.target.value
                  .split(",")
                  .filter((v) => v.trim())
                  .map(Number),
              })
            }
          />
        </Field>
        <Field label="到期提醒天数（0 表示到期当天）">
          <Input
            defaultValue={value.expiry_days.join(",")}
            onChange={(e) =>
              change({
                ...value,
                expiry_days: e.target.value
                  .split(",")
                  .filter((v) => v.trim())
                  .map(Number),
              })
            }
          />
        </Field>
      </div>
    </div>
  );
}

const NODE_CHECK_LABELS: Record<string, string> = {
  clock: "系统时钟",
  ntp: "自动时间同步",
  cert: "TLS 证书",
  agent: "Agent 连接",
  runtime: "代理运行组件",
  listeners: "端口监听",
  traffic_error: "流量账本",
  traffic_sync_error: "流量快照同步",
};

function isKernelDebugLine(line: string) {
  return /^(?:Xray|Mihomo|\d{10}\s|\d{4}[/-]\d{2}[/-]\d{2}|time=|level=|\[(?:Info|Warning|Error|Debug)\])/i.test(line);
}

function parseCheckItem(
  key: string,
  condition: Monitor["conditions"][string] | undefined,
  check: Check,
  knownLines?: { id: string; name: string }[],
  knownNodes?: { id: string; name: string }[],
) {
  const rows = (check.detail || "").split(/\r?\n/).map(row => row.trim()).filter(Boolean);
  const firstLine = rows[0] || "";

  const matchedLine = key.startsWith("probe:")
    ? knownLines?.find((l) => key.endsWith(`:${l.id}`))?.name
    : undefined;
  const nodeKeyMatch =
    /^(clock|ntp|cert|agent|runtime|listeners|traffic_error|traffic_sync_error):([^:]+)/.exec(
      key,
    );
  const matchedNode = nodeKeyMatch
    ? knownNodes?.find((n) => n.id === nodeKeyMatch[2])?.name
    : undefined;

  const message = condition?.message?.trim() || "";
  const colon = message.indexOf("：");
  const condPrefix = colon > 0 ? message.slice(0, colon).trim() : message;
  const condRest = colon > 0 ? message.slice(colon + 1).trim() : "";
  let subject = condPrefix || matchedLine || key;
  if (nodeKeyMatch) {
    const name = matchedNode || (colon > 0 && condPrefix) || nodeKeyMatch[2];
    subject = `${name} · ${NODE_CHECK_LABELS[nodeKeyMatch[1]]}`;
  } else if (matchedLine && condPrefix && !condPrefix.includes(matchedLine)) {
    subject = `${matchedLine} · ${condPrefix}`;
  } else if (!message && !matchedLine && firstLine.includes("：")) {
    const prefix = firstLine.slice(0, firstLine.indexOf("：")).trim();
    if (
      !/^(HTTP|HTTPS|TCP|TLS|DNS|连接|探测|请求|超时|自动分段检查)/i.test(prefix)
    ) {
      subject = prefix;
    }
  }

  const summaryParts: string[] = [];
  const logLines: string[] = [];
  let inLog = false;
  const inlineLog = /[;；]\s*(?:Xray|Mihomo):/i;
  for (const [index, row] of rows.entries()) {
    // The backend appends the separate path check after the entry probe's logs.
    if (row.startsWith("自动分段检查")) inLog = false;
    if (inLog || (index > 0 && isKernelDebugLine(row))) {
      logLines.push(row);
      inLog = true;
      continue;
    }
    const marker = row.search(inlineLog);
    if (marker >= 0) {
      summaryParts.push(row.slice(0, marker).trim());
      logLines.push(row.slice(marker + 1).trim());
      inLog = true;
    } else {
      summaryParts.push(row);
    }
  }

  if (summaryParts[0]?.startsWith(subject + "：")) {
    summaryParts[0] = summaryParts[0].slice(subject.length + 1).trim();
  }
  if (typeof check.outbound?.ok === "boolean" && !summaryParts.some(row => row.startsWith("自动分段检查"))) {
    summaryParts.push(check.outbound.ok ? "自动分段检查：出口及后续链路通过" : "自动分段检查：出口及后续链路也未通过");
  }
  // Clock conditions retain their last message after a healthy or unverified sample.
  const showCondition = !nodeKeyMatch || (!check.ok && (condition?.bad ?? 0) > 0);
  if (showCondition && condRest && (!firstLine || !condRest.includes(firstLine))) {
    const context = condRest.split(inlineLog, 1)[0].trim();
    if (context && context !== summaryParts[0]) summaryParts.unshift(context);
  }
  const finalSummary = summaryParts.filter(Boolean).join(" · ") || (!nodeKeyMatch && condPrefix) || "无详细信息";
  const rawLogs = logLines.join("\n");

  return {
    subject,
    summary: finalSummary,
    rawLogs,
    searchText: `${subject} ${finalSummary} ${matchedLine || ""} ${showCondition ? message : ""} ${rawLogs}`.toLowerCase(),
  };
}

function parseNoticeItem(msg: string) {
  const trimmed = msg.trim();
  let kind: "fault" | "recover" | "info" = "info";
  if (trimmed.includes("故障：") || trimmed.startsWith("故障")) kind = "fault";
  else if (trimmed.includes("恢复：") || trimmed.startsWith("恢复")) kind = "recover";

  // Split headline vs verbose probe tail
  const firstSentenceEnd = trimmed.indexOf("。");
  const headline =
    firstSentenceEnd > 0 && firstSentenceEnd < 90
      ? trimmed.slice(0, firstSentenceEnd + 1)
      : trimmed.slice(0, 96) + (trimmed.length > 96 ? "…" : "");
  const more =
    firstSentenceEnd > 0 && firstSentenceEnd + 1 < trimmed.length
      ? trimmed.slice(firstSentenceEnd + 1).trim()
      : trimmed.length > 96
        ? trimmed
        : "";
  return { kind, headline, more };
}

const LOG_PAGE_SIZE = 10;

export function Monitoring({
  lines,
  nodes,
}: {
  lines?: { id: string; name: string }[];
  nodes?: { id: string; name: string }[];
} = {}) {
  const [data, setData] = useState<Monitor>();
  const [policy, setPolicy] = useState<Policy>(defaultPolicy);
  const [channels, setChannels] = useState<Channels>();
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [clearTelegram, setClearTelegram] = useState(false);
  const [clearPassword, setClearPassword] = useState(false);

  const [checkSearch, setCheckSearch] = useState("");
  const [checkFilter, setCheckFilter] = useState<"all" | "fail" | "ok">("all");
  const [checkPage, setCheckPage] = useState(1);
  const [expandedLogKey, setExpandedLogKey] = useState<string | null>(null);

  const [noticePage, setNoticePage] = useState(1);
  const [expandedNoticeId, setExpandedNoticeId] = useState<string | null>(null);

  const refresh = useCallback(async (initial = false) => {
    const value = await api<Monitor>("/monitoring");
    setData(value);
    if (initial) {
      setPolicy(value.local);
      setChannels(value.channels);
    }
  }, []);

  useEffect(() => {
    void refresh(true).catch((e) => setError(errorText(e)));
    const timer = setInterval(
      () => void refresh().catch((e) => setError(errorText(e))),
      10000,
    );
    return () => clearInterval(timer);
  }, [refresh]);

  const checkEntries = useMemo(() => {
    const entries = Object.entries(data?.checks || {}).map(([key, check]) => {
      const cond = data?.conditions[key];
      const parsed = parseCheckItem(
        key,
        cond,
        check,
        lines,
        nodes,
      );
      const passed = check.ok && !(data?.effective.enabled && cond?.active);
      return { key, check, cond, passed, ...parsed };
    });
    entries.sort((a, b) => Number(a.passed) - Number(b.passed) || b.check.checked_at - a.check.checked_at);
    return entries.filter((item) => {
      if (checkFilter === "fail" && item.passed) return false;
      if (checkFilter === "ok" && !item.passed) return false;
      if (
        checkSearch.trim() &&
        !item.searchText.includes(checkSearch.trim().toLowerCase())
      )
        return false;
      return true;
    });
  }, [data, checkFilter, checkSearch, lines, nodes]);

  const checkPages = Math.max(1, Math.ceil(checkEntries.length / LOG_PAGE_SIZE));
  const currentCheckPage = Math.min(checkPage, checkPages);
  const visibleChecks = checkEntries.slice(
    (currentCheckPage - 1) * LOG_PAGE_SIZE,
    currentCheckPage * LOG_PAGE_SIZE,
  );

  const notices = data?.notices || [];
  const noticePages = Math.max(1, Math.ceil(notices.length / LOG_PAGE_SIZE));
  const currentNoticePage = Math.min(noticePage, noticePages);
  const visibleNotices = notices.slice(
    (currentNoticePage - 1) * LOG_PAGE_SIZE,
    currentNoticePage * LOG_PAGE_SIZE,
  );

  return (
    <div className="space-y-5">
      {error && (
        <p
          role="alert"
          className="rounded-lg border border-destructive/25 bg-destructive/10 px-4 py-3 text-xs text-destructive"
        >
          {error}
        </p>
      )}
      {message && (
        <p
          role="status"
          className="rounded-lg border border-emerald-500/30 bg-emerald-500/10 px-4 py-3 text-xs font-medium text-foreground"
        >
          {message}
        </p>
      )}

      <Card>
        <CardHeader className="gap-3">
          <CardHeading className="flex items-center gap-2 space-y-0">
            <CardTitle>监控策略与通知渠道</CardTitle>
            {data && (
              <Badge
                variant={data.override ? "info" : "secondary"}
                appearance="light"
                size="sm"
              >
                {data.override ? "总控策略覆盖中" : "本地策略"}
              </Badge>
            )}
          </CardHeading>
          {data?.override && (
            <span className="text-xs text-muted-foreground">
              总控参数：每 {data.effective.interval}s 探测 · 连续失败{" "}
              {data.effective.failures} 次告警 · 连续成功 {data.effective.recoveries} 次恢复
            </span>
          )}
        </CardHeader>

        <CardContent>
          {data && channels ? (
            <form
              className="space-y-6"
              onSubmit={async (e) => {
                e.preventDefault();
                setBusy(true);
                setError("");
                try {
                  await api("/monitoring", "POST", {
                    local: policy,
                    channels,
                    clear_telegram: clearTelegram,
                    clear_password: clearPassword,
                  });
                  await refresh(true);
                  setClearTelegram(false);
                  setClearPassword(false);
                  setMessage("配置已保存");
                } catch (e) {
                  setError(errorText(e));
                } finally {
                  setBusy(false);
                }
              }}
            >
              {data.controller && (
                <p className="rounded-lg border border-primary/20 bg-primary/8 px-3.5 py-2.5 text-xs text-foreground">
                  当前已接入上级总控，外部消息推送由总控统一发送；本机仍持续独立探测并留存记录。
                </p>
              )}

              <PolicyFields value={policy} change={setPolicy} />

              <div className="grid gap-4 pt-2 lg:grid-cols-2">
                <section className="space-y-3.5 rounded-xl border border-border bg-muted/20 p-4">
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-2">
                      <Send className="size-4 text-primary" />
                      <h4 className="text-xs font-semibold text-foreground">
                        Telegram 机器人通知
                      </h4>
                    </div>
                    <Badge
                      variant={data.telegram_set ? "success" : "secondary"}
                      appearance="light"
                      size="xs"
                    >
                      {data.telegram_set ? "已配置" : "未配置"}
                    </Badge>
                  </div>
                  <Field label="Telegram Bot Token">
                    <Input
                      type="password"
                      autoComplete="new-password"
                      value={channels.telegram_token}
                      placeholder={
                        data.telegram_set ? "留空保留已保存 Token" : "输入 Bot Token"
                      }
                      onChange={(e) =>
                        setChannels({
                          ...channels,
                          telegram_token: e.target.value,
                        })
                      }
                    />
                  </Field>
                  <Field label="Telegram Chat ID">
                    <Input
                      type="text"
                      autoComplete="off"
                      value={channels.telegram_chat}
                      placeholder="例如 -1001234567890"
                      onChange={(e) =>
                        setChannels({
                          ...channels,
                          telegram_chat: e.target.value,
                        })
                      }
                    />
                  </Field>
                  <label className="flex items-center gap-2 pt-1 text-xs text-muted-foreground">
                    <input
                      type="checkbox"
                      checked={clearTelegram}
                      onChange={(e) => setClearTelegram(e.target.checked)}
                    />
                    <span>
                      清除已保存的 Telegram Token
                      {data.telegram_set ? "（当前已配置）" : ""}
                    </span>
                  </label>
                </section>

                <section className="space-y-3.5 rounded-xl border border-border bg-muted/20 p-4">
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-2">
                      <Mail className="size-4 text-violet-500" />
                      <h4 className="text-xs font-semibold text-foreground">
                        SMTP 邮件通知
                      </h4>
                    </div>
                    <Badge
                      variant={data.password_set ? "success" : "secondary"}
                      appearance="light"
                      size="xs"
                    >
                      {data.password_set ? "密码已保存" : "未设密码"}
                    </Badge>
                  </div>
                  <div className="grid gap-3 sm:grid-cols-3">
                    <div className="sm:col-span-2">
                      <Field label="SMTP 主机">
                        <Input
                          type="text"
                          autoComplete="off"
                          value={channels.smtp_host}
                          placeholder="smtp.example.com"
                          onChange={(e) =>
                            setChannels({
                              ...channels,
                              smtp_host: e.target.value,
                            })
                          }
                        />
                      </Field>
                    </div>
                    <Field label="SMTP 端口">
                      <Input
                        type="number"
                        min={1}
                        max={65535}
                        value={channels.smtp_port || ""}
                        onChange={(e) =>
                          setChannels({
                            ...channels,
                            smtp_port: Number(e.target.value),
                          })
                        }
                      />
                    </Field>
                  </div>
                  <div className="grid gap-3 sm:grid-cols-2">
                    <Field label="SMTP 用户名">
                      <Input
                        type="text"
                        autoComplete="off"
                        value={channels.smtp_user}
                        onChange={(e) =>
                          setChannels({
                            ...channels,
                            smtp_user: e.target.value,
                          })
                        }
                      />
                    </Field>
                    <Field label="SMTP 密码">
                      <Input
                        type="password"
                        autoComplete="new-password"
                        value={channels.smtp_password}
                        placeholder="留空保留已保存密码"
                        onChange={(e) =>
                          setChannels({
                            ...channels,
                            smtp_password: e.target.value,
                          })
                        }
                      />
                    </Field>
                    <Field label="发件邮箱">
                      <Input
                        type="text"
                        autoComplete="off"
                        value={channels.mail_from}
                        onChange={(e) =>
                          setChannels({
                            ...channels,
                            mail_from: e.target.value,
                          })
                        }
                      />
                    </Field>
                    <Field label="收件邮箱">
                      <Input
                        type="text"
                        autoComplete="off"
                        value={channels.mail_to}
                        onChange={(e) =>
                          setChannels({ ...channels, mail_to: e.target.value })
                        }
                      />
                    </Field>
                  </div>
                  <div className="flex flex-wrap gap-x-5 gap-y-2 pt-1 text-xs text-muted-foreground">
                    <label className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        checked={channels.smtp_starttls}
                        onChange={(e) =>
                          setChannels({
                            ...channels,
                            smtp_starttls: e.target.checked,
                          })
                        }
                      />
                      <span>使用 STARTTLS（如 587 端口；不勾选为隐式 TLS 465）</span>
                    </label>
                    <label className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        checked={clearPassword}
                        onChange={(e) => setClearPassword(e.target.checked)}
                      />
                      <span>清除已保存的 SMTP 密码</span>
                    </label>
                  </div>
                </section>
              </div>

              <div className="flex flex-wrap items-center gap-2.5 border-t border-border pt-4">
                <Button disabled={busy} type="submit">
                  保存配置
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy || !!data.controller}
                  onClick={() =>
                    void api("/monitoring/test", "POST", {})
                      .then(() => {
                        setMessage("测试通知已加入发送队列，可在下方通知记录查看状态");
                        return refresh();
                      })
                      .catch((e) => setError(errorText(e)))
                  }
                >
                  发送测试通知
                </Button>
              </div>
            </form>
          ) : (
            <p className="muted py-6 text-center">正在读取监控配置…</p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="gap-3">
          <CardHeading className="flex items-center gap-2 space-y-0">
            <CardTitle>最近自动检测</CardTitle>
            <Badge
              variant={data?.effective.enabled ? "success" : "secondary"}
              appearance="light"
              size="sm"
            >
              {data?.effective.enabled ? "自动复查运行中" : "自动检测未启用"}
            </Badge>
            <InfoTip>
              {data?.effective.enabled
                ? "异常项会按间隔自动复查，连续通过达到恢复次数后自动解除告警。"
                : "当前自动检测未启用，下方展示此前留存的历史探测结果。"}
            </InfoTip>
          </CardHeading>

          <CardToolbar className="flex-wrap">
            <div className="relative w-48 sm:w-56">
              <Search className="pointer-events-none absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                className="pl-8.5"
                aria-label="搜索检测记录"
                placeholder="搜索节点或代理名称"
                value={checkSearch}
                onChange={(e) => {
                  setCheckSearch(e.target.value);
                  setCheckPage(1);
                }}
              />
            </div>
            <div className="flex rounded-lg bg-muted p-0.5 text-xs">
              {(
                [
                  ["all", "全部"],
                  ["fail", "仅看异常"],
                  ["ok", "已通过"],
                ] as const
              ).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  onClick={() => {
                    setCheckFilter(id);
                    setCheckPage(1);
                  }}
                  className={`cursor-pointer rounded-md px-2.5 py-1 font-medium transition-colors ${
                    checkFilter === id
                      ? "bg-card text-foreground shadow-2xs"
                      : "text-muted-foreground hover:text-foreground"
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
          </CardToolbar>
        </CardHeader>

        {visibleChecks.length ? (
          <CardTable>
            <div className="overflow-x-auto fleet-responsive" role="region" aria-label="最近自动检测列表">
              <table className="data-table [&_td]:align-top">
                <thead>
                  <tr>
                    <th>检测对象</th>
                    <th>状态</th>
                    <th>检测结果摘要</th>
                    <th className="whitespace-nowrap">检测时间</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleChecks.map(({ key, check, cond, subject, summary, rawLogs }) => {
                    const enabled = !!data?.effective.enabled;
                    const statusLabel = !enabled
                      ? check.ok
                        ? "历史通过"
                        : "历史未通过"
                      : check.ok
                        ? cond?.active
                          ? "恢复确认中"
                          : "通过"
                        : cond?.active
                          ? "故障复查中"
                          : "待连续确认";
                    const badgeVariant = !enabled
                      ? check.ok
                        ? "secondary"
                        : "warning"
                      : check.ok
                        ? cond?.active
                          ? "primary"
                          : "success"
                        : cond?.active
                          ? "destructive"
                          : "warning";
                    const isOpen = expandedLogKey === key;

                    return (
                      <tr key={key}>
                        <td data-label="检测对象" className="min-w-[180px] max-w-[260px]">
                          <p className="break-words font-semibold text-foreground">
                            {subject}
                          </p>
                        </td>
                        <td data-label="状态" className="whitespace-nowrap">
                          <Badge
                            variant={badgeVariant}
                            appearance="light"
                            size="sm"
                          >
                            {check.ok ? (
                              <CheckCircle2 className="size-3" />
                            ) : (
                              <XCircle className="size-3" />
                            )}
                            {statusLabel}
                          </Badge>
                        </td>
                        <td data-label="检测结果摘要" className="max-w-xl">
                          <p className="break-words text-xs leading-relaxed text-muted-foreground">
                            {summary}
                          </p>
                          {rawLogs && (
                            <div className="mt-2">
                              <Button
                                type="button"
                                size="sm"
                                variant="ghost"
                                className="h-6 px-2 text-[11px]"
                                onClick={() =>
                                  setExpandedLogKey(isOpen ? null : key)
                                }
                              >
                                <Terminal className="size-3" />
                                {isOpen ? "收起内核调试日志" : "查看内核调试日志"}
                              </Button>
                              {isOpen && (
                                <pre className="mono mt-2 max-h-52 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-zinc-950 p-3 text-[11px] leading-relaxed text-zinc-200">
                                  {rawLogs}
                                </pre>
                              )}
                            </div>
                          )}
                        </td>
                        <td
                          data-label="检测时间"
                          className="whitespace-nowrap text-xs text-muted-foreground tabular-nums"
                        >
                          {stamp(check.checked_at)}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </CardTable>
        ) : (
          <Empty icon={Activity} title="暂无匹配的检测记录">
            开启定时检测后，各代理入口的探测结果将在此汇总展示。
          </Empty>
        )}

        {checkEntries.length > LOG_PAGE_SIZE && (
          <CardFooter className="flex flex-wrap items-center justify-between gap-2 py-3 text-xs text-muted-foreground">
            <span className="tabular-nums">
              共 {checkEntries.length} 条 · 第 {currentCheckPage} / {checkPages} 页
            </span>
            <div className="flex gap-1.5">
              <Button
                size="sm"
                variant="outline"
                disabled={currentCheckPage <= 1}
                onClick={() => setCheckPage((p) => p - 1)}
              >
                上一页
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={currentCheckPage >= checkPages}
                onClick={() => setCheckPage((p) => p + 1)}
              >
                下一页
              </Button>
            </div>
          </CardFooter>
        )}
      </Card>

      <Card>
        <CardHeader>
          <CardHeading className="flex items-center gap-2 space-y-0">
            <CardTitle>最近通知记录</CardTitle>
            <Badge variant="secondary" appearance="light" size="sm">
              {notices.length}
            </Badge>
          </CardHeading>
        </CardHeader>

        {visibleNotices.length ? (
          <CardTable>
            <div className="overflow-x-auto fleet-responsive" role="region" aria-label="最近通知列表">
              <table className="data-table [&_td]:align-top">
                <thead>
                  <tr>
                    <th>类型</th>
                    <th>事件内容</th>
                    <th>推送渠道</th>
                    <th className="whitespace-nowrap">时间</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleNotices.map((n) => {
                    const parsed = parseNoticeItem(n.message);
                    const open = expandedNoticeId === n.id;
                    return (
                      <tr key={n.id}>
                        <td data-label="类型" className="whitespace-nowrap">
                          <Badge
                            variant={
                              parsed.kind === "fault"
                                ? "destructive"
                                : parsed.kind === "recover"
                                  ? "success"
                                  : "primary"
                            }
                            appearance="light"
                            size="sm"
                          >
                            {parsed.kind === "fault"
                              ? "故障"
                              : parsed.kind === "recover"
                                ? "恢复"
                                : "通知"}
                          </Badge>
                        </td>
                        <td data-label="事件内容" className="max-w-xl">
                          <p className="break-words text-xs font-medium text-foreground">
                            {parsed.headline}
                          </p>
                          {parsed.more && (
                            <div className="mt-1">
                              <button
                                type="button"
                                onClick={() =>
                                  setExpandedNoticeId(open ? null : n.id)
                                }
                                className="cursor-pointer text-[11px] text-primary hover:underline"
                              >
                                {open ? "收起详情" : "展开探测详情"}
                              </button>
                              {open && (
                                <p className="mt-1 break-words rounded-md bg-muted/40 p-2 text-[11px] leading-relaxed text-muted-foreground">
                                  {parsed.more}
                                </p>
                              )}
                            </div>
                          )}
                          {n.error && (
                            <p className="mt-1 text-xs text-destructive">
                              发送错误：{n.error}
                            </p>
                          )}
                        </td>
                        <td data-label="推送渠道" className="whitespace-nowrap">
                          <div className="flex flex-wrap gap-1">
                            <Badge
                              variant={n.telegram_done ? "success" : "secondary"}
                              appearance="light"
                              size="xs"
                            >
                              TG {n.telegram_done ? "已发" : "未发"}
                            </Badge>
                            <Badge
                              variant={n.mail_done ? "success" : "secondary"}
                              appearance="light"
                              size="xs"
                            >
                              邮件 {n.mail_done ? "已发" : "未发"}
                            </Badge>
                          </div>
                        </td>
                        <td
                          data-label="时间"
                          className="whitespace-nowrap text-xs text-muted-foreground tabular-nums"
                        >
                          {stamp(n.at)}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </CardTable>
        ) : (
          <Empty icon={Bell} title="暂无告警通知记录">
            触发状态变更或发送测试通知后，推送回执将显示于此。
          </Empty>
        )}

        {notices.length > LOG_PAGE_SIZE && (
          <CardFooter className="flex flex-wrap items-center justify-between gap-2 py-3 text-xs text-muted-foreground">
            <span className="tabular-nums">
              共 {notices.length} 条 · 第 {currentNoticePage} / {noticePages} 页
            </span>
            <div className="flex gap-1.5">
              <Button
                size="sm"
                variant="outline"
                disabled={currentNoticePage <= 1}
                onClick={() => setNoticePage((p) => p - 1)}
              >
                上一页
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={currentNoticePage >= noticePages}
                onClick={() => setNoticePage((p) => p + 1)}
              >
                下一页
              </Button>
            </div>
          </CardFooter>
        )}
      </Card>
    </div>
  );
}

export { Fleet } from "./fleet";
