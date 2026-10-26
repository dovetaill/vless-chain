import { useEffect, useState } from "react";
import { api, remoteApi } from "../api";
import { Field, Modal, errorText } from "./shared";
import { requestId, type Remote } from "./fleet-state";

export type NetworkStatus = {
  congestion_control: string; qdisc: string; kernel: string;
  algorithms: string[]; queues: string[];
  interfaces: { dev: string; kind: string; parent: string }[];
  warning: string;
};

export type NetworkTarget = Pick<Remote, "id" | "name" | "snapshot">;
export type NetworkRequest = { id: string; action: "network-tune"; congestion_control: string; qdisc: string };

export function NetworkTuning({ targets, close, submitted, submitLocal }: {
  targets: NetworkTarget[]; close: () => void; submitted?: (id: string) => Promise<void>;
  submitLocal?: (request: NetworkRequest) => Promise<void>;
}) {
  const [states, setStates] = useState<Record<string, NetworkStatus>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [cc, setCc] = useState("bbr");
  const [queue, setQueue] = useState("fq");
  const [id] = useState(requestId);
  useEffect(() => {
    const controller = new AbortController();
    void (async () => {
      for (let offset = 0; offset < targets.length && !controller.signal.aborted; offset += 5) {
        await Promise.all(targets.slice(offset, offset + 5).map(async remote => {
          try {
            if (!remote.snapshot?.maintenance_actions?.includes("network-tune")) throw new Error("请先升级该 VPS 的面板及维护组件");
            const request = remote.id === "local" ? api : remoteApi(remote.id);
            const value = await request<NetworkStatus>("/federation/task", "POST", {
              id: requestId(), action: "network-status",
            }, controller.signal);
            if (!controller.signal.aborted) setStates(old => ({ ...old, [remote.id]: value }));
          } catch (error) {
            if (!controller.signal.aborted) setErrors(old => ({ ...old, [remote.id]: errorText(error) }));
          }
        }));
      }
    })();
    return () => controller.abort();
  }, [targets]);
  const ready = targets.length > 0 && targets.every(remote => states[remote.id]);
  const common = (key: "algorithms" | "queues") => ready
    ? states[targets[0].id][key].filter(value => targets.every(remote => states[remote.id][key].includes(value))) : [];
  const algorithms = common("algorithms"), queues = common("queues");
  const valid = ready && algorithms.includes(cc) && queues.includes(queue);
  return <Modal title={`网络算法 · ${targets.length} 台 VPS`} close={close} wide
    description="保存并立即应用系统默认设置，无需重启服务器或服务。新版维护组件会在线切换可安全替换的网卡队列，可能短暂丢包；自定义限速或过滤规则保持不变，结果中会说明跳过项。已有 TCP 连接和监听器不会自动切换算法。"
    submit={valid ? "确认保存并应用" : undefined} action={valid ? async () => {
      const request: NetworkRequest = { id, action: "network-tune", congestion_control: cc, qdisc: queue };
      if (submitLocal) await submitLocal(request);
      else {
        await api("/fleet/batch", "POST", { ...request, targets: targets.map(remote => remote.id) });
        await submitted?.(id);
      }
      close();
    } : undefined}>
    <div className="grid gap-4 sm:grid-cols-2">
      <Field label="拥塞控制算法"><select className="select" value={cc} onChange={e => setCc(e.target.value)}>
        {!algorithms.includes(cc) && <option value={cc} disabled>{cc}（所选机器尚未全部确认支持）</option>}
        {algorithms.map(value => <option key={value} value={value}>{value}</option>)}
      </select></Field>
      <Field label="默认队列"><select className="select" value={queue} onChange={e => setQueue(e.target.value)}>
        {!queues.includes(queue) && <option value={queue} disabled>{queue}（所选机器尚未全部确认支持）</option>}
        {queues.map(value => <option key={value} value={value}>{value}</option>)}
      </select></Field>
    </div>
    <p className="muted">可选项为所选 VPS 检测结果的交集。BBR 用于 TCP；不会更改 TUIC / Hysteria 的拥塞控制。单台失败会单独记录。</p>
    {targets.map(remote => <section className="space-y-2 rounded-lg border p-3 text-sm" key={remote.id}>
      <strong>{remote.name}</strong>
      {errors[remote.id] ? <p role="alert" className="text-red-600">{errors[remote.id]}</p> : !states[remote.id] ? <p>正在检测…</p> : <>
        <p>系统默认：{states[remote.id].congestion_control} + {states[remote.id].qdisc} · 内核 {states[remote.id].kernel}</p>
        <p>可用算法：{states[remote.id].algorithms.join("、")}；队列：{states[remote.id].queues.join("、") || "未检测到"}</p>
        <p>网卡实际队列：{states[remote.id].interfaces.map(q => `${q.dev} / ${q.parent}: ${q.kind}`).join("；") || "未知"}</p>
        {states[remote.id].warning && <p className="text-amber-700">{states[remote.id].warning}</p>}
      </>}
    </section>)}
  </Modal>;
}
