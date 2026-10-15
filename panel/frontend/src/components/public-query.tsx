import { useCallback, useEffect, useRef, useState } from "react";
import { Network, Search } from "lucide-react";
import { api } from "../api";
import type { PublicLookup } from "../types";
import { beijingTime, money, terms } from "../lib/business";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { CopyButton, Field, errorText } from "./shared";
import { RentalDates, RentalPrices, RentalSpeed, RentalStatusBadge, RentalUsage, SyncLabel } from "./rental-shared";

export function PublicQuery() {
  const [config, setConfig] = useState("");
  const [token, setToken] = useState(() => new URLSearchParams(location.hash.slice(1)).get("token") || "");
  const [result, setResult] = useState<PublicLookup | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const sequence = useRef(0);
  const activeInput = useRef<{ config?: string; token?: string } | null>(null);
  const pending = useRef<AbortController | null>(null);
  const lookup = useCallback(async (input: { config?: string; token?: string }, background = false) => {
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
      const next = await api<PublicLookup>("/public/lookup", "POST", input, controller.signal);
      if (current === sequence.current) {
        activeInput.current = input;
        setResult(next);
        setError("");
      }
    }
    catch (error) { if (current === sequence.current && !controller.signal.aborted) setError(`${background ? "刷新失败，以下保留上次查询结果：" : ""}${errorText(error)}`); }
    finally { if (current === sequence.current) { setLoading(false); pending.current = null; } }
  }, []);
  function clearQuery() {
    sequence.current++;
    pending.current?.abort();
    pending.current = null;
    activeInput.current = null;
    setLoading(false);
    setResult(null);
    setError("");
  }
  useEffect(() => { if (token) void lookup({ token }); }, [token, lookup]);
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
      if (document.visibilityState === "visible" && activeInput.current && !pending.current) void lookup(activeInput.current, true);
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
  return <div className="min-h-dvh bg-slate-50">
    <header className="border-b border-border bg-white"><div className="mx-auto flex max-w-5xl items-center gap-3 px-4 py-5 sm:px-6"><span className="flex size-9 items-center justify-center rounded-xl bg-blue-600 text-white"><Network className="size-5" /></span><div><h1 className="text-lg font-semibold">代理服务查询</h1><p className="mt-0.5 text-xs text-slate-400">用量、到期与续费信息</p></div></div></header>
    <main className="mx-auto max-w-5xl space-y-5 px-4 py-7 sm:px-6">
      <Card><CardHeader><CardTitle>{token ? "通过私密链接查询" : "查询我的代理"}</CardTitle></CardHeader><CardContent>
        {token ? <div className="flex flex-wrap items-center gap-3"><p className="mr-auto text-sm text-slate-500">此链接只查询对应的代理。</p><Button disabled={loading} onClick={() => void lookup({ token })}>{loading ? "正在查询…" : "刷新查询"}</Button><Button variant="outline" disabled={loading} onClick={() => { history.replaceState(null, "", location.pathname); setToken(""); clearQuery(); }}>使用代理配置查询</Button></div> : <form className="space-y-4" onSubmit={event => { event.preventDefault(); if (!loading) void lookup({ config }); }}>
          <Field label="代理链接或 Mihomo 配置" hint="支持本平台签发的代理链接、单节点及整份 Mihomo 配置；整份配置最多 100 个节点，只检查其中的 proxies。"><textarea className="textarea mono min-h-36" value={config} onChange={event => setConfig(event.target.value)} required rows={6} maxLength={262144} placeholder={'vless://…\n或粘贴包含 proxies 的 Mihomo YAML 配置'} spellCheck={false} autoComplete="off" /></Field>
          <div className="flex flex-wrap items-center gap-3"><Button type="submit" disabled={loading || !config.trim()}><Search />{loading ? "正在查询…" : "查询服务"}</Button><p className="text-xs text-slate-500">无需登录，使用配置中的凭据验证对应代理。</p></div>
        </form>}
        {error && <p role="alert" className="mt-4 rounded-lg bg-red-50 p-3 text-sm text-red-700">{error}</p>}
      </CardContent></Card>
      {loading && <p role="status" className="text-sm text-slate-500">正在读取服务信息…</p>}
      {result && <>
        <div className="flex flex-wrap justify-between gap-2 text-xs text-slate-500" role="status"><span>已匹配 {result.lines.length} 条代理 · 页面可见时每 10 秒刷新</span><span>查询时间 {beijingTime(result.time)} · 北京时间</span></div>
        {result.unmatched.length > 0 && <p className="rounded-lg bg-amber-50 p-4 text-sm text-amber-800">以下配置未匹配本平台出租服务：{result.unmatched.join("、")}。已匹配的服务显示如下。</p>}
        {result.lines.map(line => {
          const renewal = [`代理：${line.name}`, `服务编号：${line.id}`, `到期：${beijingTime(line.rental.expires_at)}（北京时间）`, ...terms.filter(term => line.rental.prices[term.id] != null).map(term => `${term.name}报价：${money(line.rental.prices[term.id])}`), "希望续费周期：", "请运营方确认收款后开通或续期。"].join("\n");
          return <Card key={line.id}><CardHeader className="flex flex-wrap items-center justify-between gap-3"><div><CardTitle>{line.name}</CardTitle><p className="mt-2 text-xs text-slate-400">服务编号 {line.id}</p></div><div className="flex items-center gap-3"><RentalStatusBadge status={line.status} /><SyncLabel status={line.sync_status} /></div></CardHeader><CardContent>
            <div className="grid gap-6 sm:grid-cols-2 lg:grid-cols-4"><section><h3 className="mb-3 text-xs font-semibold text-slate-400">当前周期用量</h3><RentalUsage line={line} /></section><section><h3 className="mb-3 text-xs font-semibold text-slate-400">查询时网速</h3><RentalSpeed line={line} now={result.time} /></section><section><h3 className="mb-3 text-xs font-semibold text-slate-400">到期 / 额度重置（北京时间）</h3><RentalDates line={line} /></section><section><h3 className="mb-3 text-xs font-semibold text-slate-400">续费报价 · 人民币</h3><RentalPrices line={line} /></section></div>
            {!line.rental.permanent && <div className="mt-6 border-t border-border pt-4"><CopyButton value={renewal} label="复制续费信息" /><p className="mt-2 text-xs text-slate-500">将续费信息发给运营方，确认付款后由运营方续期。</p></div>}
          </CardContent></Card>;
        })}
        <Card><CardHeader><CardTitle>联系运营方</CardTitle></CardHeader><CardContent>{result.renewal_contact ? <><p className="whitespace-pre-wrap break-words text-sm leading-7">{result.renewal_contact}</p><div className="mt-4 flex flex-wrap gap-2"><CopyButton value={result.renewal_contact} label="复制联系方式" />{/^https?:\/\/\S+$/.test(result.renewal_contact) && <a className="inline-flex items-center rounded-lg border border-border px-4 py-2 text-sm text-blue-700" href={result.renewal_contact} target="_blank" rel="noopener noreferrer">打开联系入口</a>}</div></> : <p className="text-sm text-slate-500">运营方尚未设置统一联系入口，请使用购买时的联系方式。</p>}</CardContent></Card>
      </>}
    </main>
  </div>;
}
