import { useState, type ReactNode } from "react";
import {
  KeyRound,
  Fingerprint,
  Smartphone,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Input } from "./ui/input";
import { Field, Modal, CopyButton, field, stamp, Status } from "./shared";
import { QR } from "./access-dialog";
import { api } from "../api";
import { registerPasskey } from "../passkey";
import type { PanelState } from "../types";

function Proof({ otp }: { otp: boolean }) {
  return (
    <>
      <Field label="当前管理员密钥">
        <Input
          name="token"
          type="password"
          autoComplete="current-password"
          required
        />
      </Field>
      {otp && (
        <Field label="动态验证码或恢复码">
          <Input name="code" autoComplete="one-time-code" required />
        </Field>
      )}
    </>
  );
}
function TotpSetup({
  close,
  saved,
}: {
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [setup, setSetup] = useState<{ secret: string; url: string } | null>(
    null,
  );
  const [codes, setCodes] = useState<string[]>([]);
  return (
    <Modal
      title="绑定身份验证器"
      description="使用身份验证器扫码，再输入一次验证码完成绑定。"
      submit={codes.length ? undefined : setup ? "验证并启用" : "开始绑定"}
      close={close}
      action={async (data) => {
        if (!setup)
          setSetup(
            await api("/security/totp/start", "POST", {
              token: field(data, "token"),
            }),
          );
        else {
          const result = await api<{ recovery_codes: string[] }>(
            "/security/totp/finish",
            "POST",
            { code: field(data, "code") },
          );
          setCodes(result.recovery_codes);
          await saved();
        }
      }}
    >
      {codes.length ? (
        <>
          <p className="rounded-lg bg-green-50 p-3 text-sm text-green-800">
            TOTP 已启用。请离线保存恢复码，每个只能使用一次。
          </p>
          <pre className="secret">{codes.join("\n")}</pre>
          <CopyButton value={codes.join("\n")} label="复制恢复码" />
        </>
      ) : setup ? (
        <>
          <QR value={setup.url} />
          <details className="details">
            <summary>手动输入密钥</summary>
            <pre className="secret mt-3">{setup.secret}</pre>
            <CopyButton value={setup.secret} />
          </details>
          <Field label="6 位验证码">
            <Input
              name="code"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9]{6}"
              maxLength={6}
              required
            />
          </Field>
        </>
      ) : (
        <Proof otp={false} />
      )}
    </Modal>
  );
}
export function SecuritySettings({
  state,
  saved,
  sessionEnded,
}: {
  state: PanelState;
  saved: () => Promise<void>;
  sessionEnded: (message?: string) => void;
}) {
  const [modal, setModal] = useState<ReactNode>(null);
  const close = () => setModal(null);
  const security = state.security;
  const passkeyAvailable = window.isSecureContext && Boolean(window.PublicKeyCredential) && !/^\d+\.\d+\.\d+\.\d+$/.test(location.hostname) && !location.hostname.includes(":");
  const proof = (data: FormData) => ({
    token: field(data, "token"),
    code: field(data, "code"),
  });
  return (
    <div className="space-y-6">
      <div className="grid gap-5 xl:grid-cols-3">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <KeyRound className="size-5 text-blue-600" />
              管理员密钥
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-5">
            <p className="muted">
              更新登录密钥，并撤销旧会话。保留单管理员模式。
            </p>
            <Button
              variant="outline"
              onClick={() =>
                setModal(
                  <Modal
                    title="修改管理员密钥"
                    description="保存后全部旧会话失效。新密钥请立即私下保存。"
                    close={close}
                    submit="更新密钥"
                    action={async (data) => {
                      const result = await api<{ token: string }>(
                        "/security/token",
                        "POST",
                        { ...proof(data), new_token: field(data, "new_token") },
                      );
                      sessionEnded(
                        `新的管理员密钥（仅本次显示）：\n${result.token}`,
                      );
                    }}
                  >
                    <Proof otp={security.totp_enabled} />
                    <Field
                      label="新密钥"
                      hint="至少 32 字符；留空自动生成强随机密钥。"
                    >
                      <Input
                        name="new_token"
                        type="password"
                        minLength={32}
                        maxLength={512}
                        autoComplete="new-password"
                      />
                    </Field>
                  </Modal>,
                )
              }
            >
              修改密钥
            </Button>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Smartphone className="size-5 text-violet-600" />
              动态验证码
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-5">
            <Status tone={security.totp_enabled ? "good" : "muted"}>
              {security.totp_enabled
                ? `已启用 · 剩余 ${security.recovery_remaining} 个恢复码`
                : "尚未启用"}
            </Status>
            <p className="muted">
              密钥登录时增加一次验证，支持常用 TOTP 身份验证器。
            </p>
            {security.totp_enabled ? (
              <Button
                variant="outline"
                onClick={() =>
                  setModal(
                    <Modal
                      title="关闭 TOTP"
                      description="关闭后需重新登录。"
                      submit="验证并关闭"
                      close={close}
                      action={async (data) => {
                        await api(
                          "/security/totp/disable",
                          "POST",
                          proof(data),
                        );
                        sessionEnded();
                      }}
                    >
                      <Proof otp />
                    </Modal>,
                  )
                }
              >
                关闭二步验证
              </Button>
            ) : (
              <Button
                variant="outline"
                onClick={() =>
                  setModal(<TotpSetup close={close} saved={saved} />)
                }
              >
                绑定身份验证器
              </Button>
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Fingerprint className="size-5 text-emerald-600" />
              Passkey
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-5">
            <p className="muted">
              通过设备解锁或安全密钥完成登录。Passkey
              自身要求用户验证，可作为密钥＋TOTP 的替代登录方式。
            </p>
            <Button
              variant="outline"
              disabled={!passkeyAvailable}
              onClick={() =>
                setModal(
                  <Modal
                    title="添加 Passkey"
                    description="凭据绑定当前控制台域名。请使用日常访问的固定 HTTPS 地址。"
                    submit="验证并绑定"
                    close={close}
                    action={async (data) => {
                      await registerPasskey({
                        ...proof(data),
                        name: field(data, "name"),
                        origin: location.origin,
                      });
                      await saved();
                      close();
                    }}
                  >
                    <Field label="凭据名称">
                      <Input
                        name="name"
                        placeholder="例如：我的笔记本"
                        maxLength={80}
                        required
                      />
                    </Field>
                    <Proof otp={security.totp_enabled} />
                    <p className="muted">绑定地址：{location.origin}</p>
                  </Modal>,
                )
              }
            >
              添加 Passkey
            </Button>
            {!passkeyAvailable && (
              <p className="text-xs text-amber-700">
                先通过域名和 HTTPS 访问控制台，再绑定 Passkey。
              </p>
            )}
          </CardContent>
        </Card>
      </div>
      {security.passkeys.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>已绑定的 Passkey</CardTitle>
          </CardHeader>
          <CardContent>
            {security.passkeys.map((p) => (
              <div
                key={p.id}
                className="flex items-center justify-between gap-3 border-b border-border py-4 last:border-0"
              >
                <div className="min-w-0">
                  <p className="break-all text-sm font-medium">{p.name}</p>
                  <p className="muted mt-1">{stamp(p.created)}</p>
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`删除 ${p.name}`}
                  onClick={() =>
                    setModal(
                      <Modal
                        title="撤销 Passkey"
                        description={`撤销后，${p.name} 无法再登录。其他会话也会退出。`}
                        submit="验证并撤销"
                        close={close}
                        action={async (data) => {
                          await api("/security/passkey/remove", "POST", {
                            ...proof(data),
                            id: p.id,
                          });
                          await saved();
                          close();
                        }}
                      >
                        <Proof otp={security.totp_enabled} />
                      </Modal>,
                    )
                  }
                >
                  <Trash2 />
                </Button>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <ShieldCheck className="size-5" />
            订阅与恢复
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-5">
          <p className="muted">
            订阅仅包含所选的已应用代理，撤销后链接立即失效。
          </p>
          {state.subscriptions.length ? (
            state.subscriptions.map((s) => (
              <div
                key={s.id}
                className="flex items-center justify-between gap-4 rounded-lg border border-border p-3"
              >
                <div className="min-w-0">
                  <p className="break-all text-sm font-medium">{s.name}</p>
                  <p className="muted">
                    {s.line_ids.length} 条线路 · {stamp(s.created)}
                  </p>
                </div>
                <Button
                  variant="outline"
                  className="shrink-0"
                  onClick={() =>
                    setModal(
                      <Modal
                        title="撤销订阅"
                        description={`撤销「${s.name}」后，客户端无法再从该链接更新配置。已下载的节点凭据仍有效。`}
                        submit="撤销订阅"
                        close={close}
                        action={async () => {
                          await api(`/subscriptions/${s.id}`, "DELETE");
                          await saved();
                          close();
                        }}
                      />,
                    )
                  }
                >
                  撤销
                </Button>
              </div>
            ))
          ) : (
            <p className="text-sm text-slate-400">
              尚未创建订阅。在代理的「接入」窗口中创建。
            </p>
          )}
          <div className="rounded-lg bg-slate-50 p-4 text-sm leading-7 text-slate-600">
            无法登录时，使用恢复码，或在控制台所在服务器运行{" "}
            <code>chainctl</code>{" "}
            的「重置登录安全」操作。恢复需要该服务器的系统管理权限。
          </div>
        </CardContent>
      </Card>
      {modal}
    </div>
  );
}
