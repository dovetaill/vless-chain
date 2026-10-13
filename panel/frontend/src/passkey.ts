import { api } from "./api";
const decode = (s: string): ArrayBuffer =>
  Uint8Array.from(atob(s.replaceAll("-", "+").replaceAll("_", "/")), (c) =>
    c.charCodeAt(0),
  ).buffer;
const encode = (b: ArrayBuffer) =>
  btoa(String.fromCharCode(...new Uint8Array(b)))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
type Descriptor = Omit<PublicKeyCredentialDescriptor, "id"> & { id: string };
type Creation = Omit<
  PublicKeyCredentialCreationOptions,
  "challenge" | "user" | "excludeCredentials"
> & {
  challenge: string;
  user: Omit<PublicKeyCredentialUserEntity, "id"> & { id: string };
  excludeCredentials?: Descriptor[];
};
type Request = Omit<
  PublicKeyCredentialRequestOptions,
  "challenge" | "allowCredentials"
> & { challenge: string; allowCredentials?: Descriptor[] };
function serialized(credential: PublicKeyCredential) {
  const r = credential.response;
  const response =
    r instanceof AuthenticatorAttestationResponse
      ? {
          attestationObject: encode(r.attestationObject),
          clientDataJSON: encode(r.clientDataJSON),
          transports: r.getTransports(),
        }
      : (() => {
          const a = r as AuthenticatorAssertionResponse;
          return {
            authenticatorData: encode(a.authenticatorData),
            clientDataJSON: encode(a.clientDataJSON),
            signature: encode(a.signature),
            userHandle: a.userHandle ? encode(a.userHandle) : null,
          };
        })();
  return {
    id: credential.id,
    rawId: encode(credential.rawId),
    type: credential.type,
    response,
    extensions: credential.getClientExtensionResults(),
  };
}
export async function loginPasskey() {
  const { challenge_id, options } = await api<{
    challenge_id: string;
    options: { publicKey: Request };
  }>("/auth/passkey/start", "POST", {});
  const p = options.publicKey;
  const credential = (await navigator.credentials.get({
    publicKey: {
      ...p,
      challenge: decode(p.challenge),
      allowCredentials: p.allowCredentials?.map((c) => ({
        ...c,
        id: decode(c.id),
      })),
    },
  })) as PublicKeyCredential | null;
  if (!credential) throw new Error("未完成 Passkey 验证");
  await api("/auth/passkey/finish", "POST", {
    challenge_id,
    credential: serialized(credential),
  });
}
export async function registerPasskey(input: {
  name: string;
  origin: string;
  token: string;
  code: string;
}) {
  const { challenge_id, options } = await api<{
    challenge_id: string;
    options: { publicKey: Creation };
  }>("/security/passkey/start", "POST", input);
  const p = options.publicKey;
  const credential = (await navigator.credentials.create({
    publicKey: {
      ...p,
      challenge: decode(p.challenge),
      user: { ...p.user, id: decode(p.user.id) },
      excludeCredentials: p.excludeCredentials?.map((c) => ({
        ...c,
        id: decode(c.id),
      })),
    },
  })) as PublicKeyCredential | null;
  if (!credential) throw new Error("未完成 Passkey 绑定");
  await api("/security/passkey/finish", "POST", {
    challenge_id,
    credential: serialized(credential),
  });
}
