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

export function NetworkTuning({ targets, close, submitted }: {
  targets: Remote[]; close: () => void; submitted: (id: string) => Promise<void>;
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
            const value = await remoteApi(remote.id)<NetworkStatus>("/federation/task", "POST", {
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
    description="设置 TCP 默认拥塞算法和默认队列，并写入持久配置。已有连接、监听套接字和网卡队列保持现状；其实际生效需在维护窗口重建后核对。其他调优工具可能覆盖默认设置，请以重新读取的状态为准。"
    submit={valid ? "确认保存并应用默认设置" : undefined} action={valid ? async () => {
      await api("/fleet/batch", "POST", { id, targets: targets.map(remote => remote.id), action: "network-tune", congestion_control: cc, qdisc: queue });
      await submitted(id);
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
