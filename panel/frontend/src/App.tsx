import { Resources, RulesLibrary, AutoGroups, ManualGroup, BatchImport } from "./components/sharing";
import { Diagnostics } from "./components/diagnostics";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type FormEvent,
} from "react";
import {
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
  Server,
  Settings,
  Trash2,
  X,
  KeyRound,
  LayoutDashboard,
  Users,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { api, ApiError, publishChecked } from "./api";
import { LineEditor } from "./components/line-editor";
import { AccessDialog } from "./components/access-dialog";
import { SecuritySettings } from "./components/security-settings";
import { Maintenance } from "./components/maintenance";
import { Dashboard } from "./components/dashboard";
import { Customers } from "./components/customers";
import { ProxyList } from "./components/proxy-list";
import { Fleet, Monitoring } from "./components/management";
import { RentalEditor } from "./components/rental-editor";
import {
  Modal,
  Field,
  Status,
  PathView,
  CopyButton,
  stamp,
  field,
  endpoint,
  splitAddress,
  errorText,
  roleNames,
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
  { id: "dashboard", label: "总览", icon: LayoutDashboard },
  { id: "lines", label: "代理", icon: Network },
  { id: "customers", label: "客户与收款", icon: Users },
  { id: "nodes", label: "外部代理", icon: Server },
  { id: "resources", label: "资源额度", icon: Server },
  { id: "rules", label: "网站规则", icon: Network },
  { id: "fleet", label: "多机总控", icon: Network },
  { id: "monitoring", label: "监控告警", icon: Bell },
  { id: "settings", label: "设置", icon: Settings },
] as const;
type Page = (typeof pages)[number]["id"];
const workspaceModes = [
  { id: "personal", label: "自用", pages: ["lines", "nodes", "dashboard", "settings"], guide: "先创建代理并检查连接，再通过分享链接或二维码导入客户端。需要指定落地时，再配置外部代理与链式出网。", tasks: [["lines", "创建与分享代理"], ["nodes", "配置落地"], ["settings", "检查登录安全"]] },
  { id: "small", label: "小型运营", pages: ["dashboard", "lines", "customers", "nodes", "fleet", "monitoring", "resources", "rules", "settings"], guide: "先建客户档案，再为代理设置归属与报价。确认线下收款后开通，交付订阅和私密查询链接，并检查到期与用量告警。", tasks: [["customers", "建立客户档案"], ["lines", "开通与交付代理"], ["monitoring", "设置告警"]] },
  { id: "business", label: "商业运营", pages: ["dashboard", "customers", "lines", "fleet", "monitoring", "nodes", "resources", "rules", "settings"], guide: "集中处理客户与收款、代理交付和多机异常。收款与开通仍由管理员确认；先核对经营记录，再安排维护和售后。", tasks: [["customers", "客户与收款记录"], ["fleet", "集中管理 VPS"], ["monitoring", "处理运行告警"]] },
] as const;
type WorkspaceMode = (typeof workspaceModes)[number]["id"];
const remotePanel = new URLSearchParams(location.search).get('remote');
const visiblePages = remotePanel ? pages.filter(p => ['lines', 'nodes', 'resources', 'rules'].includes(p.id)) : pages;
const pageFromHash = (): Page =>
  visiblePages.find((p) => p.id === location.hash.slice(1))?.id || (remotePanel ? 'lines' : 'dashboard');
const jobNames: Record<string, string> = {
  publishing: "正在应用",
  rolling_back: "正在恢复",
  applied: "配置已应用",
  rolled_back: "已恢复旧配置",
  degraded: "部分服务器尚未确认恢复",
};
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
  const [mode, setMode] = useState("http");
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
          <option value="tcp">代理端口连通</option>
          <option value="http">HTTP / HTTPS 响应</option>
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
        检查会验证实际请求。
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
  const [mode, setMode] = useState<WorkspaceMode>(() => workspaceModes.find(item => item.id === localStorage.getItem("chain-workspace-mode"))?.id || "small");
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem("chain-sidebar") === "collapsed",
  );
  const [modal, setModal] = useState<ReactNode>(null);
  const [editor, setEditor] = useState<{ kind: Line["kind"]; line?: Line; initialChain?: string[]; pendingImport?: { id: string; name: string } } | null>(null);
  const [toast, setToast] = useState("");
  const [pendingShare, setPendingShare] = useState<string | null>(null);
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
    const desktop = window.matchMedia("(min-width: 1024px)");
    const resized = () => { if (desktop.matches) setMobile(false); };
    desktop.addEventListener("change", resized);
    return () => desktop.removeEventListener("change", resized);
  }, []);
  useEffect(() => {
    if (!pendingShare || !state || state.busy) return;
    if (state.viewer && state.viewer.role !== "admin") { setPendingShare(null); return; }
    const line = state.lines.find((item) => item.id === pendingShare);
    if (state.jobs[0]?.state === "applied" && line?.published && line.enabled) {
      setPendingShare(null);
      setModal(<AccessDialog line={line} state={state} close={() => setModal(null)} />);
    } else if (["rolled_back", "degraded"].includes(state.jobs[0]?.state)) {
      setPendingShare(null);
    }
  }, [pendingShare, state]);
  const navigate = (next: string) => {
    location.hash = next;
    setMobile(false);
  };
  const run = async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (e) {
      setToast(errorText(e));
    }
  };
  const sessionEnded = (message = "") => {
    setLoginNotice(message);
    setState(null);
    setEditor(null);
    setPendingShare(null);
    close();
  };
  async function apply(id?: string, share = false, created: string[] = []) {
    const preview = await api<Preview>("/preview", "POST", {});
    const otherLines = preview.affected_line_ids.filter((n) => n !== id && !created.includes(n));
    if (otherLines.length || preview.affected_legacy_configs?.length) {
      setModal(
        <Modal
          title="应用这些修改"
          description="以下代理共用本机，应用时可能短暂重连。"
          submit="确认并应用"
          close={close}
          action={async () => {
            await publishChecked();
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
            {preview.affected_legacy_configs?.map((n) => (
              <Status key={`legacy-${n}`}>迁移配置：{n}</Status>
            ))}
          </div>
          <p className="muted">{preview.message}</p>
        </Modal>,
      );
    } else {
      await publishChecked();
      await refresh();
      if (share && id) setPendingShare(id);
      close();
      setToast("正在应用，请在当前页面查看结果。");
    }
  }
  function editLine(kind: Line["kind"], line?: Line, initialChain?: string[]) {
    if (!state) return;
    setEditor({ kind, line, initialChain });
  }
  async function ensureLocalServer(host: string): Promise<NodeInfo> {
    const current = state?.nodes.find(n => n.id === state.local_node_id);
    if (current?.online) return current;
    if (remotePanel) throw new Error('远端 Agent 尚未在线，请先在对应 VPS 修复服务。');
    const value = await api<{ id: string }>("/local-node", "POST", { host });
    for (let attempt = 0; attempt < 30; attempt++) {
      const next = await api<PanelState>("/state");
      setState(next);
      const node = next.nodes.find(n => n.id === value.id);
      if (node?.online) return node;
      if (node?.report.error) throw new Error(node.report.error);
      await new Promise(resolve => setTimeout(resolve, 1000));
    }
    throw new Error("本机未能上线，请在本机执行 chainctl 查看服务状态后重试。已填写的代理配置已保留。");
  }
  function importExternal(done?: (external?: External) => void) {
    setModal(<Modal title="导入外部代理" description="粘贴分享链接或单个 Mihomo / Clash YAML、JSON 节点。" submit="导入" close={() => { close(); done?.(); }} action={async data => {
      const value = await api<External>("/externals", "POST", { name: field(data, "name"), url: field(data, "url") });
      await refresh();
      if (done) { close(); done(value); return; }
      if(data.has("group")){setModal(<AutoGroups state={{...state!,externals:[...state!.externals,value]}} ids={[value.id]} close={close} saved={refresh}/>);return;}
      setModal(<AccessDialog external={value} state={state!} close={close} />);
    }}>
      <Field label="名称" hint="留空使用节点中的名称"><Input name="name" maxLength={80} placeholder="自动识别" /></Field>
      <Field label="链接或 Mihomo 节点"><textarea className="textarea mono" name="url" rows={7} placeholder={'vless://…\n或 - {name: 落地 A, type: vless, server: …}'} required /></Field>
      <label className="flex gap-2 text-sm"><input type="checkbox" name="group"/>导入后自动识别分组候选</label>
    </Modal>);
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
              使用安装时生成的管理员密钥，或管理员发放的成员密钥。
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
              <Field label="管理员密钥" hint="成员可在此输入自己的独立密钥。">
                <Input
                  name="token"
                  type="password"
                  autoComplete="current-password"
                  placeholder="输入管理员密钥"
                  required
                />
              </Field>
              {options.totp && (
                <Field label="动态验证码或恢复码" hint="管理员启用二步验证时必填；成员密钥无需填写。">
                  <Input
                    name="code"
                    autoComplete="one-time-code"
                    placeholder="6 位验证码或一次性恢复码"
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
  const kind: Line["kind"] = "vless";
  const latest = state.jobs[0];
  const role = state.viewer?.role || "admin";
  const admin = role === "admin";
  const billing = admin || role === "finance";
  const allowedPages = visiblePages.filter(p => admin || (role === "ops"
    ? ["dashboard", "lines", "monitoring", "settings"]
    : role === "finance" ? ["dashboard", "lines", "customers"] : ["dashboard", "lines"]).includes(p.id));
  const pageAllowed = allowedPages.some(p => p.id === page);
  const workspace = workspaceModes.find(item => item.id === mode)!;
  const primaryPages = remotePanel ? allowedPages : workspace.pages.map(id => allowedPages.find(p => p.id === id)).filter(p => p !== undefined);
  const secondaryPages = allowedPages.filter(p => !primaryPages.some(primary => primary.id === p.id));
  const navigationButton = (p: (typeof pages)[number]) => <button
    key={p.id}
    aria-label={p.label}
    aria-current={page === p.id ? "page" : undefined}
    title={collapsed ? p.label : undefined}
    className={`nav-item ${page === p.id ? "active" : ""}`}
    onClick={() => navigate(p.id)}
  ><p.icon className="size-[18px] shrink-0" /><span className="sidebar-label">{p.label}</span></button>;
  const sidebar = <>
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
        {!remotePanel && <div className="sidebar-label px-4 pb-3">
          <Field label="使用模式"><select className="select" value={mode} onChange={event => {
            const next = event.target.value as WorkspaceMode;
            localStorage.setItem("chain-workspace-mode", next);
            setMode(next);
          }}>{workspaceModes.map(item => <option key={item.id} value={item.id}>{item.label}</option>)}</select></Field>
        </div>}
        <nav className="flex-1 space-y-1 px-3 py-2">
          {primaryPages.map(navigationButton)}
          {secondaryPages.length > 0 && <details key={mode} open={secondaryPages.some(p => p.id === page) || undefined}>
            <summary className="nav-item" aria-label="更多功能"><MoreHorizontal className="size-[18px] shrink-0" /><span className="sidebar-label">更多功能</span></summary>
            {secondaryPages.map(navigationButton)}
          </details>}
        </nav>
        <div className="sidebar-footer">
          {remotePanel && <a className="nav-item" href="/admin#fleet">返回总控</a>}
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
      </>;
  return (
    <Dialog open={mobile} onOpenChange={setMobile}>
    <div className={collapsed ? "app-shell is-collapsed" : "app-shell"}>
      <aside className="app-sidebar">{sidebar}</aside>
      <div className="app-main">
        <header className="app-header">
          <div className="flex items-center gap-3">
            <DialogTrigger asChild><Button
              size="icon"
              variant="ghost"
              className="lg:hidden"
              aria-label="打开导航"
            >
              <Menu />
            </Button></DialogTrigger>
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
            <span aria-label="当前身份" title={`${state.viewer?.name || "管理员"} · ${roleNames[role]}`} className="max-w-48 truncate rounded-full border border-blue-100 bg-blue-50 px-3 py-2 text-xs font-medium text-blue-700">
              {state.viewer?.name || "管理员"} · {roleNames[role]}
            </span>
          </div>
        </header>
        <main className="app-content">
          {page !== "fleet" && <div className="mb-7 flex flex-wrap items-end justify-between gap-4">
            <div>
              <h1 className="text-2xl font-semibold tracking-tight text-slate-900">
                {current.label}
              </h1>
            </div>
            {page === "lines" && admin && (
              <Button disabled={state.busy} onClick={() => editLine(kind)}>
                <Plus />
                新建代理
              </Button>
            )}
          </div>}
          {!remotePanel && admin && ["dashboard", "lines", "customers"].includes(page) && <details className="mb-5 rounded-lg border border-border bg-white p-4" open={!state.lines.length || undefined}>
            <summary className="cursor-pointer text-sm font-medium">{workspace.label} · 开始使用</summary>
            <p className="muted mt-3">{workspace.guide}</p>
            <p className="muted mt-2">模式仅调整本浏览器的导航与引导；已有代理、客户、收款和设置仍可访问。</p>
            <div className="mt-3 flex flex-wrap gap-2">{workspace.tasks.map(([target, label]) => <Button key={target} size="sm" variant="outline" onClick={() => navigate(target)}>{label}</Button>)}</div>
          </details>}
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
          {!pageAllowed && <div role="alert" className="rounded-lg border border-border bg-white p-5 text-sm">当前身份无权访问此页面。<Button className="ml-3" variant="outline" onClick={() => navigate("dashboard")}>返回总览</Button></div>}
          {pageAllowed && <>
          {page === "dashboard" && <Dashboard state={state} manage={billing ? line => line.customer_plan_id ? navigate("customers") : setModal(<RentalEditor line={line} state={state} close={close} saved={refresh} />) : undefined} />}
          {remotePanel && <p role="status" className="mb-4 rounded border p-3">正在管理远端 VPS；此页面操作作用于该 VPS。<a className="ml-2 underline" href="/admin#fleet">返回总控</a></p>}
          {page === "fleet" && <Fleet />}
          {page === "monitoring" && <><Button className="mb-4" variant="outline" onClick={() => setModal(<Diagnostics nodes={state.nodes} canSyncTime={admin} close={close} />)}>日志与诊断</Button><Monitoring /></>}
          {page === "customers" && <Customers state={state} saved={refresh} />}
          {page === "lines" && <ProxyList state={state} saved={refresh} remote={!!remotePanel}
            configureImport={pendingImport => setEditor({ kind: "vless", pendingImport })}
            create={() => editLine(kind)} edit={line => editLine(line.kind, line)}
            share={line => setModal(<AccessDialog line={line} state={state} close={close} />)}
            probe={line => setModal(<ProbeEditor line={line} state={state} close={close} saved={refresh} />)}
            apply={line => void run(() => apply(line?.id))}
          />}
          {page === "resources" && <Resources state={state} saved={refresh}/>}
          {page === "rules" && <RulesLibrary/>}
          {page === "nodes" && (
            <div className="space-y-6">
              <div className="flex flex-wrap gap-2"><Button onClick={() => importExternal()}><Plus />导入外部代理</Button><Button variant="outline" onClick={()=>setModal(<BatchImport state={state} close={close} saved={refresh}/>)}>批量导入</Button><Button variant="outline" onClick={()=>setModal(<AutoGroups state={state} close={close} saved={refresh}/>)}>自动分组</Button></div>
              <Card>
                <CardHeader>
                  <CardTitle>外部代理</CardTitle>
                </CardHeader>
                <CardContent>
                  {state.externals.length ? (
                    state.externals.map((e) => (
                      <div
                        key={e.id}
                        className="flex flex-wrap items-center justify-between gap-3 border-b border-border py-4 last:border-0"
                      >
                        <div className="min-w-0 max-w-full">
                          <p className="break-all text-sm font-medium">{e.name}</p>
                          <p className="mt-1 text-xs text-slate-400">
                            {e.url.split(":")[0].toUpperCase()} · {state.resources?.find(r=>r.members.includes(`external:${e.id}`))?.name || "未分组"}
                          </p>
                        </div>
                        <div className="flex flex-wrap gap-2">
                          <Button variant="outline" onClick={()=>setModal(<ManualGroup external={e} state={state} close={close} saved={refresh}/>)}>分组</Button>
                          <Button variant="outline" onClick={() => setModal(<AccessDialog external={e} state={state} close={close} />)}>链接 / Mihomo</Button>
                          <Button variant="outline" onClick={() => setModal(<Modal title="编辑外部代理" description="保留引用关系；保存后确认应用受影响代理。" submit="保存" close={close} action={async data => {
                            await api(`/externals/${e.id}`, 'POST', { name: field(data, 'name'), url: field(data, 'url') });
                            await refresh(); close(); await apply();
                          }}><Field label="名称"><Input name="name" defaultValue={e.name} required /></Field><Field label="分享链接或单节点配置"><textarea className="input min-h-32 w-full" name="url" defaultValue={e.url} required /></Field></Modal>)}>编辑</Button>
                          <Button variant="outline" onClick={() => editLine("vless", undefined, [`external:${e.id}`])}>用于新建代理</Button>
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
            <div className="space-y-6">
            <Maintenance state={state} />
            {admin && <SecuritySettings
              state={state}
              saved={refresh}
              sessionEnded={sessionEnded}
            />}
            </div>
          )}
          </>}
          <footer className="mt-10 flex items-center justify-between border-t border-border pt-5 text-[11px] text-slate-400">
            <span>Chain Panel {state.version ? `v${state.version}` : "版本未知"} · 节点租赁运营控制台</span>
          </footer>
        </main>
      </div>
      {editor && <LineEditor
        {...editor}
        state={state}
        ensureLocalServer={ensureLocalServer}
        importExternal={importExternal}
        close={() => setEditor(null)}
        saved={async (value, publish = true) => {
          setEditor(null);
          await refresh();
          if (!publish) {
            setToast("草稿已保存，可在客户与收款中绑定共享套餐，或在代理列表继续应用。");
            return;
          }
          try {
            await apply(value[0].id, value.length === 1 && value[0].kind === "vless", value.map(line => line.id));
          } catch (e) {
            setToast(`配置已保存，尚未应用：${errorText(e)}。请在代理列表继续应用，无需重新创建。`);
          }
        }}
      />}
      {modal}
      <DialogContent showCloseButton={false} aria-describedby={undefined} className="inset-y-0 left-0 top-0 h-dvh max-h-dvh w-64 max-w-[85vw] translate-x-0 translate-y-0 overflow-y-auto rounded-none border-l-0 p-0 sm:rounded-none">
        <DialogTitle className="sr-only">管理导航</DialogTitle>
        {sidebar}
      </DialogContent>
      {toast && (
        <div
          role="status"
          className="fixed bottom-6 left-1/2 z-[100] max-w-[90%] -translate-x-1/2 rounded-xl bg-slate-900 px-5 py-3 text-sm text-white shadow-xl"
        >
          {toast}
        </div>
      )}
    </div>
    </Dialog>
  );
}
