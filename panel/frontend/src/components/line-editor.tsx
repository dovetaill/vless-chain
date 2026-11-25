import { useId, useState } from "react";
import { Plus, X, Sparkles, ShieldCheck } from "lucide-react";
import { Input } from "./ui/input";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import {
  Field,
  InfoTip,
  Modal,
  field,
  number,
  protocolNames,
  errorText,
} from "./shared";
import { api } from "../api";
import type { Line, PanelState, Protocol, NodeInfo, External } from "../types";

type RealityCandidate = {
  name: string;
  target: string;
  server_name: string;
  detail: string;
};

function connectionHost(value: string) {
  const text = value.trim();
  if (/^https?:\/\//i.test(text)) {
    try {
      return new URL(text).hostname.replace(/^\[|\]$/g, "");
    } catch {
      return text;
    }
  }
  return text;
}

export function LineEditor({
  line,
  state,
  initialChain,
  pendingImport,
  close,
  saved,
  ensureLocalServer,
  importExternal,
  request = api,
  remoteName,
  defaultHost = location.hostname.replace(/^\[|\]$/g, ""),
}: {
  line?: Line;
  state: PanelState;
  initialChain?: string[];
  pendingImport?: { id: string; name: string };
  close: () => void;
  ensureLocalServer: (host: string) => Promise<NodeInfo>;
  importExternal?: (done: (external?: External) => void) => void;
  saved: (lines: Line[], apply?: boolean) => Promise<void>;
  request?: typeof api;
  remoteName?: string;
  defaultHost?: string;
}) {
  const local = state.nodes.find((n) => n.id === state.local_node_id);
  const existing = state.nodes.find((n) => n.id === line?.hops[0]?.node_id);
  const schemeId = useId();
  const [host, setHost] = useState(
    line
      ? line.access.host || line.access.tls.domain || existing?.host || ""
      : defaultHost,
  );
  const address = connectionHost(host);
  const [chain, setChain] = useState<string[]>(
    line?.chain?.length ? line.chain : initialChain || [],
  );
  const domain =
    Boolean(address) &&
    !/[\s:/?#@]/.test(address) &&
    !/^[\d.]+$/.test(address) &&
    !address.includes(":");
  const choices = [
    ...state.externals.map((e) => ({ id: `external:${e.id}`, name: e.name })),
    ...state.lines
      .filter((l) => l.id !== line?.id && l.enabled && l.published)
      .map((l) => ({ id: `line:${l.id}`, name: `${l.name}（本机代理）` })),
  ];
  const available = choices.find((c) => !chain.includes(c.id));
  const [protocol, setProtocol] = useState<Protocol>(
    line?.access.protocol || "vless",
  );
  const [certificate, setCertificate] = useState(
    line?.access.tls.mode ||
      (protocol === "vless"
        ? "off"
        : ["trojan", "hysteria2", "tuic"].includes(protocol)
          ? domain
            ? "auto"
            : "manual"
          : domain
            ? "auto"
            : "off"),
  );
  const [method, setMethod] = useState(
    line?.access.method || "2022-blake3-aes-128-gcm",
  );
  const usesUuid = ["vless", "vmess", "tuic"].includes(protocol);
  const supportsTls = [
    "vless",
    "trojan",
    "http",
    "vmess",
    "hysteria2",
    "tuic",
  ].includes(protocol);
  const tls = supportsTls ? certificate : "off";
  const showTls =
    supportsTls && (protocol !== "vless" || certificate !== "off");
  const [batch, setBatch] = useState(false);
  const [combinations, setCombinations] = useState(["tcp-reality"]);
  const [transport, setTransport] = useState(
    line?.access.transport?.network || "tcp",
  );
  const [realityTarget, setRealityTarget] = useState(
    line ? line.access.reality?.target || existing?.target || "" : "",
  );
  const [sni, setSni] = useState(
    line ? line.access.reality?.server_name || existing?.server_name || "" : "",
  );
  const [shared, setShared] = useState(line?.access.shared_port || false);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState("");
  const [recommending, setRecommending] = useState(false);
  const [candidates, setCandidates] = useState<RealityCandidate[]>([]);
  const [recommendation, setRecommendation] = useState("");
  const [buttonChanges, setButtonChanges] = useState(false);

  const usesReality =
    protocol === "vless" &&
    (batch ? combinations.some((c) => c.endsWith("reality")) : tls === "off");
  const usesTls =
    protocol === "vless" &&
    (batch ? combinations.some((c) => c.endsWith("tls")) : tls !== "off");
  const usesXhttp =
    protocol === "vless" &&
    (batch ? combinations.some((c) => c.startsWith("xhttp")) : transport === "xhttp");

  const names: Record<string, string> = {
    "tcp-reality": "REALITY + Vision",
    "tcp-tls": "TLS + Vision",
    "xhttp-reality": "REALITY + XHTTP",
    "xhttp-tls": "TLS + XHTTP",
  };
  const combination = `${transport}-${certificate === "off" ? "reality" : "tls"}`;
  const selected = batch ? combinations : [combination];
  const batchError = !batch
    ? ""
    : !combinations.length
      ? "请选择至少一种连接方案。"
      : shared &&
          (combinations.filter((c) => c.endsWith("tls")).length > 1 ||
            combinations.filter((c) => c.endsWith("reality")).length > 1)
        ? "相同 SNI 无法同时分流 TCP 和 XHTTP；请选择一种传输，或使用独立端口。"
        : !shared && combinations.filter((c) => c.endsWith("reality")).length > 1
          ? "两种 REALITY 方案都默认使用 443，不能同批创建。请保留一种，另一种单独创建并指定不同端口。"
          : "";

  function selectCombination(value: string) {
    setTransport(value.startsWith("tcp") ? "tcp" : "xhttp");
    setCertificate(
      value.endsWith("reality")
        ? "off"
        : certificate === "manual" || !domain
          ? "manual"
          : "auto",
    );
  }

  return (
    <Modal
      wide
      drawer={!!remoteName}
      dirty={buttonChanges}
      title={`${line ? "编辑代理" : "新建代理"}${remoteName ? ` · ${remoteName}` : ""}`}
      description={
        remoteName
          ? `远端节点：${remoteName}（保存后预览并同步）`
          : "配置入口协议与出网路径，保存后生成客户端连接信息。"
      }
      submit={
        remoteName
          ? "保存并预览"
          : line
            ? "保存并应用"
            : batch
              ? "创建所选组合"
              : "创建代理"
      }
      secondarySubmit={
        !line && !remoteName ? { name: "draft", label: "仅保存草稿" } : undefined
      }
      close={close}
      action={async (data) => {
        if (checking || recommending)
          throw new Error("目标检测尚未完成，请稍候。");
        if (!address || /[\s/?#@]/.test(address))
          throw new Error("请输入有效的本机 IP、域名或完整的 HTTP/HTTPS 地址。");
        if (batchError) throw new Error(batchError);
        const tlsMode = certificate === "manual" ? "manual" : "auto";
        if ((usesTls ? tlsMode === "auto" : tls === "auto") && !domain)
          throw new Error(
            "自动申请证书需要解析到本机的域名；使用 IP 地址请改选「手动粘贴 PEM 证书」。",
          );
        if (usesReality && (!realityTarget.trim() || !sni.trim()))
          throw new Error("请选择检测通过的推荐，或填写 REALITY 目标与 SNI。");
        if (["trojan", "hysteria2", "tuic"].includes(protocol) && tls === "off")
          throw new Error(
            `${protocolNames[protocol]} 需要启用 TLS（域名自动签发或手动粘贴 PEM 证书）。`,
          );
        if (chain.some((id) => !id)) throw new Error("请选择链式代理的落地。");
        const node = line ? existing : await ensureLocalServer(address);
        if (!node) throw new Error("本机记录不存在，请先修复本机设置。");
        const changedProtocol = line && line.access.protocol !== protocol;
        const uuid = usesUuid ? field(data, "uuid") : "";
        const credential = uuid ? { uuid } : {};
        const tlsDomain = domain ? address : line?.access.tls.domain || "";
        const payload = {
          pending_import_id: pendingImport?.id,
          id: line?.id,
          kind: "vless" as const,
          name:
            field(data, "name").trim() ||
            `${protocolNames[protocol]} ${address}`.slice(0, 80),
          enabled: line ? line.enabled : data.has("enabled"),
          hops: line
            ? line.hops.map((h, i) => ({
                node_id: h.node_id,
                port: i === 0 ? (shared ? 443 : number(data, "port")) : h.port,
                ...(i === 0 ? credential : {}),
              }))
            : [
                {
                  node_id: node.id,
                  port: shared ? 443 : batch ? null : number(data, "port"),
                  ...credential,
                },
              ],
          chain,
          access: {
            transport: {
              network: protocol === "vless" ? transport : "tcp",
              path: field(data, "xhttp_path") || "/",
              host: field(data, "xhttp_host"),
              mode: field(data, "xhttp_mode") || "auto",
            },
            reality: usesReality
              ? {
                  ...line?.access.reality,
                  target: realityTarget.trim(),
                  server_name: sni.trim(),
                }
              : null,
            shared_port: protocol === "vless" && shared,
            fallback: usesTls && transport === "tcp" ? field(data, "fallback") : "",
            host: address,
            protocol,
            username: field(data, "username") || line?.access.username || "",
            password:
              String(data.get("password") || "") ||
              (changedProtocol ? "" : line?.access.password || ""),
            method: protocol === "shadowsocks" ? method : "",
            tls: {
              mode: supportsTls ? tls : "off",
              domain: tls !== "off" ? tlsDomain : "",
              cert: tls === "manual" ? field(data, "cert") : "",
              key: tls === "manual" ? field(data, "key") : "",
            },
          },
          limits: line
            ? line.limits
            : {
                upload_mbps: number(data, "upload"),
                download_mbps: number(data, "download"),
                monthly_gib: number(data, "quota"),
              },
        };
        if (batch) {
          const rows = combinations.map((combination) => {
            const [network, security] = combination.split("-");
            return {
              ...payload,
              name: `${payload.name} · ${names[combination]}`,
              access: {
                ...payload.access,
                transport: { ...payload.access.transport, network },
                reality:
                  security === "reality"
                    ? { target: realityTarget.trim(), server_name: sni.trim() }
                    : null,
                tls:
                  security === "tls"
                    ? {
                        mode: tlsMode,
                        domain: tlsDomain,
                        cert: tlsMode === "manual" ? field(data, "cert") : "",
                        key: tlsMode === "manual" ? field(data, "key") : "",
                      }
                    : { mode: "off", domain: "", cert: "", key: "" },
                fallback:
                  security === "tls" && network === "tcp"
                    ? field(data, "fallback")
                    : "",
              },
            };
          });
          await saved(
            await request<Line[]>("/lines/batch", "POST", rows),
            !data.has("draft"),
          );
        } else {
          await saved(
            [await request<Line>("/lines", "POST", payload)],
            !data.has("draft"),
          );
        }
      }}
    >
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="名称">
          <Input
            name="name"
            defaultValue={line?.name || pendingImport?.name}
            placeholder="留空自动按协议与地址命名"
            maxLength={80}
          />
        </Field>
        <Field label="协议">
          <select
            className="select"
            value={protocol}
            onChange={(e) => {
              const next = e.target.value as Protocol;
              setProtocol(next);
              setBatch(false);
              setShared(false);
              setCertificate(
                next === "vless"
                  ? "off"
                  : ["trojan", "hysteria2", "tuic"].includes(next)
                    ? domain
                      ? "auto"
                      : "manual"
                    : domain
                      ? "auto"
                      : "off",
              );
            }}
          >
            {Object.entries(protocolNames).map(([id, label]) => (
              <option key={id} value={id}>
                {id === "vless" ? "VLESS" : label}
              </option>
            ))}
          </select>
        </Field>
      </div>

      {protocol === "vless" && (
        <section className="space-y-3.5 rounded-xl border border-border bg-muted/20 p-4">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <span className="text-xs font-semibold text-foreground">
              VLESS 传输与安全方案
            </span>
            {!line && !pendingImport && (
              <div className="flex flex-wrap items-center gap-3 text-xs">
                <label className="flex cursor-pointer items-center gap-1.5">
                  <input
                    type="radio"
                    name="creation_mode"
                    checked={!batch}
                    onChange={() => {
                      selectCombination(combinations[0] || combination);
                      setBatch(false);
                    }}
                  />
                  单个创建
                </label>
                <label className="flex cursor-pointer items-center gap-1.5">
                  <input
                    type="radio"
                    name="creation_mode"
                    checked={batch}
                    onChange={() => {
                      setCombinations([combination]);
                      setBatch(true);
                    }}
                  />
                  批量创建不同方案
                </label>
              </div>
            )}
          </div>

          <div className="grid gap-2.5 sm:grid-cols-2">
            {Object.entries(names).map(([value, name]) => {
              const active = selected.includes(value);
              return (
                <label
                  key={value}
                  className={`flex cursor-pointer items-start gap-2.5 rounded-lg border p-3 transition-colors focus-within:ring-2 focus-within:ring-ring ${
                    active
                      ? "border-primary bg-primary/8"
                      : "border-border bg-card hover:bg-accent/50"
                  }`}
                >
                  <input
                    type={batch ? "checkbox" : "radio"}
                    name="connection_scheme"
                    aria-label={name}
                    aria-describedby={`${schemeId}-${value}`}
                    className="mt-0.5"
                    checked={active}
                    onChange={(e) => {
                      if (!batch) selectCombination(value);
                      else {
                        setCombinations((old) =>
                          e.target.checked
                            ? [...old, value]
                            : old.filter((v) => v !== value),
                        );
                        if (
                          e.target.checked &&
                          value.endsWith("tls") &&
                          certificate === "off"
                        )
                          setCertificate(domain ? "auto" : "manual");
                      }
                    }}
                  />
                  <span className="min-w-0">
                    <span className="block text-xs font-semibold text-foreground">
                      {name}
                    </span>
                    <span
                      id={`${schemeId}-${value}`}
                      className="mt-0.5 block text-[11px] leading-relaxed text-muted-foreground"
                    >
                      {value.endsWith("reality")
                        ? "免证书伪装，需指定目标站点"
                        : "需自备域名，支持自动签发证书"}
                      {" · "}
                      {value.startsWith("xhttp") ? "XHTTP 传输" : "TCP 传输"}
                    </span>
                  </span>
                </label>
              );
            })}
          </div>

          <p role="status" className="text-xs text-muted-foreground">
            {line
              ? `当前方案：VLESS · ${names[combination]}`
              : selected.length
                ? `将创建 ${selected.length} 个代理：${selected
                    .map((value) => `VLESS · ${names[value]}`)
                    .join("、")}`
                : "请选择至少一种连接方案。"}
          </p>
          {batchError && (
            <p role="alert" className="text-xs font-medium text-destructive">
              {batchError}
            </p>
          )}
        </section>
      )}

      <Field
        label="本机 IP 或域名"
        hint={
          usesReality && !usesTls
            ? "使用 REALITY，无需证书；填写域名时需解析到本机。"
            : showTls
              ? "域名需解析到本机，并放行 TCP 80 和监听端口。"
              : ["trojan", "hysteria2", "tuic"].includes(protocol)
                ? "填写解析到本机的域名以自动申请证书。"
                : "客户端连接此地址接入。"
        }
      >
        <Input
          value={host}
          onChange={(e) => setHost(e.target.value)}
          onBlur={() => setHost(address)}
          onPaste={(e) => {
            const text = e.clipboardData.getData("text");
            if (/^https?:\/\//i.test(text.trim())) {
              e.preventDefault();
              setHost(connectionHost(text));
              setButtonChanges(true);
            }
          }}
          required
          placeholder="域名、IP 或粘贴 http(s):// 地址"
        />
      </Field>

      <details className="details">
        <summary>
          {usesReality ? "监听端口 · REALITY 默认 443" : "监听端口 · 自动分配"}
        </summary>
        <div className="mt-3.5 space-y-3.5">
          {protocol === "vless" && (
            <label className="flex items-center gap-2 text-xs font-medium">
              <input
                type="checkbox"
                checked={shared}
                onChange={(e) => setShared(e.target.checked)}
              />
              使用统一 443 入口（按 SNI 分流）
            </label>
          )}
          {shared ? (
            <p className="muted">
              需先在本机执行 chainctl ingress-enable；可与现有 HTTPS 站点共存。
            </p>
          ) : batch ? (
            <p className="muted">
              REALITY 默认占用 443；同机多条 REALITY 请分开创建并指定独立端口。
            </p>
          ) : (
            <Field
              label="监听端口"
              hint={
                usesReality
                  ? "留空默认使用 TCP 443"
                  : ["hysteria2", "tuic"].includes(protocol)
                    ? "留空自动分配，需放行对应 UDP 端口"
                    : "留空自动分配空闲端口"
              }
            >
              <Input
                name="port"
                type="number"
                min={1}
                max={65535}
                defaultValue={line?.hops[0]?.port}
                placeholder={usesReality ? "443" : "自动分配"}
              />
            </Field>
          )}
        </div>
      </details>

      {usesXhttp && (
        <section className="grid gap-3.5 rounded-xl border border-border bg-muted/20 p-4 sm:grid-cols-3">
          <Field label="XHTTP 路径">
            <Input
              name="xhttp_path"
              defaultValue={line?.access.transport?.path || "/"}
              required
            />
          </Field>
          <Field label="XHTTP Host" tooltip="留空不限制 Host；与 REALITY SNI 或证书域名相互独立。">
            <Input
              name="xhttp_host"
              defaultValue={line?.access.transport?.host || ""}
              placeholder="可选"
            />
          </Field>
          <Field label="XHTTP 模式">
            <select
              name="xhttp_mode"
              className="select"
              defaultValue={line?.access.transport?.mode || "auto"}
            >
              {["auto", "packet-up", "stream-up", "stream-one"].map((mode) => (
                <option key={mode}>{mode}</option>
              ))}
            </select>
          </Field>
        </section>
      )}

      {usesReality && (
        <section className="space-y-3.5 rounded-xl border border-border bg-muted/20 p-4">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-1.5">
              <span className="text-xs font-semibold text-foreground">
                REALITY 伪装目标与握手验证
              </span>
              <InfoTip>
                由节点本地实测候选站点的 TLS 1.3、H2 与证书链并排除 CDN 网段；点选候选即可自动填入下方字段。
              </InfoTip>
            </div>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={checking || recommending || !host.trim()}
              onClick={async () => {
                setRecommending(true);
                setCandidates([]);
                setRecommendation("");
                try {
                  const node =
                    existing || local || (await ensureLocalServer(address));
                  const result = await request<{
                    candidates: RealityCandidate[];
                    checked: number;
                    detail: string;
                  }>(`/diagnostics/${node.id}/reality-candidates`, "POST", {});
                  setCandidates(result.candidates);
                  setRecommendation(
                    `本批检查 ${result.checked} 个官网。${result.detail}`,
                  );
                } catch (e) {
                  setRecommendation(errorText(e));
                } finally {
                  setRecommending(false);
                }
              }}
            >
              <Sparkles />
              {recommending
                ? "正在探测候选…"
                : recommendation
                  ? "换一批推荐"
                  : "检测并推荐目标"}
            </Button>
          </div>

          {recommendation && (
            <p className="muted" role="status">
              {recommendation}
            </p>
          )}

          {candidates.length > 0 && (
            <div
              className="grid gap-2 sm:grid-cols-2"
              role="group"
              aria-label="检测通过的 REALITY 候选"
            >
              {candidates.map((candidate) => (
                <div
                  key={candidate.server_name}
                  className="rounded-lg border border-border bg-card p-2.5"
                >
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    className="max-w-full whitespace-normal break-all text-left"
                    disabled={checking || recommending}
                    onClick={() => {
                      setRealityTarget(candidate.target);
                      setSni(candidate.server_name);
                      setCheck("");
                      setButtonChanges(true);
                    }}
                  >
                    {candidate.server_name}
                  </Button>
                  <p className="mt-1.5 text-[11px] leading-relaxed text-muted-foreground">
                    {candidate.name} · {candidate.detail}
                  </p>
                </div>
              ))}
            </div>
          )}

          <div className="grid gap-3.5 sm:grid-cols-2">
            <Field label="REALITY 目标地址" tooltip="支持 TLS 1.3 与 H2 的目标域名:端口，无需解析到本机。">
              <Input
                value={realityTarget}
                readOnly={checking || recommending}
                onChange={(e) => {
                  setRealityTarget(e.target.value);
                  setCheck("");
                }}
                required
                placeholder="例如 example.edu:443"
              />
            </Field>
            <Field label="REALITY SNI" tooltip="目标站点证书覆盖的域名。">
              <Input
                value={sni}
                readOnly={checking || recommending}
                onChange={(e) => {
                  setSni(e.target.value);
                  setCheck("");
                }}
                required
                placeholder="例如 example.edu"
              />
            </Field>
          </div>

          <div className="flex flex-wrap items-center gap-3">
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={
                checking ||
                recommending ||
                !realityTarget.trim() ||
                !sni.trim()
              }
              onClick={async () => {
                setChecking(true);
                setCheck("");
                try {
                  const result = await request<{
                    ok: boolean;
                    detail: string;
                    latency_ms: number;
                    cdn?: { detail: string };
                    fallback?: { detail: string };
                  }>(
                    `/diagnostics/${
                      (existing || local)?.id ||
                      (await ensureLocalServer(address)).id
                    }/reality`,
                    "POST",
                    { target: realityTarget.trim(), server_name: sni.trim() },
                  );
                  setCheck(
                    `握手${result.ok ? "通过" : "未通过"}：${result.detail}（${result.latency_ms} ms）\nCDN：${result.cdn?.detail || "当前 Agent 未提供 CDN 风险检测"}\nSNI 过滤：${result.fallback?.detail || "当前 Agent 未提供过滤验证，不能判定无盗流风险"}`,
                  );
                } catch (e) {
                  setCheck(errorText(e));
                } finally {
                  setChecking(false);
                }
              }}
            >
              <ShieldCheck />
              {checking ? "正在检测…" : "检测 REALITY 握手与回落风险"}
            </Button>
          </div>
          {check && (
            <p
              className="whitespace-pre-line rounded-lg border border-border bg-card p-3 text-xs leading-relaxed text-muted-foreground"
              role="status"
            >
              {check}
            </p>
          )}
        </section>
      )}

      {showTls && (protocol !== "vless" || usesTls) && (
        <details className="details" open={protocol === "vless" || undefined}>
          <summary>
            {protocol === "vless"
              ? "TLS 证书设置"
              : `传输安全 · ${
                  tls === "off"
                    ? protocol === "vmess"
                      ? "VMess AEAD"
                      : "未启用 TLS"
                    : "TLS 证书"
                }`}
          </summary>
          <div className="mt-3.5 space-y-3.5">
            <Field
              label="HTTPS / 证书"
              hint={
                tls === "auto"
                  ? "首次下发时自动签发，并在到期前自动续期。"
                  : undefined
              }
            >
              <select
                className="select"
                value={tls}
                onChange={(e) => {
                  setCertificate(e.target.value);
                }}
              >
                <option value="auto">自动申请与续期证书（需域名）</option>
                <option value="manual">手动粘贴 PEM 证书</option>
                {["vmess", "http"].includes(protocol) && (
                  <option value="off">
                    {protocol === "vmess"
                      ? "VMess AEAD（不启用 TLS）"
                      : "不启用 TLS"}
                  </option>
                )}
              </select>
            </Field>
            {tls === "manual" && (
              <div className="grid gap-3.5 sm:grid-cols-2">
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
              </div>
            )}
          </div>
        </details>
      )}

      {usesTls &&
        (batch ? combinations.includes("tcp-tls") : transport === "tcp") && (
          <Field
            label="TLS 网站回落"
            tooltip="浏览器直接访问该端口时展示的页面。builtin 使用内置伪装页，留空关闭回落。"
          >
            <Input
              name="fallback"
              defaultValue={line?.access.fallback ?? "builtin"}
              placeholder="builtin 或 https://你的站点"
            />
          </Field>
        )}

      <section
        key={protocol}
        className="space-y-3 rounded-xl border border-border bg-muted/20 p-4"
      >
        <div className="flex items-center gap-1.5">
          <h3 className="text-xs font-semibold text-foreground">认证凭据</h3>
          {line && (
            <InfoTip>
              留空保留原凭据；修改 UUID、密码或加密算法后需同步更新客户端配置。
            </InfoTip>
          )}
        </div>
        {protocol === "shadowsocks" && (
          <Field label="Shadowsocks 加密方式">
            <select
              className="select"
              value={method}
              onChange={(e) => setMethod(e.target.value)}
            >
              <option value="2022-blake3-aes-128-gcm">
                2022-blake3-aes-128-gcm（推荐）
              </option>
              <option value="2022-blake3-aes-256-gcm">
                2022-blake3-aes-256-gcm
              </option>
              <option value="aes-128-gcm">aes-128-gcm</option>
              <option value="aes-256-gcm">aes-256-gcm</option>
              <option value="chacha20-ietf-poly1305">
                chacha20-ietf-poly1305
              </option>
            </select>
          </Field>
        )}
        <div className="grid gap-3.5 sm:grid-cols-2">
          {usesUuid && (
            <Field label="UUID">
              <Input
                name="uuid"
                defaultValue={line?.hops[0]?.uuid}
                autoComplete="off"
                spellCheck={false}
                placeholder={line ? "留空保留原 UUID" : "留空自动生成"}
              />
            </Field>
          )}
          {["http", "socks"].includes(protocol) && (
            <Field label="用户名">
              <Input
                name="username"
                defaultValue={line?.access.username}
                autoComplete="off"
                placeholder="默认 chain"
              />
            </Field>
          )}
          {!["vless", "vmess"].includes(protocol) && (
            <Field
              label="连接密码"
              tooltip={
                protocol === "shadowsocks" && method.startsWith("2022-")
                  ? `此算法需要 ${method === "2022-blake3-aes-128-gcm" ? 16 : 32} 字节 Base64 密钥；留空自动生成或保留。`
                  : undefined
              }
            >
              <Input
                name="password"
                type="password"
                autoComplete="new-password"
                placeholder={line ? "留空保留原密码" : "留空自动生成"}
              />
            </Field>
          )}
        </div>
      </section>

      <section className="space-y-3 rounded-xl border border-border bg-muted/20 p-4">
        <Field
          label="出网方式"
          tooltip="链式出网时，HTTP/HTTPS 出口仅支持 TCP；Hysteria 2 与 TUIC 的前置链路需支持 UDP。"
        >
          <select
            className="select"
            value={chain.length ? "chain" : "direct"}
            onChange={(e) =>
              setChain(
                e.target.value === "direct" ? [] : [available?.id || ""],
              )
            }
          >
            <option value="direct">直接出网</option>
            <option value="chain">链式代理</option>
          </select>
        </Field>

        {chain.map((reference, index) => (
          <div key={index} className="flex items-end gap-2">
            <div className="min-w-0 flex-1">
              <Field label={`落地 ${index + 1}`}>
                <select
                  className="select"
                  value={reference}
                  onChange={(e) =>
                    setChain(
                      chain.map((id, i) => (i === index ? e.target.value : id)),
                    )
                  }
                >
                  <option value="" disabled>
                    选择已有落地节点，或点击下方导入
                  </option>
                  {reference && !choices.some((c) => c.id === reference) && (
                    <option value={reference}>当前节点不可用，请重新选择</option>
                  )}
                  {choices.map((c) => (
                    <option
                      key={c.id}
                      value={c.id}
                      disabled={chain.some((id, i) => i !== index && id === c.id)}
                    >
                      {c.name}
                    </option>
                  ))}
                </select>
              </Field>
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={`移除落地 ${index + 1}`}
              onClick={() => {
                setChain(chain.filter((_, i) => i !== index));
                setButtonChanges(true);
              }}
            >
              <X />
            </Button>
          </div>
        ))}

        {chain.length > 0 && (
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={!available || chain.length >= 8}
              onClick={() => {
                if (available) {
                  setChain([...chain, available.id]);
                  setButtonChanges(true);
                }
              }}
            >
              <Plus />
              添加下一跳
            </Button>
            {importExternal && (
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={chain.filter(Boolean).length >= 8}
                onClick={() =>
                  importExternal((e) => {
                    if (e) {
                      setChain((old) => [
                        ...old.filter(Boolean),
                        `external:${e.id}`,
                      ]);
                      setButtonChanges(true);
                    }
                  })
                }
              >
                导入落地代理
              </Button>
            )}
          </div>
        )}

        <p
          aria-label="代理路径"
          className="rounded-lg border border-border/70 bg-card px-3 py-2 text-xs leading-6 text-muted-foreground"
        >
          <span className="inline-block rounded border border-border bg-muted px-1.5 py-0.5 text-[11px] font-medium text-secondary-foreground">
            客户端
          </span>
          {" → "}
          <span className="inline-block rounded border border-blue-500/25 bg-blue-500/12 px-1.5 py-0.5 text-[11px] font-medium text-blue-600 dark:text-blue-300">
            本机
          </span>
          {chain.map((id, i) => (
            <span key={i}>
              {" → "}
              <span className="inline-block rounded border border-violet-500/25 bg-violet-500/12 px-1.5 py-0.5 text-[11px] font-medium text-violet-600 dark:text-violet-300">
                {choices.find((c) => c.id === id)?.name || "待选落地"}
              </span>
            </span>
          ))}
          {" → "}
          <span className="inline-block rounded border border-emerald-500/25 bg-emerald-500/12 px-1.5 py-0.5 text-[11px] font-medium text-emerald-600 dark:text-emerald-300">
            互联网
          </span>
        </p>
      </section>

      {!line && (
        <details className="details">
          <summary>流量额度与带宽上限（可选）</summary>
          <div className="mt-3.5 grid gap-3.5 sm:grid-cols-3">
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
        </details>
      )}

      {!line && (
        <label className="flex items-center gap-2 text-xs font-medium text-foreground">
          <input type="checkbox" name="enabled" defaultChecked />
          创建后立即启用
        </label>
      )}
    </Modal>
  );
}
