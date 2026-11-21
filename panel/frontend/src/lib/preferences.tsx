import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

export type ThemeSetting = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

export const workspaceModes = [
  {
    id: "personal",
    label: "自用精简",
    shortLabel: "自用",
    description: "侧栏优先展示代理、外部落地、总览与设置，其余模块收纳在展开菜单中。",
    pages: ["lines", "nodes", "dashboard", "settings"],
  },
  {
    id: "small",
    label: "小型运营",
    shortLabel: "小型运营",
    description: "平铺展示全部 9 个功能模块，按日常开通与运维动线排序。",
    pages: ["dashboard", "lines", "customers", "nodes", "fleet", "monitoring", "resources", "rules", "settings"],
  },
  {
    id: "business",
    label: "商业运营",
    shortLabel: "商业运营",
    description: "优先置顶客户账务、代理交付与多机总控，适合多节点租赁团队。",
    pages: ["dashboard", "customers", "lines", "fleet", "monitoring", "nodes", "resources", "rules", "settings"],
  },
] as const;

export type WorkspaceMode = (typeof workspaceModes)[number]["id"];

type PreferencesContextValue = {
  theme: ThemeSetting;
  resolvedTheme: ResolvedTheme;
  setTheme: (next: ThemeSetting) => void;
  toggleTheme: () => void;
  mode: WorkspaceMode;
  setMode: (next: WorkspaceMode) => void;
};

const THEME_KEY = "chain-theme";
const MODE_KEY = "chain-workspace-mode";

function readInitialTheme(): ThemeSetting {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark" || saved === "system") return saved;
  } catch {}
  return "light";
}

function readInitialMode(): WorkspaceMode {
  try {
    const saved = localStorage.getItem(MODE_KEY);
    const found = workspaceModes.find((item) => item.id === saved);
    if (found) return found.id;
  } catch {}
  return "small";
}

function systemPrefersDark(): boolean {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-color-scheme: dark)").matches;
}

const PreferencesContext = createContext<PreferencesContextValue>({
  theme: "light",
  resolvedTheme: "light",
  setTheme: () => {},
  toggleTheme: () => {},
  mode: "small",
  setMode: () => {},
});

export function PreferencesProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<ThemeSetting>(readInitialTheme);
  const [systemDark, setSystemDark] = useState<boolean>(systemPrefersDark);
  const [mode, setModeState] = useState<WorkspaceMode>(readInitialMode);

  useEffect(() => {
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    if (!media) return;
    const listener = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    media.addEventListener("change", listener);
    return () => media.removeEventListener("change", listener);
  }, []);

  const resolvedTheme: ResolvedTheme = theme === "system" ? (systemDark ? "dark" : "light") : theme;

  useEffect(() => {
    const root = document.documentElement;
    const freeze = document.createElement("style");
    freeze.textContent = "*,*::before,*::after{transition:none!important}";
    document.head.appendChild(freeze);
    root.classList.toggle("dark", resolvedTheme === "dark");
    root.style.colorScheme = resolvedTheme;
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute("content", resolvedTheme === "dark" ? "#0b0f19" : "#f6f8fb");
    void root.offsetHeight;
    const frame = requestAnimationFrame(() => freeze.remove());
    return () => {
      cancelAnimationFrame(frame);
      freeze.remove();
    };
  }, [resolvedTheme]);

  const setTheme = useCallback((next: ThemeSetting) => {
    setThemeState(next);
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch {}
  }, []);

  const toggleTheme = useCallback(() => {
    setTheme(resolvedTheme === "dark" ? "light" : "dark");
  }, [resolvedTheme, setTheme]);

  const setMode = useCallback((next: WorkspaceMode) => {
    setModeState(next);
    try {
      localStorage.setItem(MODE_KEY, next);
    } catch {}
  }, []);

  const value = useMemo(
    () => ({ theme, resolvedTheme, setTheme, toggleTheme, mode, setMode }),
    [theme, resolvedTheme, setTheme, toggleTheme, mode, setMode],
  );

  return <PreferencesContext.Provider value={value}>{children}</PreferencesContext.Provider>;
}

export function usePreferences() {
  return useContext(PreferencesContext);
}
