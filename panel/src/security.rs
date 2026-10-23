use crate::model::{Security, now};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Mutex};
use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, Secret, TOTP};
use webauthn_rs::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role { Admin, Ops, Finance, Viewer }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Actor {
    pub id: String,
    pub name: String,
    pub role: Role,
}
impl Actor {
    pub fn admin() -> Self {
        Self { id: "administrator".into(), name: "内置管理员".into(), role: Role::Admin }
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub expires: u64,
    pub actor: Actor,
}
impl Session {
    pub fn actor(&self, members: &[crate::model::OperatorMember], at: u64) -> Option<Actor> {
        if self.expires <= at { return None; }
        if self.actor.role == Role::Admin { return Some(self.actor.clone()); }
        members.iter().find(|m| m.id == self.actor.id && m.revoked_at == 0 && m.role != Role::Admin)
            .map(|m| Actor { id: m.id.clone(), name: m.name.clone(), role: m.role })
    }
}

impl Role {
    pub fn allows(self, method: &str, path: &str) -> bool {
        if self == Self::Admin { return true; }
        if path.contains('%') || path.contains("..") { return false; }
        let parts: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
        match (method, parts.as_slice()) {
            ("GET", ["api", "state"]) | ("POST", ["api", "logout"]) => true,
            ("GET" | "POST", ["api", "monitoring"]) => self == Self::Ops,
            ("POST", ["api", "monitoring", "test"] | ["api", "preview" | "publish"]) => self == Self::Ops,
            ("GET", ["api", "fleet", "release", _]) => self == Self::Ops,
            ("GET" | "POST", ["api", "federation", "task"]) => self == Self::Ops,
            ("POST", ["api", "diagnostics", id, "logs"]) => self == Self::Ops && (*id == "panel" || uuid::Uuid::parse_str(id).is_ok()),
            ("POST", ["api", "diagnostics", id, "reality" | "path"] | ["api", "lines", id, "probe"]) => self == Self::Ops && uuid::Uuid::parse_str(id).is_ok(),
            ("GET" | "POST", ["api", "customer-plans"]) => self == Self::Finance,
            ("POST", ["api", "customer-plans", id, "query-link"]) => self == Self::Finance && uuid::Uuid::parse_str(id).is_ok(),
            ("POST", ["api", "customer-plans", id, "members", member, "query-link"]) => self == Self::Finance && uuid::Uuid::parse_str(id).is_ok() && uuid::Uuid::parse_str(member).is_ok(),
            ("POST", ["api", "customers"] | ["api", "rentals", "renew" | "contact" | "orders" | "adjustments"]) => self == Self::Finance,
            ("GET", ["api", "rentals", "context"]) => self == Self::Finance,
            ("GET", ["api", "rentals", "orders" | "adjustments", "by-request", id]) => self == Self::Finance && !id.is_empty(),
            ("DELETE", ["api", "customers", id]) | ("POST", ["api", "rentals", id] | ["api", "rentals", id, "query-link"] | ["api", "rentals", "orders", id, "confirm" | "cancel"]) => self == Self::Finance && uuid::Uuid::parse_str(id).is_ok(),
            _ => false,
        }
    }
}

fn fields(value: &serde_json::Value, names: &[&str]) -> serde_json::Value {
    serde_json::Value::Object(names.iter().filter_map(|key| value.get(*key).map(|v| ((*key).into(), v.clone()))).collect())
}

pub fn state_for(value: serde_json::Value, actor: &Actor) -> serde_json::Value {
    use serde_json::{Value, json};
    if actor.role == Role::Admin {
        let mut value = value;
        value["viewer"] = json!(actor);
        return value;
    }
    let mut out = fields(&value, &["busy", "time", "version", "agent_version", "agent_seen", "cores", "local_node_id"]);
    out["viewer"] = json!(actor);
    out["maintenance"] = json!(actor.role == Role::Ops && value["maintenance"] == true);
    out["maintenance_actions"] = if actor.role == Role::Ops { value["maintenance_actions"].clone() } else { json!([]) };
    out["nodes"] = json!(value["nodes"].as_array().into_iter().flatten().map(|node| {
        let mut n = fields(node, &["id", "name", "seen", "online", "desired_revision"]);
        n["host"] = if actor.role == Role::Ops { node["host"].clone() } else { json!("") };
        n["target"] = json!(""); n["server_name"] = json!("");
        n["report"] = fields(&node["report"], &["running", "applied", "revision", "traffic"]);
        n["report"]["error"] = json!(if node["report"]["error"].as_str().is_some_and(|s| !s.is_empty()) { "节点运行异常，请联系管理员" } else { "" });
        n["report"]["system"] = fields(&node["report"]["system"], &["memory_total", "memory_used", "load_1m", "network_rx", "network_tx", "sampled_at", "cpu_percent", "disk_total", "disk_used", "network_rx_bps", "network_tx_bps", "agent_version"]);
        n
    }).collect::<Vec<_>>());
    out["lines"] = json!(value["lines"].as_array().into_iter().flatten().map(|line| {
        let mut l = fields(line, &["id", "name", "kind", "enabled", "published", "limits", "traffic", "rental_status", "sync_status", "multiplier", "network"]);
        l["hops"] = json!(line["hops"].as_array().into_iter().flatten().map(|h| fields(h, &["node_id", "port"])).collect::<Vec<_>>());
        l["access"] = json!(crate::model::Access::default());
        l["access"]["protocol"] = line["access"]["protocol"].clone();
        l["share"] = Value::Null; l["external_id"] = Value::Null; l["target"] = json!(""); l["chain"] = json!([]);
        l["rental"] = if actor.role == Role::Finance { fields(&line["rental"], &["customer_id", "prices", "started_at", "expires_at", "paid_months", "next_reset_at", "query_link_exists", "permanent"]) } else { Value::Null };
        if line["rental"].is_null() { l["rental"] = Value::Null; }
        if actor.role == Role::Finance {
            l["customer_plan_id"] = line["customer_plan_id"].clone();
            l["plan_eligible"] = line["plan_eligible"].clone();
        }
        l
    }).collect::<Vec<_>>());
    for key in ["externals", "legacy_configs", "pending_imports", "subscriptions", "resources", "alerts"] { out[key] = json!([]); }
    out["checks"] = json!({});
    out["jobs"] = json!(value["jobs"].as_array().into_iter().flatten().map(|job| {
        let mut j = fields(job, &["id", "state", "started", "actor"]); j["events"] = json!([]); j
    }).collect::<Vec<_>>());
    out["security"] = json!({"totp_enabled":false,"recovery_remaining":0,"origin":"","passkeys":[]});
    for key in ["customers", "payments", "rental_orders", "payment_adjustments", "customer_plans"] {
        out[key] = if actor.role == Role::Finance { value.get(key).cloned().unwrap_or(json!([])) } else { json!([]) };
    }
    out["renewal_contact"] = if actor.role == Role::Finance { value["renewal_contact"].clone() } else { json!("") };
    out
}

pub fn digest(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
pub fn equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
pub fn totp(secret: &str) -> Result<TOTP> {
    Ok(TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.into()).to_bytes()?,
        Some("Chain Panel".into()),
        "administrator".into(),
    )?)
}
pub fn new_totp() -> Result<(String, String)> {
    let secret = Secret::generate_secret().to_encoded().to_string();
    Ok((secret.clone(), totp(&secret)?.get_url()))
}
pub fn second_factor(security: &mut Security, code: &str) -> Result<()> {
    if security.totp_secret.is_empty() {
        return Ok(());
    }
    let t = totp(&security.totp_secret)?;
    let step = now() / 30;
    if let Some(valid) = [step.saturating_sub(1), step, step + 1]
        .into_iter()
        .find(|s| *s > security.totp_step && equal(&t.generate(*s * 30), code))
    {
        security.totp_step = valid;
        return Ok(());
    }
    let hash = digest(code.trim());
    if let Some(index) = security
        .recovery_hashes
        .iter()
        .position(|v| equal(v, &hash))
    {
        security.recovery_hashes.remove(index);
        return Ok(());
    }
    anyhow::bail!("请输入未使用的动态验证码或恢复码")
}
pub fn engine(origin: &str) -> Result<Webauthn> {
    let url = url::Url::parse(origin).context("请输入完整 HTTPS 控制台地址")?;
    let host = url.host_str().context("控制台地址缺少域名")?;
    ensure!(
        url.scheme() == "https" || host == "localhost" && url.scheme() == "http",
        "Passkey 需要 HTTPS 域名（本机 localhost 除外）"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "" | "/"),
        "Passkey 地址不得含路径、认证信息或查询"
    );
    ensure!(
        matches!(url.host(), Some(url::Host::Domain(_))),
        "Passkey 请使用稳定的域名"
    );
    Ok(WebauthnBuilder::new(host, &url)?
        .rp_name("Chain Panel")
        .build()?)
}
pub enum Challenge {
    Register {
        state: PasskeyRegistration,
        session: String,
        origin: String,
        name: String,
        expires: u64,
    },
    Login {
        state: PasskeyAuthentication,
        origin: String,
        expires: u64,
    },
}
impl Challenge {
    pub fn expires(&self) -> u64 {
        match self {
            Self::Register { expires, .. } | Self::Login { expires, .. } => *expires,
        }
    }
}
#[derive(Default)]
pub struct AuthState {
    pub challenges: Mutex<HashMap<String, Challenge>>,
    pub enroll: Mutex<HashMap<String, (String, u64)>>,
    pub failures: Mutex<Vec<u64>>,
}
impl AuthState {
    pub fn rate_limit(&self) -> Result<()> {
        let mut recent = self.failures.lock().unwrap();
        recent.retain(|t| now().saturating_sub(*t) < 60);
        ensure!(recent.len() < 30, "登录尝试过于频繁，请一分钟后重试");
        recent.push(now());
        Ok(())
    }
    pub fn insert(&self, id: String, challenge: Challenge) -> Result<()> {
        let mut pending = self.challenges.lock().unwrap();
        pending.retain(|_, v| v.expires() > now());
        ensure!(pending.len() < 100, "验证请求过多，请稍后重试");
        pending.insert(id, challenge);
        Ok(())
    }
    pub fn take(&self, id: &str) -> Result<Challenge> {
        let c = self
            .challenges
            .lock()
            .unwrap()
            .remove(id)
            .context("验证已使用或不存在，请重新开始")?;
        ensure!(c.expires() > now(), "验证已过期，请重新开始");
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn member_roles_default_deny_and_cannot_use_admin_tunnels() {
        let id = uuid::Uuid::new_v4();
        for role in [Role::Ops, Role::Finance, Role::Viewer] {
            assert!(role.allows("GET", "/api/state"));
            assert!(role.allows("POST", "/api/logout"));
            for (method, path) in [
                ("GET", "/api/new-sensitive-feature".into()),
                ("GET", "/api/security/members".into()),
                ("GET", "/api/federation/document".into()),
                ("POST", "/api/security/token".into()),
                ("POST", "/api/federation/ticket".into()),
                ("POST", format!("/api/fleet/{id}/forward")),
                ("POST", format!("/api/fleet/{id}/login")),
                ("POST", "/api/lines/operate".into()),
                ("POST", "/api/lines".into()),
                ("GET", "/api/%73tate".into()),
                ("GET", "/api/../state".into()),
            ] { assert!(!role.allows(method, &path), "{role:?} {method} {path}"); }
        }
        assert!(Role::Ops.allows("POST", "/api/publish"));
        assert!(Role::Ops.allows("POST", "/api/federation/task"));
        assert!(!Role::Ops.allows("POST", "/api/rentals/renew"));
        assert!(Role::Finance.allows("POST", &format!("/api/rentals/orders/{id}/confirm")));
        assert!(Role::Finance.allows("POST", "/api/rentals/adjustments"));
        assert!(Role::Finance.allows("GET", "/api/customer-plans"));
        for path in [format!("/api/customer-plans/{id}/query-link"), format!("/api/customer-plans/{id}/members/{id}/query-link")] {
            assert!(Role::Finance.allows("POST", &path));
            assert!(!Role::Ops.allows("POST", &path));
            assert!(!Role::Viewer.allows("POST", &path));
        }
        assert!(!Role::Finance.allows("POST", "/api/publish"));
        assert!(!Role::Viewer.allows("POST", "/api/monitoring"));
    }

    #[test]
    fn member_revocation_and_removal_invalidate_existing_sessions() {
        let actor = Actor { id: "member".into(), name: "财务".into(), role: Role::Finance };
        let session = Session { actor: actor.clone(), expires: 1000 };
        let mut members = vec![crate::model::OperatorMember { id: actor.id.clone(), name: actor.name.clone(), role: actor.role,
            token_hash: digest("fixture-token"), created_at: 1, revoked_at: 0 }];
        assert_eq!(session.actor(&members, 100), Some(actor));
        assert!(session.actor(&members, 1000).is_none());
        members[0].revoked_at = 100;
        assert!(session.actor(&members, 100).is_none());
        assert!(session.actor(&[], 100).is_none());
        let admin = Session { actor: Actor::admin(), expires: 1000 };
        assert_eq!(admin.actor(&members, 100), Some(Actor::admin()));
    }

    #[test]
    fn finance_state_excludes_credentials_and_other_members_cannot_read_billing() {
        use serde_json::json;
        let raw = json!({"nodes":[{"id":"node","host":"192.0.2.1","target":"secret-marker","report":{
            "error":"secret-marker","running":true,"traffic":{},"system":{"private_key":"secret-marker","cpu_percent":10}}}],
            "lines":[{"id":"line","name":"线路","hops":[{"node_id":"node","port":443,"uuid":"secret-marker"}],
                "share":"secret-marker","access":{"protocol":"vless","password":"secret-marker","tls":{"key":"secret-marker"}},
                "chain":["secret-marker"],"target":"secret-marker","rental":{"customer_id":"customer","prices":{"monthly":2000},"query_token_hash":"secret-marker"},
                "customer_plan_id":"plan","plan_eligible":false}],
            "customers":[{"id":"customer","name":"客户","contact":"private-contact"}],"payments":[{"amount_fen":2000}],
            "externals":[{"url":"secret-marker"}],"subscriptions":[{"token":"secret-marker"}],"legacy_configs":[{"config":"secret-marker"}],
            "security":{"totp_secret":"secret-marker"},"jobs":[{"id":"job","state":"failed","events":[{"message":"secret-marker"}]}],
            "checks":{"line":{"detail":"secret-marker"}},"unknown_future_secret":"secret-marker","customer_query_tokens":{"plan":"secret-marker"},"customer_plans":[{"id":"plan"}]});
        for role in [Role::Ops, Role::Finance, Role::Viewer] {
            let actor = Actor { id: "member".into(), name: "成员".into(), role };
            let view = state_for(raw.clone(), &actor);
            assert!(!view.to_string().contains("secret-marker"), "{role:?}");
            assert_eq!(view["viewer"]["role"], json!(role));
            if role == Role::Finance {
                assert_eq!(view["customers"][0]["contact"], "private-contact");
                assert_eq!(view["payments"][0]["amount_fen"], 2000);
                assert_eq!(view["lines"][0]["customer_plan_id"], "plan");
                assert_eq!(view["lines"][0]["plan_eligible"], false);
            } else {
                assert_eq!(view["customers"], json!([]));
                assert_eq!(view["payments"], json!([]));
                assert_eq!(view["customer_plans"], json!([]));
                assert!(view["lines"][0]["rental"].is_null());
            }
        }
        assert!(state_for(raw, &Actor::admin()).to_string().contains("secret-marker"), "管理员保留原视图");
        let doc: crate::model::Document = serde_json::from_value(json!({"nodes":[],"externals":[],"lines":[],"applied_lines":[],"applied_nodes":[],"applied_externals":[],"checks":{},"jobs":[]})).unwrap();
        assert!(doc.operator_members.is_empty());
        assert!(doc.operator_audit.is_empty());
    }

    #[test]
    fn totp_and_recovery_codes_cannot_be_replayed() {
        let (secret, _) = new_totp().unwrap();
        let code = totp(&secret).unwrap().generate_current().unwrap();
        let mut s = Security {
            totp_secret: secret,
            recovery_hashes: vec![digest("one-use")],
            ..Default::default()
        };
        second_factor(&mut s, &code).unwrap();
        assert!(second_factor(&mut s, &code).is_err());
        second_factor(&mut s, "one-use").unwrap();
        assert!(second_factor(&mut s, "one-use").is_err());
    }
    #[test]
    fn passkeys_require_stable_secure_origins() {
        assert!(engine("https://panel.example.com").is_ok());
        assert!(engine("http://panel.example.com").is_err());
        assert!(engine("https://192.0.2.1").is_err());
        assert!(engine("https://panel.example.com/path").is_err());
    }
}
