import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type FormEvent,
} from "react";
import {
  Activity,
  ArrowDownLeft,
  ArrowLeftRight,
  ArrowRight,
  ArrowUpRight,
  Bell,
  BookOpen,
  ChevronLeft,
  ChevronRight,
  Fingerprint,
  Globe2,
  LayoutDashboard,
  LogOut,
  Menu,
  MoreHorizontal,
  Network,
  Plus,
  Search,
  Server,
  Settings,
  ShieldCheck,
  Trash2,
  X,
  Zap,
  KeyRound,
  Cable,
} from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { api, ApiError } from "./api";
import { AgentSetup } from "./components/agent-setup";
import { LineEditor } from "./components/line-editor";
import { AccessDialog, QR } from "./components/access-dialog";
import { Dashboard } from "./components/dashboard";
import { SecuritySettings } from "./components/security-settings";
import {
  Modal,
  Field,
  Status,
  Empty,
  PathView,
  CopyButton,
  bytes,
  stamp,
  field,
  endpoint,
  splitAddress,
  errorText,
  protocolNames,
} from "./components/shared";
import { loginPasskey } from "./passkey";
import type {
  PanelState,
  NodeInfo,
  Line,
  Preview,
  Check as CheckResult,
} from "./types";

const pages = [
  {
    id: "overview",
    label: "总览",
    icon: LayoutDashboard,
    description: "查看网络状态、流量和需要关注的事项。",
  },
  {
    id: "lines",
    label: "代理接入",
    icon: Network,
    description: "给客户端创建代理，或通过多台服务器中转出网。",
  },
  {
    id: "forward",
    label: "端口转发",
    icon: ArrowLeftRight,
    description: "把一个入口端口转发到固定目标。",
  },
  {
    id: "websites",
    label: "网站反代",
    icon: Globe2,
    description: "通过域名访问后端网站，管理 HTTPS 与 WebSocket。",
  },
  {
    id: "nodes",
    label: "服务器与资源",
    icon: Server,
    description: "接入自己的服务器，或使用已有代理和中转入口。",
  },
  {
    id: "monitor",
    label: "监控与告警",
    icon: Activity,
    description: "查看用量、带宽上限、连接检查和操作记录。",
  },
  {
    id: "settings",
    label: "登录与安全",
    icon: Settings,
    description: "管理登录密钥、二步验证、Passkey 和订阅。",
  },
  {
    id: "help",
    label: "使用手册",
    icon: BookOpen,
    description: "从你要完成的事情开始，找到对应的操作步骤。",
  },
] as const;
type Page = (typeof pages)[number]["id"];
const pageFromHash = (): Page =>
  pages.find((p) => p.id === location.hash.slice(1))?.id || "overview";
const jobNames: Record<string, string> = {
  publishing: "正在应用",
  rolling_back: "正在恢复",
  applied: "配置已应用",
  rolled_back: "已恢复旧配置",
  degraded: "部分服务器尚未确认恢复",
};
function LineStatus({ line, state }: { line: Line; state: PanelState }) {
  if (!line.published) return <Status tone="warn">有待应用的修改</Status>;
  if (!line.enabled) return <Status>已停用</Status>;
  if (line.traffic?.blocked) return <Status tone="bad">额度用尽</Status>;
  if (
    line.hops.some((h) => !state.nodes.find((n) => n.id === h.node_id)?.online)
  )
    return <Status tone="warn">服务器离线</Status>;
  const check = state.checks[line.id];
  return (
    <Status tone={check?.ok === false ? "warn" : "good"}>
      {check?.ok === false ? "最近检查未通过" : "运行中"}
    </Status>
  );
}
function ProbeEditor({
  line,
  state,
  close,
  saved,
}: {
  line: Line;
  state: PanelState;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [mode, setMode] = useState(line.kind === "forward" ? "tcp" : "http");
  const [result, setResult] = useState<CheckResult | null>(null);
  return (
    <Modal
      title="检查连接"
      description={line.name}
      submit="开始检查"
      close={close}
      action={async (data) => {
        const r = await api<CheckResult>(`/lines/${line.id}/probe`, "POST", {
          mode,
          url: field(data, "url"),
        });
        setResult(r);
        await saved();
      }}
    >
      <PathView line={line} state={state} />
      <Field label="检查方式">
        <select
          className="select"
          value={mode}
          onChange={(e) => {
            setMode(e.target.value);
            setResult(null);
          }}
        >
          <option value="tcp">入口 TCP 连通</option>
          <option value="http">HTTP / HTTPS 响应</option>
          {line.kind === "forward" && (
            <>
              <option value="tcp_echo">TCP 回显（需要回显服务）</option>
              <option value="udp_echo">UDP 回显（需要回显服务）</option>
            </>
          )}
        </select>
      </Field>
      {mode === "http" && line.kind !== "website" && (
        <Field label="检查网址">
          <Input
            name="url"
            defaultValue={line.kind === "vless" ? "https://example.com" : ""}
            placeholder="http://目标网站/"
            required
          />
        </Field>
      )}
      <p className="muted">
        TCP 连通只说明端口可连接。HTTP
        检查会验证实际请求；回显检查需要目标运行相应回显服务。
      </p>
      {result && (
        <div
          role="status"
          className={`rounded-lg border p-4 text-sm leading-6 ${result.ok ? "border-green-200 bg-green-50 text-green-800" : "border-red-200 bg-red-50 text-red-800"}`}
        >
          <strong>{result.ok ? "检查通过" : "检查未通过"}</strong>
          <p className="mt-2 break-all">{result.detail}</p>
          {result.latency_ms != null && (
            <p className="mt-1 text-xs">
              整个检查耗时 {result.latency_ms} ms（含检查程序开销）
            </p>
          )}
        </div>
      )}
    </Modal>
  );
}
function Help() {
  const guides = [
    [
      "给客户端建代理",
      "代理接入",
      "选择入口服务器、客户端协议和监听端口，点击「保存并应用」。应用完成后点击「接入」，扫码、复制链接或下载完整 Mihomo 配置。",
      "客户端 → 你的服务器 → Internet",
    ],
    [
      "只转发一个端口",
      "端口转发",
      "选择入口服务器，分别填写监听端口、目标地址、目标端口，选择 TCP/UDP 后保存。业务客户端仍使用原来的协议和账号，连接地址换为转发入口。",
      "客户端 → A:20000 → B:3306",
    ],
    [
      "使用购买的中转端口",
      "服务器与资源",
      "若已有完整代理链接，选择「购买的中转入口」并填写原链接、服务商给的地址和端口。若它转发到自己创建的代理，在该代理编辑窗口展开同名设置。服务商处需先配置好映射。",
      "客户端 → 服务商入口:端口 → 原代理节点",
    ],
    [
      "多台服务器中转",
      "代理接入 / 端口转发",
      "先把自己的服务器接入，再在编辑窗口展开「经过其他服务器」。按入口、中转、出口顺序添加，内部隧道会自动配置。",
      "客户端 → A → B → C → 目标",
    ],
    [
      "让域名访问一个网站",
      "网站反代",
      "填写域名和后端 HTTP/HTTPS 地址。使用自动证书前，将域名解析到入口服务器并放行 TCP 80、443。需要时也可以上传自己的证书。",
      "浏览器 → https://app.example.com → http://后端:3000",
    ],
    [
      "限制用量与速度",
      "规则编辑 → 流量额度与带宽上限",
      "上传和下载上限分别填写 Mbps，所有并发连接共享上限。月额度使用 GiB，按入口上传＋下载合计，每月 1 日 UTC 重置。留空表示不限制。",
      "监控显示用量 · 限速控制持续速度 · 配额控制总量",
    ],
  ];
  return (
    <div className="space-y-5">
      <div className="rounded-xl border border-blue-100 bg-blue-50 p-5 text-sm leading-7 text-blue-900">
        <strong>服务器是运行程序的机器，线路是数据经过的路径。</strong>
        <p>
          拥有 VPS 才需要安装
          Agent。已有代理链接和购买的转发端口属于外部资源，无需提供它们的 SSH。
        </p>
      </div>
      {guides.map(([title, location, text, path], i) => (
        <Card key={title}>
          <CardHeader>
            <CardTitle className="flex items-center gap-3">
              <span className="flex size-7 items-center justify-center rounded-lg bg-blue-50 text-sm text-blue-600">
                {i + 1}
              </span>
              {title}
            </CardTitle>
            <Status>{location}</Status>
          </CardHeader>
          <CardContent>
            <p className="text-sm leading-7 text-slate-600">{text}</p>
            <p className="mono mt-4 rounded-lg bg-slate-50 p-3 text-xs text-slate-500">
              {path}
            </p>
          </CardContent>
        </Card>
      ))}
      <Card>
        <CardHeader>
          <CardTitle>连接失败时从哪里查</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 text-sm leading-7 text-slate-600">
          <p>
            先看服务器是否在线、规则是否已应用，再检查防火墙和云安全组是否放行监听端口。代理客户端需要支持相应协议，并使用最新导出的配置。
          </p>
          <p>
            REALITY 的伪装站点端口通常为
            443；它与客户端连接的监听端口是两个不同设置。普通转发不会自动穿透目标的
            NAT，目标必须能从入口服务器访问。
          </p>
          <p>
            修改共享服务器上的规则可能使其他连接短暂重连。应用失败会尝试恢复旧配置；离线服务器尚未确认恢复时，页面会明确显示。
          </p>
          <a
            className="text-blue-600 underline"
            href="https://github.com/dovetaill/vless-chain/blob/main/panel/README.md"
            target="_blank"
            rel="noreferrer"
          >
            打开完整图文手册
          </a>
        </CardContent>
      </Card>
    </div>
  );
}

export function App() {
  const [state, setState] = useState<PanelState | null>(null);
  const [loading, setLoading] = useState(true);
  const [connected, setConnected] = useState(true);
  const [page, setPage] = useState<Page>(pageFromHash);
  const [mobile, setMobile] = useState(false);
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem("chain-sidebar") === "collapsed",
  );
  const [search, setSearch] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);
  const [toast, setToast] = useState("");
  const [loginError, setLoginError] = useState("");
  const [loggingIn, setLoggingIn] = useState(false);
  const [loginNotice, setLoginNotice] = useState("");
  const [options, setOptions] = useState({ totp: false, passkey: false });
  const polling = useRef(false);
  const close = () => setModal(null);
  const refresh = useCallback(async () => {
    try {
      const data = await api<PanelState>("/state");
      setState(data);
      setConnected(true);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        setState(null);
        setModal(null);
      } else setConnected(false);
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (polling.current) return;
      polling.current = true;
      void refresh().finally(() => {
        polling.current = false;
      });
    }, 3000);
    const change = () => {
      setPage(pageFromHash());
      setSearch("");
    };
    window.addEventListener("hashchange", change);
    return () => {
      clearInterval(timer);
      window.removeEventListener("hashchange", change);
    };
  }, [refresh]);
  useEffect(() => {
    if (!state)
      void api<typeof options>("/auth/options")
        .then(setOptions)
        .catch(() => {});
  }, [Boolean(state)]);
  useEffect(() => {
    if (toast) {
      const timer = setTimeout(() => setToast(""), 5000);
      return () => clearTimeout(timer);
    }
  }, [toast]);
  const navigate = (next: string) => {
    location.hash = next;
    setMobile(false);
    setSearch("");
  };
  const run = async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (e) {
      setToast(errorText(e));
    }
  };
  const copy = async (value: string) => {
    if (navigator.clipboard && window.isSecureContext)
      await navigator.clipboard.writeText(value);
    else {
      const el = document.createElement("textarea");
      el.value = value;
      document.body.appendChild(el);
      el.select();
      document.execCommand("copy");
      el.remove();
    }
    setToast("已复制");
  };
  const sessionEnded = (message = "") => {
    setLoginNotice(message);
    setState(null);
    close();
  };
  async function apply(id?: string) {
    const preview = await api<Preview>("/preview", "POST", {});
    const otherLines = preview.affected_line_ids.filter((n) => n !== id);
    if (otherLines.length) {
      setModal(
        <Modal
          title="应用这些修改"
          description="以下线路共享受影响的服务器，应用时可能短暂重连。"
          submit="确认并应用"
          close={close}
          action={async () => {
            await api("/publish", "POST", {});
            await refresh();
            close();
            setToast("正在应用，请在当前页面查看结果。");
          }}
        >
          <div className="flex flex-wrap gap-2">
            {preview.affected_lines.map((n) => (
              <Status key={n}>{n}</Status>
            ))}
          </div>
          <p className="muted">{preview.message}</p>
        </Modal>,
      );
    } else {
      await api("/publish", "POST", {});
      await refresh();
      close();
      setToast("正在应用，请在当前页面查看结果。");
    }
  }
  function editLine(kind: Line["kind"], line?: Line) {
    if (!state) return;
    if (!state.nodes.length) {
      navigate("nodes");
      setToast("先接入一台自己的服务器，即可创建。");
      return;
    }
    setModal(
      <LineEditor
        kind={kind}
        line={line}
        state={state}
        close={close}
        saved={async (value, draft) => {
          await refresh();
          if (draft) {
            close();
            setToast("草稿已保存，可稍后应用。");
          } else await apply(value.id);
        }}
      />,
    );
  }
  function nodeEditor(node?: NodeInfo) {
    const [host, port] = splitAddress(node?.target || "www.microsoft.com:443");
    setModal(
      <Modal
        title={node ? "编辑服务器" : "接入我的服务器"}
        description="服务器是你拥有并能安装程序的机器。只需安装一次 Agent，以后在此管理。"
        submit={node ? "保存服务器" : "创建并获取接入方式"}
        close={close}
        action={async (data) => {
          const value = await api<{ id: string; token: string }>(
            node ? `/nodes/${node.id}` : "/nodes",
            "POST",
            {
              name: field(data, "name"),
              host: field(data, "host"),
              target: endpoint(
                field(data, "target_host"),
                field(data, "target_port"),
              ),
              server_name:
                field(data, "server_name") || field(data, "target_host"),
            },
          );
          await refresh();
          if (node) {
            close();
            setToast("服务器信息已保存；关联配置修改需应用后生效。");
          } else
            setModal(
              <Modal
                title="安装并接入服务器"
                description="选择复制安装命令，或使用 Web SSH 自动安装。"
                close={close}
              >
                <AgentSetup id={value.id} token={value.token} copy={copy} />
              </Modal>,
            );
        }}
      >
        <Field label="服务器名称">
          <Input
            name="name"
            defaultValue={node?.name}
            placeholder="例如：香港服务器"
            required
            maxLength={80}
          />
        </Field>
        <Field label="公网 IP 或域名">
          <Input
            name="host"
            defaultValue={node?.host}
            placeholder="例如：192.0.2.10"
            required
          />
        </Field>
        <details className="details">
          <summary>高级：REALITY 伪装站点</summary>
          <p className="muted mt-3">
            代理和多服务器中转使用此默认设置。普通单服务器端口转发无需填写。
          </p>
          <div className="mt-4 grid gap-4 sm:grid-cols-[1fr_130px]">
            <Field label="目标域名">
              <Input name="target_host" defaultValue={host} required />
            </Field>
            <Field label="目标端口">
              <Input
                name="target_port"
                type="number"
                min={1}
                max={65535}
                defaultValue={port}
                required
              />
            </Field>
          </div>
          <div className="mt-4">
            <Field label="SNI" hint="留空时使用目标域名。">
              <Input
                name="server_name"
                defaultValue={node?.server_name}
                placeholder="自动使用目标域名"
              />
            </Field>
          </div>
        </details>
      </Modal>,
    );
  }
  function importExternal(relay = false) {
    setModal(
      <Modal
        title={relay ? "使用购买的中转入口" : "导入已有代理"}
        description={
          relay
            ? "先在服务商处配置好转发目标，再用这里生成经过中转的新链接。"
            : "粘贴完整链接，可作为线路的外部出口，无需 SSH 或安装 Agent。"
        }
        submit="导入"
        close={close}
        action={async (data) => {
          const value = await api<{ url: string }>("/externals", "POST", {
            name: field(data, "name"),
            url: field(data, "url"),
            relay_host: field(data, "relay_host"),
            relay_port: field(data, "relay_port")
              ? Number(field(data, "relay_port"))
              : null,
          });
          await refresh();
          setModal(
            <Modal
              title="代理已导入"
              description="可以复制到客户端，也可以在创建线路时选择它作为外部出口。"
              close={close}
            >
              <QR value={value.url} />
              <textarea
                className="textarea mono"
                readOnly
                rows={4}
                value={value.url}
              />
              <CopyButton value={value.url} label="复制代理链接" />
            </Modal>,
          );
        }}
      >
        <Field label="资源名称">
          <Input
            name="name"
            required
            maxLength={80}
            placeholder={relay ? "购买的中转线路" : "已有代理出口"}
          />
        </Field>
        <Field
          label={relay ? "原节点完整分享链接" : "完整分享链接"}
          hint="支持 VLESS、HTTP(S)、SOCKS5、Shadowsocks、Trojan。"
        >
          <textarea
            className="textarea mono"
            name="url"
            rows={4}
            placeholder="vless://… 或 socks5://用户名:密码@地址:端口"
            required
          />
        </Field>
        {relay && (
          <div className="grid gap-4 sm:grid-cols-[1fr_130px]">
            <Field label="中转入口地址">
              <Input
                name="relay_host"
                placeholder="relay.example.com"
                required
              />
            </Field>
            <Field label="中转入口端口">
              <Input
                name="relay_port"
                type="number"
                min={1}
                max={65535}
                required
              />
            </Field>
          </div>
        )}
      </Modal>,
    );
  }
  async function login(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (loggingIn) return;
    const data = new FormData(event.currentTarget);
    setLoggingIn(true);
    setLoginError("");
    try {
      await api("/login", "POST", {
        token: field(data, "token"),
        code: field(data, "code"),
      });
      setLoginNotice("");
      await refresh();
    } catch (e) {
      setLoginError(errorText(e));
    } finally {
      setLoggingIn(false);
    }
  }
  if (loading)
    return (
      <div className="flex min-h-screen items-center justify-center text-sm text-slate-500">
        正在连接控制台…
      </div>
    );
  if (!state)
    return (
      <div className="login-layout">
        <section className="login-brand">
          <div className="flex items-center gap-3">
            <span className="brand-symbol">
              <Network />
            </span>
            <span className="text-xl font-semibold tracking-tight">
              Chain Panel
            </span>
          </div>
          <div className="my-auto py-16">
            <span className="rounded-full border border-white/20 bg-white/5 px-3 py-1.5 text-xs tracking-wider text-blue-100">
              PERSONAL NETWORK CONSOLE
            </span>
            <h1 className="mt-7 max-w-md text-4xl font-semibold leading-tight tracking-tight">
              你的服务器。
              <br />
              你的连接方式。
            </h1>
            <p className="mt-6 max-w-sm text-sm leading-8 text-blue-100">
              在一个控制台里管理代理、端口转发和网站访问，让复杂网络回到清楚的日常操作。
            </p>
            <div className="mt-10 space-y-4 text-sm text-blue-100">
              {[
                [Network, "多种代理协议，快速连接客户端"],
                [ArrowLeftRight, "简单端口转发，按需增加中转"],
                [ShieldCheck, "流量控制与安全登录"],
              ].map(([Icon, text], i) => {
                const I = Icon as typeof Network;
                return (
                  <p key={i} className="flex items-center gap-3">
                    <I className="size-4 text-blue-300" />
                    {String(text)}
                  </p>
                );
              })}
            </div>
          </div>
          <p className="text-xs text-blue-200/60">
            Chain Panel · 个人多服务器控制台
          </p>
        </section>
        <main className="flex min-h-screen flex-1 items-center justify-center p-6 sm:p-12">
          <div className="w-full max-w-sm">
            <div className="mb-8 flex items-center gap-3 lg:hidden">
              <span className="brand-symbol">
                <Network />
              </span>
              <strong>Chain Panel</strong>
            </div>
            <p className="text-xs font-semibold uppercase tracking-widest text-blue-600">
              WELCOME BACK
            </p>
            <h2 className="mt-3 text-2xl font-semibold tracking-tight">
              登录控制台
            </h2>
            <p className="mt-3 text-sm leading-6 text-slate-500">
              使用安装时生成的管理员密钥。
            </p>
            {loginNotice && (
              <div className="my-5 space-y-3 rounded-lg border border-amber-200 bg-amber-50 p-4">
                <pre className="whitespace-pre-wrap break-all text-xs leading-6">
                  {loginNotice}
                </pre>
                <CopyButton
                  value={loginNotice.split("\n").at(-1) || ""}
                  label="复制新密钥"
                />
              </div>
            )}
            <form onSubmit={login} className="mt-8 space-y-5">
              <Field label="管理员密钥">
                <Input
                  name="token"
                  type="password"
                  autoComplete="current-password"
                  placeholder="输入管理员密钥"
                  required
                />
              </Field>
              {options.totp && (
                <Field label="动态验证码或恢复码">
                  <Input
                    name="code"
                    autoComplete="one-time-code"
                    placeholder="6 位验证码或一次性恢复码"
                    required
                  />
                </Field>
              )}
              {loginError && (
                <p
                  role="alert"
                  className="rounded-lg bg-red-50 p-3 text-sm text-red-700"
                >
                  {loginError}
                </p>
              )}
              <Button type="submit" className="w-full" disabled={loggingIn}>
                {loggingIn ? "正在验证…" : "登录"}
                <ArrowRight />
              </Button>
            </form>
            {options.passkey && (
              <Button
                className="mt-3 w-full"
                variant="outline"
                disabled={loggingIn || !window.isSecureContext}
                onClick={() =>
                  void run(async () => {
                    setLoggingIn(true);
                    try {
                      await loginPasskey();
                      await refresh();
                    } catch (e) {
                      setLoginError(errorText(e));
                    } finally {
                      setLoggingIn(false);
                    }
                  })
                }
              >
                <Fingerprint />
                使用 Passkey 登录
              </Button>
            )}
            <p className="mt-8 border-t border-border pt-6 text-xs leading-6 text-slate-400">
              找不到密钥？在控制台所在服务器输入 <code>chainctl</code>
              ，选择查看登录信息。
            </p>
            {!connected && (
              <p className="mt-3 text-sm text-red-600">
                暂时无法连接控制台，请稍后重试。
              </p>
            )}
          </div>
        </main>
      </div>
    );
  const current = pages.find((p) => p.id === page)!;
  const kind: Line["kind"] =
    page === "forward" ? "forward" : page === "websites" ? "website" : "vless";
  const lines = state.lines.filter(
    (l) =>
      l.kind === kind && l.name.toLowerCase().includes(search.toLowerCase()),
  );
  const latest = state.jobs[0];
  return (
    <div className={collapsed ? "app-shell is-collapsed" : "app-shell"}>
      {mobile && (
        <button
          aria-label="关闭导航遮罩"
          className="fixed inset-0 z-40 bg-slate-950/40 lg:hidden"
          onClick={() => setMobile(false)}
        />
      )}
      <aside className={`app-sidebar ${mobile ? "mobile-open" : ""}`}>
        <div className="sidebar-brand">
          <span className="brand-symbol">
            <Network />
          </span>
          <span className="sidebar-label text-lg font-semibold tracking-tight">
            Chain Panel
          </span>
          <button
            className="ms-auto text-slate-400 lg:hidden"
            aria-label="关闭导航"
            onClick={() => setMobile(false)}
          >
            <X className="size-5" />
          </button>
        </div>
        <div className="sidebar-workspace">
          <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-blue-100 text-xs font-semibold text-blue-600">
            CP
          </span>
          <div className="sidebar-label">
            <p className="text-xs font-semibold">我的工作空间</p>
            <p className="mt-1 text-[10px] text-slate-400">个人网络控制台</p>
          </div>
        </div>
        <p className="sidebar-label px-6 pb-2 pt-6 text-[10px] font-semibold tracking-widest text-slate-400">
          工作空间
        </p>
        <nav className="flex-1 space-y-1 px-3 py-2">
          {pages.map((p, i) => (
            <button
              key={p.id}
                aria-label={p.label}
              title={collapsed ? p.label : undefined}
              className={`nav-item ${page === p.id ? "active" : ""} ${i === 6 ? "mt-7!" : ""}`}
              onClick={() => navigate(p.id)}
            >
              <p.icon className="size-[18px] shrink-0" />
              <span className="sidebar-label">{p.label}</span>
              {p.id === "monitor" && state.alerts.length > 0 && (
                <span className="sidebar-label ms-auto rounded bg-amber-100 px-1.5 py-0.5 text-[10px] text-amber-800">
                  {state.alerts.length}
                </span>
              )}
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          <div className="sidebar-label mb-4 rounded-lg border border-border bg-slate-50 p-3">
            <p className="flex items-center gap-2 text-xs font-medium">
              <ShieldCheck className="size-4 text-emerald-600" />
              私人控制台
            </p>
            <p className="mt-2 text-[11px] leading-5 text-slate-400">
              仅管理已接入的服务器与规则。
            </p>
          </div>
          <button
            className="nav-item w-full"
            onClick={() =>
              void run(async () => {
                await api("/logout", "POST", {});
                sessionEnded();
              })
            }
          >
            <LogOut className="size-[18px] shrink-0" />
            <span className="sidebar-label">退出登录</span>
          </button>
        </div>
        <button
          className="sidebar-toggle hidden lg:flex"
          aria-label={collapsed ? "展开侧栏" : "收起侧栏"}
          onClick={() =>
            setCollapsed((old) => {
              localStorage.setItem(
                "chain-sidebar",
                old ? "expanded" : "collapsed",
              );
              return !old;
            })
          }
        >
          {collapsed ? (
            <ChevronRight className="size-3.5" />
          ) : (
            <ChevronLeft className="size-3.5" />
          )}
        </button>
      </aside>
      <div className="app-main">
        <header className="app-header">
          <div className="flex items-center gap-3">
            <Button
              size="icon"
              variant="ghost"
              className="lg:hidden"
              aria-label="打开导航"
              onClick={() => setMobile(true)}
            >
              <Menu />
            </Button>
            <span className="text-xs text-slate-400">工作空间</span>
            <ChevronRight className="size-3 text-slate-300" />
            <span className="text-xs font-medium">{current.label}</span>
          </div>
          <div className="flex items-center gap-4">
            <span className="hidden items-center gap-2 text-xs text-slate-400 sm:flex">
              <span
                className={`size-1.5 rounded-full ${connected ? "bg-emerald-500" : "bg-red-500"}`}
              />
              {connected ? "实时同步" : "连接中断"}
            </span>
            <button
              className="relative text-slate-500"
              aria-label="查看告警"
              onClick={() => navigate("monitor")}
            >
              <Bell className="size-[18px]" />
              {state.alerts.length > 0 && (
                <span className="absolute -right-1 -top-1 size-1.5 rounded-full bg-amber-500" />
              )}
            </button>
            <span className="flex size-8 items-center justify-center rounded-full border border-blue-100 bg-blue-50 text-[11px] font-semibold text-blue-700">
              管理
            </span>
          </div>
        </header>
        <main className="app-content">
          <div className="mb-7 flex flex-wrap items-end justify-between gap-4">
            <div>
              <h1 className="text-2xl font-semibold tracking-tight text-slate-900">
                {current.label}
              </h1>
              <p className="mt-2 text-sm leading-6 text-slate-500">
                {current.description}
              </p>
            </div>
            {["lines", "forward", "websites"].includes(page) && (
              <Button disabled={state.busy} onClick={() => editLine(kind)}>
                <Plus />
                {kind === "vless"
                  ? "创建代理"
                  : kind === "forward"
                    ? "创建转发"
                    : "添加网站"}
              </Button>
            )}
          </div>
          {!connected && (
            <div
              role="alert"
              className="mb-5 rounded-lg bg-red-50 p-4 text-sm text-red-700"
            >
              连接已中断，当前展示最后一次收到的数据。
            </div>
          )}
          {latest && (state.busy || latest.state !== "applied") && (
            <div
              role="status"
              className={`mb-5 rounded-lg border p-4 text-sm ${state.busy ? "border-blue-100 bg-blue-50 text-blue-800" : "border-amber-100 bg-amber-50 text-amber-800"}`}
            >
              <strong>{jobNames[latest.state] || latest.state}</strong>
              <p className="mt-1 text-xs leading-6">
                {latest.events.at(-1)?.message}
              </p>
            </div>
          )}
          {page === "overview" && (
            <Dashboard state={state} create={editLine} navigate={navigate} />
          )}
          {["lines", "forward", "websites"].includes(page) && (
            <Card>
              <CardHeader>
                <CardTitle>
                  {kind === "vless"
                    ? "我的代理"
                    : kind === "forward"
                      ? "转发规则"
                      : "我的网站"}{" "}
                  <span className="ms-2 text-sm font-normal text-slate-400">
                    {lines.length}
                  </span>
                </CardTitle>
                <div className="relative w-48 max-w-[50%]">
                  <Search className="absolute left-3 top-2.5 size-4 text-slate-400" />
                  <Input
                    className="pl-9"
                    aria-label="搜索规则"
                    placeholder="搜索名称…"
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                  />
                </div>
              </CardHeader>
              {lines.length ? (
                <div className="overflow-x-auto">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>名称 / 路径</th>
                        <th>类型 / 入口</th>
                        <th>状态</th>
                        <th>本月流量</th>
                        <th className="text-right">操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {lines.map((l) => (
                        <tr key={l.id}>
                          <td>
                            <p className="mb-2 text-sm font-semibold">
                              {l.name}
                            </p>
                            <PathView line={l} state={state} />
                          </td>
                          <td>
                            <p className="text-xs font-medium">
                              {l.kind === "vless"
                                ? protocolNames[l.access.protocol]
                                : l.kind === "website"
                                  ? "网站反代"
                                  : l.network.toUpperCase()}
                            </p>
                            <p className="mono mt-2 text-xs text-slate-400">
                              端口 {l.hops[0].port}
                            </p>
                          </td>
                          <td>
                            <LineStatus line={l} state={state} />
                          </td>
                          <td>
                            <p className="text-xs">
                              {l.traffic
                                ? bytes(l.traffic.upload + l.traffic.download)
                                : "尚未采集"}
                            </p>
                            <p className="mt-2 text-[11px] text-slate-400">
                              {l.limits.monthly_gib
                                ? `额度 ${l.limits.monthly_gib} GiB`
                                : "不限量"}
                            </p>
                          </td>
                          <td>
                            <div className="flex items-center justify-end gap-1">
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={
                                  !l.published || !l.enabled || state.busy
                                }
                                onClick={() =>
                                  setModal(
                                    <AccessDialog
                                      line={l}
                                      state={state}
                                      close={close}
                                    />,
                                  )
                                }
                              >
                                {l.kind === "website" ? "访问" : "接入"}
                              </Button>
                              <Button
                                variant="ghost"
                                size="sm"
                                disabled={state.busy}
                                onClick={() => editLine(l.kind, l)}
                              >
                                编辑
                              </Button>
                              <DropdownMenu.Root>
                                <DropdownMenu.Trigger
                                  className="row-menu-trigger"
                                  aria-label={`${l.name} 更多操作`}
                                >
                                  <MoreHorizontal className="size-4" />
                                </DropdownMenu.Trigger>
                                <DropdownMenu.Portal>
                                  <DropdownMenu.Content
                                    className="row-menu-content"
                                    align="end"
                                    sideOffset={4}
                                    collisionPadding={8}
                                  >
                                    <DropdownMenu.Item
                                      disabled={
                                        !l.published || !l.enabled || state.busy
                                      }
                                      onSelect={() =>
                                        setModal(
                                          <ProbeEditor
                                            line={l}
                                            state={state}
                                            close={close}
                                            saved={refresh}
                                          />,
                                        )
                                      }
                                    >
                                      检查连接
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Item
                                      disabled={state.busy}
                                      onSelect={() => void run(() => apply(l.id))}
                                    >
                                      应用修改
                                    </DropdownMenu.Item>
                                    <DropdownMenu.Item
                                      disabled={state.busy}
                                      className="text-red-600"
                                      onSelect={() =>
                                        setModal(
                                          <Modal
                                            title="删除规则"
                                            description={`删除「${l.name}」并应用配置后，该入口停止服务。`}
                                            submit="删除并应用"
                                            close={close}
                                            action={async () => {
                                              await api(
                                                `/lines/${l.id}`,
                                                "DELETE",
                                              );
                                              await refresh();
                                              await apply(l.id);
                                            }}
                                          />,
                                        )
                                      }
                                    >
                                      删除规则
                                    </DropdownMenu.Item>
                                  </DropdownMenu.Content>
                                </DropdownMenu.Portal>
                              </DropdownMenu.Root>
                            </div>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <Empty
                  title={
                    search
                      ? "没有找到匹配的规则"
                      : kind === "forward"
                        ? "从一个端口开始"
                        : kind === "website"
                          ? "让网站拥有自己的访问入口"
                          : "创建第一条代理"
                  }
                  action={
                    !search && (
                      <Button onClick={() => editLine(kind)}>
                        <Plus />
                        开始创建
                      </Button>
                    )
                  }
                >
                  {kind === "forward"
                    ? "选择服务器和目标地址，无需填写 VLESS、UUID 或 REALITY 参数。"
                    : kind === "website"
                      ? "填写域名和后端地址，配置 HTTPS 后即可访问。"
                      : "选择入口服务器和协议，其余复杂设置按需展开。"}
                </Empty>
              )}
              <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border px-5 py-4 text-xs text-slate-400">
                <span>保存并应用后，即可使用入口。</span>
                {state.lines.some((l) => !l.published) && (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={state.busy}
                    onClick={() => void run(() => apply())}
                  >
                    <Zap />
                    应用待处理修改
                  </Button>
                )}
              </div>
            </Card>
          )}
          {page === "nodes" && (
            <div className="space-y-6">
              <div className="grid gap-4 md:grid-cols-3">
                {[
                  {
                    icon: Server,
                    title: "我的服务器",
                    text: "拥有 VPS，安装 Agent 后统一管理。",
                    action: () => nodeEditor(),
                  },
                  {
                    icon: Network,
                    title: "已有代理",
                    text: "导入完整链接，无需 SSH。",
                    action: () => importExternal(),
                  },
                  {
                    icon: Cable,
                    title: "购买的中转入口",
                    text: "把服务商的端口用于已有节点。",
                    action: () => importExternal(true),
                  },
                ].map((r) => (
                  <button
                    key={r.title}
                    className="quick-task"
                    onClick={r.action}
                  >
                    <r.icon className="size-6 shrink-0 text-blue-600" />
                    <span className="text-left">
                      <strong className="text-sm">{r.title}</strong>
                      <span className="mt-1 block text-xs leading-5 text-slate-500">
                        {r.text}
                      </span>
                    </span>
                    <Plus className="ms-auto size-4 text-slate-300" />
                  </button>
                ))}
              </div>
              <div className="grid gap-5 xl:grid-cols-2">
                {state.nodes.map((n) => (
                  <Card key={n.id}>
                    <CardHeader>
                      <CardTitle className="flex items-center gap-2">
                        <Server className="size-4 text-blue-600" />
                        {n.name}
                      </CardTitle>
                      <Status tone={n.online ? "good" : "warn"}>
                        {n.online ? "在线" : "未连接"}
                      </Status>
                    </CardHeader>
                    <CardContent className="space-y-4">
                      <p className="mono text-sm text-slate-600">{n.host}</p>
                      <div className="grid grid-cols-2 gap-4 rounded-lg bg-slate-50 p-3 text-xs">
                        <div>
                          <p className="text-slate-400">服务状态</p>
                          <p className="mt-1.5">
                            {n.report.running ? "运行中" : "尚未运行"}
                          </p>
                        </div>
                        <div>
                          <p className="text-slate-400">最后心跳</p>
                          <p className="mt-1.5">{stamp(n.seen)}</p>
                        </div>
                      </div>
                      {n.report.error && (
                        <p className="text-xs leading-6 text-red-600">
                          {n.report.error}
                        </p>
                      )}
                      <div className="flex flex-wrap gap-2">
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={state.busy}
                          onClick={() => nodeEditor(n)}
                        >
                          编辑信息
                        </Button>
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={state.busy}
                          onClick={() =>
                            setModal(
                              <Modal
                                title="重新接入服务器"
                                description="生成新接入码后，旧凭据失效。请在目标服务器重新接入。"
                                submit="生成新接入码"
                                close={close}
                                action={async () => {
                                  const result = await api<{ token: string }>(
                                    `/nodes/${n.id}/token`,
                                    "POST",
                                    {},
                                  );
                                  setModal(
                                    <Modal
                                      title="重新接入服务器"
                                      description={n.name}
                                      close={close}
                                    >
                                      <AgentSetup
                                        id={n.id}
                                        token={result.token}
                                        copy={copy}
                                      />
                                    </Modal>,
                                  );
                                }}
                              />,
                            )
                          }
                        >
                          重新接入
                        </Button>
                        <Button
                          size="icon"
                          variant="ghost"
                          aria-label={`删除服务器 ${n.name}`}
                          disabled={state.busy}
                          onClick={() =>
                            setModal(
                              <Modal
                                title="删除服务器"
                                description={`移除「${n.name}」的管理记录。关联规则需先删除并应用。`}
                                submit="删除服务器"
                                close={close}
                                action={async () => {
                                  await api(`/nodes/${n.id}`, "DELETE");
                                  await refresh();
                                  close();
                                }}
                              />,
                            )
                          }
                        >
                          <Trash2 />
                        </Button>
                      </div>
                    </CardContent>
                  </Card>
                ))}
              </div>
              <Card>
                <CardHeader>
                  <CardTitle>外部代理资源</CardTitle>
                </CardHeader>
                <CardContent>
                  {state.externals.length ? (
                    state.externals.map((e) => (
                      <div
                        key={e.id}
                        className="flex flex-wrap items-center justify-between gap-3 border-b border-border py-4 last:border-0"
                      >
                        <div>
                          <p className="text-sm font-medium">{e.name}</p>
                          <p className="mt-1 text-xs text-slate-400">
                            {e.url.split(":")[0].toUpperCase()} · 由提供方管理
                          </p>
                        </div>
                        <div className="flex gap-2">
                          <CopyButton value={e.url} label="复制链接" />
                          <Button
                            variant="ghost"
                            size="icon"
                            aria-label={`删除资源 ${e.name}`}
                            onClick={() =>
                              setModal(
                                <Modal
                                  title="删除外部资源"
                                  description={`移除「${e.name}」的连接信息。`}
                                  submit="删除"
                                  close={close}
                                  action={async () => {
                                    await api(`/externals/${e.id}`, "DELETE");
                                    await refresh();
                                    close();
                                  }}
                                />,
                              )
                            }
                          >
                            <Trash2 />
                          </Button>
                        </div>
                      </div>
                    ))
                  ) : (
                    <p className="muted py-8 text-center">
                      已有代理链接可以作为出口导入，或直接复制到客户端。
                    </p>
                  )}
                </CardContent>
              </Card>
            </div>
          )}
          {page === "monitor" && (
            <div className="space-y-6">
              <Card>
                <CardHeader>
                  <CardTitle>当前告警</CardTitle>
                  <Status tone={state.alerts.length ? "warn" : "good"}>
                    {state.alerts.length || "正常"}
                  </Status>
                </CardHeader>
                <CardContent className="space-y-3">
                  {state.alerts.length ? (
                    state.alerts.map((a) => (
                      <div
                        key={a.id}
                        className={`rounded-lg p-3 text-sm leading-6 ${a.level === "error" ? "bg-red-50 text-red-800" : "bg-amber-50 text-amber-800"}`}
                      >
                        {a.message}
                      </div>
                    ))
                  ) : (
                    <p className="muted">当前没有需要处理的异常。</p>
                  )}
                </CardContent>
              </Card>
              <Card>
                <CardHeader>
                  <CardTitle>线路用量与限制</CardTitle>
                  <span className="text-xs text-slate-400">
                    入口计量 · UTC 月周期
                  </span>
                </CardHeader>
                <div className="overflow-x-auto">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>线路</th>
                        <th>当前上传 / 下载</th>
                        <th>本月用量 / 额度</th>
                        <th>带宽上限</th>
                        <th>操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {state.lines.map((l) => {
                        const live = state.nodes.find(
                          (n) => n.id === l.hops[0]?.node_id,
                        )?.online;
                        return (
                          <tr key={l.id}>
                            <td>
                              <p className="text-sm font-medium">{l.name}</p>
                              <p className="mt-2">
                                <LineStatus line={l} state={state} />
                              </p>
                            </td>
                            <td>
                              {l.traffic && live ? (
                                <div className="space-y-2 text-xs">
                                  <p className="flex items-center gap-1">
                                    <ArrowUpRight className="size-3 text-violet-500" />
                                    {bytes(l.traffic.upload_bps)}/s
                                  </p>
                                  <p className="flex items-center gap-1">
                                    <ArrowDownLeft className="size-3 text-blue-500" />
                                    {bytes(l.traffic.download_bps)}/s
                                  </p>
                                </div>
                              ) : (
                                <span className="text-xs text-slate-400">
                                  {live ? "尚未采集" : "服务器离线"}
                                </span>
                              )}
                            </td>
                            <td>
                              <p className="text-xs">
                                {l.traffic
                                  ? bytes(l.traffic.upload + l.traffic.download)
                                  : "尚未采集"}{" "}
                                /{" "}
                                {l.limits.monthly_gib
                                  ? `${l.limits.monthly_gib} GiB`
                                  : "不限量"}
                              </p>
                              {l.limits.monthly_gib && l.traffic && (
                                <div className="mt-2 h-1.5 w-32 overflow-hidden rounded-full bg-slate-100">
                                  <div
                                    className="h-full rounded-full bg-blue-500"
                                    style={{
                                      width: `${Math.min(100, ((l.traffic.upload + l.traffic.download) / (l.limits.monthly_gib * 1073741824)) * 100)}%`,
                                    }}
                                  />
                                </div>
                              )}
                            </td>
                            <td className="text-xs leading-6">
                              <p>
                                上传{" "}
                                {l.limits.upload_mbps
                                  ? `${l.limits.upload_mbps} Mbps`
                                  : "不限速"}
                              </p>
                              <p>
                                下载{" "}
                                {l.limits.download_mbps
                                  ? `${l.limits.download_mbps} Mbps`
                                  : "不限速"}
                              </p>
                              <p className="text-slate-400">
                                {l.published ? "策略已应用" : "策略待应用"}
                              </p>
                            </td>
                            <td>
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={state.busy}
                                onClick={() => editLine(l.kind, l)}
                              >
                                调整限制
                              </Button>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </Card>
              <Card>
                <CardHeader>
                  <CardTitle>连接检查</CardTitle>
                </CardHeader>
                <CardContent className="space-y-4">
                  {state.lines.map((l) => (
                    <div
                      key={l.id}
                      className="flex items-center justify-between gap-4 border-b border-border pb-4 last:border-0"
                    >
                      <div>
                        <p className="text-sm font-medium">{l.name}</p>
                        <p className="muted mt-1">
                          {state.checks[l.id]
                            ? `${state.checks[l.id].ok ? "通过" : "未通过"} · ${stamp(state.checks[l.id].checked_at)}`
                            : "尚未检查"}
                        </p>
                      </div>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={!l.published || state.busy || !l.enabled}
                        onClick={() =>
                          setModal(
                            <ProbeEditor
                              line={l}
                              state={state}
                              close={close}
                              saved={refresh}
                            />,
                          )
                        }
                      >
                        检查连接
                      </Button>
                    </div>
                  ))}
                </CardContent>
              </Card>
              <Card>
                <CardHeader>
                  <CardTitle>最近操作</CardTitle>
                </CardHeader>
                <CardContent>
                  {state.jobs.length ? (
                    state.jobs.map((j) => (
                      <details
                        key={j.id}
                        className="border-b border-border py-4 last:border-0"
                      >
                        <summary className="flex cursor-pointer items-center justify-between gap-3 text-sm">
                          <span>{jobNames[j.state] || j.state}</span>
                          <span className="text-xs text-slate-400">
                            {stamp(j.started)}
                          </span>
                        </summary>
                        <div className="mt-3 space-y-2 border-l-2 border-blue-100 pl-4">
                          {j.events.map((e, i) => (
                            <p
                              key={i}
                              className="text-xs leading-6 text-slate-500"
                            >
                              {stamp(e.at)} · {e.message}
                            </p>
                          ))}
                        </div>
                      </details>
                    ))
                  ) : (
                    <p className="muted py-6">应用配置后，这里会记录结果。</p>
                  )}
                </CardContent>
              </Card>
            </div>
          )}
          {page === "settings" && (
            <SecuritySettings
              state={state}
              saved={refresh}
              sessionEnded={sessionEnded}
            />
          )}
          {page === "help" && <Help />}
          <footer className="mt-10 flex items-center justify-between border-t border-border pt-5 text-[11px] text-slate-400">
            <span>Chain Panel · 个人网络控制台</span>
            <button onClick={() => navigate("help")}>使用手册</button>
          </footer>
        </main>
      </div>
      {modal}
      {toast && (
        <div
          role="status"
          className="fixed bottom-6 left-1/2 z-[100] max-w-[90%] -translate-x-1/2 rounded-xl bg-slate-900 px-5 py-3 text-sm text-white shadow-xl"
        >
          {toast}
        </div>
      )}
    </div>
  );
}
