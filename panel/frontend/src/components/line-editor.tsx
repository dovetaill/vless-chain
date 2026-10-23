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
  errorText,
} from "./shared";
import { api } from "../api";
import type { Line, PanelState, Protocol, NodeInfo, External } from "../types";

export function LineEditor({
  line,
  kind,
  state,
  initialChain,
  pendingImport,
  close,
  saved,
  ensureLocalServer,
  importExternal,
  request = api,
  remoteName,
}: {
  line?: Line;
  kind: Line["kind"];
  state: PanelState;
  initialChain?: string[];
  pendingImport?: { id: string; name: string };
  close: () => void;
  ensureLocalServer: (host: string) => Promise<NodeInfo>;
  importExternal?: (done: (external?: External) => void) => void;
  saved: (lines: Line[], apply?: boolean) => Promise<void>;
  request?: typeof api;
  remoteName?: string;
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
  const [certificate, setCertificate] = useState(line?.access.tls.mode || (protocol === "vless" ? "off" : "auto"));
  const supportsTls = ["vless", "trojan", "http", "vmess", "hysteria2", "tuic"].includes(protocol);
  const tls = supportsTls && domain ? certificate : "off";
  const showTls = supportsTls && domain;
  const [batch, setBatch] = useState(false);
  const [combinations, setCombinations] = useState(["tcp-reality", "tcp-tls"]);
  const [transport, setTransport] = useState(line?.access.transport?.network || "tcp");
  const [realityTarget, setRealityTarget] = useState(line?.access.reality?.target || existing?.target || local?.target || "www.microsoft.com:443");
  const [sni, setSni] = useState(line?.access.reality?.server_name || existing?.server_name || local?.server_name || "www.microsoft.com");
  const [shared, setShared] = useState(line?.access.shared_port || false);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState("");
  const [buttonChanges, setButtonChanges] = useState(false);
  const usesReality = protocol === "vless" && (batch ? combinations.some(c => c.endsWith("reality")) : tls === "off");
  const usesTls = protocol === "vless" && (batch ? combinations.some(c => c.endsWith("tls")) : tls !== "off");
  const usesXhttp = protocol === "vless" && (batch ? combinations.some(c => c.startsWith("xhttp")) : transport === "xhttp");
  const names: Record<string, string> = { "tcp-reality": "REALITY + Vision", "tcp-tls": "TLS + Vision", "xhttp-reality": "REALITY + XHTTP", "xhttp-tls": "TLS + XHTTP" };
  return (
    <Modal
      wide
      drawer={!!remoteName}
      dirty={buttonChanges}
      title={`${line ? "编辑代理" : "新建代理"}${remoteName ? ` · ${remoteName}` : ''}`}
      description={remoteName ? `操作目标：${remoteName}。保存后预览并应用到该 VPS。` : "在本机运行，创建后自动生成客户端链接。"}
      submit={remoteName ? "保存并预览" : line ? "保存并应用" : batch ? "创建所选组合" : "创建代理"}
      secondarySubmit={!line && !remoteName ? { name: "draft", label: "仅保存草稿" } : undefined}
      close={close}
      action={async (data) => {
        if (["trojan", "hysteria2", "tuic"].includes(protocol) && !domain) throw new Error(`${protocolNames[protocol]} 需要 TLS，请填写解析到本机的域名。`);
        if (chain.some(id => !id)) throw new Error("请选择链式代理的落地。");
        const node = line ? existing : await ensureLocalServer(host.trim());
        if (!node) throw new Error("本机记录不存在，请先修复本机设置。");
        const changedProtocol = line && line.access.protocol !== protocol;
        if (batch && (!combinations.length || usesTls && !domain)) throw new Error("请选择组合；TLS 组合需要解析到本机的域名。");
        if (batch && shared && (combinations.filter(c => c.endsWith("tls")).length > 1 || combinations.filter(c => c.endsWith("reality")).length > 1)) throw new Error("相同 SNI 无法同时分流 TCP 和 XHTTP；请选择一种传输，或使用独立端口。");
        const payload = {
          pending_import_id: pendingImport?.id,
          id: line?.id,
          kind,
          name: field(data, "name").trim() || `${protocolNames[protocol]} ${host.trim()}`.slice(0, 80),
          enabled: line ? line.enabled : data.has("enabled"),
          hops: line ? line.hops.map((h, i) => ({ node_id: h.node_id, port: i === 0 ? shared ? 443 : number(data, "port") : h.port })) : [{ node_id: node.id, port: shared ? 443 : batch ? null : number(data, "port") }],
          external_id: null, chain, target: "", network: "tcp",
          access: {
            transport: { network: protocol === "vless" ? transport : "tcp", path: field(data, "xhttp_path") || "/", host: field(data, "xhttp_host"), mode: field(data, "xhttp_mode") || "auto" },
            reality: usesReality ? { ...line?.access.reality, target: realityTarget.trim(), server_name: sni.trim() } : null,
            shared_port: protocol === "vless" && shared,
            fallback: usesTls && transport === "tcp" ? field(data, "fallback") : "",
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
          limits: line ? line.limits : {
            upload_mbps: number(data, "upload"),
            download_mbps: number(data, "download"),
            monthly_gib: number(data, "quota"),
          },
        };
        if (batch) {
          const rows = combinations.map(combination => {
            const [network, security] = combination.split("-");
            return { ...payload, name: `${payload.name} · ${names[combination]}`, access: { ...payload.access,
              transport: { ...payload.access.transport, network }, reality: security === "reality" ? { target: realityTarget.trim(), server_name: sni.trim() } : null,
              tls: security === "tls" ? { mode: certificate === "manual" ? "manual" : "auto", domain: host.trim(), cert: field(data, "cert"), key: field(data, "key") } : { mode: "off", domain: "", cert: "", key: "" },
              fallback: security === "tls" && network === "tcp" ? field(data, "fallback") : "",
            } };
          });
          await saved(await request<Line[]>("/lines/batch", "POST", rows), !data.has("draft"));
        } else {
          await saved([await request<Line>("/lines", "POST", payload)], !data.has("draft"));
        }
      }}
    >
      <Field label="名称">
        <Input
          name="name"
          defaultValue={line?.name || pendingImport?.name}
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
              setBatch(false); setShared(false);
              setCertificate(e.target.value === "vless" ? "off" : "auto");
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
      {protocol === "vless" && !line && <section className="space-y-3">
        {!pendingImport && <label className="flex items-center gap-2"><input type="checkbox" checked={batch} onChange={e => { setBatch(e.target.checked); if (e.target.checked) setCertificate("auto"); }} />一次创建多种组合</label>}
        {batch && <div className="grid gap-3 sm:grid-cols-2">{Object.entries(names).map(([value, name]) => <label key={value} className="flex items-center gap-2"><input type="checkbox" checked={combinations.includes(value)} onChange={e => setCombinations(old => e.target.checked ? [...old, value] : old.filter(v => v !== value))} />{name}</label>)}</div>}
      </section>}
      <Field label="本机 IP 或域名" hint={usesReality && !usesTls ? "使用 REALITY，无需证书；填写域名时需解析到本机。" : showTls ? "TLS 使用有效证书；自动申请需域名解析到本机并开放 TCP 80。" : ["trojan", "hysteria2", "tuic"].includes(protocol) ? "填写域名后自动申请 SSL 证书。" : "客户端通过此地址连接本机。"}>
        <Input value={host} onChange={e => setHost(e.target.value)} required placeholder="本机公网 IP 或域名" />
      </Field>
      <details className="details">
        <summary>监听端口 · 自动分配</summary>
        {protocol === "vless" && <label className="my-3 flex items-center gap-2"><input type="checkbox" checked={shared} onChange={e => setShared(e.target.checked)} />使用统一 443 入口（按 SNI 分流）</label>}
        {shared ? <p className="muted">需先在本机运行 chainctl ingress-enable；保留已有 HTTPS 网站。每个 SNI 对应一种入口。</p> : batch ? <p className="muted mt-3">为每个组合自动分配独立端口。</p> : <Field label="监听端口" hint={["hysteria2", "tuic"].includes(protocol) ? "留空自动分配；需放行此 UDP 端口。" : "留空自动选择空闲端口"}><Input name="port" type="number" min={1} max={65535} defaultValue={line?.hops[0]?.port} placeholder="自动分配" /></Field>}
      </details>
      {protocol === "vless" && !batch && <Field label="传输方式"><select className="select" value={transport} onChange={e => setTransport(e.target.value as "tcp" | "xhttp")}><option value="tcp">TCP + Vision</option><option value="xhttp">XHTTP</option></select></Field>}
      {usesXhttp && <section className="space-y-4 rounded-xl border border-border p-4">
        <Field label="XHTTP 路径"><Input name="xhttp_path" defaultValue={line?.access.transport?.path || "/"} required /></Field>
        <Field label="XHTTP Host" hint="留空不限制 Host；与 REALITY SNI 或证书域名是不同设置。"><Input name="xhttp_host" defaultValue={line?.access.transport?.host || ""} /></Field>
        <Field label="XHTTP 模式"><select name="xhttp_mode" className="select" defaultValue={line?.access.transport?.mode || "auto"}>{["auto", "packet-up", "stream-up", "stream-one"].map(mode => <option key={mode}>{mode}</option>)}</select></Field>
      </section>}
      {usesReality && <section className="space-y-4 rounded-xl border border-border p-4">
        <div className="flex flex-wrap items-center gap-2" role="group" aria-label="REALITY 目标快捷选择">
          <span className="muted">快捷选择</span>
          {[
            ["微软", "www.microsoft.com"],
            ["苹果", "www.apple.com"],
            ["AWS", "aws.amazon.com"],
            ["三星", "www.samsung.com"],
            ["索尼", "www.sony.com"],
            ["戴尔", "www.dell.com"],
            ["惠普", "www.hp.com"],
            ["Cloudflare", "www.cloudflare.com"],
          ].map(([name, host]) => <Button key={host} type="button" variant="outline" size="sm" disabled={checking} onClick={() => { setRealityTarget(`${host}:443`); setSni(host); setCheck(""); setButtonChanges(true); }}>{name}</Button>)}
        </div>
        <Field label="REALITY 目标地址" hint="目标网站的域名或 IP 加端口，需支持 TLS 1.3 与 H2；无需解析到本机。"><Input value={realityTarget} readOnly={checking} onChange={e => { setRealityTarget(e.target.value); setCheck(""); }} required /></Field>
        <Field label="REALITY SNI" hint="必须是目标网站证书覆盖的域名。无需申请自己的证书。"><Input value={sni} readOnly={checking} onChange={e => { setSni(e.target.value); setCheck(""); }} required /></Field>
        <Button type="button" variant="outline" disabled={checking} onClick={async () => { setChecking(true); setCheck(""); try { const result = await request<{ ok: boolean; detail: string; latency_ms: number; cdn?: { detail: string }; fallback?: { detail: string } }>(`/diagnostics/${(existing || local)?.id || (await ensureLocalServer(host.trim())).id}/reality`, "POST", { target: realityTarget.trim(), server_name: sni.trim() }); setCheck(`握手${result.ok ? "通过" : "未通过"}：${result.detail}（${result.latency_ms} ms）\nCDN：${result.cdn?.detail || "当前 Agent 未提供 CDN 风险检测"}\nSNI 过滤：${result.fallback?.detail || "当前 Agent 未提供过滤验证，不能判定无盗流风险"}`); } catch (e) { setCheck(errorText(e)); } finally { setChecking(false); } }}>{checking ? "正在检测…" : "检测 REALITY 握手与回落风险"}</Button>
        {check && <p className="muted whitespace-pre-line" role="status">{check}</p>}
      </section>}
      {kind === "vless" && ["http", "socks", "trojan", "hysteria2", "tuic"].includes(protocol) && <details className="details">
        <summary>账号与密码 · 高级</summary>
        <div className="grid gap-4 sm:grid-cols-2">
          {["http", "socks"].includes(protocol) && (
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
      {showTls && (!batch || usesTls) && (
        <details className="details"><summary>传输安全 · {tls === "off" ? protocol === "vless" ? "REALITY" : protocol === "vmess" ? "VMess AEAD" : "未启用 TLS" : "TLS 证书"}</summary>
          <Field label="HTTPS / 证书" hint={tls === "auto" ? "域名需解析到本机，并放行 TCP 80 和监听端口。" : undefined}>
            <select
              className="select"
              value={tls}
              onChange={(e) => {
                setCertificate(e.target.value);
              }}
            >
              <option value="auto">自动申请与续期证书</option>
              <option value="manual">使用自己的证书</option>
              {protocol === "vless" && !batch && <option value="off">REALITY（无需证书）</option>}
              {protocol === "vmess" && <option value="off">VMess AEAD（不加 TLS）</option>}
            </select>
          </Field>
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
      {usesTls && (batch ? combinations.includes("tcp-tls") : transport === "tcp") && <Field label="TLS 网站回落" hint="普通浏览器访问会显示此网站。留空关闭；builtin 使用内置主页，也可填写自己的 HTTP/HTTPS 网站地址。"><Input name="fallback" defaultValue={line?.access.fallback ?? "builtin"} placeholder="builtin 或 https://你的站点" /></Field>}
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
          <Button type="button" variant="ghost" size="icon" aria-label={`移除落地 ${index + 1}`} onClick={() => { setChain(chain.filter((_, i) => i !== index)); setButtonChanges(true); }}><X /></Button>
        </div>)}
        {chain.length > 0 && <div className="flex flex-wrap gap-2">
          <Button type="button" variant="outline" disabled={!available || chain.length >= 8} onClick={() => { if (available) { setChain([...chain, available.id]); setButtonChanges(true); } }}><Plus />添加下一跳</Button>
          {importExternal && <Button type="button" variant="outline" disabled={chain.filter(Boolean).length >= 8} onClick={() => importExternal(e => { if (e) { setChain(old => [...old.filter(Boolean), `external:${e.id}`]); setButtonChanges(true); } })}>导入落地代理</Button>}
        </div>}
        <p className="muted" aria-label="代理路径">客户端 → 本机{chain.map(id => ` → ${choices.find(c => c.id === id)?.name || "待选择落地"}`).join("")} → 互联网</p>
        {chain.length > 0 && <p className="muted">HTTP/HTTPS 出口仅支持 TCP；经它连接 Shadowsocks 或 SOCKS5 时也不支持 UDP。Hysteria 2、TUIC 的前置路径必须支持 UDP。</p>}
      </section>
      {!line && <details
        className="details"
      >
        <summary>流量额度与带宽上限</summary>
        <div className="grid gap-4 pt-4 sm:grid-cols-3">
          <Field label="上传上限 · Mbps">
            <Input
              name="upload"
              type="number"
              min="0.001"
              step="any"
              placeholder="不限速"
            />
          </Field>
          <Field label="下载上限 · Mbps">
            <Input
              name="download"
              type="number"
              min="0.001"
              step="any"
              placeholder="不限速"
            />
          </Field>
          <Field label="月流量额度 · GiB">
            <Input
              name="quota"
              type="number"
              min="0.001"
              step="any"
              placeholder="不限量"
            />
          </Field>
        </div>
        <p className="muted mt-3">
          按入口上传＋下载统计；出租后按开通日逐月重置（北京时间），用尽暂停。
        </p>
      </details>}
      {line && <p className="muted">额度、限速及手动启停请在代理列表的运营操作中调整。</p>}
      {!line && <div className="flex flex-wrap gap-5 text-sm">
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            name="enabled"
            defaultChecked
          />
          启用
        </label>

      </div>}
    </Modal>
  );
}
