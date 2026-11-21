import { useState } from "react";
import { api } from "../api";
import type { Line, PanelState, Check as CheckResult } from "../types";
import { Modal, Field, PathView, field } from "./shared";
import { Input } from "./ui/input";

export function ProbeEditor({
  line,
  state,
  close,
  saved,
  request = api,
}: {
  line: Line;
  state: PanelState;
  close: () => void;
  saved: () => Promise<void>;
  request?: typeof api;
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
        const r = await request<CheckResult>(`/lines/${line.id}/probe`, "POST", {
          mode,
          url: field(data, "url"),
        });
        setResult(r);
        await saved();
      }}
    >
      <PathView line={line} state={state} />
      <div className="grid gap-3.5 sm:grid-cols-2">
        <Field label="检查方式">
          <select
            className="select"
            value={mode}
            onChange={(e) => {
              setMode(e.target.value);
              setResult(null);
            }}
          >
            <option value="tcp">TCP 端口连通性</option>
            <option value="http">HTTP / HTTPS 完整链路</option>
          </select>
        </Field>
        {mode === "http" && line.kind !== "website" && (
          <Field label="检查网址">
            <Input
              name="url"
              defaultValue={line.kind === "vless" ? "https://example.com" : ""}
              placeholder="https://example.com"
              required
            />
          </Field>
        )}
      </div>

      {result && (
        <div
          role="status"
          className={`rounded-lg border p-3.5 text-xs leading-relaxed ${
            result.ok
              ? "border-emerald-500/30 bg-emerald-500/10 text-foreground"
              : "border-destructive/30 bg-destructive/10 text-foreground"
          }`}
        >
          <strong className="font-semibold">
            {result.ok ? "检查通过" : "检查未通过"}
          </strong>
          {result.latency_ms != null && (
            <span className="ml-2 text-muted-foreground tabular-nums">
              耗时 {result.latency_ms} ms
            </span>
          )}
          <p className="mt-1.5 break-all text-muted-foreground">{result.detail}</p>
        </div>
      )}
    </Modal>
  );
}
