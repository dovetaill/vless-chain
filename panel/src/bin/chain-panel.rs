use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use chain_panel::{compiler::*, model::*, runtime::*};
use chain_panel::security::{Actor, Role, Session};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use subtle::ConstantTimeEq;
#[path = "../delivery_api.rs"]
mod delivery_api;
#[path = "../rental_api.rs"]
mod rental_api;
#[path = "../plans_api.rs"]
mod plans_api;
#[path = "../rental_public.rs"]
mod rental_public;
#[path = "../security_api.rs"]
mod security_api;
#[path = "../fleet_api.rs"]
mod fleet_api;
#[path = "../monitor_api.rs"]
mod monitor_api;
#[path = "../diagnostics_api.rs"]
mod diagnostics_api;
#[path = "../sharing_api.rs"]
mod sharing_api;

struct Store {
    conn: Connection,
    doc: Document,
}
impl Store {
    fn save(&self) -> Result<()> {
        self.conn.execute("INSERT INTO state(id,body) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",[serde_json::to_string(&self.doc)?])?;
        Ok(())
    }
}
const PROTECTED_STATE_FIELDS: [&str; 7] = [
    "rental_orders", "payment_adjustments", "customer_plans", "customer_ledgers",
    "customer_query_tokens", "operator_members", "operator_audit",
];
fn install_state_write_guard(conn: &Connection) -> Result<()> {
    let keys = PROTECTED_STATE_FIELDS.map(|key| format!("'{key}'")).join(",");
    // Persist the boundary in SQLite so a previous binary cannot silently discard new fields.
    for (name, event, previous) in [
        ("insert", "INSERT", "(SELECT body FROM state WHERE id=NEW.id)"),
        ("update", "UPDATE OF body", "OLD.body"),
    ] {
        conn.execute_batch(&format!(
            "CREATE TRIGGER IF NOT EXISTS state_preserve_operations_{name}
             BEFORE {event} ON state
             WHEN EXISTS (SELECT 1 FROM json_each({previous}) WHERE key IN ({keys}))
              AND (SELECT count(DISTINCT key) FROM json_each(NEW.body) WHERE key IN ({keys})) != 7
             BEGIN SELECT RAISE(ABORT, '数据库含新版业务字段，拒绝旧程序覆盖；请恢复兼容版本，勿回退数据库'); END;"
        ))?;
    }
    Ok(())
}
#[derive(Clone)]
struct App {
    diagnostics: Arc<diagnostics_api::Broker>,
    store: Arc<Mutex<Store>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    tickets: Arc<Mutex<HashMap<String, (u64, String)>>>,
    delegated_sessions: Arc<Mutex<BTreeSet<String>>>,
    admin_hash: Arc<Mutex<String>>,
    token_file: PathBuf,
    auth: Arc<chain_panel::security::AuthState>,
    secure_cookie: bool,
    xray: PathBuf,
    probe_lock: Arc<tokio::sync::Mutex<()>>,
    apply_timeout: u64,
    local_agent: Arc<tokio::sync::Mutex<chain_panel::local_agent::LocalAgent>>,
    local_url: String,
}
fn digest(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
fn equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}
macro_rules! api_from {($($t:ty),* $(,)?)=>{$(impl From<$t> for ApiError {fn from(e:$t)->Self {anyhow::Error::from(e).into()}})*};}
api_from!(
    serde_json::Error,
    serde_yaml_ng::Error,
    url::ParseError,
    std::io::Error
);
type ApiResult<T> = std::result::Result<T, ApiError>;
fn error(status: StatusCode, s: &str) -> ApiError {
    ApiError(status, s.into())
}

async fn guard(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut audit = None;
    let agent = path.starts_with("/api/agent/");
    if path == "/api/federation/login" {
        let session = app.sessions.lock().unwrap().get(&digest(&cookie(request.headers()))).cloned();
        if session.and_then(|s| s.actor(&app.store.lock().unwrap().doc.operator_members, now())).is_some_and(|a| a.role != Role::Admin) {
            return error(StatusCode::FORBIDDEN, "运营成员不能兑换管理员免密登录票据").into_response();
        }
    }
    if path.starts_with("/api/") && !agent {
        if request.method() != axum::http::Method::GET {
            if request
                .headers()
                .get("x-chain-request")
                .and_then(|v| v.to_str().ok())
                != Some("1")
            {
                return error(StatusCode::FORBIDDEN, "请求缺少来源标识").into_response();
            }
            if let Some(origin) = request
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
            {
                let host = request
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if ![format!("http://{host}"), format!("https://{host}")]
                    .contains(&origin.to_string())
                {
                    return error(StatusCode::FORBIDDEN, "请求来源与控制台不匹配").into_response();
                }
            }
        }
        if !matches!(
            path.as_str(),
            "/api/login"
                | "/api/auth/options"
                | "/api/auth/passkey/start"
                | "/api/auth/passkey/finish"
                | "/api/public/lookup"
                | "/api/federation/pair"
                | "/api/federation/login"
        ) {
            let machine = request.headers().get("authorization").and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "));
            let machine_ok = machine.is_some_and(|token| {
                let s = app.store.lock().unwrap();
                !s.doc.fleet.controller_hash.is_empty()
                    && equal(&digest(token), &s.doc.fleet.controller_hash)
                    && chain_panel::management::machine_route(request.method().as_str(), &path)
            });
            let token = cookie(request.headers());
            let hash = digest(&token);
            let session = app.sessions.lock().unwrap().get(&hash).cloned();
            let session_actor = session.and_then(|session| session.actor(&app.store.lock().unwrap().doc.operator_members, now()));
            let interactive = session_actor.is_some();
            let actor = session_actor.or_else(|| machine_ok.then(|| Actor { id: "controller".into(), name: "已授权总控".into(), role: Role::Admin }));
            let Some(actor) = actor else {
                return error(StatusCode::UNAUTHORIZED, "请先登录").into_response();
            };
            if !actor.role.allows(request.method().as_str(), &path) {
                return error(StatusCode::FORBIDDEN, "当前成员无权执行此操作").into_response();
            }
            if request.method() != axum::http::Method::GET && interactive {
                match begin_operator_audit(&app, &actor, request.method().as_str(), &path) {
                    Ok(id) => { request.extensions_mut().insert(AuditContext(id.clone())); audit = Some(id); }
                    Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "无法保存操作审计，尚未执行操作").into_response(),
                }
            }
            request.extensions_mut().insert(actor);
        }
    }
    let mut response = next.run(request).await;
    if let Some(id) = audit {
        if finish_operator_audit(&app, &id, response.status().as_u16()).is_err() {
            response = error(StatusCode::SERVICE_UNAVAILABLE, "操作结果未能写入审计，业务可能已完成；请先查询原任务或订单，不要重复提交").into_response();
        }
    }
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response.headers_mut().insert("content-security-policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

#[derive(Clone)]
struct AuditContext(String);

fn begin_operator_audit(app: &App, actor: &Actor, method: &str, path: &str) -> Result<String> {
    let mut store = app.store.lock().unwrap();
    let id = id();
    store.doc.operator_audit.push(OperatorAudit { id: id.clone(), at: now(), actor: actor.clone(), method: method.into(), path: path.into(), target: None, response_status: None });
    if let Err(e) = store.save() { store.doc.operator_audit.pop(); return Err(e); }
    Ok(id)
}

fn finish_operator_audit(app: &App, id: &str, status: u16) -> Result<()> {
    let mut store = app.store.lock().unwrap();
    let event = store.doc.operator_audit.iter_mut().find(|e| e.id == id).context("操作审计不存在")?;
    event.response_status = Some(status);
    if let Err(e) = store.save() {
        store.doc.operator_audit.iter_mut().find(|e| e.id == id).unwrap().response_status = None;
        return Err(e);
    }
    Ok(())
}
fn cookie(headers: &HeaderMap) -> String {
    headers
        .get("cookie")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .find_map(|p| p.trim().strip_prefix("chain_session="))
        .unwrap_or("")
        .into()
}
#[derive(Deserialize)]
struct Login {
    token: String,
    #[serde(default)]
    code: String,
}
async fn login(State(app): State<App>, Json(input): Json<Login>) -> ApiResult<Response> {
    app.auth.rate_limit()?;
    let hash = digest(&input.token);
    if !equal(&hash, &app.admin_hash.lock().unwrap()) {
        let member = app.store.lock().unwrap().doc.operator_members.iter()
            .find(|m| m.revoked_at == 0 && m.role != Role::Admin && equal(&hash, &m.token_hash))
            .map(|m| Actor { id: m.id.clone(), name: m.name.clone(), role: m.role });
        if let Some(actor) = member { return Ok(member_session_response(&app, actor, false)); }
        tokio::time::sleep(Duration::from_millis(400)).await;
        return Err(error(StatusCode::UNAUTHORIZED, "管理令牌不正确"));
    }
    {
        let mut store = app.store.lock().unwrap();
        chain_panel::security::second_factor(&mut store.doc.security, &input.code)
            .map_err(|_| error(StatusCode::UNAUTHORIZED, "动态验证码或恢复码不正确"))?;
        store.save()?;
    }
    Ok(session_response(&app))
}
fn session_response(app: &App) -> Response {
    session_response_as(app, false)
}
fn session_response_as(app: &App, delegated: bool) -> Response {
    let actor = if delegated { Actor { id: "controller".into(), name: "总控委托管理员".into(), role: Role::Admin } } else { Actor::admin() };
    member_session_response(app, actor, delegated)
}
fn member_session_response(app: &App, actor: Actor, delegated: bool) -> Response {
    let token = secret();
    {
        let mut sessions = app.sessions.lock().unwrap();
        sessions.retain(|_, session| session.expires > now());
        if sessions.len() > 100 {
            sessions.clear();
            app.delegated_sessions.lock().unwrap().clear();
        }
        sessions.insert(digest(&token), Session { expires: now() + 28800, actor });
        if delegated { app.delegated_sessions.lock().unwrap().insert(digest(&token)); }
    }
    let mut response = Json(json!({"ok":true})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&format!(
            "chain_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=28800{}",
            if app.secure_cookie || delegated { "; Secure" } else { "" }
        ))
        .unwrap(),
    );
    response
}
async fn logout(State(app): State<App>, headers: HeaderMap) -> Response {
    app.sessions
        .lock()
        .unwrap()
        .remove(&digest(&cookie(&headers)));
    let mut r = Json(json!({"ok":true})).into_response();
    r.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_static("chain_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    );
    r
}
fn edit<T>(app: &App, f: impl FnOnce(&mut Document) -> Result<T>) -> ApiResult<T> {
    let mut store = app.store.lock().unwrap();
    if store.doc.busy() {
        return Err(error(
            StatusCode::CONFLICT,
            "正在发布或恢复配置，请等待完成",
        ));
    }
    let previous = store.doc.clone();
    match f(&mut store.doc).and_then(|v| {
        store.save()?;
        Ok(v)
    }) {
        Ok(v) => Ok(v),
        Err(e) => {
            store.doc = previous;
            Err(e.into())
        }
    }
}
fn snapshot(doc: &Document) -> Value {
    let configs = compile(doc).ok();
    let nodes:Vec<Value>=doc.nodes.iter().map(|n|json!({"id":n.id,"name":n.name,"host":n.host,"target":n.reality.target,"server_name":n.reality.server_name,
        "seen":n.seen,"online":now().saturating_sub(n.seen)<15,"report":n.report,"desired_revision":n.desired.as_ref().map(|d|&d.revision)})).collect();
    let lines: Vec<Value> = doc
        .lines
        .iter()
        .map(|l| {
            let mut v = serde_json::to_value(l).unwrap();
            v["share"] = json!(share(l, doc).ok());
            v["plan_eligible"] = json!(!doc.rentals.contains_key(&l.id) && !doc.applied_lines.iter().any(|old| old.id == l.id) && chain_panel::plans::for_line(doc, &l.id).is_none());
            let rental = chain_panel::rental::view(doc, l, now());
            for (key, value) in rental.as_object().unwrap() {
                v[key] = value.clone();
            }
            let resources_match = configs
                .as_ref()
                .map(|configs| {
                    l.hops.iter().all(|h| {
                        let node = doc.nodes.iter().find(|n| n.id == h.node_id).unwrap();
                        node.desired.as_ref().map(|d| &d.config) == configs.get(&h.node_id)
                    })
                })
                .unwrap_or(false);
            v["published"] = json!(
                resources_match
                    && doc
                        .applied_lines
                        .iter()
                        .any(|old| serde_json::to_value(old).ok() == serde_json::to_value(l).ok())
            );
            v
        })
        .collect();
    let jobs: Vec<Value> = doc
        .jobs
        .iter()
        .rev()
        .take(20)
        .map(|j| json!({"id":j.id,"state":j.state,"started":j.started,"events":j.events,"actor":j.actor}))
        .collect();
    let subscriptions: Vec<_> = doc
        .subscriptions
        .iter()
        .map(|s| json!({"id":s.id,"name":s.name,"line_ids":s.line_ids,"created":s.created}))
        .collect();
    let legacy: Vec<_> = doc.legacy_configs.iter().chain(doc.applied_legacy_configs.iter().filter(|p| !doc.legacy_configs.iter().any(|draft| draft.id == p.id))).map(|p| json!({"id":p.id,"node_id":p.node_id,"name":p.name,"enabled":p.enabled,
        "pending_delete":!doc.legacy_configs.iter().any(|draft| draft.id == p.id),
        "ports":chain_panel::legacy_config::ports(&p.config).unwrap_or_default(),
        "published":doc.applied_legacy_configs.contains(p) && doc.nodes.iter().find(|n| n.id == p.node_id).is_some_and(|n|
            n.report.applied && n.desired.as_ref().is_some_and(|d| d.revision == n.report.revision
                && configs.as_ref().and_then(|c| c.get(&p.node_id)) == Some(&d.config)))})).collect();
    json!({"nodes":nodes,"externals":doc.externals,"lines":lines,"legacy_configs":legacy,"pending_imports":doc.pending_imports,"jobs":jobs,"checks":doc.checks,"busy":doc.busy(),"time":now(),"subscriptions":subscriptions,"security":security_api::summary(&doc.security),"alerts":delivery_api::alerts(doc),"customers":doc.customers,"customer_plans":chain_panel::plans::view(doc,now()),"payments":doc.payments,"rental_orders":doc.rental_orders,"payment_adjustments":doc.payment_adjustments,"renewal_contact":doc.renewal_contact,"resources":doc.resources.resources})
}
async fn state(State(app): State<App>) -> Json<Value> {
    let mut value = snapshot(&app.store.lock().unwrap().doc);
    let id = chain_panel::local_agent::node_id(app.token_file.parent().unwrap());
    value["local_node_id"] = json!(id.filter(|id| {
        value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == *id)
    }));
    let agent = value["nodes"].as_array().unwrap().iter()
        .find(|n| n["id"] == value["local_node_id"]);
    let agent_version = agent.map(|n| n["report"]["system"]["agent_version"].clone()).unwrap_or(Value::Null);
    let agent_seen = agent.map(|n| n["seen"].clone()).unwrap_or(Value::Null);
    value["version"] = json!(env!("CARGO_PKG_VERSION"));
    value["agent_version"] = agent_version;
    value["agent_seen"] = agent_seen;
    value["cores"] = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent()?.parent().map(|p| p.join("release.json")))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|manifest| json!({"xray":manifest["xray"],"mihomo":manifest["mihomo"],
            "xray_channel":manifest["xray_channel"],"xray_prerelease":manifest["xray_prerelease"]}))
        .unwrap_or(Value::Null);
    value["maintenance"] = json!(cfg!(target_os = "linux")
        && std::path::Path::new("/run/chain-maintenance/control.sock").exists());
    value["maintenance_actions"] = if value["maintenance"] == true {
        json!(["upgrade", "upgrade-xray-latest", "restart-all", "restart-panel", "restart-agent"])
    } else { json!([]) };
    Json(value)
}
async fn state_view(State(app): State<App>, Extension(actor): Extension<Actor>) -> Json<Value> {
    let Json(value) = state(State(app)).await;
    Json(chain_panel::security::state_for(value, &actor))
}
#[derive(Deserialize)]
struct LocalInput {
    host: String,
    #[serde(default)]
    name: String,
}
async fn local_node(
    State(app): State<App>,
    Json(input): Json<LocalInput>,
) -> ApiResult<Json<Value>> {
    let dir = app.token_file.parent().unwrap();
    let mut local = app.local_agent.lock().await;
    if dir.join("local-system-node").exists() && chain_panel::local_agent::node_id(dir).is_none() {
        return Err(error(
            StatusCode::CONFLICT,
            "系统本机身份文件无效，请修复 local-system-node",
        ));
    }
    if let Some(id) = chain_panel::local_agent::node_id(dir) {
        if !app
            .store
            .lock()
            .unwrap()
            .doc
            .nodes
            .iter()
            .any(|n| n.id == id)
        {
            if dir.join("local-system-node").exists() {
                let recovery = std::fs::read(dir.join("local-system-identity.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .ok_or_else(|| {
                        error(
                            StatusCode::CONFLICT,
                            "本机接入记录缺失，请在本机运行 chainctl install 更新接入信息后重试",
                        )
                    })?;
                let hash = recovery["token_hash"]
                    .as_str()
                    .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                    .filter(|_| recovery["id"] == id)
                    .ok_or_else(|| {
                        error(
                            StatusCode::CONFLICT,
                            "本机恢复信息无效，请在本机运行 chainctl install",
                        )
                    })?;
                let node = NodeInput {
                    name: "本机".into(),
                    host: input.host.clone(),
                    target: default_target(),
                    server_name: default_server_name(),
                };
                validate_node(&node)?;
                edit(&app, |doc| {
                    doc.nodes.push(Node {
                        id: id.clone(),
                        name: node.name,
                        host: node.host,
                        reality: Reality::new(node.target, node.server_name),
                        token_hash: hash.into(),
                        seen: 0,
                        report: Report::default(),
                        desired: None,
                    });
                    Ok(())
                })?;
                return Ok(Json(json!({"id":id})));
            }
            local.stop().await;
            std::fs::remove_file(dir.join("local-agent.json"))?;
        } else {
            local.ensure(dir, &app.local_url, &app.xray).await?;
            return Ok(Json(json!({"id":id})));
        }
    }
    let input = NodeInput {
        name: if input.name.trim().is_empty() {
            "控制台本机".into()
        } else {
            input.name
        },
        host: input.host,
        target: default_target(),
        server_name: default_server_name(),
    };
    validate_node(&input)?;
    chain_panel::local_agent::binary()?;
    let identity = chain_panel::local_agent::Identity {
        id: id(),
        token: secret(),
    };
    let saved = edit(&app, |d| {
        d.nodes.push(Node {
            id: identity.id.clone(),
            name: input.name,
            host: input.host,
            reality: Reality::new(input.target, input.server_name),
            token_hash: digest(&identity.token),
            seen: 0,
            report: Report::default(),
            desired: None,
        });
        chain_panel::local_agent::save(dir, &identity)?;
        Ok(())
    });
    if let Err(error) = saved {
        let _ = std::fs::remove_file(dir.join("local-agent.json"));
        return Err(error);
    }
    local.ensure(dir, &app.local_url, &app.xray).await?;
    Ok(Json(json!({"id":identity.id})))
}
#[derive(Deserialize)]
struct NodeInput {
    name: String,
    host: String,
    #[serde(default = "default_target")]
    target: String,
    #[serde(default = "default_server_name")]
    server_name: String,
}
fn default_target() -> String {
    "www.microsoft.com:443".into()
}
fn default_server_name() -> String {
    "www.microsoft.com".into()
}
fn validate_node(n: &NodeInput) -> Result<()> {
    ensure!(
        !n.name.trim().is_empty() && n.name.len() <= 120,
        "节点名称不能为空或过长"
    );
    ensure!(
        host_valid(&n.host),
        "节点地址应为 IP 或域名，不含协议和端口"
    );
    target(&n.target)?;
    ensure!(host_valid(&n.server_name), "REALITY server name 无效");
    Ok(())
}
async fn add_node(State(app): State<App>, Json(input): Json<NodeInput>) -> ApiResult<Json<Value>> {
    validate_node(&input)?;
    let token = secret();
    let node_id = id();
    edit(&app, |d| {
        d.nodes.push(Node {
            id: node_id.clone(),
            name: input.name,
            host: input.host,
            reality: Reality::new(input.target, input.server_name),
            token_hash: digest(&token),
            seen: 0,
            report: Report::default(),
            desired: None,
        });
        Ok(())
    })?;
    Ok(Json(json!({"id":node_id,"token":token})))
}

async fn update_node(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<NodeInput>,
) -> ApiResult<Json<Value>> {
    validate_node(&input)?;
    edit(&app, |d| {
        let n = d
            .nodes
            .iter_mut()
            .find(|n| n.id == id)
            .context("节点不存在")?;
        n.name = input.name;
        n.host = input.host;
        n.reality.target = input.target;
        n.reality.server_name = input.server_name;
        d.checks.clear();
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn remove_node(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if chain_panel::local_agent::node_id(app.token_file.parent().unwrap()).as_deref()
        == Some(id.as_str())
    {
        return Err(error(
            StatusCode::CONFLICT,
            "本机由控制台管理，不能删除服务器记录；可删除其代理",
        ));
    }
    edit(&app, |d| {
        ensure!(
            !d.lines
                .iter()
                .chain(d.applied_lines.iter())
                .any(|l| l.hops.iter().any(|h| h.node_id == id)),
            "节点仍被草稿或已发布线路引用；请移除线路并发布后再删除"
        );
        ensure!(d.nodes.iter().any(|n| n.id == id), "节点不存在");
        ensure!(
            !d.legacy_configs
                .iter()
                .chain(d.applied_legacy_configs.iter())
                .any(|p| p.node_id == id),
            "节点仍有旧代理配置，请移除并发布后再删除节点"
        );
        d.nodes.retain(|n| n.id != id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn rotate_node(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if chain_panel::local_agent::node_id(app.token_file.parent().unwrap()).as_deref()
        == Some(id.as_str())
    {
        return Err(error(
            StatusCode::CONFLICT,
            "本机凭据由控制台管理，无需重新接入",
        ));
    }
    let token = secret();
    edit(&app, |d| {
        d.nodes
            .iter_mut()
            .find(|n| n.id == id)
            .context("节点不存在")?
            .token_hash = digest(&token);
        Ok(())
    })?;
    Ok(Json(json!({"token":token})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalInput {
    #[serde(default)]
    name: String,
    url: String,
}
async fn add_external(
    State(app): State<App>,
    Json(mut input): Json<ExternalInput>,
) -> ApiResult<Json<Value>> {
    let (url, name) = chain_panel::external::import(&input.url)?;
    input.url = url;
    if input.name.trim().is_empty() {
        input.name = if name.is_empty() {
            url::Url::parse(&input.url)?
                .host_str()
                .unwrap_or("代理")
                .to_owned()
        } else {
            name
        };
    }
    if input.name.trim().is_empty() || input.name.len() > 120 {
        return Err(error(StatusCode::BAD_REQUEST, "名称无效"));
    }
    let item = External {
        id: id(),
        name: input.name,
        url: input.url,
    };
    edit(&app, |d| {
        d.externals.push(item.clone());
        Ok(())
    })?;
    Ok(Json(json!(item)))
}
#[derive(Deserialize)]
struct BatchExternalInput { urls: Vec<String> }
async fn batch_external(State(app):State<App>,Json(input):Json<BatchExternalInput>)->ApiResult<Json<Value>> {
    if input.urls.is_empty() || input.urls.len()>500 {return Err(error(StatusCode::BAD_REQUEST,"一次导入 1–500 条链接"));}
    let mut values=Vec::new();
    for value in input.urls {
        let (url,name)=chain_panel::external::import(&value)?;
        let name=if name.is_empty(){url::Url::parse(&url)?.host_str().unwrap_or("外部代理").to_owned()}else{name};
        if name.trim().is_empty() || name.len()>120 {return Err(error(StatusCode::BAD_REQUEST,"名称无效"));}
        values.push(External{id:id(),name,url});
    }
    edit(&app,|doc|{doc.externals.extend(values.clone());Ok(())})?;
    Ok(Json(json!(values)))
}
async fn remove_external(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    edit(&app, |d| {
        ensure!(
            !d.lines
                .iter()
                .chain(d.applied_lines.iter())
                .any(|l| l.proxy_chain().contains(&format!("external:{id}"))),
            "外部节点仍被草稿或已发布线路引用"
        );
        d.externals.retain(|e| e.id != id);
        for r in &mut d.resources.resources {r.members.retain(|m|m!=&format!("external:{id}"));}
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn update_external(State(app): State<App>, Path(id): Path<String>, Json(input): Json<ExternalInput>) -> ApiResult<Json<Value>> {
    let (url, imported_name) = chain_panel::external::import(&input.url)?;
    let name = if input.name.trim().is_empty() { imported_name } else { input.name };
    if name.trim().is_empty() || name.len() > 120 { return Err(error(StatusCode::BAD_REQUEST, "名称无效")); }
    let item = edit(&app, |d| {
        let external = d.externals.iter_mut().find(|e| e.id == id).context("外部代理不存在")?;
        external.name = name; external.url = url;
        let item = external.clone();
        compile(d)?;
        Ok(item)
    })?;
    Ok(Json(json!(item)))
}
#[derive(Deserialize)]
struct HopInput {
    node_id: String,
    #[serde(default)]
    port: Option<u16>,
}
#[derive(Deserialize)]
struct LineInput {
    #[serde(default)]
    pending_import_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    name: String,
    kind: Kind,
    hops: Vec<HopInput>,
    #[serde(default)]
    external_id: Option<String>,
    #[serde(default)]
    chain: Vec<String>,
    #[serde(default)]
    target: String,
    #[serde(default = "tcp")]
    network: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    access: Option<Access>,
    #[serde(default)]
    limits: Option<Limits>,
}
fn tcp() -> String {
    "tcp".into()
}
fn yes() -> bool {
    true
}
async fn save_line(State(app): State<App>, Json(input): Json<LineInput>) -> ApiResult<Json<Value>> {
    let line = edit(&app, |d| {
        let line = put_line(d, input)?;
        compile(d)?;
        Ok(line)
    })?;
    Ok(Json(json!(line)))
}
async fn save_lines(
    State(app): State<App>,
    Json(inputs): Json<Vec<LineInput>>,
) -> ApiResult<Json<Value>> {
    if inputs.is_empty() || inputs.len() > 16 || inputs.iter().any(|input| input.id.is_some()) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "批量创建接受 1–16 条新代理，不接受已有线路 ID",
        ));
    }
    let lines = edit(&app, |d| {
        let lines = inputs
            .into_iter()
            .map(|input| put_line(d, input))
            .collect::<Result<Vec<_>>>()?;
        compile(d)?;
        Ok(lines)
    })?;
    Ok(Json(json!(lines)))
}
fn put_line(d: &mut Document, input: LineInput) -> Result<Line> {
    if let Some(id) = &input.pending_import_id {
        ensure!(d.pending_imports.iter().any(|p| &p.id == id), "待配置项目不存在");
        d.pending_imports.retain(|p| &p.id != id);
    }
    ensure!(
        input.kind != Kind::Forward,
        "仅支持创建代理，端口转发已移除"
    );
    ensure!(
        !input
            .access
            .as_ref()
            .is_some_and(|a| !a.public_host.is_empty() || a.public_port.is_some()),
        "购买中转入口功能已移除"
    );
    let existing = input
        .id
        .as_ref()
        .and_then(|id| d.lines.iter().find(|l| &l.id == id))
        .cloned();
    ensure!(input.id.is_none() || existing.is_some(), "线路不存在");
    let line_id = existing.as_ref().map(|l| l.id.clone()).unwrap_or_else(id);
    ensure!(chain_panel::plans::for_line(d, &line_id).is_none() || input.kind == Kind::Vless, "客户套餐线路须保留代理类型");
    let mut used: BTreeSet<(String, u16)> = d
        .lines
        .iter()
        .filter(|l| l.id != line_id)
        .flat_map(|l| l.hops.iter().map(|h| (h.node_id.clone(), h.port)))
        .collect();
    let mut hops = vec![];
    for h in input.hops {
        let occupied: BTreeSet<u16> = d
            .nodes
            .iter()
            .find(|n| n.id == h.node_id)
            .and_then(|n| n.report.system["listening_ports"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_u64().and_then(|p| u16::try_from(p).ok()))
            .collect();
        let old = existing
            .as_ref()
            .and_then(|l| l.hops.iter().find(|o| o.node_id == h.node_id));
        let port = h
            .port
            .or(old.map(|h| h.port))
            .or_else(|| {
                (20000..50000)
                    .find(|p| !used.contains(&(h.node_id.clone(), *p)) && !occupied.contains(p))
            })
            .context("没有可分配的端口")?;
        used.insert((h.node_id.clone(), port));
        hops.push(Hop {
            node_id: h.node_id,
            port,
            uuid: old.map(|h| h.uuid.clone()).unwrap_or_else(id),
        });
    }
    let mut access = input
        .access
        .or_else(|| existing.as_ref().map(|l| l.access.clone()))
        .unwrap_or_default();
    chain_panel::access::fill_credentials(&mut access);
    let line = Line {
        id: line_id.clone(),
        name: input.name,
        kind: input.kind,
        hops,
        external_id: input.external_id.filter(|s| !s.is_empty()),
        chain: input.chain,
        target: input.target,
        network: input.network,
        enabled: input.enabled,
        access,
        limits: input
            .limits
            .or_else(|| existing.as_ref().map(|l| l.limits.clone()))
            .unwrap_or_default(),
    };
    d.lines.retain(|l| l.id != line_id);
    d.lines.push(line.clone());
    d.checks.remove(&line_id);
    Ok(line)
}
async fn import_legacy(
    State(app): State<App>,
    Json(mut input): Json<chain_panel::legacy::Import>,
) -> ApiResult<Json<Value>> {
    if input.preserve_config {
        for source in &mut input.sources {
            if !source.error.is_empty() {
                continue;
            }
            let profile = LegacyConfig {
                id: id(),
                node_id: input.node_id.clone(),
                name: source.name.clone(),
                config: source.config.clone(),
                asset_dir: source.asset_dir.clone(),
                source_inbounds: source.source_inbounds.clone(),
                enabled: true,
            };
            if let Err(error) = validate_legacy_config(&app, &profile).await {
                source.error = format!("{error:#}");
            }
        }
    }
    if input.preview {
        let mut doc = app.store.lock().unwrap().doc.clone();
        if doc.busy() {
            return Err(error(StatusCode::CONFLICT, "请等待发布完成"));
        }
        return Ok(Json(chain_panel::legacy::import(&mut doc, &input)?));
    }
    Ok(Json(edit(&app, |doc| {
        chain_panel::legacy::import(doc, &input)
    })?))
}

async fn validate_legacy_config(app: &App, profile: &LegacyConfig) -> Result<()> {
    chain_panel::legacy_config::validate(profile)?;
    if !profile.enabled {
        return Ok(());
    }
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.json");
    atomic_write(
        &path,
        &serde_json::to_vec(&chain_panel::legacy_config::runtime_config(profile))?,
    )?;
    validate_legacy(&app.xray, &path, &profile.asset_dir).await
}

async fn get_legacy_config(
    State(app): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Json<LegacyConfig>> {
    let doc = &app.store.lock().unwrap().doc;
    Ok(Json(
        doc.legacy_configs
            .iter()
            .find(|p| p.id == id)
            .context("旧配置不存在")?
            .clone(),
    ))
}

async fn save_legacy_config(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<LegacyConfig>,
) -> ApiResult<Json<Value>> {
    if input.id != id {
        return Err(error(StatusCode::BAD_REQUEST, "旧配置 ID 不一致"));
    }
    validate_legacy_config(&app, &input).await?;
    edit(&app, |doc| {
        let old = doc
            .legacy_configs
            .iter_mut()
            .find(|p| p.id == id)
            .context("旧配置不存在")?;
        *old = input;
        compile(doc)?;
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}

async fn delete_legacy_config(
    State(app): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        doc.legacy_configs.retain(|p| p.id != id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}

async fn remove_pending_import(
    State(app): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        doc.pending_imports.retain(|p| p.id != id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}

async fn check_reality(
    State(app): State<App>,
    Json(input): Json<RealityTarget>,
) -> ApiResult<Json<Value>> {
    let _guard = app
        .probe_lock
        .try_lock()
        .map_err(|_| error(StatusCode::CONFLICT, "已有检测正在运行"))?;
    Ok(Json(chain_panel::runtime::check_reality(&input).await?))
}
async fn remove_line(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let proxy = app
        .store
        .lock()
        .unwrap()
        .doc
        .lines
        .iter()
        .any(|l| l.id == id && l.kind == Kind::Vless);
    if proxy {
        return rental_api::delete_one(State(app), Path(id)).await;
    }
    edit(&app, |d| {
        ensure!(d.lines.iter().any(|l| l.id == id), "线路不存在");
        ensure!(
            !d.lines
                .iter()
                .chain(d.applied_lines.iter())
                .any(|l| l.id != id && l.proxy_chain().contains(&format!("line:{id}"))),
            "该代理仍被链式代理引用，请先移除引用并应用"
        );
        d.lines.retain(|l| l.id != id);
        d.checks.remove(&id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
fn changed(doc: &Document, configs: &BTreeMap<String, Value>) -> BTreeSet<String> {
    doc.nodes
        .iter()
        .filter(|n| {
            let config = &configs[&n.id];
            (n.desired.is_some()
                || !config["inbounds"].as_array().unwrap().is_empty()
                || config["_chain"].as_array().is_some_and(|v| !v.is_empty())
                || config["_legacy"].as_array().is_some_and(|v| !v.is_empty()))
                && (n.desired.as_ref().map(|v| &v.config) != Some(config)
                    || !n.report.applied
                    || !n.report.running
                    || n.desired.as_ref().map(|v| &v.revision) != Some(&n.report.revision))
        })
        .map(|n| n.id.clone())
        .collect()
}
async fn preview(State(app): State<App>) -> ApiResult<Json<Value>> {
    let doc = &app.store.lock().unwrap().doc;
    Ok(Json(publication_preview(doc)?))
}
fn publication_preview(doc: &Document) -> Result<Value> {
    let configs = compile(doc)?;
    let affected = changed(doc, &configs);
    let restarting: BTreeSet<_> = doc
        .nodes
        .iter()
        .filter(|n| affected.contains(&n.id))
        .filter(|n| {
            let Some(old) = &n.desired else { return true };
            chain_panel::gateway::xray_config(&old.config)
                != chain_panel::gateway::xray_config(&configs[&n.id])
                || old.config.get("_mihomo") != configs[&n.id].get("_mihomo")
                || old.config.get("_legacy") != configs[&n.id].get("_legacy")
                || !chain_panel::gateway::same_listeners(
                    &chain_panel::gateway::specs(&old.config).unwrap_or_default(),
                    &chain_panel::gateway::specs(&configs[&n.id]).unwrap_or_default(),
                )
                || !n.report.running
                || !n.report.applied
        })
        .map(|n| n.id.clone())
        .collect();
    let nodes:Vec<Value>=doc.nodes.iter().filter(|n|affected.contains(&n.id)).map(|n|json!({"id":n.id,"name":n.name,"online":now().saturating_sub(n.seen)<15,
        "ports":doc.lines.iter().filter(|l|l.enabled).flat_map(|l|l.hops.iter()).filter(|h|h.node_id==n.id).map(|h|h.port).collect::<BTreeSet<_>>()})).collect();
    let lines: BTreeSet<String> = doc
        .lines
        .iter()
        .chain(doc.applied_lines.iter())
        .filter(|l| l.hops.iter().any(|h| restarting.contains(&h.node_id)))
        .map(|l| l.name.clone())
        .collect();
    let affected_ids: BTreeSet<_> = doc
        .lines
        .iter()
        .chain(doc.applied_lines.iter())
        .filter(|l| l.hops.iter().any(|h| restarting.contains(&h.node_id)))
        .map(|l| l.id.clone())
        .collect();
    let legacy: BTreeSet<_> = doc.legacy_configs.iter().chain(doc.applied_legacy_configs.iter())
        .filter(|p| restarting.contains(&p.node_id)).map(|p| p.name.clone()).collect();
    Ok(json!({"nodes":nodes,"affected_lines":lines,"affected_line_ids":affected_ids,"affected_legacy_configs":legacy,"message":if restarting.is_empty(){"流量和限速设置会在线更新，无需重启线路。"}else{"应用会重启受影响服务器上由本平台管理的实例；共享该实例的线路会短暂重连。"}}))
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishOptions { #[serde(default)] allow_unverified_reality: bool }
fn authorize_publication(doc: &Document, role: Role) -> Result<()> {
    if role == Role::Ops {
        let leased: BTreeSet<_> = doc.lines.iter().chain(&doc.applied_lines).filter(|l| chain_panel::plans::lease(doc, &l.id).is_some()).map(|l| &l.id).collect();
        for id in leased {
            let old = doc.applied_lines.iter().find(|l| &l.id == id);
            let draft = doc.lines.iter().find(|l| &l.id == id);
            ensure!(matches!((old, draft), (Some(old), Some(draft)) if old.limits == draft.limits && old.enabled == draft.enabled), "运维不能通过发布改变已出租代理的额度、启停或删除状态，请由管理员确认");
        }
    }
    Ok(())
}
async fn publish(State(app): State<App>, Extension(actor): Extension<Actor>, input: Option<Json<PublishOptions>>) -> ApiResult<Json<Value>> {
    let _probe_guard = app
        .probe_lock
        .try_lock()
        .map_err(|_| error(StatusCode::CONFLICT, "正在执行整链探测，请完成后发布"))?;
    let checked = diagnostics_api::preflight(&app, input.map(|v| v.0.allow_unverified_reality).unwrap_or(false)).await?;
    let job = edit(&app, |d| {
        authorize_publication(d, actor.role)?;
        let configs = compile(d)?;
        ensure!(configs == checked, "检测期间配置已改变，请重新预览并下发");
        let affected = changed(d, &configs);
        for node in d.nodes.iter().filter(|n| affected.contains(&n.id)) {
            if configs[&node.id]["_legacy"]
                .as_array()
                .is_some_and(|s| !s.is_empty())
            {
                ensure!(
                    node.report.system["legacy_config_version"] == 1,
                    "{} 的 Agent 不支持完整配置迁移，请先升级 Agent",
                    node.name
                );
            }
            if configs[&node.id]["_chain"]
                .as_array()
                .is_some_and(|s| !s.is_empty())
            {
                if configs[&node.id]["_chain"].as_array().unwrap().iter().any(|s| s["resource_ids"].as_array().is_some_and(|ids| ids.iter().any(|id| id.as_str().is_some_and(chain_panel::plans::is_budget)))) {
                    ensure!(node.report.system["customer_plan_version"] == 1, "{} 的 Agent 不支持客户共享套餐，请先升级并等待心跳", node.name);
                }
                if configs[&node.id]["_chain"].as_array().unwrap().iter().any(|s|s["multiplier"].as_u64().unwrap_or(1)>1 || s["resource_ids"].as_array().is_some_and(|p|!p.is_empty()) || chain_panel::rules::requires_capability(&s["access_policy"]["policy"])) {
                    ensure!(node.report.system["sharing_version"]==1,"请先升级 Agent 以支持倍率和访问控制");
                }
                ensure!(
                    node.report.system["gateway_version"] == 2,
                    "{} 的 Agent 尚未就绪或版本过旧，请先更新 Agent 并等待心跳后再应用",
                    node.name
                );
            }
        }
        let mut order = vec![];
        for line in &d.lines {
            for h in line.hops.iter().rev() {
                if affected.contains(&h.node_id) && !order.contains(&h.node_id) {
                    order.push(h.node_id.clone());
                }
            }
        }
        for id in &affected {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        let job_id = id();
        let before = d
            .nodes
            .iter()
            .filter(|n| affected.contains(&n.id))
            .map(|n| (n.id.clone(), n.desired.clone()))
            .collect();
        let target = affected
            .iter()
            .map(|node| {
                (
                    node.clone(),
                    Desired {
                        revision: format!("{job_id}-{node}"),
                        config: configs[node].clone(),
                    },
                )
            })
            .collect();
        let mut nodes = d.nodes.clone();
        for n in &mut nodes {
            n.token_hash.clear();
            n.desired = None;
            n.report = Report::default();
        }
        let job = Job {
            actor: Some(actor.clone()),
            legacy_configs: d.legacy_configs.clone(),
            id: job_id,
            state: "publishing".into(),
            started: now(),
            events: vec![Event {
                at: now(),
                message: format!("开始发布，影响 {} 个节点", affected.len()),
            }],
            before,
            target,
            order,
            lines: d.lines.clone(),
            nodes,
            externals: d.externals.clone(),
        };
        d.jobs.push(job.clone());
        if d.jobs.len() > 20 {
            d.jobs.remove(0);
        }
        Ok(job)
    })?;
    let job_id = job.id.clone();
    drop(_probe_guard);
    tokio::spawn(async move {
        run_publish(app, job, false).await;
    });
    Ok(Json(json!({"job_id":job_id})))
}
fn log_event(app: &App, id: &str, message: String) {
    let mut s = app.store.lock().unwrap();
    if let Some(j) = s.doc.jobs.iter_mut().find(|j| j.id == id) {
        j.events.push(Event { at: now(), message });
    }
    if let Err(e) = s.save() {
        eprintln!("记录发布事件失败: {e}");
        let _ = chain_panel::diagnostics::append(&app.token_file.parent().unwrap().join("diagnostics.log"), &format!("记录发布事件失败: {e}"));
    }
}
async fn push(app: &App, node_id: &str, desired: Desired) -> Result<()> {
    {
        let mut s = app.store.lock().unwrap();
        let node = s
            .doc
            .nodes
            .iter_mut()
            .find(|n| n.id == node_id)
            .context("节点已删除")?;
        node.desired = Some(desired.clone());
        s.save()?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(app.apply_timeout);
    loop {
        {
            let s = app.store.lock().unwrap();
            let node = s
                .doc
                .nodes
                .iter()
                .find(|n| n.id == node_id)
                .context("节点不存在")?;
            if node.report.revision == desired.revision {
                ensure!(
                    node.report.applied && node.report.running,
                    "节点应用失败: {}",
                    node.report.error
                );
                return Ok(());
            }
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "节点未在 {} 秒内确认（离线或应用超时）",
            app.apply_timeout
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
async fn run_publish(app: App, job: Job, recovery: bool) {
    let mut attempted = vec![];
    let mut failure = if recovery {
        Some("控制台重启，恢复发布前配置".to_string())
    } else {
        None
    };
    if !recovery {
        for node in &job.order {
            attempted.push(node.clone());
            log_event(&app, &job.id, format!("向节点 {} 下发配置", node));
            if let Err(e) = push(&app, node, job.target[node].clone()).await {
                failure = Some(format!("{e:#}"));
                break;
            }
            log_event(&app, &job.id, format!("节点 {} 已应用", node));
        }
    } else {
        attempted = job.order.clone();
    }
    let outcome = if let Some(reason) = failure {
        log_event(&app, &job.id, format!("发布未完成: {reason}；开始恢复"));
        {
            let mut s = app.store.lock().unwrap();
            if let Some(j) = s.doc.jobs.iter_mut().find(|j| j.id == job.id) {
                j.state = "rolling_back".into();
            }
            let _ = s.save();
        }
        let mut restored = true;
        for node in attempted.iter().rev() {
            let old = job.before[node]
                .as_ref()
                .map(|d| d.config.clone())
                .unwrap_or_else(empty_config);
            let desired = Desired {
                revision: format!("restore-{}-{}", job.id, id()),
                config: old,
            };
            match push(&app, node, desired).await {
                Ok(()) => log_event(&app, &job.id, format!("节点 {node} 已恢复")),
                Err(e) => {
                    restored = false;
                    log_event(
                        &app,
                        &job.id,
                        format!("节点 {node} 尚未确认恢复: {e}；恢复配置保留，等待 Agent 重连"),
                    );
                }
            }
        }
        if restored { "rolled_back" } else { "degraded" }
    } else {
        "applied"
    };
    let mut s = app.store.lock().unwrap();
    if outcome == "applied" {
        s.doc.applied_lines = job.lines;
        s.doc.applied_nodes = job.nodes;
        s.doc.applied_externals = job.externals;
        s.doc.applied_legacy_configs = job.legacy_configs;
        s.doc.checks.clear();
    }
    if let Some(j) = s.doc.jobs.iter_mut().find(|j| j.id == job.id) {
        j.state = outcome.into();
        j.events.push(Event {
            at: now(),
            message: if outcome == "applied" {
                "配置已应用；请继续执行整链诊断".into()
            } else {
                format!("发布结果: {outcome}")
            },
        });
    }
    if let Err(e) = s.save() {
        eprintln!("保存发布结果失败: {e}");
        let _ = chain_panel::diagnostics::append(&app.token_file.parent().unwrap().join("diagnostics.log"), &format!("保存发布结果失败: {e}"));
    }
}
async fn sync(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(mut report): Json<Report>,
) -> ApiResult<Json<Option<Desired>>> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut s = app.store.lock().unwrap();
    let n = s
        .doc
        .nodes
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "节点身份无效"))?;
    if !equal(&digest(token), &n.token_hash) {
        return Err(error(StatusCode::UNAUTHORIZED, "节点身份无效"));
    }
    report.error = report.error.chars().take(2000).collect();
    report.revision = report.revision.chars().take(180).collect();
    let mut persist = report != n.report;
    n.report = report;
    n.seen = now();
    let desired = n.desired.clone();
    if let Some(job) = s.doc.jobs.last().filter(|j| j.state == "degraded").cloned() {
        let restored = job.before.iter().all(|(id, old)| {
            s.doc
                .nodes
                .iter()
                .find(|n| &n.id == id)
                .map(|n| {
                    n.desired.as_ref().map(|d| d.config.clone())
                        == Some(
                            old.as_ref()
                                .map(|d| d.config.clone())
                                .unwrap_or_else(empty_config),
                        )
                        && n.report.applied
                        && n.report.running
                        && n.desired.as_ref().map(|d| &d.revision) == Some(&n.report.revision)
                })
                .unwrap_or(false)
        });
        if restored {
            let j = s.doc.jobs.last_mut().unwrap();
            j.state = "rolled_back".into();
            j.events.push(Event {
                at: now(),
                message: "离线节点已重新连接，全部恢复配置均已确认".into(),
            });
            persist = true;
        }
    }
    // Heartbeats stay in memory; only version/result changes require a database write.
    if persist {
        s.save()?;
    }
    Ok(Json(desired))
}
async fn diagnose(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<ProbeRequest>,
) -> ApiResult<Json<Value>> {
    let _guard = app
        .probe_lock
        .try_lock()
        .map_err(|_| error(StatusCode::CONFLICT, "已有探测正在进行"))?;
    let doc = {
        let s = app.store.lock().unwrap();
        if s.doc.busy() {
            return Err(error(StatusCode::CONFLICT, "发布期间不能进行诊断"));
        }
        Document {
            nodes: s.doc.applied_nodes.clone(),
            lines: s.doc.applied_lines.clone(),
            externals: s.doc.applied_externals.clone(),
            legacy_configs: s.doc.applied_legacy_configs.clone(),
            ..Default::default()
        }
    };
    let line = doc
        .lines
        .iter()
        .find(|l| l.id == id)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "请先发布该线路"))?;
    let local_node = chain_panel::local_agent::node_id(app.token_file.parent().unwrap());
    let result = match tokio::time::timeout(
        Duration::from_secs(40),
        probe(&app.xray, line, &doc, &request, local_node.as_deref()),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => json!({"ok":false,"detail":format!("{e:#}"),"checked_at":now()}),
        Err(_) => {
            json!({"ok":false,"detail":"探测超时；UDP/TCP 回显模式需要目标提供相应回显服务","checked_at":now()})
        }
    };
    {
        let mut s = app.store.lock().unwrap();
        monitor_api::record_manual_probe(&mut s.doc, line, &request, result.clone());
        s.doc.checks.insert(id, result.clone());
        s.save()?;
    }
    Ok(Json(result))
}

static WEB: include_dir::Dir<'_> = include_dir::include_dir!("$CHAIN_EMBED_WEB");

async fn web_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return error(StatusCode::NOT_FOUND, "接口不存在").into_response();
    }
    let path = if path.is_empty() || matches!(path, "admin" | "admin/" | "query" | "query/") {
        "index.html"
    } else {
        path
    };
    let Some(file) = WEB.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    };
    ([(axum::http::header::CONTENT_TYPE, mime)], file.contents()).into_response()
}

#[cfg(test)]
mod web_tests {
    use super::*;

    #[test]
    fn state_write_guard_preserves_new_data_against_previous_writers() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY,body TEXT NOT NULL)").unwrap();
        let mut legacy = serde_json::to_value(Document::default()).unwrap();
        for key in PROTECTED_STATE_FIELDS { legacy.as_object_mut().unwrap().remove(key); }
        let legacy_body = legacy.to_string();
        conn.execute("INSERT INTO state VALUES(1,?1)", [&legacy_body]).unwrap();
        install_state_write_guard(&conn).unwrap();
        install_state_write_guard(&conn).unwrap();
        let read_body = |conn: &Connection| conn.query_row("SELECT body FROM state WHERE id=1", [], |row| row.get::<_, String>(0)).unwrap();
        assert_eq!(read_body(&conn), legacy_body, "打开旧库不得改写业务记录");
        let old_writes = [
            "INSERT INTO state(id,body) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            "UPDATE state SET body=?1 WHERE id=1",
        ];
        legacy["renewal_contact"] = json!("旧程序仍可写入尚未迁移的文档");
        for sql in old_writes { conn.execute(sql, [legacy.to_string()]).unwrap(); }
        let mut store = Store { doc: serde_json::from_str(&read_body(&conn)).unwrap(), conn };
        store.doc.operator_members.push(OperatorMember { id:"member".into(), name:"财务".into(), role:Role::Finance,
            token_hash:digest("fixture-member"), created_at:1, revoked_at:0 });
        store.doc.rental_orders.push(RentalOrder { id:"order".into(), request_id:"request".into(), request_hash:"hash".into(),
            created:1, confirmed_at:0, cancelled_at:0, term:"monthly".into(), amounts:BTreeMap::new(), note:"新订单".into(), items:vec![],
            created_session:String::new(), confirmed_session:String::new(), cancelled_session:String::new(),
            created_by:Some(Actor::admin()), confirmed_by:None, cancelled_by:None });
        store.save().unwrap();
        let saved = read_body(&store.conn);
        for sql in old_writes {
            let error = store.conn.execute(sql, [legacy.to_string()]).unwrap_err();
            assert!(error.to_string().contains("拒绝旧程序覆盖"));
            assert_eq!(read_body(&store.conn), saved, "旧程序写入失败后数据库须逐字不变");
            for field in PROTECTED_STATE_FIELDS {
                let mut incomplete: Value = serde_json::from_str(&saved).unwrap();
                incomplete.as_object_mut().unwrap().remove(field);
                assert!(store.conn.execute(sql, [incomplete.to_string()]).is_err(), "{field}");
                assert_eq!(read_body(&store.conn), saved, "{field}");
            }
        }
        store.doc.rental_orders[0].note = "新版继续保存".into();
        store.save().unwrap();
        let loaded: Document = serde_json::from_str(&read_body(&store.conn)).unwrap();
        assert_eq!(loaded.rental_orders[0].note, "新版继续保存");
        assert_eq!(loaded.operator_members[0].id, "member");
    }

    #[test]
    fn ops_publication_preserves_independent_and_shared_entitlements() {
        let line: Line = serde_json::from_value(json!({"id":"line","name":"line","kind":"vless","enabled":true,"hops":[]})).unwrap();
        for shared in [false, true] {
            let mut doc = Document { lines: vec![line.clone()], applied_lines: vec![line.clone()], ..Default::default() };
            if shared {
                doc.customer_plans.push(chain_panel::plans::Plan { id:"plan".into(), name:"plan".into(), customer_id:"customer".into(), monthly_gib:10.0,
                    members: vec![chain_panel::plans::Member { id:"member".into(), name:"member".into(), line_ids:vec![line.id.clone()], monthly_gib:None }] });
            }
            doc.rentals.insert(if shared { "plan:plan" } else { "line" }.into(), Rental::default());
            assert!(authorize_publication(&doc, Role::Ops).is_ok());
            doc.lines[0].limits.monthly_gib = Some(1.0);
            assert!(authorize_publication(&doc, Role::Ops).is_err());
            assert!(authorize_publication(&doc, Role::Admin).is_ok());
            doc.lines[0] = line.clone();
            doc.lines[0].enabled = false;
            assert!(authorize_publication(&doc, Role::Ops).is_err());
            doc.lines.clear();
            assert!(authorize_publication(&doc, Role::Ops).is_err());
            doc.lines.push(line.clone());
            doc.applied_lines.clear();
            assert!(authorize_publication(&doc, Role::Ops).is_err(), "首次发布已出租线路需管理员确认");
        }
    }

    #[test]
    fn legacy_profile_publication_and_snapshot_keep_secrets_out_of_summary() {
        let node = Node {
            id: id(),
            name: "node".into(),
            host: "127.0.0.1".into(),
            reality: Reality::new("example.com:443".into(), "example.com".into()),
            token_hash: String::new(),
            seen: 0,
            report: Report::default(),
            desired: None,
        };
        let profile = LegacyConfig {
            id: id(),
            node_id: node.id.clone(),
            name: "imported".into(),
            enabled: true,
            asset_dir: String::new(),
            source_inbounds: Vec::new(),
            config: json!({"inbounds":[{"port":12345,"protocol":"shadowsocks","settings":{"password":"must-stay-private"}}],
                "outbounds":[{"protocol":"freedom"}]}),
        };
        let mut doc = Document {
            nodes: vec![node],
            legacy_configs: vec![profile.clone()],
            ..Default::default()
        };
        let configs = compile(&doc).unwrap();
        assert!(changed(&doc, &configs).contains(&profile.node_id));
        let summary = snapshot(&doc);
        assert!(!summary.to_string().contains("must-stay-private"));
        assert_eq!(summary["legacy_configs"][0]["published"], false);
        doc.applied_legacy_configs = vec![profile];
        assert_eq!(snapshot(&doc)["legacy_configs"][0]["published"], false);
        doc.nodes[0].desired = Some(Desired {
            revision: "applied".into(),
            config: configs[&doc.nodes[0].id].clone(),
        });
        doc.nodes[0].report.revision = "applied".into();
        doc.nodes[0].report.applied = true;
        doc.nodes[0].report.running = true;
        assert_eq!(snapshot(&doc)["legacy_configs"][0]["published"], true);
        assert_eq!(publication_preview(&doc).unwrap()["affected_legacy_configs"], json!([]));
        doc.legacy_configs[0].config["inbounds"][0]["port"] = json!(12346);
        assert_eq!(publication_preview(&doc).unwrap()["affected_legacy_configs"], json!(["imported"]));
        doc.legacy_configs.clear();
        assert!(changed(&doc, &compile(&doc).unwrap()).contains(&doc.nodes[0].id));
        let summary = snapshot(&doc);
        assert_eq!(summary["legacy_configs"][0]["pending_delete"], true);
        assert_eq!(summary["legacy_configs"][0]["published"], false);
        assert!(!summary.to_string().contains("must-stay-private"));
        assert_eq!(publication_preview(&doc).unwrap()["affected_legacy_configs"], json!(["imported"]));
        doc.applied_legacy_configs.clear();
        assert_eq!(snapshot(&doc)["legacy_configs"], json!([]));
    }

    #[tokio::test]
    async fn public_and_admin_entrypoints_serve_the_app() {
        for path in ["/", "/admin", "/admin/", "/query", "/query/"] {
            let response = web_asset(path.parse().unwrap()).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers()["content-type"],
                "text/html; charset=utf-8"
            );
        }
        assert_eq!(
            web_asset("/api/missing".parse().unwrap()).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}

fn main() -> Result<()> {
    #[cfg(target_os = "macos")]
    chain_panel::runtime::watch_parent()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    let listen = std::env::var("CHAIN_LISTEN").unwrap_or_else(|_| "127.0.0.1:8088".into());
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("无法监听 {listen}；不会停止任何现有服务"))?;
    let mut local_address = listener.local_addr()?;
    if local_address.ip().is_unspecified() {
        local_address.set_ip(if local_address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    let dir = PathBuf::from(std::env::var("CHAIN_DATA_DIR").unwrap_or_else(|_| "data".into()));
    private_dir(&dir, "chain-panel")?;
    let _lock = lock_dir(&dir)?;
    chain_panel::diagnostics::append(&dir.join("diagnostics.log"), "面板启动")?;
    let token_file = dir.join("admin-token");
    if !token_file.exists() {
        atomic_write(&token_file, secret().as_bytes())?;
    }
    let token = std::fs::read_to_string(&token_file)?;
    ensure!(token.trim().len() >= 32, "管理令牌至少需要 32 个字符");
    let conn = Connection::open(dir.join("panel.db"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; CREATE TABLE IF NOT EXISTS state(id INTEGER PRIMARY KEY,body TEXT NOT NULL);")?;
    install_state_write_guard(&conn)?;
    let mut doc: Document = match conn.query_row("SELECT body FROM state WHERE id=1", [], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(v) => serde_json::from_str(&v)?,
        Err(rusqlite::Error::QueryReturnedNoRows) => Document::default(),
        Err(e) => return Err(e.into()),
    };
    chain_panel::rules::seed_presets(&mut doc.rule_sources);
    let app = App {
        diagnostics: Arc::new(diagnostics_api::Broker::default()),
        store: Arc::new(Mutex::new(Store { conn, doc })),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        tickets: Arc::new(Mutex::new(HashMap::new())),
        delegated_sessions: Arc::new(Mutex::new(BTreeSet::new())),
        admin_hash: Arc::new(Mutex::new(digest(token.trim()))),
        token_file: token_file.clone(),
        auth: Arc::new(chain_panel::security::AuthState::default()),
        secure_cookie: std::env::var("CHAIN_SECURE_COOKIE").as_deref() == Ok("1"),
        xray: std::env::var("CHAIN_XRAY_BIN")
            .unwrap_or_else(|_| "xray".into())
            .into(),
        probe_lock: Arc::new(tokio::sync::Mutex::new(())),
        apply_timeout: std::env::var("CHAIN_APPLY_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(240),
        local_agent: Arc::new(tokio::sync::Mutex::new(
            chain_panel::local_agent::LocalAgent::default(),
        )),
        local_url: format!("http://{local_address}"),
    };
    let interrupted: Vec<Job> = app
        .store
        .lock()
        .unwrap()
        .doc
        .jobs
        .iter()
        .filter(|j| matches!(j.state.as_str(), "publishing" | "rolling_back"))
        .cloned()
        .collect();
    for job in interrupted {
        let a = app.clone();
        tokio::spawn(async move {
            run_publish(a, job, true).await;
        });
    }
    ensure!(
        WEB.get_file("index.html").is_some(),
        "缺少前端构建：请先在 frontend/ 执行 npm ci && npm run build，再重新编译 chain-panel"
    );
    let router = plans_api::routes(sharing_api::routes(diagnostics_api::routes(monitor_api::routes(fleet_api::routes(Router::new())))))
        .fallback(get(web_asset))
        .route("/api/login", post(login))
        .route("/api/auth/options", get(security_api::options))
        .route("/api/auth/passkey/start", post(security_api::login_start))
        .route("/api/auth/passkey/finish", post(security_api::login_finish))
        .route("/api/security/token", post(security_api::rotate))
        .route("/api/security/totp/start", post(security_api::totp_start))
        .route("/api/security/totp/finish", post(security_api::totp_finish))
        .route(
            "/api/security/totp/disable",
            post(security_api::totp_disable),
        )
        .route(
            "/api/security/passkey/start",
            post(security_api::register_start),
        )
        .route(
            "/api/security/passkey/finish",
            post(security_api::register_finish),
        )
        .route(
            "/api/security/passkey/remove",
            post(security_api::remove_passkey),
        )
        .route("/api/logout", post(logout))
        .route("/api/state", get(state_view))
        .route("/api/security/members", get(security_api::members).post(security_api::create_member))
        .route("/api/security/members/{id}/revoke", post(security_api::revoke_member))
        .route("/api/security/audit", get(security_api::audit))
        .route("/api/customers", post(rental_api::save_customer))
        .route("/api/customers/{id}", delete(rental_api::delete_customer))
        .route("/api/rentals/renew", post(rental_api::renew))
        .route("/api/rentals/context", get(rental_api::context))
        .route("/api/rentals/orders", post(rental_api::create_order))
        .route("/api/rentals/orders/by-request/{id}", get(rental_api::order_by_request))
        .route("/api/rentals/orders/{id}/confirm", post(rental_api::confirm_order))
        .route("/api/rentals/orders/{id}/cancel", post(rental_api::cancel_order))
        .route("/api/rentals/adjustments", post(rental_api::adjust))
        .route("/api/rentals/adjustments/by-request/{id}", get(rental_api::adjustment_by_request))
        .route("/api/rentals/contact", post(rental_api::save_contact))
        .route("/api/rentals/{id}", post(rental_api::save_rental))
        .route(
            "/api/rentals/{id}/query-link",
            post(rental_public::query_link),
        )
        .route("/api/public/lookup", post(rental_public::lookup))
        .route("/api/local-node", post(local_node))
        .route("/api/nodes", post(add_node))
        .route("/api/nodes/{id}", post(update_node).delete(remove_node))
        .route("/api/nodes/{id}/token", post(rotate_node))
        .route("/api/externals", post(add_external))
        .route("/api/externals/batch", post(batch_external))
        .route("/api/externals/{id}", delete(remove_external).post(update_external))
        .route(
            "/api/externals/{id}/export",
            get(delivery_api::external_export),
        )
        .route("/api/lines", post(save_line))
        .route("/api/legacy-import", post(import_legacy).layer(DefaultBodyLimit::max(16 * 1024 * 1024)))
        .route(
            "/api/legacy-configs/{id}",
            get(get_legacy_config)
                .put(save_legacy_config)
                .delete(delete_legacy_config)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/api/legacy-import/{id}", delete(remove_pending_import))
        .route("/api/lines/batch", post(save_lines))
        .route("/api/lines/operate", post(rental_api::operate))
        .route("/api/reality/check", post(check_reality))
        .route("/api/lines/{id}", delete(remove_line))
        .route("/api/lines/{id}/probe", post(diagnose))
        .route("/api/lines/{id}/export", get(delivery_api::export))
        .route("/api/subscriptions", post(delivery_api::subscribe))
        .route("/api/subscriptions/{id}", delete(delivery_api::revoke))
        .route("/subscription/{token}", get(delivery_api::subscription))
        .route("/api/preview", post(preview))
        .route("/api/publish", post(publish))
        .route("/api/agent/{id}/sync", post(sync))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app.clone());
    let local_app = app.clone();
    let monitor_stop = tokio_util::sync::CancellationToken::new();
    let rules_task = tokio::spawn(sharing_api::run(app.clone(), monitor_stop.clone()));
    let monitor_task = tokio::spawn(monitor_api::run(app.clone(), monitor_stop.clone()));
    let local_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let _ = chain_panel::diagnostics::maintain(local_app.token_file.parent().unwrap());
            if let Err(error) = local_app
                .local_agent
                .lock()
                .await
                .ensure(
                    local_app.token_file.parent().unwrap(),
                    &local_app.local_url,
                    &local_app.xray,
                )
                .await
            {
                eprintln!("本机 Agent: {error:#}");
                let _ = chain_panel::diagnostics::append(&local_app.token_file.parent().unwrap().join("diagnostics.log"), &format!("本机 Agent: {error:#}"));
            }
        }
    });
    eprintln!(
        "Chain Panel: http://{listen}；首次登录令牌位于 {}",
        token_file.display()
    );
    let result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let mut sig =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=sig.recv()=>{}}
    })
    .await;
    local_task.abort();
    let _ = local_task.await;
    monitor_stop.cancel();
    let _ = monitor_task.await;
    let _ = rules_task.await;
    app.local_agent.lock().await.stop().await;
    result?;
    Ok(())
}
