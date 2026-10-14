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
  initialNodeId,
  close,
  saved,
  addServer,
  addLocalServer,
  importExternal,
}: {
  line?: Line;
  kind: Line["kind"];
  state: PanelState;
  initialNodeId?: string;
  close: () => void;
  addServer?: (done: (node?: NodeInfo) => void) => void;
  addLocalServer?: (done: (node?: NodeInfo) => void) => void;
  importExternal?: (done: (external?: External) => void) => void;
  saved: (line: Line, draft: boolean) => Promise<void>;
}) {
  const forward = kind === "forward";
  const [protocol, setProtocol] = useState<Protocol>(
    line?.access.protocol || "vless",
  );
  const [tls, setTls] = useState(
    line?.access.tls.mode || "off",
  );
  const [hops, setHops] = useState(
    line?.hops.map((h) => ({ node_id: h.node_id, port: String(h.port) })) || [
      { node_id: initialNodeId || state.local_node_id || "", port: "" },
    ],
  );
  const [externalId, setExternalId] = useState(line?.external_id || "");
  const [targetHost, targetPort] = splitAddress(
    forward ? line?.target || "" : "",
  );
  const update = (i: number, key: "node_id" | "port", value: string) =>
    setHops((old) => old.map((h, j) => (i === j ? { ...h, [key]: value } : h)));
  const available = state.nodes.find(
    (n) => !hops.some((h) => h.node_id === n.id),
  );
  const supportsTls = kind === "vless" && ["vless", "trojan"].includes(protocol);
  const showTls = supportsTls && tls !== "off";
  const exit = externalId ? `external:${externalId}` : hops.length > 1 ? `node:${hops[hops.length - 1].node_id}` : "direct";
  const middle = externalId ? hops.slice(1) : hops.slice(1, -1);
  const nodeName = (id: string) => state.nodes.find((n) => n.id === id)?.name || (id ? "未找到服务器" : "本机");
  const setExit = (value: string) => {
    if (value === "direct") {
      setHops([hops[0]]);
      setExternalId("");
    } else if (value.startsWith("external:")) {
      setHops([hops[0], ...middle]);
      setExternalId(value.slice(9));
    } else {
      const nodeId = value.slice(5);
      setHops([hops[0], ...middle.filter((h) => h.node_id !== nodeId),
        { node_id: nodeId, port: hops[hops.length - 1]?.node_id === nodeId ? hops[hops.length - 1].port : "" }]);
      setExternalId("");
    }
  };
  return (
    <Modal
      wide
      title={line ? `编辑${forward ? "端口转发" : "代理"}` : forward ? "创建端口转发" : "创建代理"}
      description=""
      submit={line ? "保存并应用" : "创建"}
      close={close}
      action={async (data) => {
        if (kind === "website") throw new Error("不再支持网站反代。");
        let entryId = hops[0].node_id;
        if (!entryId) {
          if (!addLocalServer) throw new Error("请先添加服务器。");
          const node = await new Promise<NodeInfo | undefined>((resolve) => addLocalServer(resolve));
          if (!node) return;
          entryId = node.id;
          update(0, "node_id", node.id);
        }
        const changedProtocol = line && line.access.protocol !== protocol;
        const value = await api<Line>("/lines", "POST", {
          id: line?.id,
          kind,
          name: field(data, "name").trim() || `${nodeName(hops[0].node_id)} ${forward ? "转发" : protocol.toUpperCase()}`.slice(0, 80),
          enabled: data.has("enabled"),
          hops: hops.map((h, i) => ({
            node_id: i === 0 ? entryId : h.node_id,
            port: h.port ? Number(h.port) : null,
          })),
          external_id: externalId || null,
          target: forward
            ? endpoint(field(data, "target_host"), field(data, "target_port"))
            : "",
          network: forward ? field(data, "network") : "tcp",
          access: {
            protocol: kind === "vless" ? protocol : "vless",
            username: field(data, "username") || line?.access.username || "",
            password:
              field(data, "password") ||
              (changedProtocol ? "" : line?.access.password || ""),
            method: "",
            public_host: field(data, "public_host"),
            public_port: number(data, "public_port"),
            tls: {
              mode: supportsTls ? tls : "off",
              domain: showTls ? field(data, "domain") : "",
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
      <div className="grid gap-4 sm:grid-cols-[1fr_160px]">
        <Field label="入口服务器">
          <select
            className="select"
            value={hops[0].node_id}
            onChange={(e) => update(0, "node_id", e.target.value)}
          >
            {!state.local_node_id && <option value="">本机（控制台服务器）</option>}
            {state.nodes.map((n) => (
              <option
                key={n.id}
                value={n.id}
                disabled={hops.slice(1).some((h) => h.node_id === n.id)}
              >
                {n.name}
                {!n.online ? " · 离线" : ""}
              </option>
            ))}
          </select>
        </Field>
        <Field
          label="监听端口"
          hint="留空自动分配"
        >
          <Input
            type="number"
            min={1}
            max={65535}
            aria-label="监听端口"
            placeholder="自动分配"
            value={hops[0].port}
            onChange={(e) => update(0, "port", e.target.value)}
          />
        </Field>
      </div>
      <div className="flex flex-wrap gap-2">
        {addServer && <Button type="button" variant="outline"
          onClick={() => addServer((node) => { if (node) update(0, "node_id", node.id); })}><Plus />添加远程服务器</Button>}
      </div>
      {forward && (
        <section className="rounded-xl border border-blue-100 bg-blue-50/40 p-4">
          <div className="mb-3 flex items-center gap-2 text-sm font-semibold text-blue-700">
            <ArrowRight className="size-4" />
            转发到固定目标
          </div>
          <div className="grid gap-4 sm:grid-cols-[1fr_140px]">
            <Field label="目标地址">
              <Input
                name="target_host"
                defaultValue={targetHost}
                placeholder="例如 192.0.2.20"
                required
              />
            </Field>
            <Field label="目标端口">
              <Input
                name="target_port"
                type="number"
                min={1}
                max={65535}
                defaultValue={targetPort}
                placeholder="例如 3306"
                required
              />
            </Field>
          </div>
          <div className="mt-4">
            <Field label="转发类型">
              <select
                className="select"
                name="network"
                defaultValue={line?.network || "tcp"}
              >
                <option value="tcp">TCP · 网页 / SSH / 数据库</option>
                <option value="udp">UDP · 游戏 / DNS 等服务</option>
                <option value="tcp,udp">TCP + UDP</option>
              </select>
            </Field>
          </div>
        </section>
      )}
      {kind === "vless" && (
        <Field label="客户端协议">
          <select
            className="select"
            value={protocol}
            onChange={(e) => {
              setProtocol(e.target.value as Protocol);
              setTls(e.target.value === "trojan" ? "auto" : "off");
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
      {kind === "vless" && protocol === "vless" && (
        <Field label="安全方式">
          <select className="select" value={tls === "off" ? "off" : "tls"} onChange={(e) => setTls(e.target.value === "off" ? "off" : "auto")}>
            <option value="off">REALITY + Vision</option>
            <option value="tls">TLS + Vision</option>
          </select>
        </Field>
      )}
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
        <section className="space-y-4 rounded-xl border border-border p-4">
          <Field label="TLS 证书域名">
            <Input
              name="domain"
              defaultValue={line?.access.tls.domain}
              placeholder="app.example.com"
              required
            />
          </Field>
          <Field label="HTTPS / 证书">
            <select
              className="select"
              value={tls}
              onChange={(e) => {
                setTls(e.target.value);
              }}
            >
              <option value="auto">自动申请与续期证书</option>
              <option value="manual">使用自己的证书</option>
            </select>
          </Field>
          {tls === "auto" && (
            <p className="muted">
              域名需解析到入口服务器，并放行 TCP 80 和监听端口。
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
        </section>
      )}
      <details className="details" open={exit !== "direct" || undefined}>
          <summary aria-label="代理路径">
            {hops.map((h, i) => `${nodeName(h.node_id)}${hops.length > 1 ? `（${i === 0 ? "入口" : !externalId && i === hops.length - 1 ? "出口" : "中转"}）` : ""}`).join(" → ")}
            {externalId && ` → ${state.externals.find((e) => e.id === externalId)?.name || "已有代理"}（出口）`}
            {forward ? " → 转发目标" : " → 互联网"} · 修改
          </summary>
          <div className="space-y-3 pt-4">
            <Field label={forward ? "经由服务器 / 代理" : "出口"}>
              <select className="select" value={exit} onChange={(e) => setExit(e.target.value)}>
                <option value="direct">{forward ? "入口直接连接转发目标" : "入口服务器直接出网"}</option>
                {state.nodes.filter((n) => n.id !== hops[0].node_id).map((n) => (
                  <option key={n.id} value={`node:${n.id}`} disabled={Boolean(externalId) && hops.length >= 8 && !middle.some((h) => h.node_id === n.id)}>{n.name}</option>
                ))}
                {state.externals.map((e) => <option key={e.id} value={`external:${e.id}`}>已有代理 · {e.name}</option>)}
              </select>
            </Field>
            {importExternal && <Button type="button" variant="outline" onClick={() => importExternal((external) => {
              if (external) setExit(`external:${external.id}`);
            })}>粘贴已有代理</Button>}
            {middle.map((h, index) => (
              <div key={index} className="flex items-center gap-2">
                <span className="text-xs text-slate-500">中转 {index + 1}</span>
                <select className="select flex-1" aria-label={`中转 ${index + 1} 服务器`} value={h.node_id}
                  onChange={(e) => update(index + 1, "node_id", e.target.value)}>
                  {state.nodes.map((n) => <option key={n.id} value={n.id}
                    disabled={hops.some((x, j) => j !== index + 1 && x.node_id === n.id)}>{n.name}</option>)}
                </select>
                <Input className="w-28" aria-label={`中转 ${index + 1} 端口`} value={h.port}
                  type="number" min={1} max={65535} placeholder="自动端口" onChange={(e) => update(index + 1, "port", e.target.value)} />
                <Button type="button" variant="ghost" size="icon" aria-label={`移除中转 ${index + 1}`}
                  onClick={() => setHops(hops.filter((_, i) => i !== index + 1))}><X /></Button>
              </div>
            ))}
            {exit !== "direct" && <Button type="button" variant="dashed" className="w-full"
              disabled={!available || hops.length >= 8} onClick={() => {
                if (!available) return;
                const hop = { node_id: available.id, port: "" };
                setHops(externalId ? [...hops, hop] : [...hops.slice(0, -1), hop, hops[hops.length - 1]]);
              }}><Plus />添加中转</Button>}
            {!externalId && hops.length > 1 && <Field label="出口监听端口" hint="留空自动分配">
              <Input type="number" min={1} max={65535} value={hops[hops.length - 1].port}
                onChange={(e) => update(hops.length - 1, "port", e.target.value)} />
            </Field>}
          </div>
        </details>
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
      {kind === "vless" && (
        <details
          className="details"
          open={Boolean(line?.access.public_host) || undefined}
        >
          <summary>使用购买的中转入口</summary>
          <p className="muted mt-3">
            服务商已配置端口映射时填写，仅替换分享地址。
          </p>
          <div className="mt-4 grid gap-4 sm:grid-cols-[1fr_140px]">
            <Field label="中转入口地址">
              <Input
                name="public_host"
                defaultValue={line?.access.public_host}
                placeholder="relay.example.com"
              />
            </Field>
            <Field label="中转入口端口">
              <Input
                name="public_port"
                type="number"
                min={1}
                max={65535}
                defaultValue={line?.access.public_port ?? ""}
              />
            </Field>
          </div>
        </details>
      )}
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
