import { useState } from "react";
import { remoteApi } from "../api";
import type { Line } from "../types";
import type { Remote } from "./fleet-state";
import { Field, Modal, errorText, number } from "./shared";
import { Input } from "./ui/input";

export function FleetLineBatch({ targets, action, close, saved }: {
  targets: { r: Remote; line: Line }[]; action: "limits" | "delete";
  close: () => void; saved: () => Promise<void>;
}) {
  const [results, setResults] = useState<{ name: string; error?: string }[] | null>(null);
  const groups = [...new Set(targets.map(t => t.r.id))].map(id => ({
    remote: targets.find(t => t.r.id === id)!.r, lines: targets.filter(t => t.r.id === id).map(t => t.line),
  }));
  const protectedLine = action === "delete" && targets.some(t => t.line.customer_plan_id);
  return <Modal title={`${action === "limits" ? "批量调整额度 / 限速" : "批量删除代理"} · ${targets.length} 条`}
    description={action === "limits" ? "整体替换所选代理的额度和限速；留空表示不限。不清零已有流量，保存后下发。" : "删除后无法撤回，重新下发可能使同机连接短暂重连。"}
    close={close} submit={!results && !protectedLine ? "确认执行" : undefined} action={async data => {
      const limits = action === "limits" ? { monthly_gib: number(data, "quota"), upload_mbps: number(data, "upload"), download_mbps: number(data, "download") } : undefined;
      const output: { name: string; error?: string }[] = [];
      for (let i = 0; i < groups.length; i += 5) {
        output.push(...await Promise.all(groups.slice(i, i + 5).map(async ({ remote, lines }) => {
          try {
            await remoteApi(remote.id)("/lines/operate", "POST", { line_ids: lines.map(l => l.id), action, limits });
            return { name: remote.name };
          } catch (error) { return { name: remote.name, error: errorText(error) }; }
        })));
      }
      setResults(output);
      await saved();
    }}>
    <p className="text-sm">{targets.map(t => `${t.r.name} / ${t.line.name}`).join("、")}</p>
    {protectedLine && <p role="alert">套餐线路不可删除；请取消选择后重试，或使用停用操作。</p>}
    {action === "limits" && !results && <div className="grid gap-4 sm:grid-cols-3">{[
      ["quota", "月额度 · GiB"], ["upload", "上传上限 · Mbps"], ["download", "下载上限 · Mbps"],
    ].map(([key, label]) => <Field key={key} label={label}><Input name={key} type="number" min="0.001" step="any" placeholder="不限" /></Field>)}</div>}
    {results && <div role="status" className="space-y-2 text-sm">{results.map((result, i) => <p key={i} className={result.error ? "text-red-600" : "text-emerald-700"}>
      {result.name}：{result.error || "已提交下发，请核对同步状态"}
    </p>)}<p>未成功的机器可在刷新列表后重新选择；本窗口不会重复提交。</p></div>}
  </Modal>;
}
