import { useEffect, useState } from "react";
import QRCode from "qrcode";
import { Download } from "lucide-react";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import {
  Modal,
  CopyButton,
  PathView,
  download,
  errorText,
  Field,
  field,
} from "./shared";
import { api } from "../api";
import type { Line, PanelState, External } from "../types";

export function QR({ value }: { value: string }) {
  const [url, setUrl] = useState("");
  const [error, setError] = useState("");
  useEffect(() => {
    let alive = true;
    setUrl("");
    void QRCode.toDataURL(value, {
      width: 232,
      margin: 2,
      errorCorrectionLevel: "M",
    })
      .then((url) => {
        if (alive) setUrl(url);
      })
      .catch(() => {
        if (alive) setError("链接过长，请直接复制下方文本或下载配置。");
      });
    return () => {
      alive = false;
    };
  }, [value]);
  return url ? (
    <div className="mx-auto w-fit rounded-xl border border-border bg-white p-2.5 shadow-2xs">
      <img src={url} width={220} height={220} alt="接入二维码" className="block rounded-lg" />
    </div>
  ) : (
    <p className="muted py-6 text-center">{error || "正在生成二维码…"}</p>
  );
}

export function AccessDialog({
  line,
  external,
  state,
  close,
  request = api,
  remote = false,
}: {
  line?: Line;
  external?: External;
  state: PanelState;
  close: () => void;
  request?: typeof api;
  remote?: boolean;
}) {
  const [tab, setTab] = useState("qr");
  const [subscription, setSubscription] = useState("");
  const localLine =
    line && !remote && !new URLSearchParams(location.search).has("remote");
  const [value, setValue] = useState<{
    share: string;
    node: string;
  } | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!line && !external) return;
    let alive = true;
    void request<{ share: string; node: string }>(
      external
        ? `/externals/${external.id}/export`
        : `/lines/${line!.id}/export`,
    )
      .then((v) => {
        if (alive) setValue(v);
      })
      .catch((e) => {
        if (alive) setError(errorText(e));
      });
    return () => {
      alive = false;
    };
  }, [line?.id, external?.id, request]);

  return (
    <Modal
      title={external ? "外部代理配置" : "分享代理"}
      description={external?.name || line?.name}
      close={close}
      submit={tab === "subscription" && !subscription ? "创建订阅链接" : undefined}
      action={
        tab === "subscription" && !subscription && localLine
          ? async (data) => {
              const result = await request<{ path: string }>(
                "/subscriptions",
                "POST",
                {
                  name: field(data, "subscription_name"),
                  line_ids: [line.id],
                },
              );
              setSubscription(new URL(result.path, location.origin).href);
            }
          : undefined
      }
    >
      {line && <PathView line={line} state={state} />}

      <div className="flex flex-wrap gap-1 rounded-lg bg-muted p-1">
        {[
          ["qr", "链接 / 二维码"],
          ["node", "Mihomo 节点"],
          ...(localLine ? [["subscription", "订阅链接"]] : []),
        ].map(([id, label]) => (
          <button
            type="button"
            key={id}
            className={`flex-1 cursor-pointer rounded-md px-3 py-1.5 text-xs font-medium transition-colors ${
              tab === id
                ? "bg-card text-foreground shadow-2xs"
                : "text-muted-foreground hover:text-foreground"
            }`}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </div>

      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}

      {!value && !error && <p className="muted">正在读取已生效配置…</p>}

      {value && tab === "qr" && (
        <div className="space-y-3.5">
          <QR value={value.share} />
          <textarea
            className="textarea mono"
            readOnly
            aria-label="分享链接"
            rows={3}
            value={value.share}
          />
          <CopyButton value={value.share} label="复制分享链接" />
        </div>
      )}

      {value && tab === "node" && (
        <div className="space-y-3.5">
          <p className="muted">
            这是单节点片段，不是完整客户端配置。请合并到客户端配置的 proxies 列表中。
          </p>
          <textarea
            className="textarea mono"
            readOnly
            aria-label="Mihomo 节点配置"
            rows={6}
            value={value.node}
          />
          <div className="flex flex-wrap gap-2">
            <CopyButton value={value.node} label="复制节点配置" />
            <Button
              type="button"
              variant="outline"
              onClick={() => download("proxy.yaml", value.node)}
            >
              <Download />
              下载
            </Button>
          </div>
        </div>
      )}

      {tab === "subscription" && localLine && (
        <div className="space-y-3.5">
          <p className="muted">
            生成包含此线路的 Mihomo 完整配置订阅地址；如泄露可在「设置」中随时撤销。
          </p>
          {subscription ? (
            <>
              <Field label="订阅地址">
                <textarea
                  className="textarea mono"
                  readOnly
                  rows={3}
                  value={subscription}
                />
              </Field>
              <CopyButton value={subscription} label="复制订阅链接" />
            </>
          ) : (
            <Field label="订阅名称">
              <Input
                name="subscription_name"
                defaultValue={line.name}
                required
                maxLength={120}
              />
            </Field>
          )}
        </div>
      )}
    </Modal>
  );
}
