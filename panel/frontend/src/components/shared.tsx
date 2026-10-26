import {
  useState,
  useId,
  cloneElement,
  isValidElement,
  type ReactElement,
  type ReactNode,
  type FormEvent,
} from "react";
import { ChevronRight, Layers3, Copy, Check } from "lucide-react";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
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
export const endpoint = (host: string, port: number | string) =>
  `${host.includes(":") && !host.startsWith("[") ? `[${host}]` : host}:${port}`;
export function splitAddress(s: string): [string, string] {
  try {
    const u = new URL(`tcp://${s}`);
    return [u.hostname.replace(/^\[|\]$/g, ""), u.port];
  } catch {
    return [s, ""];
  }
}
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
export function Field({
  label,
  children,
  hint,
}: {
  label: string;
  children: ReactNode;
  hint?: string;
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
      <label htmlFor={id}>{label}</label>
      {control}
      {hint && (
        <span
          id={`${id}-hint`}
          className="text-xs font-normal leading-5 text-slate-500"
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
  tone?: "good" | "bad" | "warn" | "muted";
}) {
  return (
    <Badge
      variant={
        (
          {
            good: "success",
            bad: "destructive",
            warn: "warning",
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
    if (!pending && (!(drawer || protectChanges) || !action || (!dirty && !changed) || window.confirm('放弃尚未保存的修改？'))) close();
  };
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!action || pending) return;
    const data = new FormData(event.currentTarget, (event.nativeEvent as SubmitEvent).submitter);
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
        className={drawer ? "inset-y-0 right-0 left-auto top-0 h-dvh max-h-dvh w-full max-w-3xl translate-x-0 translate-y-0 overflow-y-auto rounded-none p-6 sm:rounded-none sm:p-7" : `max-h-[92dvh] w-[calc(100%-2rem)] ${wide ? "max-w-3xl" : "max-w-2xl"} overflow-y-auto rounded-xl p-6 sm:p-7`}
      >
        <DialogHeader>
          <DialogTitle className="break-words pe-6">{title}</DialogTitle>
          {description && <DialogDescription className="break-words pt-2 leading-relaxed">
            {description}
          </DialogDescription>}
        </DialogHeader>
        <form onSubmit={save} onChangeCapture={() => setDirty(true)} className="space-y-5">
          {children}
          {error && (
            <p
              role="alert"
              className="rounded-lg bg-red-50 p-3 text-sm text-red-700"
            >
              {error}
            </p>
          )}
          <DialogFooter className="gap-2 border-t border-border pt-4">
            <Button
              type="button"
              variant="outline"
              disabled={pending}
              onClick={dismiss}
            >
              关闭
            </Button>
            {submit && (
              <Button type="submit" disabled={pending}>
                {pending ? "正在处理…" : submit}
              </Button>
            )}
            {secondarySubmit && <Button type="submit" name={secondarySubmit.name} value="true" variant="outline" disabled={pending}>{secondarySubmit.label}</Button>}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
export function CopyButton({
  value,
  label = "复制",
}: {
  value: string;
  label?: string;
}) {
  const [done, setDone] = useState(false);
  const [error, setError] = useState("");
  return (
    <>
      <Button
        type="button"
        variant="outline"
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
          } catch (e) {
            setError(errorText(e));
          }
        }}
      >
        {done ? <Check /> : <Copy />}
        {done ? "已复制" : label}
      </Button>
      {error && (
        <span role="alert" className="text-xs text-red-600">
          {error}
        </span>
      )}
    </>
  );
}
export function PathView({ line, state }: { line: Line; state: PanelState }) {
  const names = line.hops.map(
    (h) => state.nodes.find((n) => n.id === h.node_id)?.name || "已移除服务器",
  );
  const chain = line.chain?.length ? line.chain : line.external_id ? [`external:${line.external_id}`] : [];
  names.push(...chain.map(reference => {
    const [kind, id] = reference.split(":");
    return (kind === "line" ? state.lines.find(l => l.id === id)?.name : state.externals.find(e => e.id === id)?.name) || "已移除代理";
  }), "互联网");
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {names.map((name, i) => (
        <span key={i} className="inline-flex items-center gap-1.5">
          {i > 0 && <ChevronRight className="size-3 text-slate-400" />}
          <span
            className={`max-w-56 truncate rounded-md border px-2 py-1 text-xs ${i === 0 ? "border-blue-100 bg-blue-50 text-blue-700" : "border-border bg-white text-slate-600"}`}
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
}: {
  title: string;
  children?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center px-6 py-14 text-center">
      <span className="mb-5 flex size-14 items-center justify-center rounded-2xl border border-blue-100 bg-blue-50 text-blue-600">
        <Layers3 className="size-6" />
      </span>
      <h3 className="text-base font-semibold">{title}</h3>
      <p className="muted mt-2 max-w-md">{children}</p>
      <div className="mt-6">{action}</div>
    </div>
  );
}
export function download(name: string, content: string) {
  const url = URL.createObjectURL(
    new Blob([content], { type: "text/yaml;charset=utf-8" }),
  );
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
