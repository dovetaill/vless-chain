import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  KeyRound,
  Fingerprint,
  Smartphone,
  ShieldCheck,
  Trash2,
  UserPlus,
} from "lucide-react";
import { Button } from "./ui/button";
import { Badge } from "./ui/badge";
import {
  Card,
  CardContent,
  CardHeader,
  CardHeading,
  CardTitle,
  CardToolbar,
} from "./ui/card";
import { Input } from "./ui/input";
import {
  Field,
  InfoTip,
  Modal,
  CopyButton,
  field,
  stamp,
  Status,
  errorText,
  roleNames,
} from "./shared";
import { QR } from "./access-dialog";
import { api } from "../api";
import { registerPasskey } from "../passkey";
import type { PanelState } from "../types";

type Member = {
  id: string;
  name: string;
  role: "ops" | "finance" | "viewer";
  created_at: number;
  revoked_at: number;
};

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
      description="使用验证器 App 扫描二维码并输入 6 位验证码完成绑定。"
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
          <p className="rounded-lg border border-emerald-500/30 bg-emerald-500/10 p-3 text-xs text-foreground">
            二步验证已启用。请离线妥善保存以下一次性恢复码。
          </p>
          <pre className="secret">{codes.join("\n")}</pre>
          <CopyButton value={codes.join("\n")} label="复制恢复码" />
        </>
      ) : setup ? (
        <>
          <QR value={setup.url} />
          <details className="details">
            <summary>手动输入密钥</summary>
            <div className="mt-3.5 space-y-2.5">
              <pre className="secret">{setup.secret}</pre>
              <CopyButton size="sm" value={setup.secret} />
            </div>
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

function Members({ otp }: { otp: boolean }) {
  const [members, setMembers] = useState<Member[] | null>(null);
  const [error, setError] = useState("");
  const [modal, setModal] = useState<ReactNode>(null);
  const close = () => setModal(null);

  const load = useCallback(async () => {
    try {
      const value = await api<{ members: Member[] }>("/security/members");
      setMembers(value.members);
      setError("");
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <Card>
      <CardHeader className="gap-3">
        <CardHeading className="flex items-center gap-1.5 space-y-0">
          <CardTitle>运营成员</CardTitle>
          <InfoTip>
            每位成员使用独立登录密钥，无需管理员二步验证码；撤销成员仅终止其登录权限，保留既有操作台账。
          </InfoTip>
        </CardHeading>
        <CardToolbar>
          <Button
            size="sm"
            variant="outline"
            onClick={() =>
              setModal(
                <Modal
                  title="新建运营成员"
                  submit="生成成员密钥"
                  close={close}
                  action={async (data) => {
                    const result = await api<{ member: Member; token: string }>(
                      "/security/members",
                      "POST",
                      {
                        name: field(data, "name"),
                        role: field(data, "role"),
                        token: field(data, "token"),
                        code: field(data, "code"),
                      },
                    );
                    setModal(
                      <Modal
                        title="成员密钥（仅显示一次）"
                        description={`${result.member.name} · ${roleNames[result.member.role]}`}
                        close={close}
                      >
                        <p className="muted">
                          请立即复制并私下交付该成员；关闭窗口后将无法再次查看明文。
                        </p>
                        <pre className="secret">{result.token}</pre>
                        <CopyButton
                          value={result.token}
                          label="复制成员密钥"
                        />
                      </Modal>,
                    );
                    await load();
                  }}
                >
                  <Field label="成员名称">
                    <Input
                      name="name"
                      required
                      maxLength={80}
                      autoComplete="off"
                    />
                  </Field>
                  <Field
                    label="成员角色"
                    hint="只读：查看脱敏状态 · 运维：监控诊断与下发维护 · 财务：客户账单与开通续费"
                  >
                    <select
                      name="role"
                      className="select"
                      defaultValue="viewer"
                    >
                      <option value="viewer">只读</option>
                      <option value="ops">运维</option>
                      <option value="finance">财务</option>
                    </select>
                  </Field>
                  <Proof otp={otp} />
                </Modal>,
              )
            }
          >
            <UserPlus />
            新建成员
          </Button>
        </CardToolbar>
      </CardHeader>

      <CardContent className="space-y-3">
        {error && (
          <p role="alert" className="text-xs text-destructive">
            {error}
          </p>
        )}
        {members === null ? (
          <p className="muted">正在读取成员列表…</p>
        ) : !members.length ? (
          <p className="muted">暂未添加协作运营成员。</p>
        ) : (
          members.map((member) => (
            <div
              key={member.id}
              className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-muted/20 px-4 py-3"
            >
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <p className="break-all text-xs font-semibold text-foreground">
                    {member.name}
                  </p>
                  <Badge variant="primary" appearance="light" size="xs">
                    {roleNames[member.role]}
                  </Badge>
                  <Badge
                    variant={member.revoked_at ? "secondary" : "success"}
                    appearance="light"
                    size="xs"
                  >
                    {member.revoked_at ? "已撤销" : "有效"}
                  </Badge>
                </div>
                <p className="mt-1 text-[11px] text-muted-foreground tabular-nums">
                  创建于 {stamp(member.created_at)}
                  {member.revoked_at ? ` · 撤销于 ${stamp(member.revoked_at)}` : ""}
                </p>
              </div>
              {!member.revoked_at && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    setModal(
                      <Modal
                        title="撤销成员"
                        description={`停用「${member.name}」的密钥与登录会话。`}
                        submit="确认撤销成员"
                        close={close}
                        action={async (data) => {
                          await api(
                            `/security/members/${member.id}/revoke`,
                            "POST",
                            {
                              token: field(data, "token"),
                              code: field(data, "code"),
                            },
                          );
                          await load();
                          close();
                        }}
                      >
                        <Proof otp={otp} />
                      </Modal>,
                    )
                  }
                >
                  撤销成员
                </Button>
              )}
            </div>
          ))
        )}
      </CardContent>
      {modal}
    </Card>
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
  const passkeyAvailable =
    window.isSecureContext && Boolean(window.PublicKeyCredential);

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
              <span className="inline-flex size-7 items-center justify-center rounded-lg bg-primary/10 text-primary">
                <KeyRound className="size-4" />
              </span>
              管理员密钥
            </CardTitle>
          </CardHeader>
          <CardContent className="flex flex-col justify-between gap-4">
            <p className="text-xs leading-relaxed text-muted-foreground">
              轮换主管理员登录密钥并立即注销所有旧会话。
            </p>
            <div>
              <Button
                variant="outline"
                onClick={() =>
                  setModal(
                    <Modal
                      title="修改管理员密钥"
                      description="更新后旧会话立即失效，请妥善保存新生成的密钥。"
                      close={close}
                      submit="更新密钥"
                      action={async (data) => {
                        const result = await api<{ token: string }>(
                          "/security/token",
                          "POST",
                          {
                            ...proof(data),
                            new_token: field(data, "new_token"),
                          },
                        );
                        sessionEnded(
                          `新的管理员密钥（仅本次显示）：\n${result.token}`,
                        );
                      }}
                    >
                      <Proof otp={security.totp_enabled} />
                      <Field
                        label="新密钥"
                        hint="至少 32 字符；留空自动生成高强度随机密钥"
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
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <span className="inline-flex size-7 items-center justify-center rounded-lg bg-violet-500/10 text-violet-600 dark:text-violet-400">
                <Smartphone className="size-4" />
              </span>
              动态验证码
            </CardTitle>
            <Status tone={security.totp_enabled ? "good" : "muted"}>
              {security.totp_enabled
                ? `已启用 · 余 ${security.recovery_remaining} 恢复码`
                : "尚未启用"}
            </Status>
          </CardHeader>
          <CardContent className="flex flex-col justify-between gap-4">
            <p className="text-xs leading-relaxed text-muted-foreground">
              管理员密钥登录时校验 TOTP 六位动态口令。
            </p>
            <div>
              {security.totp_enabled ? (
                <Button
                  variant="outline"
                  onClick={() =>
                    setModal(
                      <Modal
                        title="关闭二步验证"
                        description="关闭后需重新登录控制台。"
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
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <span className="inline-flex size-7 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
                <Fingerprint className="size-4" />
              </span>
              Passkey 通行密钥
            </CardTitle>
            {security.passkeys.length > 0 && (
              <Badge variant="success" appearance="light" size="sm">
                已绑 {security.passkeys.length} 个
              </Badge>
            )}
          </CardHeader>
          <CardContent className="space-y-4">
            <p className="text-xs leading-relaxed text-muted-foreground">
              使用指纹、面容或硬件安全密钥一键免密登录。
            </p>
            <div>
              <Button
                variant="outline"
                disabled={!passkeyAvailable}
                onClick={() =>
                  setModal(
                    <Modal
                      title="添加 Passkey"
                      description={`绑定至当前域名 ${location.origin}`}
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
                          placeholder="例如：MacBook Pro / YubiKey"
                          maxLength={80}
                          required
                        />
                      </Field>
                      <Proof otp={security.totp_enabled} />
                    </Modal>,
                  )
                }
              >
                添加 Passkey
              </Button>
              {!passkeyAvailable && (
                <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
                  需通过 HTTPS 域名或本机 localhost 访问面板方可绑定 Passkey。
                </p>
              )}
            </div>

            {security.passkeys.length > 0 && (
              <div className="space-y-2 border-t border-border pt-3">
                {security.passkeys.map((p) => (
                  <div
                    key={p.id}
                    className="flex items-center justify-between gap-2 rounded-lg border border-border bg-muted/25 px-3 py-2"
                  >
                    <div className="min-w-0">
                      <p className="truncate text-xs font-semibold text-foreground">
                        {p.name}
                      </p>
                      <p className="text-[11px] text-muted-foreground tabular-nums">
                        {stamp(p.created)}
                      </p>
                    </div>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="size-7"
                      aria-label={`删除 ${p.name}`}
                      onClick={() =>
                        setModal(
                          <Modal
                            title="撤销 Passkey"
                            description={`撤销后「${p.name}」无法再用于登录，其他活跃会话也将同步下线。`}
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
                      <Trash2 className="size-3.5" />
                    </Button>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      <Members otp={security.totp_enabled} />

      <Card>
        <CardHeader>
          <CardHeading className="flex items-center gap-1.5 space-y-0">
            <ShieldCheck className="size-4 text-primary" />
            <CardTitle>已签发订阅与应急恢复</CardTitle>
          </CardHeading>
        </CardHeader>
        <CardContent className="space-y-4">
          {state.subscriptions.length ? (
            <div className="space-y-2">
              {state.subscriptions.map((s) => (
                <div
                  key={s.id}
                  className="flex items-center justify-between gap-4 rounded-xl border border-border bg-muted/20 px-4 py-3"
                >
                  <div className="min-w-0">
                    <p className="break-all text-xs font-semibold text-foreground">
                      {s.name}
                    </p>
                    <p className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">
                      包含 {s.line_ids.length} 条线路 · 创建于 {stamp(s.created)}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    variant="outline"
                    className="shrink-0"
                    onClick={() =>
                      setModal(
                        <Modal
                          title="撤销订阅"
                          description={`撤销「${s.name}」后订阅链接立即失效（已拉取的节点凭据不受影响）。`}
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
              ))}
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">
              暂无订阅链接（可在代理列表「分享配置 → 订阅链接」中按需生成）。
            </p>
          )}

          <div className="rounded-lg border border-border/70 bg-muted/30 px-3.5 py-2.5 text-xs text-muted-foreground">
            锁死登录凭据时，可在面板服务器终端运行{" "}
            <code className="rounded bg-card px-1.5 py-0.5 font-mono text-[11px] text-foreground">
              chainctl
            </code>{" "}
            选择「重置登录安全」进行应急恢复。
          </div>
        </CardContent>
      </Card>

      {modal}
    </div>
  );
}
