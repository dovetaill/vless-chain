import { useState } from "react";
import { api } from "../api";
import type { PanelState } from "../types";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Field, InfoTip, Modal, Status, errorText } from "./shared";

type Config = {
  id: string;
  node_id: string;
  name: string;
  enabled: boolean;
  config: unknown;
  source_inbounds?: unknown[];
};

function Editor({
  value,
  close,
  saved,
}: {
  value: Config;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [name, setName] = useState(value.name);
  const [enabled, setEnabled] = useState(value.enabled);
  const [config, setConfig] = useState(JSON.stringify(value.config, null, 2));
  return (
    <Modal
      wide
      title="编辑迁移配置"
      description="保留原面板完整用户、路由、DNS 与出口结构。配置内含连接凭据，请勿公开分享。"
      close={close}
      submit="校验并保存草稿"
      action={async () => {
        await api(`/legacy-configs/${value.id}`, "PUT", {
          ...value,
          name,
          enabled,
          config: JSON.parse(config),
        });
        await saved();
        close();
      }}
    >
      <Field label="名称">
        <Input value={name} onChange={(e) => setName(e.target.value)} />
      </Field>
      <label className="flex items-center gap-2 text-xs font-medium text-foreground">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
        />
        启用这组代理
      </label>
      <Field
        label="完整 Xray 配置"
        hint="保存后需点击下发才会生效；已有 UUID、密码和端口保持原值"
      >
        <textarea
          className="textarea mono min-h-80 text-xs"
          spellCheck={false}
          value={config}
          onChange={(e) => setConfig(e.target.value)}
        />
      </Field>
      {!!value.source_inbounds?.length && (
        <details className="details">
          <summary>原面板入口配置备份（含未启用入口）</summary>
          <div className="mt-3.5 space-y-3">
            <p className="muted">
              仅作迁移对照保留，不会自动启用原面板中未运行的入口或用户。
            </p>
            <textarea
              aria-label="原面板入口配置备份"
              readOnly
              className="textarea mono min-h-48 text-xs"
              value={JSON.stringify(value.source_inbounds, null, 2)}
            />
          </div>
        </details>
      )}
    </Modal>
  );
}

export function LegacyConfigs({
  state,
  saved,
  apply,
}: {
  state: PanelState;
  saved: () => Promise<void>;
  apply: () => void;
}) {
  const [editing, setEditing] = useState<Config | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const profiles = state.legacy_configs || [];
  if (!profiles.length) return null;
  return (
    <section
      aria-label="已迁移的完整代理配置"
      className="space-y-3 border-b border-border px-5 py-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          <h3 className="text-sm font-semibold text-foreground">
            已迁移的代理配置
          </h3>
          <InfoTip>
            每组迁移配置独立运行，完整保留原面板多用户、传输与分流路由，不与下方单条代理表单混用。
          </InfoTip>
        </div>
        <Button
          size="sm"
          variant="outline"
          disabled={state.busy || loading}
          onClick={apply}
        >
          下发配置
        </Button>
      </div>
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
      <div className="divide-y divide-border rounded-lg border border-border bg-muted/20">
        {profiles.map((p) => (
          <div
            key={p.id}
            className="flex flex-wrap items-center justify-between gap-3 px-4 py-3"
          >
            <div className="space-y-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-xs font-semibold text-foreground">
                  {p.name}
                </span>
                {p.pending_delete ? (
                  <Status tone="warn">待下发删除</Status>
                ) : (
                  <>
                    <Status tone={p.enabled ? "good" : "muted"}>
                      {p.enabled ? "启用" : "停用"}
                    </Status>
                    <Status tone={p.published ? "info" : "warn"}>
                      {p.published ? "已下发" : "待下发"}
                    </Status>
                  </>
                )}
              </div>
              {p.pending_delete && (
                <p className="muted">删除尚未下发，运行中的配置仍保持生效。</p>
              )}
              <p className="muted">
                {state.nodes.find((n) => n.id === p.node_id)?.name || "未知节点"}{" "}
                · 端口 {p.ports.slice(0, 12).join("、")}
                {p.ports.length > 12 ? ` 等 ${p.ports.length} 个` : ""}
              </p>
            </div>
            {!p.pending_delete && (
              <div className="flex gap-1.5">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={state.busy || loading}
                  onClick={async () => {
                    setLoading(true);
                    setError("");
                    try {
                      setEditing(await api<Config>(`/legacy-configs/${p.id}`));
                    } catch (e) {
                      setError(errorText(e));
                    } finally {
                      setLoading(false);
                    }
                  }}
                >
                  查看与编辑
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={state.busy || loading}
                  onClick={() => setRemoving(p.id)}
                >
                  删除
                </Button>
              </div>
            )}
          </div>
        ))}
      </div>
      {editing && (
        <Editor
          value={editing}
          close={() => setEditing(null)}
          saved={saved}
        />
      )}
      {removing && (
        <Modal
          title="删除整组迁移配置"
          description="删除该组全部入口、用户和路由草稿；下发后停止对应代理，原服务备份不受影响。"
          submit="删除草稿"
          close={() => setRemoving(null)}
          action={async () => {
            await api(`/legacy-configs/${removing}`, "DELETE");
            await saved();
            setRemoving(null);
          }}
        >
          <p className="rounded-lg border border-border bg-muted/25 px-3 py-2 text-xs font-medium">
            {profiles.find((p) => p.id === removing)?.name}
          </p>
        </Modal>
      )}
    </section>
  );
}
