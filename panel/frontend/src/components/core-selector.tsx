import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { Input } from "./ui/input";
import { Badge } from "./ui/badge";
import { InfoTip, errorText } from "./shared";

export type CoreChoice = {
  core: "xray" | "mihomo";
  version: string;
  prerelease: boolean;
  security_update: boolean;
  rollback_safe: boolean;
};

export const initialCoreChoice: CoreChoice = {
  core: "xray",
  version: "",
  prerelease: false,
  security_update: true,
  rollback_safe: false,
};

export function CoreSelector({
  value,
  onChange,
  disabled,
}: {
  value: CoreChoice;
  onChange: (value: CoreChoice) => void;
  disabled?: boolean;
}) {
  const current = useRef({ value, onChange });
  current.current = { value, onChange };
  const [releases, setReleases] = useState<{
    recommended: string;
    versions: { version: string; prerelease: boolean }[];
  }>();
  const [error, setError] = useState("");

  useEffect(() => {
    let stopped = false;
    setReleases(undefined);
    setError("");
    api<{
      recommended: string;
      versions: { version: string; prerelease: boolean }[];
    }>(`/fleet/cores/${value.core}`)
      .then((result) => {
        if (stopped) return;
        setReleases(result);
        const { value: choice, onChange: change } = current.current;
        const latest = result.versions.find((r) => !r.prerelease);
        if (!choice.version && latest)
          change({ ...choice, version: latest.version });
      })
      .catch((e) => {
        if (!stopped) setError(errorText(e));
      });
    return () => {
      stopped = true;
    };
  }, [value.core]);

  return (
    <fieldset
      disabled={disabled}
      className="space-y-3.5 rounded-xl border border-border bg-card p-4 text-xs"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-1.5">
          <span className="text-[13px] font-semibold text-foreground">代理内核版本</span>
          <InfoTip>
            切换内核保留 UUID、密钥与现有客户端链接，后台自动验收链路；若为安全修复且未勾选允许回退，切换失败时将安全隔离内核并保留 Agent 心跳。
          </InfoTip>
        </div>
        <Badge variant="primary" appearance="light" size="sm">
          目标版本：{value.version || "正在查询官方正式版…"}
        </Badge>
      </div>

      <div className="grid gap-3.5 sm:grid-cols-2">
        <label className="field">
          <span>选择内核</span>
          <select
            className="select"
            value={value.core}
            onChange={(e) =>
              onChange({
                ...value,
                core: e.target.value as CoreChoice["core"],
                version: "",
                prerelease: false,
              })
            }
          >
            <option value="xray">Xray</option>
            <option value="mihomo">Mihomo</option>
          </select>
        </label>

        <label className="field">
          <span>官方版本</span>
          <select
            aria-label="官方版本"
            className="select"
            value={value.version}
            onChange={(e) =>
              onChange({
                ...value,
                version: e.target.value,
                prerelease:
                  releases?.versions.find((r) => r.version === e.target.value)
                    ?.prerelease || false,
              })
            }
          >
            <option value="">选择官方发行版</option>
            {releases?.versions
              .filter((r) => value.prerelease || !r.prerelease)
              .map((r) => (
                <option key={r.version} value={r.version}>
                  {r.version}
                  {r.version === releases.recommended ? "（推荐基线）" : ""}
                  {r.prerelease ? " · 预发布" : ""}
                </option>
              ))}
          </select>
        </label>
      </div>

      <details className="details">
        <summary>高级版本与安全回退选项</summary>
        <div className="mt-3.5 space-y-3.5">
          <label className="field">
            <span>手动指定版本号</span>
            <Input
              placeholder="例如 26.10.21"
              value={value.version}
              onChange={(e) =>
                onChange({
                  ...value,
                  version: e.target.value.replace(/^v/, ""),
                })
              }
            />
          </label>
          <div className="space-y-2.5">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={value.prerelease}
                onChange={(e) =>
                  onChange({ ...value, prerelease: e.target.checked })
                }
              />
              <span>显示并允许选择预发布（Pre-release）版本</span>
            </label>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={value.security_update}
                onChange={(e) =>
                  onChange({
                    ...value,
                    security_update: e.target.checked,
                    rollback_safe: false,
                  })
                }
              />
              <span>本次属于安全更新，禁止自动回退到未确认安全的旧版</span>
            </label>
            {value.security_update && (
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={value.rollback_safe}
                  onChange={(e) =>
                    onChange({ ...value, rollback_safe: e.target.checked })
                  }
                />
                <span>已核实原版本不受本次漏洞影响，允许失败时回退</span>
              </label>
            )}
          </div>
        </div>
      </details>

      {error && (
        <p role="alert" className="text-amber-600 dark:text-amber-400">
          获取版本列表失败：{error}（仍可在高级选项中手动输入准确版本号）。
        </p>
      )}
    </fieldset>
  );
}
