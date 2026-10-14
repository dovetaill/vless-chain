import { useState } from "react";
import { Plus, X, ArrowRight } from "lucide-react";
import { Input } from "./ui/input";
import { Button } from "./ui/button";
import {
  Field,
  Modal,
  endpoint,
  field,
  number,
  splitAddress,
  protocolNames,
} from "./shared";
import { api } from "../api";
import type { Line, PanelState, Protocol, NodeInfo, External } from "../types";

export function LineEditor({
  line,
  kind,
  state,
  initialChain,
  close,
  saved,
  ensureLocalServer,
  importExternal,
}: {
  line?: Line;
  kind: Line["kind"];
  state: PanelState;
  initialChain?: string[];
  close: () => void;
  ensureLocalServer: (host: string) => Promise<NodeInfo>;
  importExternal?: (done: (external?: External) => void) => void;
  saved: (line: Line, draft: boolean) => Promise<void>;
}) {
  const local = state.nodes.find(n => n.id === state.local_node_id);
  const existing = state.nodes.find(n => n.id === line?.hops[0]?.node_id);
  const [host, setHost] = useState(line?.access.host || line?.access.tls.domain || existing?.host || local?.host || location.hostname.replace(/^\[|\]$/g, ""));
  const [chain, setChain] = useState<string[]>(line?.chain?.length ? line.chain : line?.external_id ? [`external:${line.external_id}`] : initialChain || []);
  const domain = Boolean(host.trim()) && !host.includes(":") && !/^[\d.]+$/.test(host.trim());
  const choices = [
    ...state.externals.map(e => ({ id: `external:${e.id}`, name: e.name })),
    ...state.lines.filter(l => l.kind === "vless" && l.id !== line?.id && l.enabled && l.published).map(l => ({ id: `line:${l.id}`, name: `${l.name}（本机代理）` })),
  ];
  const available = choices.find(c => !chain.includes(c.id));
  const [protocol, setProtocol] = useState<Protocol>(
    line?.access.protocol || "vless",
  );
  const [certificate, setCertificate] = useState(line?.access.tls.mode || "auto");
  const supportsTls = ["vless", "trojan", "http"].includes(protocol);
  const tls = supportsTls && domain ? certificate : "off";
  const showTls = supportsTls && domain;
  return (
    <Modal
      wide
      title={line ? "编辑代理" : "新建代理"}
      description="在本机运行，创建后自动生成客户端链接。"
      submit={line ? "保存并应用" : "创建代理"}
      close={close}
      action={async (data) => {
        if (protocol === "trojan" && !domain) throw new Error("Trojan 需要域名，请填写解析到本机的域名。");
        if (chain.some(id => !id)) throw new Error("请选择链式代理的落地。");
        const node = line ? existing : await ensureLocalServer(host.trim());
        if (!node) throw new Error("本机记录不存在，请先修复本机设置。");
        const changedProtocol = line && line.access.protocol !== protocol;
        const value = await api<Line>("/lines", "POST", {
          id: line?.id,
          kind,
          name: field(data, "name").trim() || `${protocolNames[protocol]} ${host.trim()}`.slice(0, 80),
          enabled: data.has("enabled"),
          hops: line ? line.hops.map((h, i) => ({ node_id: h.node_id, port: i === 0 ? number(data, "port") : h.port })) : [{ node_id: node.id, port: number(data, "port") }],
          external_id: null, chain, target: "", network: "tcp",
          access: {
            host: host.trim(),
            protocol: kind === "vless" ? protocol : "vless",
            username: field(data, "username") || line?.access.username || "",
            password:
              field(data, "password") ||
              (changedProtocol ? "" : line?.access.password || ""),
            method: "",
            public_host: "",
            public_port: null,
            tls: {
              mode: supportsTls ? tls : "off",
              domain: showTls && tls !== "off" ? host.trim() : "",
              cert: tls === "manual" ? field(data, "cert") : "",
              key: tls === "manual" ? field(data, "key") : "",
            },
          },
          limits: {
            upload_mbps: number(data, "upload"),
            download_mbps: number(data, "download"),
            monthly_gib: number(data, "quota"),
          },
        });
        await saved(value, false);
      }}
    >
      <Field label="名称">
        <Input
          name="name"
          defaultValue={line?.name}
          placeholder="自动命名"
          maxLength={80}
        />
      </Field>
      {kind === "vless" && (
        <Field label="协议">
          <select
            className="select"
            value={protocol}
            onChange={(e) => {
              setProtocol(e.target.value as Protocol);
              setCertificate("auto");
            }}
          >
            {Object.entries(protocolNames).map(([id, label]) => (
              <option key={id} value={id}>
                {id === "vless" ? "VLESS" : label}
              </option>
            ))}
          </select>
        </Field>
      )}
      <Field label="本机 IP 或域名" hint={showTls ? "自动申请并续期 SSL 证书；域名需解析到本机，TCP 80 需开放。" : protocol === "vless" ? "使用 REALITY，无需域名和证书。" : protocol === "trojan" ? "填写域名后自动申请 SSL 证书。" : "客户端通过此地址连接本机。"}>
        <Input value={host} onChange={e => { setHost(e.target.value); setCertificate("auto"); }} required placeholder="本机公网 IP 或域名" />
      </Field>
      <details className="details">
        <summary>监听端口 · 自动分配</summary>
        <Field label="监听端口" hint="留空自动选择空闲端口"><Input name="port" type="number" min={1} max={65535} defaultValue={line?.hops[0]?.port} placeholder="自动分配" /></Field>
      </details>
      {kind === "vless" && ["http", "socks", "trojan"].includes(protocol) && <details className="details">
        <summary>账号与密码 · 高级</summary>
        <div className="grid gap-4 sm:grid-cols-2">
          {protocol !== "trojan" && (
            <Field label="用户名" hint="留空生成默认用户名">
              <Input
                name="username"
                defaultValue={line?.access.username}
                autoComplete="off"
                placeholder="chain"
              />
            </Field>
          )}
          <Field label="连接密码" hint="留空自动生成；编辑时留空保留原密码">
            <Input
              name="password"
              type="password"
              autoComplete="new-password"
            />
          </Field>
        </div>
      {["http", "socks"].includes(protocol) && (
        <p className="muted">
          HTTP / SOCKS5
          使用用户名和密码。普通连接本身不加密，适合可信网络或已有加密隧道。
        </p>
      )}
      </details>}
      {showTls && (
        <details className="details"><summary>证书 · 自动申请与续期</summary>
          <Field label="HTTPS / 证书">
            <select
              className="select"
              value={tls}
              onChange={(e) => {
                setCertificate(e.target.value);
              }}
            >
              <option value="auto">自动申请与续期证书</option>
              <option value="manual">使用自己的证书</option>
              {protocol === "vless" && <option value="off">REALITY（无需证书）</option>}
            </select>
          </Field>
          {tls === "auto" && (
            <p className="muted">
              域名需解析到本机，并放行 TCP 80 和监听端口。
            </p>
          )}
          {tls === "manual" && (
            <>
              <Field label="证书 PEM">
                <textarea
                  className="textarea mono"
                  name="cert"
                  rows={4}
                  defaultValue={line?.access.tls.cert}
                  required
                />
              </Field>
              <Field label="私钥 PEM">
                <textarea
                  className="textarea mono"
                  name="key"
                  rows={4}
                  defaultValue={line?.access.tls.key}
                  autoComplete="off"
                  required
                />
              </Field>
            </>
          )}
        </details>
      )}
      <section className="space-y-3 rounded-xl border border-border p-4">
        <Field label="出网方式"><select className="select" value={chain.length ? "chain" : "direct"} onChange={e => setChain(e.target.value === "direct" ? [] : [available?.id || ""])}>
          <option value="direct">直接出网</option><option value="chain">链式代理</option>
        </select></Field>
        {chain.map((reference, index) => <div key={index} className="flex items-end gap-2">
          <div className="min-w-0 flex-1"><Field label={`落地 ${index + 1}`}><select className="select" value={reference} onChange={e => setChain(chain.map((id, i) => i === index ? e.target.value : id))}>
            <option value="" disabled>选择已有代理，或在下方导入</option>
            {reference && !choices.some(c => c.id === reference) && <option value={reference}>当前代理不可用，请重新选择</option>}
            {choices.map(c => <option key={c.id} value={c.id} disabled={chain.some((id, i) => i !== index && id === c.id)}>{c.name}</option>)}
          </select></Field></div>
          <Button type="button" variant="ghost" size="icon" aria-label={`移除落地 ${index + 1}`} onClick={() => setChain(chain.filter((_, i) => i !== index))}><X /></Button>
        </div>)}
        {chain.length > 0 && <div className="flex flex-wrap gap-2">
          <Button type="button" variant="outline" disabled={!available || chain.length >= 8} onClick={() => available && setChain([...chain, available.id])}><Plus />添加下一跳</Button>
          {importExternal && <Button type="button" variant="outline" disabled={chain.filter(Boolean).length >= 8} onClick={() => importExternal(e => { if (e) setChain(old => [...old.filter(Boolean), `external:${e.id}`]); })}>导入落地代理</Button>}
        </div>}
        <p className="muted" aria-label="代理路径">客户端 → 本机{chain.map(id => ` → ${choices.find(c => c.id === id)?.name || "待选择落地"}`).join("")} → 互联网</p>
      </section>
      <details
        className="details"
        open={
          Boolean(
            line?.limits.monthly_gib ||
              line?.limits.upload_mbps ||
              line?.limits.download_mbps,
          ) || undefined
        }
      >
        <summary>流量额度与带宽上限</summary>
        <div className="grid gap-4 pt-4 sm:grid-cols-3">
          <Field label="上传上限 · Mbps">
            <Input
              name="upload"
              type="number"
              min="0.001"
              step="any"
              defaultValue={line?.limits.upload_mbps ?? ""}
              placeholder="不限速"
            />
          </Field>
          <Field label="下载上限 · Mbps">
            <Input
              name="download"
              type="number"
              min="0.001"
              step="any"
              defaultValue={line?.limits.download_mbps ?? ""}
              placeholder="不限速"
            />
          </Field>
          <Field label="月流量额度 · GiB">
            <Input
              name="quota"
              type="number"
              min="0.001"
              step="any"
              defaultValue={line?.limits.monthly_gib ?? ""}
              placeholder="不限量"
            />
          </Field>
        </div>
        <p className="muted mt-3">
          按入口上传＋下载统计，每月 1 日 UTC 重置；用尽停用。
        </p>
      </details>
      <div className="flex flex-wrap gap-5 text-sm">
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            name="enabled"
            defaultChecked={line?.enabled !== false}
          />
          启用
        </label>

      </div>
    </Modal>
  );
}
