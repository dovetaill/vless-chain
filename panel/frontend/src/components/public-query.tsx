import { useCallback, useEffect, useRef, useState } from "react";
import { Moon, Search, Sun, Waypoints } from "lucide-react";
import { api } from "../api";
import type { PublicLookup } from "../types";
import { beijingTime, money, terms } from "../lib/business";
import { usePreferences } from "../lib/preferences";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Progress } from "./ui/progress";
import { CopyButton, Field, InfoTip, bytes, errorText } from "./shared";
import {
  RentalDates,
  RentalPrices,
  RentalSpeed,
  RentalStatusBadge,
  RentalUsage,
  SyncLabel,
} from "./rental-shared";

export function PublicQuery() {
  const { resolvedTheme, toggleTheme } = usePreferences();
  const [config, setConfig] = useState("");
  const [token, setToken] = useState(
    () => new URLSearchParams(location.hash.slice(1)).get("token") || "",
  );
  const [result, setResult] = useState<PublicLookup | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const sequence = useRef(0);
  const activeInput = useRef<{ config?: string; token?: string } | null>(null);
  const pending = useRef<AbortController | null>(null);

  const lookup = useCallback(
    async (input: { config?: string; token?: string }, background = false) => {
      const current = ++sequence.current;
      pending.current?.abort();
      const controller = new AbortController();
      pending.current = controller;
      if (!background) {
        activeInput.current = null;
        setLoading(true);
        setError("");
        setResult(null);
      }
      try {
        const next = await api<PublicLookup>(
          "/public/lookup",
          "POST",
          input,
          controller.signal,
        );
        if (current === sequence.current) {
          activeInput.current = input;
          setResult(next);
          setError("");
        }
      } catch (error) {
        if (current === sequence.current && !controller.signal.aborted)
          setError(
            `${background ? "刷新失败，保留上次查询结果：" : ""}${errorText(error)}`,
          );
      } finally {
        if (current === sequence.current) {
          setLoading(false);
          pending.current = null;
        }
      }
    },
    [],
  );

  function clearQuery() {
    sequence.current++;
    pending.current?.abort();
    pending.current = null;
    activeInput.current = null;
    setLoading(false);
    setResult(null);
    setError("");
  }

  useEffect(() => {
    if (token) void lookup({ token });
  }, [token, lookup]);

  useEffect(() => {
    const update = () => {
      const token = new URLSearchParams(location.hash.slice(1)).get("token") || "";
      if (!token) clearQuery();
      setToken(token);
    };
    window.addEventListener("hashchange", update);
    return () => window.removeEventListener("hashchange", update);
  }, []);

  useEffect(() => {
    const refresh = () => {
      if (
        document.visibilityState === "visible" &&
        activeInput.current &&
        !pending.current
      )
        void lookup(activeInput.current, true);
    };
    const visibility = () => {
      if (document.visibilityState === "hidden") {
        sequence.current++;
        pending.current?.abort();
        pending.current = null;
        setLoading(false);
      } else refresh();
    };
    const timer = setInterval(refresh, 10000);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", visibility);
      sequence.current++;
      pending.current?.abort();
      pending.current = null;
    };
  }, [lookup]);

  return (
    <div className="min-h-dvh bg-background text-foreground">
      <header className="border-b border-border bg-card/85 backdrop-blur-md">
        <div className="mx-auto flex max-w-5xl flex-wrap items-center gap-3 px-4 py-4 sm:px-6">
          <span className="brand-symbol">
            <Waypoints />
          </span>
          <div className="min-w-0 flex-1">
            <h1 className="text-base font-semibold tracking-tight">代理服务查询</h1>
            <p className="text-xs text-muted-foreground">实时流量、到期时间与续费指引</p>
          </div>
          <Button
            type="button"
            variant="ghost"
            mode="icon"
            shape="circle"
            aria-label="切换深色模式"
            onClick={toggleTheme}
          >
            {resolvedTheme === "dark" ? (
              <Sun className="size-4" />
            ) : (
              <Moon className="size-4" />
            )}
          </Button>
          <Button variant="outline" size="sm" asChild>
            <a href="/admin">管理登录</a>
          </Button>
        </div>
      </header>

      <main className="mx-auto max-w-5xl space-y-5 px-4 py-7 sm:px-6">
        <Card>
          <CardHeader>
            <CardTitle>{token ? "通过私密链接查询" : "查询我的代理"}</CardTitle>
          </CardHeader>
          <CardContent>
            {token ? (
              <div className="flex flex-wrap items-center gap-3">
                <p className="mr-auto text-xs text-muted-foreground">
                  当前通过专属查询链接查看对应线路状态。
                </p>
                <Button disabled={loading} onClick={() => void lookup({ token })}>
                  {loading ? "查询中…" : "刷新查询"}
                </Button>
                <Button
                  variant="outline"
                  disabled={loading}
                  onClick={() => {
                    history.replaceState(null, "", location.pathname);
                    setToken("");
                    clearQuery();
                  }}
                >
                  使用代理配置查询
                </Button>
              </div>
            ) : (
              <form
                className="space-y-4"
                onSubmit={(event) => {
                  event.preventDefault();
                  if (!loading) void lookup({ config });
                }}
              >
                <Field
                  label="代理链接或 Mihomo 配置"
                  tooltip="支持本平台签发的单条分享链接、节点片段或整份 Mihomo YAML 配置（单次最多匹配 100 个节点）。"
                >
                  <textarea
                    className="textarea mono min-h-36"
                    value={config}
                    onChange={(event) => setConfig(event.target.value)}
                    required
                    rows={6}
                    maxLength={262144}
                    placeholder={"vless://…\n或粘贴包含 proxies 的 Mihomo YAML 配置"}
                    spellCheck={false}
                    autoComplete="off"
                  />
                </Field>
                <div className="flex flex-wrap items-center gap-3">
                  <Button type="submit" disabled={loading || !config.trim()}>
                    <Search />
                    {loading ? "查询中…" : "查询服务"}
                  </Button>
                  <span className="text-xs text-muted-foreground">
                    免登录验证凭据并读取对应线路用量
                  </span>
                </div>
              </form>
            )}
            {error && (
              <p
                role="alert"
                className="mt-4 rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-xs text-destructive"
              >
                {error}
              </p>
            )}
          </CardContent>
        </Card>

        {loading && (
          <p role="status" className="text-xs text-muted-foreground">
            正在读取服务信息…
          </p>
        )}

        {result && (
          <>
            <div
              className="flex flex-wrap justify-between gap-2 text-xs text-muted-foreground"
              role="status"
            >
              <span>已匹配 {result.lines.length} 条代理 · 每 10 秒自动刷新</span>
              <span className="tabular-nums">查询时间 {beijingTime(result.time)}</span>
            </div>

            {result.unmatched.length > 0 && (
              <p className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3.5 text-xs text-foreground">
                以下配置未匹配本平台出租服务：{result.unmatched.join("、")}。已匹配的服务显示如下。
              </p>
            )}

            {(result.customer_plans || []).map((plan) => {
              const totalBytes = plan.monthly_gib * 1024 ** 3;
              const pct = totalBytes > 0 ? Math.min(100, (plan.usage.used / totalBytes) * 100) : 0;
              return (
                <Card key={plan.id}>
                  <CardHeader>
                    <div className="flex items-center gap-2">
                      <CardTitle>共享套餐 · {plan.name}</CardTitle>
                      <InfoTip>
                        所有成员共用套餐周期总额度；授权量含活跃连接预留块，离线节点用量可能短暂延后汇总。
                      </InfoTip>
                    </div>
                    <Badge variant="primary" appearance="light" size="sm">
                      已用 {pct.toFixed(1)}%
                    </Badge>
                  </CardHeader>
                  <CardContent className="space-y-4 text-xs">
                    <div className="space-y-2">
                      <div className="flex flex-wrap justify-between gap-2 tabular-nums">
                        <span className="font-medium text-foreground">
                          已用 {bytes(plan.usage.used)} / 总额 {bytes(totalBytes)}
                        </span>
                        <span className="text-muted-foreground">
                          已预留授权 {bytes(plan.usage.reserved)}
                        </span>
                      </div>
                      <Progress value={pct} />
                    </div>
                    <div className="grid gap-2.5 sm:grid-cols-2">
                      {plan.members.map((member) => (
                        <div
                          key={member.id}
                          className="rounded-lg border border-border bg-muted/30 p-3"
                        >
                          <p className="font-medium text-foreground">
                            {member.name} · 已用 {bytes(member.usage.used)}
                          </p>
                          <p className="mt-1 text-muted-foreground">
                            {member.monthly_gib == null
                              ? "共享套餐总额"
                              : `个人上限 ${bytes(member.monthly_gib * 1024 ** 3)}`}
                          </p>
                        </div>
                      ))}
                    </div>
                  </CardContent>
                </Card>
              );
            })}

            {result.lines.map((line) => {
              const renewal = [
                `代理：${line.name}`,
                line.customer_plan_id
                  ? `共享套餐编号：${line.customer_plan_id}（本线路共用）`
                  : `服务编号：${line.id}`,
                `到期：${beijingTime(line.rental.expires_at)}（北京时间）`,
                ...terms
                  .filter((term) => line.rental.prices[term.id] != null)
                  .map(
                    (term) => `${term.name}报价：${money(line.rental.prices[term.id])}`,
                  ),
                "希望续费周期：",
                "请运营方确认收款后开通或续期。",
              ].join("\n");

              return (
                <Card key={line.id}>
                  <CardHeader className="flex flex-wrap items-center justify-between gap-3">
                    <div className="min-w-0 max-w-full">
                      <CardTitle>{line.name}</CardTitle>
                      <p className="mono mt-1 break-all text-[11px] text-muted-foreground">
                        ID: {line.id}
                      </p>
                    </div>
                    <div className="flex items-center gap-2">
                      <RentalStatusBadge status={line.status} />
                      <SyncLabel status={line.sync_status} />
                    </div>
                  </CardHeader>
                  <CardContent>
                    <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-4">
                      <section className="rounded-lg border border-border/70 bg-muted/20 p-3.5">
                        <h3 className="mb-2.5 text-xs font-medium text-muted-foreground">
                          周期流量
                        </h3>
                        <RentalUsage line={line} />
                      </section>
                      <section className="rounded-lg border border-border/70 bg-muted/20 p-3.5">
                        <h3 className="mb-2.5 text-xs font-medium text-muted-foreground">
                          实时速率
                        </h3>
                        <RentalSpeed line={line} now={result.time} />
                      </section>
                      <section className="rounded-lg border border-border/70 bg-muted/20 p-3.5">
                        <h3 className="mb-2.5 text-xs font-medium text-muted-foreground">
                          到期与重置
                        </h3>
                        <RentalDates line={line} />
                      </section>
                      <section className="rounded-lg border border-border/70 bg-muted/20 p-3.5">
                        <h3 className="mb-2.5 text-xs font-medium text-muted-foreground">
                          {line.customer_plan_id ? "套餐续费报价" : "续费报价"}
                        </h3>
                        <RentalPrices line={line} />
                      </section>
                    </div>
                    {!line.rental.permanent && (
                      <div className="mt-5 flex flex-wrap items-center justify-between gap-3 border-t border-border pt-4">
                        <span className="text-xs text-muted-foreground">
                          复制续费单信息发送给服务商即可办理续期
                        </span>
                        <CopyButton size="sm" value={renewal} label="复制续费信息" />
                      </div>
                    )}
                  </CardContent>
                </Card>
              );
            })}

            <Card>
              <CardHeader>
                <CardTitle>联系服务商</CardTitle>
              </CardHeader>
              <CardContent>
                {result.renewal_contact ? (
                  <div className="flex flex-wrap items-center justify-between gap-4">
                    <p className="whitespace-pre-wrap break-words text-sm leading-relaxed">
                      {result.renewal_contact}
                    </p>
                    <div className="flex flex-wrap gap-2">
                      <CopyButton
                        size="sm"
                        value={result.renewal_contact}
                        label="复制联系方式"
                      />
                      {/^https?:\/\/\S+$/.test(result.renewal_contact) && (
                        <Button size="sm" variant="outline" asChild>
                          <a
                            href={result.renewal_contact}
                            target="_blank"
                            rel="noopener noreferrer"
                          >
                            打开联系链接
                          </a>
                        </Button>
                      )}
                    </div>
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground">
                    尚未配置统一续费入口，请通过开通时的渠道联系服务商。
                  </p>
                )}
              </CardContent>
            </Card>
          </>
        )}
      </main>
    </div>
  );
}
