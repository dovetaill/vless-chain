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
