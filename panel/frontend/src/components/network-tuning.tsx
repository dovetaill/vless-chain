import { useEffect, useMemo, useState } from "react";
import { api, remoteApi } from "../api";
import { Badge } from "./ui/badge";
import { Field, InfoTip, Modal, errorText } from "./shared";
import { requestId, type Remote } from "./fleet-state";

export type NetworkStatus = {
  congestion_control: string;
  qdisc: string;
  kernel: string;
  algorithms: string[];
  queues: string[];
  interfaces: { dev: string; kind: string; parent: string }[];
  warning: string;
};

export type NetworkTarget = Pick<Remote, "id" | "name" | "snapshot">;
export type NetworkRequest = {
  id: string;
  action: "network-tune";
  congestion_control: string;
  qdisc: string;
};

export function NetworkTuning({
  targets: selectedTargets,
  localTarget,
  initialIncludeLocal = false,
  close,
  submitted,
  submitLocal,
}: {
  localTarget?: NetworkTarget;
  initialIncludeLocal?: boolean;
  targets: NetworkTarget[];
  close: () => void;
  submitted?: (id: string) => Promise<void>;
  submitLocal?: (request: NetworkRequest) => Promise<void>;
}) {
  const [includeLocal, setIncludeLocal] = useState(initialIncludeLocal);
  const targets = useMemo(
    () => [
      ...selectedTargets,
      ...(includeLocal && localTarget ? [localTarget] : []),
    ],
    [selectedTargets, includeLocal, localTarget],
  );
  const [states, setStates] = useState<Record<string, NetworkStatus>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [cc, setCc] = useState("bbr");
  const [queue, setQueue] = useState("fq");
  const [id] = useState(requestId);

  useEffect(() => {
    const controller = new AbortController();
    void (async () => {
      for (
        let offset = 0;
        offset < targets.length && !controller.signal.aborted;
        offset += 5
      ) {
        await Promise.all(
          targets.slice(offset, offset + 5).map(async (remote) => {
            try {
              if (!remote.snapshot?.maintenance_actions?.includes("network-tune"))
                throw new Error("请先升级该 VPS 的面板及维护组件");
              const request = remote.id === "local" ? api : remoteApi(remote.id);
              const value = await request<NetworkStatus>(
                "/federation/task",
                "POST",
                {
                  id: requestId(),
                  action: "network-status",
                },
                controller.signal,
              );
              if (!controller.signal.aborted)
                setStates((old) => ({ ...old, [remote.id]: value }));
            } catch (error) {
              if (!controller.signal.aborted)
                setErrors((old) => ({
                  ...old,
                  [remote.id]: errorText(error),
                }));
            }
          }),
        );
      }
    })();
    return () => controller.abort();
  }, [targets]);

  const ready =
    targets.length > 0 && targets.every((remote) => states[remote.id]);
  const common = (key: "algorithms" | "queues") =>
    ready
      ? states[targets[0].id][key].filter((value) =>
          targets.every((remote) => states[remote.id][key].includes(value)),
        )
      : [];
  const algorithms = common("algorithms"),
    queues = common("queues");

  useEffect(() => {
    if (!ready) return;
    const first = states[targets[0].id];
    if (algorithms.length > 0 && !algorithms.includes(cc)) {
      setCc(
        algorithms.includes(first.congestion_control)
          ? first.congestion_control
          : algorithms[0],
      );
    }
    if (queues.length > 0 && !queues.includes(queue)) {
      setQueue(queues.includes(first.qdisc) ? first.qdisc : queues[0]);
    }
  }, [ready, algorithms, queues, cc, queue, states, targets]);

  const valid = ready && algorithms.includes(cc) && queues.includes(queue);

  return (
    <Modal
      title={`网络算法 · ${targets.length} 台 VPS`}
      close={close}
      wide
      description="在线应用内核默认 TCP 拥塞算法与网卡队列，无需重启服务器。"
      submit={valid ? "确认保存并应用" : undefined}
      action={
        valid
          ? async () => {
              const request: NetworkRequest = {
                id,
                action: "network-tune",
                congestion_control: cc,
                qdisc: queue,
              };
              if (submitLocal) await submitLocal(request);
              else {
                await api("/fleet/batch", "POST", {
                  ...request,
                  targets: targets.map((remote) => remote.id),
                });
                await submitted?.(id);
              }
              close();
            }
          : undefined
      }
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        {localTarget ? (
          <label className="flex items-center gap-2 text-xs font-medium text-foreground">
            <input
              type="checkbox"
              checked={includeLocal}
              onChange={(e) => setIncludeLocal(e.target.checked)}
            />
            同时应用到本机
          </label>
        ) : (
          <span />
        )}
        <InfoTip>
          下拉列表展示所选机器共同支持的算法交集；自定义 tc 限速规则保持原样，已有连接不强制重置。
        </InfoTip>
      </div>

      <div className="grid gap-3.5 sm:grid-cols-2">
        <Field label="拥塞控制算法">
          <select
            className="select"
            value={cc}
            onChange={(e) => setCc(e.target.value)}
          >
            {!algorithms.includes(cc) && (
              <option value={cc} disabled>
                {ready ? "无共同支持的拥塞控制算法" : `${cc}（待全部机器确认支持）`}
              </option>
            )}
            {algorithms.map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </Field>
        <Field label="默认队列">
          <select
            className="select"
            value={queue}
            onChange={(e) => setQueue(e.target.value)}
          >
            {!queues.includes(queue) && (
              <option value={queue} disabled>
                {ready ? "无共同支持的默认队列" : `${queue}（待全部机器确认支持）`}
              </option>
            )}
            {queues.map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {ready && (!algorithms.length || !queues.length) && (
        <p role="alert" className="text-xs text-destructive">
          所选机器无共同支持的算法或队列，请缩小选择范围分别设置。
        </p>
      )}

      <div className="max-h-72 space-y-2.5 overflow-y-auto pr-1">
        {targets.map((remote) => (
          <section
            className="space-y-1.5 rounded-xl border border-border bg-muted/20 p-3.5 text-xs"
            key={remote.id}
          >
            <div className="flex flex-wrap items-center justify-between gap-2">
              <strong className="font-semibold text-foreground">
                {remote.name}
              </strong>
              {states[remote.id] && (
                <Badge variant="secondary" appearance="light" size="xs">
                  {states[remote.id].congestion_control} + {states[remote.id].qdisc}
                </Badge>
              )}
            </div>
            {errors[remote.id] ? (
              <p role="alert" className="text-destructive">
                {errors[remote.id]}
              </p>
            ) : !states[remote.id] ? (
              <p className="text-muted-foreground">正在读取内核状态…</p>
            ) : (
              <div className="space-y-0.5 text-muted-foreground">
                <p>
                  内核 {states[remote.id].kernel} · 可选{" "}
                  {states[remote.id].algorithms.join(" / ")} · 队列{" "}
                  {states[remote.id].queues.join(" / ") || "未知"}
                </p>
                <p>
                  网卡队列：
                  {states[remote.id].interfaces
                    .map((q) => `${q.dev}(${q.kind})`)
                    .join("、") || "未知"}
                </p>
                {states[remote.id].warning && (
                  <p className="text-amber-600 dark:text-amber-400">
                    {states[remote.id].warning}
                  </p>
                )}
              </div>
            )}
          </section>
        ))}
      </div>
    </Modal>
  );
}
