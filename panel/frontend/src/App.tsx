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
  Activity,
  ArrowRight,
  Bell,
  ChevronLeft,
  ChevronRight,
  FileText,
  Fingerprint,
  FolderKanban,
  Gauge,
  Globe,
  Layers,
  LayoutDashboard,
  LogOut,
  Menu,
  Moon,
  MoreHorizontal,
  Pencil,
  Plus,
  Search,
  Server,
  Settings,
  ShieldCheck,
  SlidersHorizontal,
  Sun,
  Trash2,
  UserCheck,
  Users,
  Waypoints,
  X,
} from "lucide-react";
import { DropdownMenu as RadixDropdown } from "radix-ui";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardContent,
  CardHeader,
  CardHeading,
  CardTable,
  CardTitle,
  CardToolbar,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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
import { ProbeEditor } from "./components/probe-editor";
import {
  Modal,
  Field,
  Status,
  CopyButton,
  Empty,
  stamp,
  field,
  errorText,
  roleNames,
} from "./components/shared";
import { parseExternalSummary } from "./lib/business";
import { loginPasskey } from "./passkey";
import { usePreferences, workspaceModes, type WorkspaceMode } from "./lib/preferences";
import type {
  PanelState,
  NodeInfo,
  External,
  Line,
  Preview,
} from "./types";

const pages = [
  { id: "dashboard", label: "总览", group: "业务概览", icon: LayoutDashboard },
  { id: "lines", label: "代理", group: "业务概览", icon: Waypoints },
  { id: "customers", label: "客户与收款", group: "业务概览", icon: Users },
  { id: "nodes", label: "外部代理", group: "网络与规则", icon: Globe },
  { id: "resources", label: "资源额度", group: "网络与规则", icon: Gauge },
  { id: "rules", label: "网站规则", group: "网络与规则", icon: ShieldCheck },
  { id: "fleet", label: "多机总控", group: "运维与系统", icon: Server },
  { id: "monitoring", label: "监控告警", group: "运维与系统", icon: Activity },
  { id: "settings", label: "设置", group: "运维与系统", icon: Settings },
] as const;

type Page = (typeof pages)[number]["id"];

const remotePanel = new URLSearchParams(location.search).get("remote");
const visiblePages = remotePanel
  ? pages.filter((p) => ["lines", "nodes", "resources", "rules"].includes(p.id))
  : pages;
const pageFromHash = (): Page =>
  visiblePages.find((p) => p.id === location.hash.slice(1))?.id ||
  (remotePanel ? "lines" : "dashboard");

const jobNames: Record<string, string> = {
  publishing: "正在下发配置",
  rolling_back: "正在回滚配置",
  applied: "配置已生效",
  rolled_back: "已回滚至旧配置",
  degraded: "部分节点待恢复确认",
};

export function App() {
  const { resolvedTheme, toggleTheme, mode, setMode } = usePreferences();
  const [state, setState] = useState<PanelState | null>(null);
  const [loading, setLoading] = useState(true);
  const [connected, setConnected] = useState(true);
  const [page, setPage] = useState<Page>(pageFromHash);
  const [mobile, setMobile] = useState(false);
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem("chain-sidebar") === "collapsed",
  );
  const [externalSearch, setExternalSearch] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);
  const [editor, setEditor] = useState<{
    kind: Line["kind"];
    line?: Line;
    initialChain?: string[];
    pendingImport?: { id: string; name: string };
  } | null>(null);
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
    const resized = () => {
      if (desktop.matches) setMobile(false);
    };
    desktop.addEventListener("change", resized);
    return () => desktop.removeEventListener("change", resized);
  }, []);

  useEffect(() => {
    if (!pendingShare || !state || state.busy) return;
    if (state.viewer && state.viewer.role !== "admin") {
      setPendingShare(null);
      return;
    }
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
    const otherLines = preview.affected_line_ids.filter(
      (n) => n !== id && !created.includes(n),
    );
    if (otherLines.length || preview.affected_legacy_configs?.length) {
      setModal(
        <Modal
          title="应用这些修改"
          description="以下同机代理在配置重载期间可能短暂重连。"
          submit="确认并应用"
          close={close}
          action={async () => {
            await publishChecked();
            await refresh();
            if (share && id) setPendingShare(id);
            close();
            setToast("正在下发配置，请在当前页面查看同步状态。");
          }}
        >
          <div className="flex flex-wrap gap-1.5">
            {preview.affected_lines.map((n) => (
              <Status key={n} tone="info">
                {n}
              </Status>
            ))}
            {preview.affected_legacy_configs?.map((n) => (
              <Status key={`legacy-${n}`} tone="warn">
                迁移配置：{n}
              </Status>
            ))}
          </div>
          {preview.message && <p className="muted">{preview.message}</p>}
        </Modal>,
      );
    } else {
      await publishChecked();
      await refresh();
      if (share && id) setPendingShare(id);
      close();
      setToast("正在下发配置，请在当前页面查看同步状态。");
    }
  }

  function editLine(kind: Line["kind"], line?: Line, initialChain?: string[]) {
    if (!state) return;
    setEditor({ kind, line, initialChain });
  }

  async function ensureLocalServer(host: string): Promise<NodeInfo> {
    const current = state?.nodes.find((n) => n.id === state.local_node_id);
    if (current?.online) return current;
    if (remotePanel) throw new Error("远端 Agent 尚未在线，请先在对应 VPS 修复服务。");
    const value = await api<{ id: string }>("/local-node", "POST", { host });
    for (let attempt = 0; attempt < 30; attempt++) {
      const next = await api<PanelState>("/state");
      setState(next);
      const node = next.nodes.find((n) => n.id === value.id);
      if (node?.online) return node;
      if (node?.report.error) throw new Error(node.report.error);
      await new Promise((resolve) => setTimeout(resolve, 1000));
    }
    throw new Error(
      "本机未能上线，请在本机执行 chainctl 查看服务状态后重试。已填写的代理配置已保留。",
    );
  }

  function importExternal(done?: (external?: External) => void) {
    setModal(
      <Modal
        title="导入外部代理"
        description="支持粘贴分享链接或单条 Mihomo / Clash 节点配置。"
        submit="导入"
        close={() => {
          close();
          done?.();
        }}
        action={async (data) => {
          const value = await api<External>("/externals", "POST", {
            name: field(data, "name"),
            url: field(data, "url"),
          });
          await refresh();
          if (done) {
            close();
            done(value);
            return;
          }
          if (data.has("group")) {
            setModal(
              <AutoGroups
                state={{ ...state!, externals: [...state!.externals, value] }}
                ids={[value.id]}
                close={close}
                saved={refresh}
              />,
            );
            return;
          }
          setModal(<AccessDialog external={value} state={state!} close={close} />);
        }}
      >
        <Field label="名称" hint="留空则沿用节点自带名称">
          <Input name="name" maxLength={80} placeholder="自动识别" />
        </Field>
        <Field label="链接或 Mihomo 节点">
          <textarea
            className="textarea mono"
            name="url"
            rows={6}
            placeholder={"vless://…\n或 - {name: 落地 A, type: vless, server: …}"}
            required
          />
        </Field>
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          <input type="checkbox" name="group" />
          导入后按相同地址自动归组
        </label>
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
      <div className="flex min-h-screen items-center justify-center bg-background text-sm text-muted-foreground">
        正在连接控制台…
      </div>
    );

  if (!state)
    return (
      <div className="login-layout">
        <main className="relative flex min-h-screen flex-col items-center justify-center p-6 sm:p-12">
          <div className="absolute right-5 top-5">
            <Button
              type="button"
              variant="ghost"
              mode="icon"
              shape="circle"
              aria-label="切换深色模式"
              onClick={toggleTheme}
            >
              {resolvedTheme === "dark" ? <Sun className="size-4" /> : <Moon className="size-4" />}
            </Button>
          </div>
          <div className="w-full max-w-[390px]">
            <div className="mb-8 flex items-center gap-2.5 lg:hidden">
              <span className="brand-symbol">
                <Waypoints />
              </span>
              <span className="text-base font-semibold tracking-tight">Chain Panel</span>
            </div>
            <Card className="border-border/90 shadow-sm">
              <CardContent className="p-7 sm:p-8">
                <div className="mb-6">
                  <h1 className="text-xl font-semibold tracking-tight text-foreground">
                    登录控制台
                  </h1>
                  <p className="mt-1.5 text-xs leading-relaxed text-muted-foreground">
                    输入管理员密钥或已授权的运营成员密钥进入后台。
                  </p>
                </div>
                {loginNotice && (
                  <div className="mb-5 space-y-3 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3.5">
                    <pre className="whitespace-pre-wrap break-all text-xs leading-relaxed text-foreground">
                      {loginNotice}
                    </pre>
                    <CopyButton
                      size="sm"
                      value={loginNotice.split("\n").at(-1) || ""}
                      label="复制新密钥"
                    />
                  </div>
                )}
                <form onSubmit={login} className="space-y-4">
                  <Field label="管理员密钥">
                    <Input
                      name="token"
                      type="password"
                      variant="lg"
                      autoComplete="current-password"
                      placeholder="输入管理员或成员密钥"
                      required
                    />
                  </Field>
                  {options.totp && (
                    <Field
                      label="动态验证码或恢复码"
                      hint="仅主管理员启用二步验证时需填写"
                    >
                      <Input
                        name="code"
                        variant="lg"
                        autoComplete="one-time-code"
                        placeholder="6 位动态码或恢复码"
                      />
                    </Field>
                  )}
                  {loginError && (
                    <p
                      role="alert"
                      className="rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-xs text-destructive"
                    >
                      {loginError}
                    </p>
                  )}
                  <Button type="submit" size="lg" className="w-full" disabled={loggingIn}>
                    {loggingIn ? "正在验证…" : "登录"}
                    <ArrowRight />
                  </Button>
                </form>
                {options.passkey && (
                  <Button
                    className="mt-3 w-full"
                    size="lg"
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
                <div className="mt-6 border-t border-border pt-4 text-xs leading-relaxed text-muted-foreground">
                  忘记密钥可在服务器终端运行 <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-foreground">chainctl</code> 查看或重置登录凭据。
                </div>
                {!connected && (
                  <p className="mt-3 text-xs text-destructive">
                    暂时无法连接面板服务，请稍后重试。
                  </p>
                )}
              </CardContent>
            </Card>
          </div>
        </main>

        <section className="login-showcase">
          <div className="relative z-10 flex items-center justify-between">
            <div className="flex items-center gap-3">
              <span className="brand-symbol">
                <Waypoints />
              </span>
              <div>
                <span className="block text-base font-semibold tracking-tight text-white">
                  Chain Panel
                </span>
                <span className="block text-[11px] text-slate-400">
                  Multi-Hop Proxy &amp; Fleet Console
                </span>
              </div>
            </div>
            <Badge variant="primary" appearance="outline" className="border-blue-400/30 bg-blue-500/10 text-blue-300">
              Enterprise Control Plane
            </Badge>
          </div>

          <div className="relative z-10 my-auto space-y-6 py-10">
            <div className="space-y-3">
              <h2 className="max-w-md text-3xl font-semibold leading-tight tracking-tight text-white">
                多节点链式代理与联邦总控平台
              </h2>
              <p className="max-w-lg text-sm leading-relaxed text-slate-300/90">
                集中编排 VLESS / REALITY / TLS 接入、跨机链式落地、租期配额账务与多 VPS 自动化滚动运维。
              </p>
            </div>
            <div className="grid max-w-lg grid-cols-2 gap-3.5 pt-2">
              {[
                ["链式拓扑编排", "本机入口至多级外部落地可视化串联与按需热下发"],
                ["多机联邦总控", "跨区域 VPS 实时速率遥测、灰度内核更新与链路自检"],
                ["租期与共享套餐", "独立线路与团队流量池计量、账单对账及私密查询页"],
                ["硬件级安全认证", "支持 WebAuthn Passkey、TOTP 二步验证与分权成员密钥"],
              ].map(([title, desc]) => (
                <div
                  key={title}
                  className="rounded-xl border border-white/10 bg-white/[0.04] p-4 backdrop-blur-xs"
                >
                  <p className="text-xs font-semibold text-white">{title}</p>
                  <p className="mt-1 text-[11px] leading-relaxed text-slate-400">{desc}</p>
                </div>
              ))}
            </div>
          </div>

          <div className="relative z-10 flex items-center justify-between text-xs text-slate-400">
            <span>Zero-Downtime Config Rollout</span>
            <span>REALITY · Xray · Mihomo</span>
          </div>
        </section>
      </div>
    );

  const current = pages.find((p) => p.id === page)!;
  const kind: Line["kind"] = "vless";
  const latest = state.jobs[0];
  const role = state.viewer?.role || "admin";
  const admin = role === "admin";
  const billing = admin || role === "finance";
  const allowedPages = visiblePages.filter(
    (p) =>
      admin ||
      (role === "ops"
        ? ["dashboard", "lines", "monitoring", "settings"]
        : role === "finance"
          ? ["dashboard", "lines", "customers"]
          : ["dashboard", "lines"]
      ).includes(p.id),
  );
  const pageAllowed = allowedPages.some((p) => p.id === page);
  const workspace = workspaceModes.find((item) => item.id === mode)!;
  const primaryPages = remotePanel
    ? allowedPages
    : workspace.pages
        .map((id) => allowedPages.find((p) => p.id === id))
        .filter((p) => p !== undefined);
  const secondaryPages = allowedPages.filter(
    (p) => !primaryPages.some((primary) => primary.id === p.id),
  );

  const navigationButton = (p: (typeof pages)[number]) => (
    <button
      key={p.id}
      aria-label={p.label}
      aria-current={page === p.id ? "page" : undefined}
      title={collapsed ? p.label : undefined}
      className={`nav-item ${page === p.id ? "active" : ""}`}
      onClick={() => navigate(p.id)}
    >
      <p.icon className="size-[17px] shrink-0" />
      <span className="sidebar-label truncate">{p.label}</span>
    </button>
  );

  // Group primary pages cleanly in standard modes while preserving exact order for personal/business modes
  const useGroupedNav = !remotePanel && admin && mode === "small";
  const navGroups = ["业务概览", "网络与规则", "运维与系统"] as const;

  const sidebar = (
    <>
      <div className="sidebar-brand">
        <span className="brand-symbol">
          <Waypoints />
        </span>
        <div className="sidebar-label min-w-0">
          <span className="block truncate text-[15px] font-semibold tracking-tight text-foreground">
            Chain Panel
          </span>
          {state.version && (
            <span className="block text-[10px] font-medium text-muted-foreground">
              v{state.version}
            </span>
          )}
        </div>
        <button
          className="ms-auto rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground lg:hidden"
          aria-label="关闭导航"
          onClick={() => setMobile(false)}
        >
          <X className="size-4.5" />
        </button>
      </div>

      <nav className="flex-1 space-y-0.5 overflow-y-auto px-3 py-3">
        {useGroupedNav
          ? navGroups.map((group) => {
              const items = primaryPages.filter((p) => p.group === group);
              if (!items.length) return null;
              return (
                <div key={group} className="space-y-0.5">
                  <div className="sidebar-section-label">{group}</div>
                  {items.map(navigationButton)}
                </div>
              );
            })
          : primaryPages.map(navigationButton)}

        {secondaryPages.length > 0 && (
          <details
            key={mode}
            className="pt-2"
            open={secondaryPages.some((p) => p.id === page) || undefined}
          >
            <summary
              className="nav-item list-none select-none justify-between [&::-webkit-details-marker]:hidden"
              aria-label="更多功能"
            >
              <span className="flex items-center gap-2.5">
                <MoreHorizontal className="size-[17px] shrink-0" />
                <span className="sidebar-label">更多模块</span>
              </span>
            </summary>
            <div className="mt-0.5 space-y-0.5 border-l border-border ms-4 ps-2">
              {secondaryPages.map(navigationButton)}
            </div>
          </details>
        )}
      </nav>

      <div className="sidebar-footer space-y-1">
        {remotePanel && (
          <a className="nav-item" href="/admin#fleet">
            <Layers className="size-[17px] shrink-0" />
            <span className="sidebar-label">返回总控</span>
          </a>
        )}
        <button
          className="nav-item w-full"
          onClick={() =>
            void run(async () => {
              await api("/logout", "POST", {});
              sessionEnded();
            })
          }
        >
          <LogOut className="size-[17px] shrink-0" />
          <span className="sidebar-label">退出登录</span>
        </button>
      </div>

      <button
        className="sidebar-toggle hidden lg:flex"
        aria-label={collapsed ? "展开侧栏" : "收起侧栏"}
        onClick={() =>
          setCollapsed((old) => {
            localStorage.setItem("chain-sidebar", old ? "expanded" : "collapsed");
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
    </>
  );

  const rawViewerName = state.viewer?.name || "管理员";
  const displayViewerName =
    rawViewerName === "内置管理员" ? "管理员" : rawViewerName;
  const showRoleSuffix = displayViewerName !== roleNames[role];

  const externalQuery = externalSearch.trim().toLowerCase();
  const filteredExternals = page === "nodes" && pageAllowed ? state.externals
    .map(external => ({ external, ...parseExternalSummary(external.url) }))
    .filter(({ external, protocol, target }) =>
      `${external.name} ${protocol} ${target}`.toLowerCase().includes(externalQuery),
    ) : [];

  return (
    <Dialog open={mobile} onOpenChange={setMobile}>
      <div className={collapsed ? "app-shell is-collapsed" : "app-shell"}>
        <aside className="app-sidebar">{sidebar}</aside>
        <div className="app-main">
          <header className="app-header">
            <div className="flex min-w-0 items-center gap-2.5">
              <DialogTrigger asChild>
                <Button
                  size="icon"
                  variant="ghost"
                  className="lg:hidden"
                  aria-label="打开导航"
                >
                  <Menu />
                </Button>
              </DialogTrigger>
              <nav aria-label="面包屑" className="flex min-w-0 items-center gap-1.5 text-xs">
                <span className="hidden text-muted-foreground sm:inline">
                  {remotePanel ? "远端节点" : current.group}
                </span>
                <ChevronRight className="hidden size-3.5 text-muted-foreground/60 sm:inline" />
                <span className="truncate font-semibold text-foreground">
                  {current.label}
                </span>
              </nav>
            </div>

            <div className="flex items-center gap-2 sm:gap-2.5">
              <span className="hidden items-center gap-1.5 rounded-full border border-border bg-card px-2.5 py-1 text-[11px] font-medium text-muted-foreground sm:inline-flex">
                <span
                  className={`size-1.5 rounded-full ${
                    connected ? "bg-emerald-500" : "bg-destructive"
                  }`}
                />
                {connected ? "实时同步" : "连接中断"}
              </span>

              <Button
                type="button"
                variant="ghost"
                mode="icon"
                shape="circle"
                className="size-8.5"
                aria-label="切换深色模式"
                title={resolvedTheme === "dark" ? "切换为浅色模式" : "切换为深色模式"}
                onClick={toggleTheme}
              >
                {resolvedTheme === "dark" ? (
                  <Sun className="size-4" />
                ) : (
                  <Moon className="size-4" />
                )}
              </Button>

              <Popover>
                <PopoverTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    mode="icon"
                    shape="circle"
                    className="relative size-8.5"
                    aria-label="查看告警"
                  >
                    <Bell className="size-4" />
                    {state.alerts.length > 0 && (
                      <span className="absolute right-1.5 top-1.5 size-2 rounded-full bg-amber-500 ring-2 ring-background" />
                    )}
                  </Button>
                </PopoverTrigger>
                <PopoverContent align="end" className="w-80 p-0 sm:w-96">
                  <div className="flex items-center justify-between border-b border-border px-4 py-3">
                    <span className="text-xs font-semibold text-foreground">
                      运行状态与下发记录
                    </span>
                    <Badge
                      variant={state.alerts.length ? "warning" : "success"}
                      appearance="light"
                      size="sm"
                    >
                      {state.alerts.length ? `${state.alerts.length} 条告警` : "运行正常"}
                    </Badge>
                  </div>
                  <div className="max-h-80 divide-y divide-border overflow-y-auto">
                    {state.alerts.length > 0 ? (
                      <div className="space-y-2 p-3.5">
                        {state.alerts.map((alert) => (
                          <div
                            key={alert.id}
                            role="alert"
                            className="rounded-lg border border-amber-500/25 bg-amber-500/10 p-2.5 text-xs leading-relaxed text-foreground"
                          >
                            {alert.message}
                          </div>
                        ))}
                      </div>
                    ) : (
                      <p className="px-4 py-3 text-xs text-muted-foreground">
                        当前无活动告警。
                      </p>
                    )}
                    {state.jobs.length > 0 && (
                      <div className="p-3">
                        <p className="mb-2 px-1 text-[11px] font-medium text-muted-foreground">
                          最近配置同步
                        </p>
                        <div className="space-y-1.5">
                          {state.jobs.map((job) => (
                            <details
                              key={job.id}
                              className="rounded-md border border-border/70 bg-muted/25 px-2.5 py-2 text-xs"
                            >
                              <summary className="flex cursor-pointer list-none items-center justify-between gap-2 font-medium [&::-webkit-details-marker]:hidden">
                                <span>{jobNames[job.state] || job.state}</span>
                                <span className="text-[11px] font-normal text-muted-foreground tabular-nums">
                                  {stamp(job.started)}
                                </span>
                              </summary>
                              {job.events.length > 0 && (
                                <div className="mt-2 space-y-1 border-t border-border pt-2 text-[11px] text-muted-foreground">
                                  {job.events.map((event, i) => (
                                    <p key={i}>{event.message}</p>
                                  ))}
                                </div>
                              )}
                            </details>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>
                </PopoverContent>
              </Popover>

              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button
                    type="button"
                    aria-label="当前身份"
                    title={`${displayViewerName} · ${roleNames[role]}`}
                    className="inline-flex h-8.5 max-w-52 cursor-pointer items-center gap-2 rounded-full border border-border bg-card pl-1.5 pr-3 text-xs font-medium text-foreground shadow-2xs transition-colors hover:bg-accent"
                  >
                    <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-primary/15 text-[11px] font-semibold text-primary">
                      {displayViewerName.slice(0, 1)}
                    </span>
                    <span className="truncate">
                      {displayViewerName}
                      {showRoleSuffix ? ` · ${roleNames[role]}` : ""}
                    </span>
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-60">
                  <div className="flex items-center justify-between gap-2 px-2.5 py-2">
                    <div className="min-w-0">
                      <p className="truncate text-xs font-semibold text-foreground">
                        {displayViewerName}
                      </p>
                      <p className="text-[11px] text-muted-foreground">
                        {state.version ? `Chain Panel v${state.version}` : "管理控制台"}
                      </p>
                    </div>
                    <Badge variant="primary" appearance="light" size="sm">
                      {roleNames[role]}
                    </Badge>
                  </div>
                  <DropdownMenuSeparator />

                  {!remotePanel && (
                    <DropdownMenuSub>
                      <DropdownMenuSubTrigger className="text-xs">
                        <SlidersHorizontal className="size-3.5" />
                        <span>导航视图</span>
                        <Badge variant="secondary" size="sm" className="ms-auto">
                          {workspace.shortLabel}
                        </Badge>
                      </DropdownMenuSubTrigger>
                      <DropdownMenuSubContent className="w-48">
                        <DropdownMenuLabel>侧栏菜单偏好</DropdownMenuLabel>
                        <DropdownMenuRadioGroup
                          value={mode}
                          onValueChange={(value) => setMode(value as WorkspaceMode)}
                        >
                          {workspaceModes.map((item) => (
                            <DropdownMenuRadioItem
                              key={item.id}
                              value={item.id}
                              className="text-xs"
                            >
                              {item.label}
                            </DropdownMenuRadioItem>
                          ))}
                        </DropdownMenuRadioGroup>
                      </DropdownMenuSubContent>
                    </DropdownMenuSub>
                  )}

                  <DropdownMenuItem
                    className="text-xs"
                    onSelect={(event) => {
                      event.preventDefault();
                      toggleTheme();
                    }}
                  >
                    <Moon className="size-3.5" />
                    <span className="flex-1">深色模式</span>
                    <Switch
                      size="sm"
                      checked={resolvedTheme === "dark"}
                      onCheckedChange={toggleTheme}
                    />
                  </DropdownMenuItem>

                  {(admin || role === "ops") && !remotePanel && (
                    <DropdownMenuItem
                      className="text-xs"
                      onSelect={() => navigate("settings")}
                    >
                      <Settings className="size-3.5" />
                      <span>系统与安全设置</span>
                    </DropdownMenuItem>
                  )}

                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    variant="destructive"
                    className="text-xs"
                    onSelect={() =>
                      void run(async () => {
                        await api("/logout", "POST", {});
                        sessionEnded();
                      })
                    }
                  >
                    <LogOut className="size-3.5" />
                    <span>退出登录</span>
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </header>

          <main className="app-content">
            {page !== "fleet" && (
              <div className="mb-5 flex flex-wrap items-center justify-between gap-3">
                <div>
                  <h1 className="text-xl font-semibold tracking-tight text-foreground sm:text-2xl">
                    {current.label}
                  </h1>
                </div>
                <div className="flex flex-wrap items-center gap-2">
                  {page === "lines" && admin && (
                    <Button disabled={state.busy} onClick={() => editLine(kind)}>
                      <Plus />
                      新建代理
                    </Button>
                  )}
                  {page === "monitoring" && pageAllowed && (
                    <Button
                      variant="outline"
                      onClick={() =>
                        setModal(
                          <Diagnostics
                            nodes={state.nodes}
                            canSyncTime={admin}
                            close={close}
                          />,
                        )
                      }
                    >
                      <FileText />
                      日志与诊断
                    </Button>
                  )}
                </div>
              </div>
            )}

            {!connected && (
              <div
                role="alert"
                className="mb-5 rounded-lg border border-destructive/25 bg-destructive/10 px-4 py-3 text-xs font-medium text-destructive"
              >
                与控制台的连接暂时中断，当前展示最近一次同步的数据。
              </div>
            )}

            {latest && (state.busy || latest.state !== "applied") && (
              <div
                role="status"
                className={`mb-5 rounded-lg border px-4 py-3 text-xs ${
                  state.busy
                    ? "border-primary/25 bg-primary/10 text-foreground"
                    : "border-amber-500/30 bg-amber-500/10 text-foreground"
                }`}
              >
                <strong className="font-semibold">
                  {jobNames[latest.state] || latest.state}
                </strong>
                {latest.events.at(-1)?.message && (
                  <p className="mt-1 leading-relaxed text-muted-foreground">
                    {latest.events.at(-1)?.message}
                  </p>
                )}
              </div>
            )}

            {!pageAllowed && (
              <div
                role="alert"
                className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-card p-5 text-sm"
              >
                <span>当前身份无权访问此页面。</span>
                <Button variant="outline" size="sm" onClick={() => navigate("dashboard")}>
                  返回总览
                </Button>
              </div>
            )}

            {pageAllowed && (
              <>
                {remotePanel && (
                  <div
                    role="status"
                    className="mb-4 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-primary/25 bg-primary/10 px-4 py-2.5 text-xs text-foreground"
                  >
                    <span>正在管理远端 VPS 面板，当前页修改直接作用于该机器。</span>
                    <a className="font-semibold text-primary hover:underline" href="/admin#fleet">
                      返回多机总控
                    </a>
                  </div>
                )}

                {page === "dashboard" && (
                  <Dashboard
                    state={state}
                    manage={
                      billing
                        ? (line) =>
                            line.customer_plan_id
                              ? navigate("customers")
                              : setModal(
                                  <RentalEditor
                                    line={line}
                                    state={state}
                                    close={close}
                                    saved={refresh}
                                  />,
                                )
                        : undefined
                    }
                  />
                )}

                {page === "fleet" && <Fleet state={state} />}
                {page === "monitoring" && (
                  <Monitoring lines={state.lines} nodes={state.nodes} />
                )}
                {page === "customers" && <Customers state={state} saved={refresh} />}
                {page === "lines" && (
                  <ProxyList
                    state={state}
                    saved={refresh}
                    remote={!!remotePanel}
                    configureImport={(pendingImport) =>
                      setEditor({ kind: "vless", pendingImport })
                    }
                    create={() => editLine(kind)}
                    edit={(line) => editLine(line.kind, line)}
                    share={(line) =>
                      setModal(<AccessDialog line={line} state={state} close={close} />)
                    }
                    probe={(line) =>
                      setModal(
                        <ProbeEditor
                          line={line}
                          state={state}
                          close={close}
                          saved={refresh}
                        />,
                      )
                    }
                    apply={(line) => void run(() => apply(line?.id))}
                  />
                )}
                {page === "resources" && <Resources state={state} saved={refresh} />}
                {page === "rules" && <RulesLibrary />}

                {page === "nodes" && (
                  <Card>
                    <CardHeader className="gap-3">
                      <CardHeading className="flex items-center gap-2 space-y-0">
                        <CardTitle>已导入落地节点</CardTitle>
                        <Badge variant="secondary" appearance="light" size="sm">
                          {filteredExternals.length} / {state.externals.length}
                        </Badge>
                      </CardHeading>
                      <CardToolbar className="flex-wrap">
                        {state.externals.length > 0 && (
                          <div className="relative w-full sm:w-56">
                            <Search className="pointer-events-none absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
                            <Input
                              className="pl-8.5"
                              aria-label="搜索外部代理"
                              placeholder="搜索节点名称或地址"
                              value={externalSearch}
                              onChange={(event) => setExternalSearch(event.target.value)}
                            />
                          </div>
                        )}
                        <Button
                          variant="outline"
                          onClick={() =>
                            setModal(
                              <BatchImport state={state} close={close} saved={refresh} />,
                            )
                          }
                        >
                          批量导入
                        </Button>
                        <Button
                          variant="outline"
                          onClick={() =>
                            setModal(
                              <AutoGroups state={state} close={close} saved={refresh} />,
                            )
                          }
                        >
                          自动分组
                        </Button>
                        <Button onClick={() => importExternal()}>
                          <Plus />
                          导入外部代理
                        </Button>
                      </CardToolbar>
                    </CardHeader>

                    {filteredExternals.length ? (
                      <CardTable>
                        <div className="overflow-x-auto fleet-responsive" role="region" aria-label="外部代理表">
                          <table className="data-table">
                            <thead>
                              <tr>
                                <th>节点名称</th>
                                <th>协议 / 地址</th>
                                <th>资源分组</th>
                                <th className="text-right">操作</th>
                              </tr>
                            </thead>
                            <tbody>
                              {filteredExternals.map(({ external: e, protocol, target }) => {
                                const groupName = state.resources?.find((r) =>
                                  r.members.includes(`external:${e.id}`),
                                )?.name;
                                return (
                                  <tr key={e.id}>
                                    <td data-label="节点名称">
                                      <p className="break-all font-semibold text-foreground">
                                        {e.name}
                                      </p>
                                    </td>
                                    <td data-label="协议 / 地址">
                                      <div className="flex flex-wrap items-center gap-2">
                                        <Badge variant="primary" appearance="light" size="sm">
                                          {protocol}
                                        </Badge>
                                        <span className="mono break-all text-xs text-muted-foreground">
                                          {target}
                                        </span>
                                      </div>
                                    </td>
                                    <td data-label="资源分组">
                                      {groupName ? (
                                        <Badge variant="info" appearance="light" size="sm">
                                          {groupName}
                                        </Badge>
                                      ) : (
                                        <span className="text-xs text-muted-foreground">
                                          未分组
                                        </span>
                                      )}
                                    </td>
                                    <td data-label="操作">
                                      <div className="flex flex-wrap items-center justify-start gap-1.5 md:justify-end">
                                        <Button
                                          size="sm"
                                          variant="outline"
                                          onClick={() =>
                                            setModal(
                                              <AccessDialog
                                                external={e}
                                                state={state}
                                                close={close}
                                              />,
                                            )
                                          }
                                        >
                                          链接 / Mihomo
                                        </Button>
                                        <Button
                                          size="sm"
                                          variant="outline"
                                          onClick={() =>
                                            editLine("vless", undefined, [`external:${e.id}`])
                                          }
                                        >
                                          用于新建代理
                                        </Button>
                                        <RadixDropdown.Root>
                                          <RadixDropdown.Trigger
                                            className="row-menu-trigger"
                                            aria-label={`${e.name} 更多操作`}
                                          >
                                            <MoreHorizontal className="size-4" />
                                          </RadixDropdown.Trigger>
                                          <RadixDropdown.Portal>
                                            <RadixDropdown.Content
                                              className="row-menu-content"
                                              align="end"
                                              sideOffset={4}
                                              collisionPadding={8}
                                            >
                                              <RadixDropdown.Item
                                                onSelect={() =>
                                                  setModal(
                                                    <ManualGroup
                                                      external={e}
                                                      state={state}
                                                      close={close}
                                                      saved={refresh}
                                                    />,
                                                  )
                                                }
                                              >
                                                <FolderKanban />
                                                设置资源组
                                              </RadixDropdown.Item>
                                              <RadixDropdown.Item
                                                onSelect={() =>
                                                  setModal(
                                                    <Modal
                                                      title="编辑外部代理"
                                                      description="保存后将预览并同步受影响的链式代理。"
                                                      submit="保存"
                                                      close={close}
                                                      action={async (data) => {
                                                        await api(`/externals/${e.id}`, "POST", {
                                                          name: field(data, "name"),
                                                          url: field(data, "url"),
                                                        });
                                                        await refresh();
                                                        close();
                                                        await apply();
                                                      }}
                                                    >
                                                      <Field label="名称">
                                                        <Input
                                                          name="name"
                                                          defaultValue={e.name}
                                                          required
                                                        />
                                                      </Field>
                                                      <Field label="分享链接或单节点配置">
                                                        <textarea
                                                          className="textarea mono"
                                                          name="url"
                                                          rows={6}
                                                          defaultValue={e.url}
                                                          required
                                                        />
                                                      </Field>
                                                    </Modal>,
                                                  )
                                                }
                                              >
                                                <Pencil />
                                                编辑配置
                                              </RadixDropdown.Item>
                                              <RadixDropdown.Separator className="my-1 h-px bg-border" />
                                              <RadixDropdown.Item
                                                className="text-destructive"
                                                onSelect={() =>
                                                  setModal(
                                                    <Modal
                                                      title="删除外部代理"
                                                      description={`确定移除「${e.name}」吗？`}
                                                      submit="删除"
                                                      close={close}
                                                      action={async () => {
                                                        await api(
                                                          `/externals/${e.id}`,
                                                          "DELETE",
                                                        );
                                                        await refresh();
                                                        close();
                                                      }}
                                                    />,
                                                  )
                                                }
                                              >
                                                <Trash2 />
                                                删除节点
                                              </RadixDropdown.Item>
                                            </RadixDropdown.Content>
                                          </RadixDropdown.Portal>
                                        </RadixDropdown.Root>
                                        <Button
                                          variant="ghost"
                                          size="icon"
                                          className="sr-only"
                                          tabIndex={-1}
                                          aria-label={`删除资源 ${e.name}`}
                                          onClick={() =>
                                            setModal(
                                              <Modal
                                                title="删除外部代理"
                                                description={`确定移除「${e.name}」吗？`}
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
                                    </td>
                                  </tr>
                                );
                              })}
                            </tbody>
                          </table>
                        </div>
                      </CardTable>
                    ) : (
                      <Empty
                        icon={Globe}
                        title={state.externals.length ? "没有匹配的外部代理" : "尚未导入外部代理"}
                        action={
                          !state.externals.length && (
                            <Button onClick={() => importExternal()}>
                              <Plus />
                              导入外部代理
                            </Button>
                          )
                        }
                      >
                        导入第三方节点链接或 Mihomo 配置，即可作为链式代理的出口落地。
                      </Empty>
                    )}
                  </Card>
                )}

                {page === "settings" && (
                  <div className="space-y-6">
                    <Maintenance state={state} />
                    {admin && (
                      <SecuritySettings
                        state={state}
                        saved={refresh}
                        sessionEnded={sessionEnded}
                      />
                    )}
                  </div>
                )}
              </>
            )}

            <footer className="mt-10 flex flex-wrap items-center justify-between gap-2 border-t border-border pt-4 text-[11px] text-muted-foreground">
              <span>
                Chain Panel {state.version ? `v${state.version}` : "版本未知"}
              </span>
              <span className="flex items-center gap-1.5">
                <UserCheck className="size-3" />
                {roleNames[role]}视图
              </span>
            </footer>
          </main>
        </div>

        {editor && (
          <LineEditor
            {...editor}
            state={state}
            defaultHost={
              remotePanel
                ? state.nodes.find((n) => n.id === state.local_node_id)?.host || ""
                : location.hostname.replace(/^\[|\]$/g, "")
            }
            ensureLocalServer={ensureLocalServer}
            importExternal={importExternal}
            close={() => setEditor(null)}
            saved={async (value, publish = true) => {
              setEditor(null);
              await refresh();
              if (!publish) {
                setToast(
                  "草稿已保存，可在客户与收款中绑定共享套餐，或在代理列表继续应用。",
                );
                return;
              }
              try {
                await apply(
                  value[0].id,
                  value.length === 1 && value[0].kind === "vless",
                  value.map((line) => line.id),
                );
              } catch (e) {
                setToast(
                  `配置已保存，尚未应用：${errorText(e)}。请在代理列表继续应用，无需重新创建。`,
                );
              }
            }}
          />
        )}

        {modal}

        <DialogContent
          showCloseButton={false}
          aria-describedby={undefined}
          className="inset-y-0 left-0 top-0 flex h-dvh max-h-dvh w-64 max-w-[85vw] translate-x-0 translate-y-0 flex-col overflow-y-auto rounded-none border-l-0 bg-sidebar-bg p-0 sm:rounded-none"
        >
          <DialogTitle className="sr-only">管理导航</DialogTitle>
          {sidebar}
        </DialogContent>

        {toast && (
          <div
            role="status"
            className="fixed bottom-6 left-1/2 z-[100] max-w-[90%] -translate-x-1/2 rounded-xl border border-border bg-zinc-900 px-4 py-2.5 text-xs font-medium text-white shadow-xl dark:bg-zinc-100 dark:text-zinc-950"
          >
            {toast}
          </div>
        )}
      </div>
    </Dialog>
  );
}
