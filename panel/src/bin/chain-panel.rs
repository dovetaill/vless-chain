use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use chain_panel::{compiler::*, model::*, runtime::*};
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
#[path = "../rental_public.rs"]
mod rental_public;
#[path = "../security_api.rs"]
mod security_api;
#[path = "../fleet_api.rs"]
mod fleet_api;
#[path = "../monitor_api.rs"]
mod monitor_api;

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
#[derive(Clone)]
struct App {
    store: Arc<Mutex<Store>>,
    sessions: Arc<Mutex<HashMap<String, u64>>>,
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

async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let agent = path.starts_with("/api/agent/");
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
            path,
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
                    && chain_panel::management::machine_route(request.method().as_str(), path)
            });
            let token = cookie(request.headers());
            let hash = digest(&token);
            let expiry = app
                .sessions
                .lock()
                .unwrap()
                .get(&hash)
                .copied()
                .unwrap_or(0);
            if expiry <= now() && !machine_ok {
                return error(StatusCode::UNAUTHORIZED, "请先登录").into_response();
            }
        }
    }
    let mut response = next.run(request).await;
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
    if !equal(&digest(&input.token), &app.admin_hash.lock().unwrap()) {
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
    let token = secret();
    {
        let mut sessions = app.sessions.lock().unwrap();
        sessions.retain(|_, t| *t > now());
        if sessions.len() > 100 {
            sessions.clear();
            app.delegated_sessions.lock().unwrap().clear();
        }
        sessions.insert(digest(&token), now() + 28800);
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
        .map(|j| json!({"id":j.id,"state":j.state,"started":j.started,"events":j.events}))
        .collect();
    let subscriptions: Vec<_> = doc
        .subscriptions
        .iter()
        .map(|s| json!({"id":s.id,"name":s.name,"line_ids":s.line_ids,"created":s.created}))
        .collect();
    json!({"nodes":nodes,"externals":doc.externals,"lines":lines,"pending_imports":doc.pending_imports,"jobs":jobs,"checks":doc.checks,"busy":doc.busy(),"time":now(),"subscriptions":subscriptions,"security":security_api::summary(&doc.security),"alerts":delivery_api::alerts(doc),"customers":doc.customers,"payments":doc.payments,"renewal_contact":doc.renewal_contact})
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
    Json(value)
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
    Json(input): Json<chain_panel::legacy::Import>,
) -> ApiResult<Json<Value>> {
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
                || config["_chain"].as_array().is_some_and(|v| !v.is_empty()))
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
    Ok(Json(
        json!({"nodes":nodes,"affected_lines":lines,"affected_line_ids":affected_ids,"message":if restarting.is_empty(){"流量和限速设置会在线更新，无需重启线路。"}else{"应用会重启受影响服务器上由本平台管理的实例；共享该实例的线路会短暂重连。"}}),
    ))
}
async fn publish(State(app): State<App>) -> ApiResult<Json<Value>> {
    let _probe_guard = app
        .probe_lock
        .try_lock()
        .map_err(|_| error(StatusCode::CONFLICT, "正在执行整链探测，请完成后发布"))?;
    let job = edit(&app, |d| {
        let configs = compile(d)?;
        let affected = changed(d, &configs);
        for node in d.nodes.iter().filter(|n| affected.contains(&n.id)) {
            if configs[&node.id]["_chain"]
                .as_array()
                .is_some_and(|s| !s.is_empty())
            {
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
    let token_file = dir.join("admin-token");
    if !token_file.exists() {
        atomic_write(&token_file, secret().as_bytes())?;
    }
    let token = std::fs::read_to_string(&token_file)?;
    ensure!(token.trim().len() >= 32, "管理令牌至少需要 32 个字符");
    let conn = Connection::open(dir.join("panel.db"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; CREATE TABLE IF NOT EXISTS state(id INTEGER PRIMARY KEY,body TEXT NOT NULL);")?;
    let doc = match conn.query_row("SELECT body FROM state WHERE id=1", [], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(v) => serde_json::from_str(&v)?,
        Err(rusqlite::Error::QueryReturnedNoRows) => Document::default(),
        Err(e) => return Err(e.into()),
    };
    let app = App {
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
    let router = monitor_api::routes(fleet_api::routes(Router::new()))
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
        .route("/api/state", get(state))
        .route("/api/customers", post(rental_api::save_customer))
        .route("/api/customers/{id}", delete(rental_api::delete_customer))
        .route("/api/rentals/renew", post(rental_api::renew))
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
        .route("/api/externals/{id}", delete(remove_external).post(update_external))
        .route(
            "/api/externals/{id}/export",
            get(delivery_api::external_export),
        )
        .route("/api/lines", post(save_line))
        .route("/api/legacy-import", post(import_legacy))
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
    let monitor_task = tokio::spawn(monitor_api::run(app.clone(), monitor_stop.clone()));
    let local_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
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
    app.local_agent.lock().await.stop().await;
    result?;
    Ok(())
}
