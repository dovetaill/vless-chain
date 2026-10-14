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
import type { Line, PanelState, Protocol } from "../types";

export function LineEditor({
  line,
  kind,
  state,
  initialNodeId,
  close,
  saved,
}: {
  line?: Line;
  kind: Line["kind"];
  state: PanelState;
  initialNodeId?: string;
  close: () => void;
  saved: (line: Line, draft: boolean) => Promise<void>;
}) {
  const website = kind === "website",
    forward = kind === "forward";
  const [protocol, setProtocol] = useState<Protocol>(
    line?.access.protocol || "vless",
  );
  const [tls, setTls] = useState(
    line?.access.tls.mode || (website ? "auto" : "off"),
  );
  const [hops, setHops] = useState(
    line?.hops.map((h) => ({ node_id: h.node_id, port: String(h.port) })) || [
      { node_id: initialNodeId || state.nodes[0].id, port: website ? "443" : "" },
    ],
  );
  const [draft, setDraft] = useState(false);
  const [targetHost, targetPort] = splitAddress(
    forward ? line?.target || "" : "",
  );
  const update = (i: number, key: "node_id" | "port", value: string) =>
    setHops((old) => old.map((h, j) => (i === j ? { ...h, [key]: value } : h)));
  const available = state.nodes.find(
    (n) => !hops.some((h) => h.node_id === n.id),
  );
  const showTls = website || (kind === "vless" && protocol === "trojan");
  return (
    <Modal
      wide
      title={
        line
          ? `编辑${website ? "网站" : forward ? "转发规则" : "代理"}`
          : website
            ? "新建网站反代"
            : forward
              ? "新建端口转发"
              : "新建代理接入"
      }
      description={
        website
          ? "让一个域名访问指定的后端网站，支持 HTTPS 和 WebSocket。"
          : forward
            ? "把入口端口收到的数据转发到固定目标。无需配置代理客户端。"
            : "在自己的服务器创建代理，完成后导入客户端即可使用。"
      }
      submit={draft ? "仅保存草稿" : "保存并应用"}
      close={close}
      action={async (data) => {
        const changedProtocol = line && line.access.protocol !== protocol;
        const value = await api<Line>("/lines", "POST", {
          id: line?.id,
          kind,
          name: field(data, "name"),
          enabled: data.has("enabled"),
          hops: hops.map((h) => ({
            node_id: h.node_id,
            port: h.port ? Number(h.port) : null,
          })),
          external_id: website ? null : field(data, "external_id") || null,
          target: forward
            ? endpoint(field(data, "target_host"), field(data, "target_port"))
            : website
              ? field(data, "upstream")
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
              mode: showTls ? tls : "off",
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
        await saved(value, draft);
      }}
    >
      <Field label={website ? "网站名称" : "名称"}>
        <Input
          name="name"
          defaultValue={line?.name}
          placeholder={
            website ? "我的应用" : forward ? "数据库转发" : "日常上网"
          }
          maxLength={80}
          required
        />
      </Field>
      <div className="grid gap-4 sm:grid-cols-[1fr_160px]">
        <Field label="入口服务器">
          <select
            className="select"
            value={hops[0].node_id}
            onChange={(e) => update(0, "node_id", e.target.value)}
          >
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
          hint={website ? "通常 HTTP 80 / HTTPS 443" : "留空自动分配"}
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
              if (e.target.value === "trojan") setTls("auto");
            }}
          >
            {Object.entries(protocolNames).map(([id, label]) => (
              <option key={id} value={id}>
                {label}
              </option>
            ))}
          </select>
        </Field>
      )}
      {kind === "vless" && ["http", "socks", "trojan"].includes(protocol) && (
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
      )}
      {kind === "vless" && ["http", "socks"].includes(protocol) && (
        <p className="muted">
          HTTP / SOCKS5
          使用用户名和密码。普通连接本身不加密，适合可信网络或已有加密隧道。
        </p>
      )}
      {website && (
        <Field
          label="后端网站地址"
          hint="请求路径会追加到此地址之后；支持后端 HTTP 和 HTTPS。"
        >
          <Input
            name="upstream"
            defaultValue={line?.target}
            placeholder="http://127.0.0.1:3000"
            required
          />
        </Field>
      )}
      {showTls && (
        <section className="space-y-4 rounded-xl border border-border p-4">
          <Field label={website ? "访问域名" : "TLS 证书域名"}>
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
                if (website && ["80", "443"].includes(hops[0].port))
                  update(0, "port", e.target.value === "off" ? "80" : "443");
              }}
            >
              <option value="auto">自动申请与续期证书</option>
              <option value="manual">使用自己的证书</option>
              {website && <option value="off">HTTP（不启用 HTTPS）</option>}
            </select>
          </Field>
          {tls === "auto" && (
            <p className="muted">
              请先将域名解析到入口服务器，并放行 TCP 80
              和监听端口。申请时由服务器完成域名验证，首次应用可能需要几分钟。
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
      {!website && (
        <details className="details" open={hops.length > 1 || undefined}>
          <summary>经过其他服务器 / 使用外部代理出网</summary>
          <div className="space-y-3 pt-4">
            <p className="muted">
              默认由入口服务器直接连接目标。需要中转时，按顺序添加服务器。
            </p>
            {hops.slice(1).map((h, index) => (
              <div key={index} className="flex items-center gap-2">
                <span className="text-xs text-slate-400">{index + 2}</span>
                <select
                  className="select flex-1"
                  aria-label={`第 ${index + 2} 跳服务器`}
                  value={h.node_id}
                  onChange={(e) => update(index + 1, "node_id", e.target.value)}
                >
                  {state.nodes.map((n) => (
                    <option
                      key={n.id}
                      value={n.id}
                      disabled={hops.some(
                        (x, j) => j !== index + 1 && x.node_id === n.id,
                      )}
                    >
                      {n.name}
                    </option>
                  ))}
                </select>
                <Input
                  className="w-28"
                  aria-label={`第 ${index + 2} 跳端口`}
                  value={h.port}
                  type="number"
                  min={1}
                  max={65535}
                  placeholder="自动端口"
                  onChange={(e) => update(index + 1, "port", e.target.value)}
                />
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  aria-label="移除中转"
                  onClick={() =>
                    setHops(hops.filter((_, i) => i !== index + 1))
                  }
                >
                  <X />
                </Button>
              </div>
            ))}
            <Button
              type="button"
              variant="dashed"
              className="w-full"
              disabled={!available || hops.length >= 8}
              onClick={() =>
                available &&
                setHops([...hops, { node_id: available.id, port: "" }])
              }
            >
              <Plus />
              添加中转服务器
            </Button>
            <Field label="最后一步如何出网">
              <select
                className="select"
                name="external_id"
                defaultValue={line?.external_id || ""}
              >
                <option value="">直接连接目标</option>
                {state.externals.map((e) => (
                  <option key={e.id} value={e.id}>
                    已有代理 · {e.name}
                  </option>
                ))}
              </select>
            </Field>
          </div>
        </details>
      )}
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
          所有连接共享带宽上限。额度按入口上传＋下载合计，每月 1 日 UTC
          重置，用尽后自动停止传输。
        </p>
      </details>
      {kind === "vless" && (
        <details
          className="details"
          open={Boolean(line?.access.public_host) || undefined}
        >
          <summary>使用购买的中转入口</summary>
          <p className="muted mt-3">
            服务商已将此端口转发到本线路时填写。导出的连接地址会替换为中转入口，原认证和
            TLS/REALITY 参数保留。服务商处的映射仍由服务商管理。
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
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={draft}
            onChange={(e) => setDraft(e.target.checked)}
          />
          仅保存草稿，稍后批量应用
        </label>
      </div>
    </Modal>
  );
}
