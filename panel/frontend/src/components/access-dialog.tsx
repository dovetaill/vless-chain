import { useEffect, useState } from "react";
import QRCode from "qrcode";
import { Download } from "lucide-react";
import { Button } from "./ui/button";
import {
  Modal,
  CopyButton,
  PathView,
  download,
  errorText,
  endpoint,
} from "./shared";
import { api } from "../api";
import type { Line, PanelState } from "../types";
export function QR({ value }: { value: string }) {
  const [url, setUrl] = useState("");
  const [error, setError] = useState("");
  useEffect(() => {
    let alive = true;
    setUrl("");
    void QRCode.toDataURL(value, {
      width: 240,
      margin: 2,
      errorCorrectionLevel: "M",
    })
      .then((url) => {
        if (alive) setUrl(url);
      })
      .catch(() => {
        if (alive) setError("内容过长，请使用配置文件或链接导入。");
      });
    return () => {
      alive = false;
    };
  }, [value]);
  return url ? (
    <img
      src={url}
      width={240}
      height={240}
      alt="接入二维码"
      className="mx-auto rounded-xl border border-border bg-white p-2"
    />
  ) : (
    <p className="muted text-center">{error || "正在生成二维码…"}</p>
  );
}
export function AccessDialog({
  line,
  state,
  close,
}: {
  line: Line;
  state: PanelState;
  close: () => void;
}) {
  const [tab, setTab] = useState("qr");
  const [value, setValue] = useState<{
    share: string;
    node: string;
    config: string;
  } | null>(null);
  const [error, setError] = useState("");
  const [subscription, setSubscription] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (line.kind !== "vless") return;
    let alive = true;
    void api<{ share: string; node: string; config: string }>(
      `/lines/${line.id}/export`,
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
  }, [line.id, line.kind]);
  const node = state.nodes.find((n) => n.id === line.hops[0].node_id);
  const entry = endpoint(node?.host || "", line.hops[0].port);
  const websiteUrl = `${["auto", "manual"].includes(line.access.tls.mode) ? "https" : "http"}://${endpoint(line.access.tls.domain, line.hops[0].port)}/`;
  return (
    <Modal
      title={
        line.kind === "vless"
          ? "连接这条代理"
          : line.kind === "forward"
            ? "转发入口"
            : "访问网站"
      }
      description={line.name}
      close={close}
    >
      <PathView line={line} state={state} />
      {line.kind !== "vless" ? (
        <>
          <pre className="secret">
            {line.kind === "website" ? websiteUrl : entry}
          </pre>
          <CopyButton
            value={line.kind === "website" ? websiteUrl : entry}
            label="复制入口"
          />
          <p className="muted">
            {line.kind === "website"
              ? "使用浏览器访问此网址。"
              : `使用原业务客户端连接此入口，协议为 ${line.network.toUpperCase()}。端口转发不生成 VLESS 账号。`}
          </p>
        </>
      ) : (
        <>
          <div className="flex flex-wrap gap-1 rounded-lg bg-slate-100 p-1">
            {[
              ["qr", "扫码 / 链接"],
              ["config", "Mihomo 配置"],
              ["subscription", "订阅"],
            ].map(([id, label]) => (
              <button
                type="button"
                key={id}
                className={`flex-1 rounded-md px-3 py-2 text-sm ${tab === id ? "bg-white font-semibold shadow-sm" : "text-slate-500"}`}
                onClick={() => setTab(id)}
              >
                {label}
              </button>
            ))}
          </div>
          {error && (
            <p role="alert" className="text-sm text-red-600">
              {error}
            </p>
          )}
          {!value && !error && <p className="muted">正在读取已应用的配置…</p>}
          {value && tab === "qr" && (
            <>
              <QR value={value.share} />
              <textarea
                className="textarea mono"
                readOnly
                aria-label="分享链接"
                rows={3}
                value={value.share}
              />
              <CopyButton value={value.share} label="复制分享链接" />
              <p className="muted">
                二维码在浏览器本地生成。请使用支持此协议的客户端扫码或导入链接。
              </p>
            </>
          )}
          {value && tab === "config" && (
            <>
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  onClick={() => download("chain-panel.yaml", value.config)}
                >
                  <Download />
                  下载完整配置
                </Button>
                <CopyButton value={value.node} label="复制节点 YAML" />
              </div>
              <pre className="secret max-h-64 overflow-auto">
                {value.config}
              </pre>
              <p className="muted">
                适用于 Mihomo / Clash Meta。完整配置可直接导入；节点 YAML
                用于合并到自己的配置中。
              </p>
            </>
          )}
          {value && tab === "subscription" && (
            <>
              <p className="muted">
                订阅会随已应用的线路配置更新。它使用独立凭据，可在设置中撤销。
              </p>
              {subscription ? (
                <>
                  <QR value={subscription} />
                  <textarea
                    className="textarea mono"
                    readOnly
                    aria-label="订阅链接"
                    value={subscription}
                    rows={3}
                  />
                  <CopyButton value={subscription} label="复制订阅链接" />
                  <p className="muted">链接仅在创建时显示，请保存到客户端。</p>
                </>
              ) : (
                <Button
                  type="button"
                  disabled={busy}
                  onClick={async () => {
                    setBusy(true);
                    try {
                      const result = await api<{ path: string }>(
                        "/subscriptions",
                        "POST",
                        { name: `${line.name}订阅`, line_ids: [line.id] },
                      );
                      setSubscription(location.origin + result.path);
                    } catch (e) {
                      setError(errorText(e));
                    } finally {
                      setBusy(false);
                    }
                  }}
                >
                  创建此线路订阅
                </Button>
              )}
            </>
          )}
        </>
      )}
    </Modal>
  );
}
