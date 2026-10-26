import { useEffect, useState } from "react";
import { api } from "../api";
import { errorText } from "./shared";

export type CoreChoice = { core: "xray" | "mihomo"; version: string; prerelease: boolean; security_update: boolean; rollback_safe: boolean };
export const initialCoreChoice: CoreChoice = { core: "xray", version: "", prerelease: false, security_update: true, rollback_safe: false };

export function CoreSelector({ value, onChange, disabled }: { value: CoreChoice; onChange: (value: CoreChoice) => void; disabled?: boolean }) {
  const [releases, setReleases] = useState<{ recommended: string; versions: { version: string; prerelease: boolean }[] }>();
  const [error, setError] = useState("");
  useEffect(() => {
    let stopped = false;
    setReleases(undefined); setError("");
    api<{ recommended: string; versions: { version: string; prerelease: boolean }[] }>(`/fleet/cores/${value.core}`)
      .then(result => { if (!stopped) setReleases(result); })
      .catch(e => { if (!stopped) setError(errorText(e)); });
    return () => { stopped = true; };
  }, [value.core]);
  return <fieldset disabled={disabled} className="space-y-3 rounded-lg border p-3 text-sm">
    <legend className="px-1 font-medium">代理内核版本</legend>
    <label className="block space-y-1"><span>内核</span><select className="w-full rounded-md border bg-background p-2" value={value.core}
      onChange={e => onChange({ ...value, core: e.target.value as CoreChoice["core"], version: "", prerelease: false })}>
      <option value="xray">Xray</option><option value="mihomo">Mihomo</option></select></label>
    <label className="block space-y-1"><span>官方版本</span><select aria-label="官方版本" className="w-full rounded-md border bg-background p-2" value={value.version}
      onChange={e => onChange({ ...value, version: e.target.value, prerelease: releases?.versions.find(r => r.version === e.target.value)?.prerelease || false })}>
      <option value="">请选择准确版本</option>
      {releases?.versions.filter(r => value.prerelease || !r.prerelease).map(r => <option key={r.version} value={r.version}>{r.version}{r.version === releases.recommended ? "（项目目标版本，仍需试点验证）" : ""}{r.prerelease ? " · 预发布" : ""}</option>)}
    </select></label>
    <label className="block space-y-1"><span>或输入官方固定版本号</span><input className="w-full rounded-md border bg-background p-2" placeholder="例如 26.10.21" value={value.version}
      onChange={e => onChange({ ...value, version: e.target.value.replace(/^v/, "") })} /></label>
    <label className="flex items-start gap-2"><input type="checkbox" checked={value.prerelease} onChange={e => onChange({ ...value, prerelease: e.target.checked })} />允许本次指定的预发布版本</label>
    <label className="flex items-start gap-2"><input type="checkbox" checked={value.security_update} onChange={e => onChange({ ...value, security_update: e.target.checked, rollback_safe: false })} />包含安全修复，禁止回退未确认安全的版本</label>
    {value.security_update && <label className="flex items-start gap-2"><input type="checkbox" checked={value.rollback_safe} onChange={e => onChange({ ...value, rollback_safe: e.target.checked })} />已根据官方公告核实原版本不受此次漏洞影响，可用于回退</label>}
    <p className="muted">版本在提交时锁定。UUID、密钥和原 VLESS 链接保留；重启会中断本机代理连接。{value.security_update && !value.rollback_safe ? "安全更新失败且无安全回退版本时，停止本机代理，保留管理面板。" : "失败时恢复原内核。"}</p>
    {error && <p role="alert" className="text-amber-700">版本列表获取失败：{error}。可输入准确的官方版本号，下载时仍会核对官方附件。</p>}
  </fieldset>;
}
