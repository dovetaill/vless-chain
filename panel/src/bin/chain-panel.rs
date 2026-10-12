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
    admin_hash: String,
    secure_cookie: bool,
    xray: PathBuf,
    probe_lock: Arc<tokio::sync::Mutex<()>>,
    apply_timeout: u64,
}
fn digest(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
fn equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
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
        if path != "/api/login" {
            let token = cookie(request.headers());
            let hash = digest(&token);
            let expiry = app
                .sessions
                .lock()
                .unwrap()
                .get(&hash)
                .copied()
                .unwrap_or(0);
            if expiry <= now() {
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
}
async fn login(State(app): State<App>, Json(input): Json<Login>) -> ApiResult<Response> {
    if !equal(&digest(&input.token), &app.admin_hash) {
        tokio::time::sleep(Duration::from_millis(400)).await;
        return Err(error(StatusCode::UNAUTHORIZED, "管理令牌不正确"));
    }
    let token = secret();
    {
        let mut sessions = app.sessions.lock().unwrap();
        sessions.retain(|_, t| *t > now());
        if sessions.len() > 100 {
            sessions.clear();
        }
        sessions.insert(digest(&token), now() + 28800);
    }
    let mut response = Json(json!({"ok":true})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&format!(
            "chain_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=28800{}",
            if app.secure_cookie { "; Secure" } else { "" }
        ))
        .unwrap(),
    );
    Ok(response)
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
    json!({"nodes":nodes,"externals":doc.externals,"lines":lines,"jobs":jobs,"checks":doc.checks,"busy":doc.busy(),"time":now()})
}
async fn state(State(app): State<App>) -> Json<Value> {
    Json(snapshot(&app.store.lock().unwrap().doc))
}
#[derive(Deserialize)]
struct NodeInput {
    name: String,
    host: String,
    target: String,
    server_name: String,
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
struct ExternalInput {
    name: String,
    url: String,
}
async fn add_external(
    State(app): State<App>,
    Json(input): Json<ExternalInput>,
) -> ApiResult<Json<Value>> {
    external_outbound(&input.url, "check")?;
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
                .any(|l| l.external_id.as_ref() == Some(&id)),
            "外部节点仍被草稿或已发布线路引用"
        );
        d.externals.retain(|e| e.id != id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
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
    id: Option<String>,
    name: String,
    kind: Kind,
    hops: Vec<HopInput>,
    #[serde(default)]
    external_id: Option<String>,
    #[serde(default)]
    target: String,
    #[serde(default = "tcp")]
    network: String,
    #[serde(default = "yes")]
    enabled: bool,
}
fn tcp() -> String {
    "tcp".into()
}
fn yes() -> bool {
    true
}
async fn save_line(State(app): State<App>, Json(input): Json<LineInput>) -> ApiResult<Json<Value>> {
    let line = edit(&app, |d| {
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
            let old = existing
                .as_ref()
                .and_then(|l| l.hops.iter().find(|o| o.node_id == h.node_id));
            let port = h
                .port
                .or(old.map(|h| h.port))
                .or_else(|| (20000..50000).find(|p| !used.contains(&(h.node_id.clone(), *p))))
                .context("没有可分配的端口")?;
            used.insert((h.node_id.clone(), port));
            hops.push(Hop {
                node_id: h.node_id,
                port,
                uuid: old.map(|h| h.uuid.clone()).unwrap_or_else(id),
            });
        }
        let line = Line {
            id: line_id.clone(),
            name: input.name,
            kind: input.kind,
            hops,
            external_id: input.external_id.filter(|s| !s.is_empty()),
            target: input.target,
            network: input.network,
            enabled: input.enabled,
        };
        d.lines.retain(|l| l.id != line_id);
        d.lines.push(line.clone());
        compile(d)?;
        d.checks.remove(&line_id);
        Ok(line)
    })?;
    Ok(Json(json!(line)))
}
async fn remove_line(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    edit(&app, |d| {
        ensure!(d.lines.iter().any(|l| l.id == id), "线路不存在");
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
            (n.desired.is_some() || !config["inbounds"].as_array().unwrap().is_empty())
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
    let nodes:Vec<Value>=doc.nodes.iter().filter(|n|affected.contains(&n.id)).map(|n|json!({"id":n.id,"name":n.name,"online":now().saturating_sub(n.seen)<15,
        "ports":configs[&n.id]["inbounds"].as_array().unwrap().iter().map(|i|i["port"].clone()).collect::<Vec<_>>()})).collect();
    let lines: BTreeSet<String> = doc
        .lines
        .iter()
        .chain(doc.applied_lines.iter())
        .filter(|l| l.hops.iter().any(|h| affected.contains(&h.node_id)))
        .map(|l| l.name.clone())
        .collect();
    Ok(Json(
        json!({"nodes":nodes,"affected_lines":lines,"message":"发布会重启受影响节点上由本平台管理的 Xray；共享该实例的线路会短暂重连。现有脚本的服务不在管理范围内。"}),
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
    let result = match tokio::time::timeout(
        Duration::from_secs(40),
        probe(&app.xray, line, &doc, &request),
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
    let path = if path.is_empty() { "index.html" } else { path };
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

#[tokio::main]
async fn main() -> Result<()> {
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
        admin_hash: digest(token.trim()),
        secure_cookie: std::env::var("CHAIN_SECURE_COOKIE").as_deref() == Ok("1"),
        xray: std::env::var("CHAIN_XRAY_BIN")
            .unwrap_or_else(|_| "xray".into())
            .into(),
        probe_lock: Arc::new(tokio::sync::Mutex::new(())),
        apply_timeout: std::env::var("CHAIN_APPLY_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(35),
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
    let router = Router::new()
        .fallback(get(web_asset))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/state", get(state))
        .route("/api/nodes", post(add_node))
        .route("/api/nodes/{id}", post(update_node).delete(remove_node))
        .route("/api/nodes/{id}/token", post(rotate_node))
        .route("/api/externals", post(add_external))
        .route("/api/externals/{id}", delete(remove_external))
        .route("/api/lines", post(save_line))
        .route("/api/lines/{id}", delete(remove_line))
        .route("/api/lines/{id}/probe", post(diagnose))
        .route("/api/preview", post(preview))
        .route("/api/publish", post(publish))
        .route("/api/agent/{id}/sync", post(sync))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app);
    let listen = std::env::var("CHAIN_LISTEN").unwrap_or_else(|_| "127.0.0.1:8088".into());
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("无法监听 {listen}；不会停止任何现有服务"))?;
    eprintln!(
        "Chain Panel: http://{listen}；首次登录令牌位于 {}",
        token_file.display()
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let mut sig =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=sig.recv()=>{}}
        })
        .await?;
    Ok(())
}
