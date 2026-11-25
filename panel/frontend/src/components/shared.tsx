import {
  useState,
  useId,
  cloneElement,
  isValidElement,
  type ReactElement,
  type ReactNode,
  type FormEvent,
} from "react";
import { ChevronRight, Layers3, Copy, Check, CircleHelp } from "lucide-react";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";
import type { Line, PanelState } from "../types";

export const errorText = (e: unknown) =>
  e instanceof Error ? e.message : "操作失败，请重试";
export const roleNames = { admin: "管理员", ops: "运维", finance: "财务", viewer: "只读" };
export const field = (data: FormData, key: string) =>
  String(data.get(key) || "").trim();
export const number = (data: FormData, key: string) =>
  field(data, key) ? Number(field(data, key)) : null;
export const stamp = (n: number) =>
  n
    ? new Date(n * 1000).toLocaleString("zh-CN", { hour12: false })
    : "尚未连接";
export const bytes = (n: number) => {
  const i = n > 0 ? Math.max(0, Math.min(4, Math.floor(Math.log(n) / Math.log(1024)))) : 0;
  return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${["B", "KiB", "MiB", "GiB", "TiB"][i]}`;
};
export const protocolNames = {
  vless: "VLESS",
  http: "HTTP 代理",
  socks: "SOCKS5",
  shadowsocks: "Shadowsocks",
  trojan: "Trojan / TLS",
  vmess: "VMess AEAD",
  hysteria2: "Hysteria 2",
  tuic: "TUIC v5",
};

export function InfoTip({
  children,
  label = "查看说明",
}: {
  children: ReactNode;
  label?: string;
}) {
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label={label}
          title={label}
          className="inline-flex size-4 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted-foreground/75 transition-colors hover:text-primary focus-visible:outline-2 focus-visible:outline-ring"
        >
          <CircleHelp className="size-3.5" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72 p-3 text-xs leading-relaxed text-muted-foreground">
        {children}
      </PopoverContent>
    </Popover>
  );
}

export function Field({
  label,
  children,
  hint,
  tooltip,
}: {
  label: string;
  children: ReactNode;
  hint?: string;
  tooltip?: ReactNode;
}) {
  const id = useId();
  const control = isValidElement(children)
    ? cloneElement(
        children as ReactElement<{ id?: string; "aria-describedby"?: string }>,
        { id, "aria-describedby": hint ? `${id}-hint` : undefined },
      )
    : children;
  return (
    <div className="field">
      <div className="flex items-center gap-1.5">
        <label htmlFor={id} className="text-[13px] font-medium text-foreground">
          {label}
        </label>
        {tooltip && <InfoTip>{tooltip}</InfoTip>}
      </div>
      {control}
      {hint && (
        <span
          id={`${id}-hint`}
          className="text-xs font-normal leading-5 text-muted-foreground"
        >
          {hint}
        </span>
      )}
    </div>
  );
}

export function Status({
  children,
  tone = "muted",
}: {
  children: ReactNode;
  tone?: "good" | "bad" | "warn" | "info" | "muted";
}) {
  return (
    <Badge
      variant={
        (
          {
            good: "success",
            bad: "destructive",
            warn: "warning",
            info: "primary",
            muted: "secondary",
          } as const
        )[tone]
      }
      appearance="light"
      size="sm"
    >
      {children}
    </Badge>
  );
}

export function Modal({
  title,
  description,
  children,
  submit,
  secondarySubmit,
  action,
  close,
  wide,
  drawer,
  protectChanges = true,
  dirty: changed = false,
}: {
  title: string;
  description?: string;
  children?: ReactNode;
  submit?: string;
  secondarySubmit?: { name: string; label: string };
  action?: (data: FormData) => Promise<void>;
  close: () => void;
  wide?: boolean;
  drawer?: boolean;
  protectChanges?: boolean;
  dirty?: boolean;
}) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [dirty, setDirty] = useState(false);
  const dismiss = () => {
    if (
      !pending &&
      (!(drawer || protectChanges) ||
        !action ||
        (!dirty && !changed) ||
        window.confirm("放弃尚未保存的修改？"))
    ) {
      close();
    }
  };
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!action || pending) return;
    const data = new FormData(
      event.currentTarget,
      (event.nativeEvent as SubmitEvent).submitter,
    );
    setPending(true);
    setError("");
    try {
      await action(data);
      setDirty(false);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setPending(false);
    }
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) dismiss();
      }}
    >
      <DialogContent
        {...(!description ? { "aria-describedby": undefined } : {})}
        className={
          drawer
            ? "inset-y-0 right-0 left-auto top-0 flex h-dvh max-h-dvh w-full max-w-2xl translate-x-0 translate-y-0 flex-col overflow-hidden rounded-none border-l border-border p-0 sm:rounded-none"
            : `flex max-h-[92dvh] w-[calc(100%-1.5rem)] ${wide ? "max-w-3xl" : "max-w-xl"} flex-col overflow-hidden rounded-xl p-0`
        }
      >
        <DialogHeader className="mb-0 shrink-0 border-b border-border bg-card px-6 py-4">
          <DialogTitle className="break-words pe-7 text-base font-semibold leading-6">
            {title}
          </DialogTitle>
          {description && (
            <DialogDescription className="break-words pt-1 text-xs leading-relaxed text-muted-foreground">
              {description}
            </DialogDescription>
          )}
        </DialogHeader>
        <form
          onSubmit={save}
          onChangeCapture={() => setDirty(true)}
          className="flex min-h-0 flex-1 flex-col"
        >
          <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-6 py-5">
            {children}
            {error && (
              <p
                role="alert"
                className="rounded-lg border border-destructive/25 bg-destructive/10 p-3 text-xs leading-relaxed text-destructive"
              >
                {error}
              </p>
            )}
          </div>
          <DialogFooter className="mt-0 shrink-0 gap-2 border-t border-border bg-muted/25 px-6 py-3.5 sm:space-x-0">
            <Button
              type="button"
              variant="outline"
              disabled={pending}
              onClick={dismiss}
            >
              关闭
            </Button>
            {secondarySubmit && (
              <Button
                type="submit"
                name={secondarySubmit.name}
                value="true"
                variant="outline"
                disabled={pending}
              >
                {secondarySubmit.label}
              </Button>
            )}
            {submit && (
              <Button type="submit" disabled={pending}>
                {pending ? "处理中…" : submit}
              </Button>
            )}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function CopyButton({
  value,
  label = "复制",
  size = "md",
}: {
  value: string;
  label?: string;
  size?: "sm" | "md";
}) {
  const [done, setDone] = useState(false);
  const [error, setError] = useState("");
  return (
    <>
      <Button
        type="button"
        variant="outline"
        size={size}
        onClick={async () => {
          try {
            if (navigator.clipboard && window.isSecureContext)
              await navigator.clipboard.writeText(value);
            else {
              const el = document.createElement("textarea");
              el.value = value;
              el.style.position = "fixed";
              document.body.appendChild(el);
              el.select();
              const ok = document.execCommand("copy");
              el.remove();
              if (!ok) throw new Error("请手动选择文本复制");
            }
            setDone(true);
            setTimeout(() => setDone(false), 2000);
          } catch (e) {
            setError(errorText(e));
          }
        }}
      >
        {done ? <Check /> : <Copy />}
        {done ? "已复制" : label}
      </Button>
      {error && (
        <span role="alert" className="text-xs text-destructive">
          {error}
        </span>
      )}
    </>
  );
}

export function PathView({ line, state }: { line: Line; state: PanelState }) {
  const names = line.hops.map(
    (h) => state.nodes.find((n) => n.id === h.node_id)?.name || "已移除节点",
  );
  const chain = line.chain || [];
  names.push(
    ...chain.map((reference) => {
      const [kind, id] = reference.split(":");
      return (
        (kind === "line"
          ? state.lines.find((l) => l.id === id)?.name
          : state.externals.find((e) => e.id === id)?.name) || "已移除代理"
      );
    }),
    "互联网",
  );
  return (
    <div className="flex flex-wrap items-center gap-1">
      {names.map((name, i) => (
        <span key={i} className="inline-flex items-center gap-1">
          {i > 0 && <ChevronRight className="size-3 text-muted-foreground/70" />}
          <span
            className={`max-w-44 truncate rounded-md border px-1.5 py-0.5 text-[11px] font-medium ${
              i === 0
                ? "border-blue-200 bg-blue-50 text-blue-700 dark:border-blue-500/30 dark:bg-blue-500/15 dark:text-blue-300"
                : "border-border bg-muted/40 text-muted-foreground"
            }`}
            title={name}
          >
            {name}
          </span>
        </span>
      ))}
    </div>
  );
}

export function Empty({
  title,
  children,
  action,
  icon: Icon = Layers3,
}: {
  title: string;
  children?: ReactNode;
  action?: ReactNode;
  icon?: typeof Layers3;
}) {
  return (
    <div className="flex flex-col items-center justify-center px-6 py-12 text-center">
      <span className="mb-3.5 flex size-12 items-center justify-center rounded-xl border border-primary/15 bg-primary/10 text-primary">
        <Icon className="size-5" />
      </span>
      <h3 className="text-sm font-semibold text-foreground">{title}</h3>
      {children && (
        <p className="mt-1.5 max-w-md text-xs leading-relaxed text-muted-foreground">
          {children}
        </p>
      )}
      {action && <div className="mt-5">{action}</div>}
    </div>
  );
}

export function download(
  name: string,
  content: string,
  type = name.endsWith(".csv")
    ? "text/csv;charset=utf-8"
    : /\.ya?ml$/i.test(name)
      ? "text/yaml;charset=utf-8"
      : "text/plain;charset=utf-8",
) {
  const url = URL.createObjectURL(new Blob([content], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
