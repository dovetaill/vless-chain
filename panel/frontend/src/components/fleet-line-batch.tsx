import { useState } from "react";
import { remoteApi } from "../api";
import type { Line } from "../types";
import type { Remote } from "./fleet-state";
import { Badge } from "./ui/badge";
import { Field, Modal, errorText, number } from "./shared";
import { Input } from "./ui/input";

export function FleetLineBatch({
  targets,
  action,
  close,
  saved,
}: {
  targets: { r: Remote; line: Line }[];
  action: "limits" | "delete";
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [results, setResults] = useState<
    { name: string; error?: string }[] | null
  >(null);
  const groups = [...new Set(targets.map((t) => t.r.id))].map((id) => ({
    remote: targets.find((t) => t.r.id === id)!.r,
    lines: targets.filter((t) => t.r.id === id).map((t) => t.line),
  }));
  const protectedLine =
    action === "delete" && targets.some((t) => t.line.customer_plan_id);

  return (
    <Modal
      title={`${
        action === "limits" ? "批量调整额度 / 限速" : "批量删除代理"
      } · ${targets.length} 条`}
      description={
        action === "limits"
          ? "统一更新所选线路限制（留空表示不限），不清零当月已用流量。"
          : "删除后不可恢复，同机其余线路可能短暂重连。"
      }
      close={close}
      submit={!results && !protectedLine ? "确认执行" : undefined}
      action={async (data) => {
        const limits =
          action === "limits"
            ? {
                monthly_gib: number(data, "quota"),
                upload_mbps: number(data, "upload"),
                download_mbps: number(data, "download"),
              }
            : undefined;
        const output: { name: string; error?: string }[] = [];
        for (let i = 0; i < groups.length; i += 5) {
          output.push(
            ...(await Promise.all(
              groups.slice(i, i + 5).map(async ({ remote, lines }) => {
                try {
                  await remoteApi(remote.id)("/lines/operate", "POST", {
                    line_ids: lines.map((l) => l.id),
                    action,
                    limits,
                  });
                  return { name: remote.name };
                } catch (error) {
                  return { name: remote.name, error: errorText(error) };
                }
              }),
            )),
          );
        }
        setResults(output);
        await saved();
      }}
    >
      <div className="flex max-h-40 flex-wrap gap-1.5 overflow-y-auto">
        {targets.map((t) => (
          <Badge
            key={`${t.r.id}:${t.line.id}`}
            variant="secondary"
            appearance="light"
            size="sm"
          >
            {t.r.name} / {t.line.name}
          </Badge>
        ))}
      </div>

      {protectedLine && (
        <p role="alert" className="text-xs text-destructive">
          包含共享套餐线路，无法直接删除；请取消勾选或改用停用操作。
        </p>
      )}

      {action === "limits" && !results && (
        <div className="grid gap-3.5 sm:grid-cols-3">
          {[
            ["quota", "月额度 · GiB"],
            ["upload", "上传上限 · Mbps"],
            ["download", "下载上限 · Mbps"],
          ].map(([key, label]) => (
            <Field key={key} label={label}>
              <Input
                name={key}
                type="number"
                min="0.001"
                step="any"
                placeholder="不限"
              />
            </Field>
          ))}
        </div>
      )}

      {results && (
        <div role="status" className="space-y-1.5 rounded-xl border border-border bg-muted/20 p-3.5 text-xs">
          {results.map((result, i) => (
            <p
              key={i}
              className={
                result.error
                  ? "text-destructive"
                  : "text-emerald-600 dark:text-emerald-400"
              }
            >
              {result.name}：{result.error || "已提交下发，请核对同步状态"}
            </p>
          ))}
        </div>
      )}
    </Modal>
  );
}
