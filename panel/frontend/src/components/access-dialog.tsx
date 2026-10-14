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
import type { Line, PanelState, External } from "../types";
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
  external,
  state,
  close,
}: {
  line?: Line;
  external?: External;
  state: PanelState;
  close: () => void;
}) {
  const [tab, setTab] = useState("node");
  const [value, setValue] = useState<{
    share: string;
    node: string;
  } | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!line && !external) return;
    let alive = true;
    void api<{ share: string; node: string }>(
      external ? `/externals/${external.id}/export` : `/lines/${line!.id}/export`,
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
  }, [line?.id, external?.id]);
  return (
    <Modal title={external ? "外部代理配置" : "分享代理"} description={external?.name || line?.name} close={close}>
      {line && <PathView line={line} state={state} />}
          <div className="flex flex-wrap gap-1 rounded-lg bg-slate-100 p-1">
            {[
              ["node", "Mihomo 节点"],
              ["qr", "链接 / 二维码"],
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
            </>
          )}
          {value && tab === "node" && (
            <>
              <textarea
                className="textarea mono"
                readOnly
                aria-label="Mihomo 节点配置"
                rows={7}
                value={value.node}
              />
              <div className="flex flex-wrap gap-2">
                <CopyButton value={value.node} label="复制节点配置" />
                <Button type="button" variant="outline" onClick={() => download("proxy.yaml", value.node)}>
                  <Download />下载
                </Button>
              </div>
            </>
          )}
    </Modal>
  );
}
