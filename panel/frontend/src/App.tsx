import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type FormEvent,
} from "react";
import {
  ArrowLeftRight,
  ArrowRight,
  Bell,
  ChevronLeft,
  ChevronRight,
  Fingerprint,
  LogOut,
  Menu,
  MoreHorizontal,
  Network,
  Plus,
  Search,
  Server,
  Settings,
  Trash2,
  X,
  Zap,
  KeyRound,
} from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { api, ApiError } from "./api";
import { AgentSetup } from "./components/agent-setup";
import { LineEditor } from "./components/line-editor";
import { AccessDialog, QR } from "./components/access-dialog";
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
  External,
  Line,
  Preview,
  Check as CheckResult,
} from "./types";

const pages = [
  { id: "lines", label: "代理", icon: Network },
  { id: "forward", label: "端口转发", icon: ArrowLeftRight },
  { id: "nodes", label: "服务器", icon: Server },
  { id: "settings", label: "设置", icon: Settings },
] as const;
type Page = (typeof pages)[number]["id"];
const pageFromHash = (): Page =>
  pages.find((p) => p.id === location.hash.slice(1))?.id || "lines";
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
  const [editor, setEditor] = useState<{ kind: Line["kind"]; line?: Line; initialNodeId?: string } | null>(null);
  const [toast, setToast] = useState("");
  const [pendingShare, setPendingShare] = useState<string | null>(null);
  const [pendingLocal, setPendingLocal] = useState<{ id: string; done: (node?: NodeInfo) => void } | null>(null);
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
        setEditor(null);
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
  useEffect(() => {
    if (!pendingShare || !state || state.busy) return;
    const line = state.lines.find((item) => item.id === pendingShare);
    if (state.jobs[0]?.state === "applied" && line?.published && line.enabled) {
      setPendingShare(null);
      setModal(<AccessDialog line={line} state={state} close={() => setModal(null)} />);
    } else if (["rolled_back", "degraded"].includes(state.jobs[0]?.state)) {
      setPendingShare(null);
    }
  }, [pendingShare, state]);
  useEffect(() => {
    if (!pendingLocal || !state) return;
    const node = state.nodes.find((item) => item.id === pendingLocal.id);
    if (node?.online) {
      setPendingLocal(null);
      setModal(null);
      pendingLocal.done(node);
    }
  }, [pendingLocal, state]);
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
    setEditor(null);
    setPendingShare(null);
    setPendingLocal(null);
    close();
  };
  async function apply(id?: string, share = false) {
    const preview = await api<Preview>("/preview", "POST", {});
    const otherLines = preview.affected_line_ids.filter((n) => n !== id);
    if (otherLines.length) {
      setModal(
        <Modal
          title="应用这些修改"
          description="以下代理或转发共用服务器，应用时可能短暂重连。"
          submit="确认并应用"
          close={close}
          action={async () => {
            await api("/publish", "POST", {});
            await refresh();
            if (share && id) setPendingShare(id);
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
      if (share && id) setPendingShare(id);
      close();
      setToast("正在应用，请在当前页面查看结果。");
    }
  }
  function editLine(kind: Line["kind"], line?: Line, initialNodeId?: string) {
    if (!state) return;
    setEditor({ kind, line, initialNodeId });
  }
  function addLocalServer(done: (node?: NodeInfo) => void) {
    setModal(<Modal title="设置本机" submit="启用本机" close={() => { close(); done(); }} action={async (data) => {
      const value = await api<{ id: string }>("/local-node", "POST", { host: field(data, "host") });
      setPendingLocal({ id: value.id, done });
      setModal(<Modal title="正在连接本机" close={() => { setPendingLocal(null); close(); done(); }}>
        <p role="status">等待本机上线…</p>
        <Button type="button" variant="outline" onClick={() => void refresh()}>刷新状态</Button>
      </Modal>);
      await refresh();
    }}>
      <Field label="本机公网 IP 或域名">
        <Input name="host" placeholder="例如：203.0.113.10" required />
      </Field>
    </Modal>);
  }
  function nodeEditor(node?: NodeInfo, done?: (node?: NodeInfo) => void) {
    const [host, port] = splitAddress(node?.target || "www.microsoft.com:443");
    setModal(
      <Modal
        title={node ? "编辑服务器" : "添加远程服务器"}

        submit={node ? "保存" : "下一步：SSH 安装"}
        close={() => { close(); done?.(); }}
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

                close={() => { close(); done?.(); }}
              >
                <AgentSetup id={value.id} token={value.token} host={field(data, "host")} copy={copy}
                  onOnline={done ? async (node) => { await refresh(); close(); done(node); } : undefined} />
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
  function importExternal(relay = false, done?: (external?: External) => void) {
    setModal(
      <Modal
        title={relay ? "使用购买的中转入口" : "导入已有代理"}
        description={
          relay
            ? "先在服务商处配置好转发目标，再用这里生成经过中转的新链接。"
            : "粘贴已有代理的分享链接。"
        }
        submit="导入"
        close={() => { close(); done?.(); }}
        action={async (data) => {
          const value = await api<External>("/externals", "POST", {
            name: field(data, "name"),
            url: field(data, "url"),
            relay_host: field(data, "relay_host"),
            relay_port: field(data, "relay_port")
              ? Number(field(data, "relay_port"))
              : null,
          });
          await refresh();
          if (done) { close(); done(value); return; }
          setModal(
            <Modal
              title="代理已导入"
              description="可用于客户端，也可选作代理出口。"
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
        <Field label="名称">
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
          <h1 className="my-auto py-16 text-4xl font-semibold">代理与中转</h1>
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
    page === "forward" ? "forward" : "vless";
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
        <nav className="flex-1 space-y-1 px-3 py-2">
          {pages.map((p) => (
            <button
              key={p.id}
                aria-label={p.label}
              title={collapsed ? p.label : undefined}
              className={`nav-item ${page === p.id ? "active" : ""} `}
              onClick={() => navigate(p.id)}
            >
              <p.icon className="size-[18px] shrink-0" />
              <span className="sidebar-label">{p.label}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
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
              onClick={() => setModal(<Modal title="运行状态" close={close}>
                {state.alerts.map((alert) => <p key={alert.id} role="alert">{alert.message}</p>)}
                {!state.alerts.length && <p>暂无告警</p>}
                {state.jobs.map((job) => <details key={job.id}>
                  <summary>{jobNames[job.state] || job.state} · {stamp(job.started)}</summary>
                  {job.events.map((event, i) => <p key={i} className="muted">{event.message}</p>)}
                </details>)}
              </Modal>)}
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
            </div>
            {["lines", "forward"].includes(page) && (
              <Button disabled={state.busy} onClick={() => editLine(kind)}>
                <Plus />
                {kind === "vless" ? "创建代理" : "创建转发"}
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
          {["lines", "forward"].includes(page) && (
            <Card>
              <CardHeader>
                <CardTitle>
                  {kind === "vless" ? "我的代理" : "端口转发"}{" "}
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
                                variant="ghost"
                                size="sm"
                                role="switch"
                                aria-checked={l.enabled}
                                aria-label={`${l.enabled ? "停用" : "启用"} ${l.name}`}
                                disabled={state.busy}
                                onClick={() => void run(async () => {
                                  await api("/lines", "POST", { ...l, enabled: !l.enabled });
                                  await refresh();
                                  await apply(l.id);
                                })}
                              >{l.enabled ? "停用" : "启用"}</Button>
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
                                {l.kind === "forward" ? "入口" : "分享"}
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
                                            title="删除代理或转发"
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
                                      删除
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
                      ? "没有匹配结果"
                      : kind === "forward"
                        ? "暂无端口转发"
                        : "暂无代理"
                  }
                  action={
                    !search && (
                      <Button onClick={() => editLine(kind)}>
                        <Plus />
                        {kind === "forward" ? "创建转发" : "创建代理"}
                      </Button>
                    )
                  }
                >
                </Empty>
              )}
              <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border px-5 py-4 text-xs text-slate-400">
                <span>{lines.length} 条</span>
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
              <div className="flex flex-wrap gap-2">
                <Button disabled={state.busy} onClick={() => nodeEditor()}><Plus />添加远程服务器</Button>
                <Button variant="outline" onClick={() => importExternal()}>导入已有代理</Button>
                <Button variant="ghost" onClick={() => importExternal(true)}>添加中转入口</Button>
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
                          disabled={state.busy || n.id === state.local_node_id}
                          onClick={() =>
                            setModal(
                              <Modal
                                title="重新接入服务器"
                                description="将重新配置 SSH 连接，现有连接凭据会失效。"
                                submit="继续 SSH 连接"
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
                                        host={n.host}
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
                          disabled={state.busy || n.id === state.local_node_id}
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
                  <CardTitle>已有代理</CardTitle>
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
          {page === "settings" && (
            <SecuritySettings
              state={state}
              saved={refresh}
              sessionEnded={sessionEnded}
            />
          )}
          <footer className="mt-10 flex items-center justify-between border-t border-border pt-5 text-[11px] text-slate-400">
            <span>Chain Panel · 个人网络控制台</span>
          </footer>
        </main>
      </div>
      {editor && <LineEditor
        {...editor}
        state={state}
        addServer={(done) => nodeEditor(undefined, done)}
        addLocalServer={addLocalServer}
        importExternal={(done) => importExternal(false, done)}
        close={() => setEditor(null)}
        saved={async (value) => {
          await refresh();
          await apply(value.id, value.kind === "vless");
          setEditor(null);
        }}
      />}
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
