use super::*;
use chain_panel::management::{self, Policy, Remote};
use futures_util::StreamExt;

pub fn routes(router: Router<App>) -> Router<App> {
    router
        .route("/api/fleet", get(view))
        .route("/api/fleet/invite", post(invite))
        .route("/api/fleet/connect", post(connect))
        .route("/api/fleet/policy", post(set_policy))
        .route("/api/fleet/unbind", post(revoke))
        .route("/api/fleet/batch", post(batch))
        .route("/api/fleet/batch/{id}/resume", post(resume_batch))
        .route("/api/fleet/batch/{id}/archive", post(archive_batch))
        .route("/api/fleet/release/{version}", get(release_info))
        .route("/api/fleet/{id}", delete(disconnect))
        .route("/api/fleet/{id}/forward", post(forward))
        .route("/api/fleet/{id}/login", post(remote_login))
        .route("/api/federation/pair", post(pair))
        .route("/api/federation/state", get(remote_state))
        .route("/api/federation/document", get(probe_document))
        .route("/api/federation/policy", post(receive_policy))
        .route("/api/federation/revoke", post(revoke))
        .route("/api/federation/ticket", post(ticket))
        .route("/api/federation/login", post(exchange))
        .route("/api/federation/task", get(task_status).post(task_submit))
}
pub async fn view(State(app): State<App>) -> Json<Value> {
    let s = app.store.lock().unwrap();
    let f = &s.doc.fleet;
    let remotes: Vec<_> = f
        .remotes
        .iter()
        .map(|r| {
            json!({"id":r.id,"name":r.name,"url":r.url,
        "seen":r.seen,"error":r.error,"policy_error":r.policy_error,"snapshot":r.snapshot})
        })
        .collect();
    Json(
        json!({"server_now":now(),"remotes":remotes,"controller":f.controller_name,"policy":f.policy,"tasks":f.receipts}),
    )
}
async fn invite(State(app): State<App>) -> ApiResult<Json<Value>> {
    let code = secret();
    edit(&app, |d| {
        ensure!(d.fleet.controller_hash.is_empty(), "请先解除已有总控绑定");
        ensure!(
            d.fleet.remotes.is_empty(),
            "总控不能再作为下级面板接入；请先解除它管理的 VPS"
        );
        if d.fleet.instance_id.is_empty() {
            d.fleet.instance_id = id();
        }
        d.fleet.invitation_hash = digest(&code);
        d.fleet.invitation_expires = now() + 600;
        Ok(())
    })?;
    Ok(Json(json!({"code":code,"expires":now()+600})))
}
#[derive(Deserialize)]
struct Pair {
    code: String,
    name: String,
    controller_id: String,
}
async fn pair(State(app): State<App>, Json(input): Json<Pair>) -> ApiResult<Json<Value>> {
    app.auth.rate_limit()?;
    let token = secret();
    edit(&app, |d| {
        ensure!(
            uuid::Uuid::parse_str(&input.controller_id).is_ok()
                && input.controller_id != d.fleet.instance_id,
            "不能接入自己"
        );
        ensure!(d.fleet.remotes.is_empty(), "不支持多级总控");
        ensure!(
            d.fleet.controller_hash.is_empty()
                && !d.fleet.invitation_hash.is_empty()
                && d.fleet.invitation_expires > now()
                && equal(&digest(&input.code), &d.fleet.invitation_hash),
            "接入码无效或已使用"
        );
        ensure!(
            !input.name.trim().is_empty() && input.name.len() <= 120,
            "总控名称无效"
        );
        d.fleet.invitation_hash.clear();
        d.fleet.invitation_expires = 0;
        d.fleet.controller_hash = digest(&token);
        d.fleet.controller_name = input.name;
        Ok(())
    })?;
    Ok(Json(
        json!({"token":token,"version":env!("CARGO_PKG_VERSION")}),
    ))
}
#[derive(Deserialize)]
struct Connect {
    name: String,
    url: String,
    code: String,
}
async fn connect(State(app): State<App>, Json(input): Json<Connect>) -> ApiResult<Json<Value>> {
    let url = management::panel_url(&input.url)?;
    let controller_id = edit(&app, |d| {
        ensure!(
            d.fleet.controller_hash.is_empty(),
            "已授权给其他总控的面板不能继续接入下级面板"
        );
        if d.fleet.instance_id.is_empty() {
            d.fleet.instance_id = id();
        }
        Ok(d.fleet.instance_id.clone())
    })?;
    {
        let s = app.store.lock().unwrap();
        if s.doc.fleet.remotes.iter().any(|r| r.url == url) {
            return Err(error(StatusCode::CONFLICT, "该面板已经接入"));
        }
    }
    if input.name.trim().is_empty() || input.name.len() > 120 {
        return Err(error(StatusCode::BAD_REQUEST, "名称无效"));
    }
    let mut remote = Remote {
        id: id(),
        name: input.name,
        url,
        token: String::new(),
        seen: now(),
        error: String::new(),
        policy_error: String::new(),
        snapshot: Value::Null,
    };
    let result = request(
        &remote,
        "POST",
        "/api/federation/pair",
        Some(json!({"code":input.code,"name":"Chain 总控","controller_id":controller_id})),
    )
    .await?;
    remote.token = result["token"]
        .as_str()
        .context("远端未返回接入令牌")?
        .into();
    let saved = edit(&app, |d| {
        ensure!(
            d.fleet.controller_hash.is_empty()
                && !d.fleet.remotes.iter().any(|r| r.url == remote.url),
            "接入期间授权状态发生变化，请重试"
        );
        d.fleet.invitation_hash.clear();
        d.fleet.remotes.push(remote.clone());
        Ok(())
    });
    if let Err(e) = saved {
        let _ = request(&remote, "POST", "/api/federation/revoke", Some(json!({}))).await;
        return Err(e);
    }
    Ok(Json(json!({"id":remote.id})))
}
pub fn remote(app: &App, id: &str) -> Result<Remote> {
    app.store
        .lock()
        .unwrap()
        .doc
        .fleet
        .remotes
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .context("远端面板不存在")
}
pub async fn request(
    remote: &Remote,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(if path.contains("/diagnostics/") || path == "/api/publish" {
            600
        } else if path.starts_with("/api/rules/") && path.ends_with("/update") {
            30
        } else if path.ends_with("/probe") {
            45
        } else {
            15
        }))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut request = client
        .request(method.parse()?, format!("{}{path}", remote.url))
        .header("x-chain-request", "1")
        .bearer_auth(&remote.token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("远端连接失败或超时；操作结果需重新查询"))?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "远端响应超过 8 MiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&bytes).context("远端响应不是 JSON；请检查版本和面板地址")?;
    ensure!(
        status.is_success(),
        "远端 HTTP {}：{}",
        status.as_u16(),
        value["error"].as_str().unwrap_or("请求失败")
    );
    Ok(value)
}
async fn disconnect(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let r = remote(&app, &id)?;
    request(&r, "POST", "/api/federation/revoke", Some(json!({}))).await?;
    edit(&app, |d| {
        d.fleet.remotes.retain(|r| r.id != id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn revoke(State(app): State<App>) -> ApiResult<Json<Value>> {
    edit(&app, |d| {
        d.fleet.controller_hash.clear();
        d.fleet.controller_name.clear();
        d.fleet.invitation_hash.clear();
        d.monitoring.override_policy = None;
        // Old delegated notices must not be replayed on local channels after unbinding.
        for notice in &mut d.monitoring.notices {
            notice.telegram_done = true;
            notice.mail_done = true;
        }
        Ok(())
    })?;
    app.tickets.lock().unwrap().clear();
    let delegated = std::mem::take(&mut *app.delegated_sessions.lock().unwrap());
    app.sessions
        .lock()
        .unwrap()
        .retain(|hash, _| !delegated.contains(hash));
    Ok(Json(json!({"ok":true})))
}
async fn set_policy(
    State(app): State<App>,
    Json(policy): Json<Option<Policy>>,
) -> ApiResult<Json<Value>> {
    if let Some(p) = &policy {
        p.validate()?;
    }
    edit(&app, |d| {
        d.fleet.policy = policy;
        Ok(())
    })?;
    Ok(Json(
        json!({"ok":true,"detail":"远端下次同步时应用；以远端显示的策略为准"}),
    ))
}
async fn receive_policy(
    State(app): State<App>,
    Json(policy): Json<Option<Policy>>,
) -> ApiResult<Json<Value>> {
    if let Some(p) = &policy {
        p.validate()?;
    }
    edit(&app, |d| {
        d.monitoring.override_policy = policy;
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn remote_state(State(app): State<App>) -> Json<Value> {
    let Json(mut value) = state(State(app.clone())).await;
    for key in ["customers", "payments", "subscriptions", "pending_imports"] {
        value[key] = json!([]);
    }
    value["renewal_contact"] = json!("");
    let s = app.store.lock().unwrap();
    value["monitoring"] = json!({"effective":s.doc.monitoring.policy(),"override":s.doc.monitoring.override_policy,
        "checks":s.doc.monitoring.checks,"conditions":s.doc.monitoring.conditions,
        "notices":s.doc.monitoring.notices.iter().rev().take(100).collect::<Vec<_>>()});
    Json(value)
}
fn version_valid(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|s| !s.is_empty() && s.len() <= 8 && s.bytes().all(|c| c.is_ascii_digit()))
}
fn release_version(release: &Value) -> Result<&str> {
    ensure!(release["draft"] == false && release["prerelease"] == false, "请选择正式发行版本");
    let version = release["tag_name"].as_str().and_then(|tag| tag.strip_prefix('v'))
        .filter(|version| version_valid(version)).context("正式发行版本标签无效")?;
    Ok(version)
}
async fn release_info(Path(version): Path<String>) -> ApiResult<Json<Value>> {
    if version != "latest" && !version_valid(&version) {
        return Err(error(StatusCode::BAD_REQUEST, "正式版本无效"));
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("chain-panel")
        .build()
        .map_err(anyhow::Error::from)?;
    let endpoint = if version == "latest" { "latest".to_string() } else { format!("tags/v{version}") };
    let release: Value = client
        .get(format!(
            "https://api.github.com/repos/dovetaill/vless-chain/releases/{endpoint}"
        ))
        .send()
        .await
        .map_err(anyhow::Error::from)?
        .error_for_status()
        .map_err(anyhow::Error::from)?
        .json()
        .await
        .map_err(anyhow::Error::from)?;
    let resolved = release_version(&release)?;
    if version != "latest" && version != resolved {
        return Err(error(StatusCode::BAD_REQUEST, "发行版本不匹配"));
    }
    let version = resolved;
    let source = client.get(format!("https://raw.githubusercontent.com/dovetaill/vless-chain/v{version}/scripts/chain_release.py"))
        .send().await.map_err(anyhow::Error::from)?.error_for_status().map_err(anyhow::Error::from)?
        .text().await.map_err(anyhow::Error::from)?;
    let constant = |key: &str| {
        source
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key} = ")))
            .map(|v| v.trim_matches(['\'', '"']).to_string())
            .unwrap_or_default()
    };
    Ok(Json(
        json!({"version":version,"xray":constant("XRAY_VERSION"),"mihomo":constant("MIHOMO_VERSION")}),
    ))
}
async fn probe_document(State(app): State<App>) -> Json<Value> {
    let s = app.store.lock().unwrap();
    let d = &s.doc;
    let mut nodes = d.applied_nodes.clone();
    for n in &mut nodes {
        n.token_hash.clear();
        n.reality.private_key.clear();
        n.desired = None;
    }
    let lines: Vec<_> = d
        .applied_lines
        .iter()
        .filter(|l| {
            matches!(
                chain_panel::rental::status(d, l, now()),
                "active" | "offline"
            )
        })
        .cloned()
        .collect();
    Json(json!(Document {
        nodes,
        lines,
        externals: d.applied_externals.clone(),
        ..Default::default()
    }))
}
#[derive(Deserialize)]
struct Forward {
    path: String,
    method: String,
    body: Option<Value>,
}
async fn forward(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Forward>,
) -> ApiResult<Json<Value>> {
    let path = if input.path == "/state" {
        "/api/federation/state".to_string()
    } else {
        format!("/api{}", input.path)
    };
    if !management::machine_route(&input.method, &path) {
        return Err(error(StatusCode::FORBIDDEN, "总控不支持此操作"));
    }
    Ok(Json(
        request(&remote(&app, &id)?, &input.method, &path, input.body).await?,
    ))
}
async fn ticket(State(app): State<App>) -> ApiResult<Json<Value>> {
    let controller = app.store.lock().unwrap().doc.fleet.controller_hash.clone();
    if controller.is_empty() {
        return Err(error(StatusCode::FORBIDDEN, "面板未绑定总控"));
    }
    let value = secret();
    let mut tickets = app.tickets.lock().unwrap();
    tickets.retain(|_, (expires, _)| *expires > now());
    if tickets.len() >= 100 {
        return Err(error(StatusCode::TOO_MANY_REQUESTS, "登录票据过多"));
    }
    tickets.insert(digest(&value), (now() + 60, controller));
    Ok(Json(json!({"ticket":value})))
}
#[derive(Deserialize)]
struct Ticket {
    ticket: String,
}
async fn exchange(State(app): State<App>, Json(input): Json<Ticket>) -> ApiResult<Response> {
    app.auth.rate_limit()?;
    let store = app.store.lock().unwrap();
    let controller = &store.doc.fleet.controller_hash;
    let valid = app
        .tickets
        .lock()
        .unwrap()
        .remove(&digest(&input.ticket))
        .is_some_and(|(expiry, owner)| {
            expiry > now() && !controller.is_empty() && equal(&owner, controller)
        });
    if !valid {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "登录链接已失效，请从总控重新跳转",
        ));
    }
    Ok(session_response_as(&app, true))
}
async fn remote_login(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let r = remote(&app, &id)?;
    let result = request(&r, "POST", "/api/federation/ticket", Some(json!({}))).await?;
    let ticket = result["ticket"].as_str().context("远端未返回票据")?;
    let fragment = url::form_urlencoded::byte_serialize(ticket.as_bytes()).collect::<String>();
    Ok(Json(
        json!({"url":format!("{}/admin#fleet-ticket={fragment}",r.url)}),
    ))
}

pub async fn maintenance(value: Value) -> Result<Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let stream = tokio::net::UnixStream::connect("/run/chain-maintenance/control.sock")
        .await
        .context("尚未安装 systemd 远程维护组件，请先在该 VPS 手工更新")?;
    let (read, mut write) = stream.into_split();
    write
        .write_all(format!("{}\n", serde_json::to_string(&value)?).as_bytes())
        .await?;
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::io::BufReader::new(read).read_line(&mut line),
    )
    .await??;
    ensure!(line.len() <= 65536, "维护响应过大");
    let result: Value = serde_json::from_str(&line)?;
    ensure!(
        result["state"].is_string() || result["error"].is_null(),
        "{}",
        result["error"].as_str().unwrap_or("维护失败")
    );
    Ok(result)
}
async fn task_status(
    axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        maintenance(json!({"action":"status","id":query.get("id")})).await?,
    ))
}
async fn task_submit(Json(value): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(maintenance(value).await?))
}
#[derive(Deserialize)]
struct Batch {
    id: String,
    targets: Vec<String>,
    action: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    lines: BTreeMap<String, Vec<String>>,
}
async fn archive_batch(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    edit(&app, |d| {
        let job = d.fleet.receipts.get_mut(&id).context("任务不存在")?;
        ensure!(job["state"] != "running", "正在执行的任务不能忽略，请等待任务结束或暂停");
        job["archived"] = json!(true);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn resume_batch(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    edit(&app, |d| {
        ensure!(
            !d.fleet.receipts.values().any(|v| v["state"] == "running"),
            "已有批量任务在执行"
        );
        let job = d.fleet.receipts.get_mut(&id).context("任务不存在")?;
        ensure!(job["archived"] != true, "已忽略的任务不能重新查询，请创建新任务");
        ensure!(job["state"] == "paused" || job["state"] == "completed", "只可重新查询暂停或已结束的异常任务");
        for target in job["targets"].as_array_mut().context("任务无效")? {
            target.as_object_mut().unwrap().remove("unreachable_since");
            target["state"] = json!(if maintenance_succeeded(target) { "succeeded" } else { "pending" });
        }
        job["state"] = json!("running");
        finish_batch(job);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn batch(State(app): State<App>, Json(input): Json<Batch>) -> ApiResult<Json<Value>> {
    uuid::Uuid::parse_str(&input.id).map_err(|_| error(StatusCode::BAD_REQUEST, "任务 ID 无效"))?;
    if !matches!(
        input.action.as_str(),
        "upgrade" | "restart-all" | "restart-panel" | "restart-agent" | "enable" | "disable"
    ) {
        return Err(error(StatusCode::BAD_REQUEST, "不支持的批量操作"));
    }
    if input.action == "upgrade" && !version_valid(&input.version) {
        return Err(error(StatusCode::BAD_REQUEST, "请选择正式发行版本"));
    }
    edit(&app, |d| {
        if d.fleet.receipts.contains_key(&input.id) {
            return Ok(());
        }
        ensure!(
            !input.targets.is_empty() && input.targets.len() <= 200,
            "请选择 1～200 台 VPS"
        );
        ensure!(
            input.targets.iter().collect::<BTreeSet<_>>().len() == input.targets.len(),
            "VPS 重复"
        );
        ensure!(
            !d.fleet.receipts.values().any(|v| v["state"] == "running"),
            "已有批量任务在执行"
        );
        ensure!(
            input
                .targets
                .iter()
                .all(|id| d.fleet.remotes.iter().any(|r| &r.id == id)),
            "VPS 不存在"
        );
        if !matches!(input.action.as_str(), "enable" | "disable") {
            ensure!(
                input.targets.iter().all(|id| d
                    .fleet
                    .remotes
                    .iter()
                    .find(|r| &r.id == id)
                    .is_some_and(|r| r.snapshot["maintenance"] == true)),
                "所选 VPS 尚未确认支持 systemd 维护；请先手工更新并等待状态同步"
            );
            if input.action == "restart-all" {
                ensure!(input.targets.iter().all(|id| d.fleet.remotes.iter()
                    .find(|r| &r.id == id).is_some_and(|r| r.snapshot["maintenance_actions"]
                        .as_array().is_some_and(|actions| actions.iter().any(|a| a == "restart-all")))),
                    "所选 VPS 尚不支持整套重启；请先升级并等待状态同步");
            }
        }
        if matches!(input.action.as_str(), "enable" | "disable") {
            ensure!(
                input.targets.iter().all(|id| input
                    .lines
                    .get(id)
                    .is_some_and(|lines| !lines.is_empty()
                        && lines.len() <= 1000
                        && lines.iter().all(|line| uuid::Uuid::parse_str(line).is_ok()))),
                "请选择有效的代理"
            );
        }
        let targets: Vec<_> = input
            .targets
            .iter()
            .map(|id| json!({"id":id,"task_id":crate::id(),"state":"pending"}))
            .collect();
        d.fleet.receipts.insert(
            input.id.clone(),
            json!({"id":input.id,"action":input.action,"version":input.version,
            "lines":input.lines,"targets":targets,"state":"running","at":now()}),
        );
        if d.fleet.receipts.len() > 100 {
            if let Some(old) = d
                .fleet
                .receipts
                .iter()
                .filter(|(_, v)| v["state"] != "running")
                .min_by_key(|(_, v)| v["at"].as_u64().unwrap_or(0))
                .map(|(k, _)| k.clone())
            {
                d.fleet.receipts.remove(&old);
            }
        }
        Ok(())
    })?;
    Ok(Json(json!({"id":input.id})))
}

fn maintenance_succeeded(target: &Value) -> bool {
    target["state"] == "succeeded" || target["result"]["maintenance_state"] == "succeeded"
        || target["error"].as_str().or(target["result"]["error"].as_str())
            .is_some_and(|e| e.starts_with("组件维护已完成，但整链验证失败"))
}

fn finish_batch(job: &mut Value) {
    let targets = job["targets"].as_array().unwrap();
    if targets.iter().any(|t| matches!(t["state"].as_str(), Some("pending" | "queued" | "running"))) {
        return;
    }
    job["state"] = json!(if targets.iter().all(maintenance_succeeded) { "succeeded" } else { "completed" });
}

pub async fn work_batch(app: &App) -> Result<()> {
    let pending = app
        .store
        .lock()
        .unwrap()
        .doc
        .fleet
        .receipts
        .values()
        .find(|v| v["state"] == "running")
        .cloned();
    let Some(job) = pending else {
        return Ok(());
    };
    let targets = job["targets"].as_array().unwrap();
    // Count remote operations still running, not just concurrent HTTP requests.
    let indices: Vec<_> = targets.iter().enumerate()
        .filter(|(_, t)| matches!(t["state"].as_str(), Some("running" | "queued")))
        .chain(targets.iter().enumerate().filter(|(_, t)| t["state"] == "pending"))
        .take(5).map(|(i, _)| i).collect();
    let mut work = futures_util::stream::iter(indices).map(|index| work_target(app, &job, index)).buffer_unordered(5);
    while let Some(result) = work.next().await { result?; }
    let mut s = app.store.lock().unwrap();
    if let Some(current) = s.doc.fleet.receipts.get_mut(job["id"].as_str().unwrap()) {
        finish_batch(current);
    }
    s.save()?;
    Ok(())
}

async fn work_target(app: &App, job: &Value, index: usize) -> Result<()> {
    let mut target = job["targets"][index].clone();
    let action = job["action"].as_str().unwrap().to_string();
    let lines = matches!(action.as_str(), "enable" | "disable");
    let dispatch_lines = lines && target["dispatched"] != true;
    target["state"] = json!("running");
    if dispatch_lines {
        target["dispatched"] = json!(true);
        target["started"] = json!(now());
    }
    {
        let mut s = app.store.lock().unwrap();
        s.doc
            .fleet
            .receipts
            .get_mut(job["id"].as_str().unwrap())
            .context("任务不存在")?["targets"][index] = target.clone();
        s.save()?;
    }
    let result = async {
        let remote = remote(app, target["id"].as_str().unwrap())?;
        if lines {
        if dispatch_lines {
            request(&remote, "POST", "/api/lines/operate", Some(json!({"action":action,"line_ids":job["lines"][&remote.id]}))).await
                .map(|value| json!({"state":"running","detail":"已提交，等待 Agent 确认","result":value}))
        } else {
            request(&remote, "GET", "/api/federation/state", None).await.map(|state| {
                let selected = job["lines"][&remote.id].as_array().cloned().unwrap_or_default();
                let lines = state["lines"].as_array().cloned().unwrap_or_default();
                let matched = !selected.is_empty() && selected.iter().all(|id| lines.iter().any(|l| &l["id"] == id && l["enabled"] == (action == "enable")));
                let synced = matched && selected.iter().all(|id| lines.iter().any(|l| &l["id"] == id && l["sync_status"] == "synced"));
                if synced { json!({"state":"succeeded","detail":"所选代理已同步至 Agent"}) }
                else if now().saturating_sub(target["started"].as_u64().unwrap_or(0)) >= 300 {
                    json!({"state":"unknown","detail":"未确认代理已同步；请核对远端状态，未重复执行操作"})
                } else { json!({"state":"running","detail":"正在查询实际同步状态；未重复下发"}) }
            })
        }
    } else {
        // The remote executor persists request IDs before starting, including across panel restarts.
        request(
            &remote,
            "POST",
            "/api/federation/task",
            Some(json!({"id":target["task_id"],"action":action,"version":job["version"]})),
        )
        .await
        }
    }.await;
    match result {
        Ok(mut result) => {
            target.as_object_mut().unwrap().remove("error");
            target.as_object_mut().unwrap().remove("unreachable_since");
            if !lines {
                result["maintenance_state"] = result["state"].clone();
                result["verification"] = json!({"state":"pending","detail":"组件维护完成后自动验证；验证失败不阻塞其他 VPS"});
            }
            target["result"] = result.clone();
            let state = result["state"].as_str().unwrap_or("running");
            target["state"] = json!(state);
        }
        Err(e) => {
            target["error"] = json!(format!("{e:#}"));
            let first = target["unreachable_since"].as_u64().unwrap_or(now());
            target["unreachable_since"] = json!(first);
            target["state"] = json!(if now().saturating_sub(first) >= 300 {
                "unknown"
            } else {
                "running"
            });
        }
    }
    let mut s = app.store.lock().unwrap();
    let current = s
        .doc
        .fleet
        .receipts
        .get_mut(job["id"].as_str().unwrap())
        .context("任务不存在")?;
    current["targets"][index] = target;
    finish_batch(current);
    s.save()?;
    Ok(())
}

pub async fn verify_batches(app: &App) -> Result<()> {
    let pending: Vec<_> = {
        let s = app.store.lock().unwrap();
        s.doc.fleet.receipts.iter().filter(|(_, job)| job["archived"] != true
            && !matches!(job["action"].as_str(), Some("enable" | "disable")))
            .flat_map(|(id, job)| job["targets"].as_array().into_iter().flatten().enumerate()
                .filter(|(_, t)| maintenance_succeeded(t) && t["result"]["verification"]["state"] != "succeeded"
                    && t["result"]["verification"]["retry_at"].as_u64().unwrap_or(0) <= now())
                .map(|(index, t)| (id.clone(), index, t["id"].as_str().unwrap().to_string())))
            .collect()
    };
    let mut checks = futures_util::stream::iter(pending).map(|(id, index, remote_id)| async move {
        if app.store.lock().unwrap().doc.fleet.receipts.get(&id).is_none_or(|job| job["archived"] == true) {
            return (id, index, None);
        }
        let result = async {
            let remote = remote(app, &remote_id)?;
            verify_remote(app, &remote).await
        }.await;
        (id, index, Some(result))
    }).buffer_unordered(5);
    while let Some((id, index, result)) = checks.next().await {
        let Some(result) = result else { continue; };
        let mut s = app.store.lock().unwrap();
        let interval = s.doc.monitoring.policy().interval;
        let Some(job) = s.doc.fleet.receipts.get_mut(&id).filter(|j| j["archived"] != true) else { continue; };
        let target = &mut job["targets"][index];
        if !maintenance_succeeded(target) { continue; }
        // Old paused receipts may encode successful maintenance as a failed target.
        target["state"] = json!("succeeded");
        target.as_object_mut().unwrap().remove("error");
        target["result"]["state"] = json!("succeeded");
        target["result"]["maintenance_state"] = json!("succeeded");
        target["result"].as_object_mut().unwrap().remove("error");
        target["result"]["verification"] = match result {
            Ok(detail) => json!({"state":"succeeded","detail":detail,"checked_at":now()}),
            Err(e) => json!({"state":"failed","detail":chain_panel::diagnostics::redact(&format!("{e:#}")),
                "checked_at":now(),"retry_at":now() + interval}),
        };
        finish_batch(job);
        s.save()?;
    }
    Ok(())
}

async fn verify_remote(app: &App, remote: &Remote) -> Result<String> {
    let state = request(remote, "GET", "/api/federation/state", None).await?;
    ensure!(
        state["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().all(|n| n["online"] == true
                && n["report"]["running"] == true
                && n["report"]["applied"] == true
                && n["report"]["error"] == "")),
        "存在未上线或未运行的 Agent"
    );
    let doc: Document =
        serde_json::from_value(request(remote, "GET", "/api/federation/document", None).await?)?;
    let policy: Policy = serde_json::from_value(state["monitoring"]["effective"].clone())?;
    for line in &doc.lines {
        let result = tokio::time::timeout(
            Duration::from_secs(40),
            probe(
                &app.xray,
                line,
                &doc,
                &ProbeRequest {
                    mode: "http".into(),
                    url: policy.url.clone(),
                },
                None,
            ),
        )
        .await;
        result.with_context(|| format!("{}：公网入口整链检测超时（40 秒）", line.name))?
            .with_context(|| format!("{}：公网入口整链检测未通过", line.name))?;
    }
    Ok(format!(
        "组件及 Agent 心跳通过；{} 条可用代理公网整链检测通过（不含 UDP）",
        doc.lines.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formal_release_tag_is_validated() {
        let mut release = json!({"tag_name":"v0.2.30","draft":false,"prerelease":false});
        assert_eq!(release_version(&release).unwrap(), "0.2.30");
        release["prerelease"] = json!(true);
        assert!(release_version(&release).is_err());
        release["prerelease"] = json!(false);
        release["draft"] = json!(true);
        assert!(release_version(&release).is_err());
        release["draft"] = json!(false);
        release["tag_name"] = json!("v../../main");
        assert!(release_version(&release).is_err());
    }
    fn app(dir: &std::path::Path) -> App {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY,body TEXT NOT NULL)")
            .unwrap();
        App {
            diagnostics: Arc::new(diagnostics_api::Broker::default()),
            store: Arc::new(Mutex::new(Store {
                conn,
                doc: Document::default(),
            })),
            sessions: Default::default(),
            tickets: Default::default(),
            delegated_sessions: Default::default(),
            admin_hash: Arc::new(Mutex::new(digest("test"))),
            token_file: dir.join("token"),
            auth: Arc::new(chain_panel::security::AuthState::default()),
            secure_cookie: true,
            xray: dir.join("not-executed"),
            probe_lock: Arc::new(tokio::sync::Mutex::new(())),
            apply_timeout: 10,
            local_agent: Arc::new(tokio::sync::Mutex::new(
                chain_panel::local_agent::LocalAgent::default(),
            )),
            local_url: "http://127.0.0.1:1".into(),
        }
    }
    #[tokio::test]
    async fn archive_preserves_failure_and_never_dispatches_or_resumes() {
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        let job_id = id();
        let mut receipt = json!({
            "id":job_id,"state":"running","action":"upgrade",
            "targets":[{"id":"no-remote","state":"failed","result":{
                "state":"failed","maintenance_state":"succeeded",
                "verification":{"state":"failed","detail":"fixture probe failed"}
            }}]
        });
        app.store.lock().unwrap().doc.fleet.receipts.insert(job_id.clone(), receipt.clone());
        assert!(archive_batch(State(app.clone()), Path(job_id.clone())).await.is_err());
        assert_eq!(app.store.lock().unwrap().doc.fleet.receipts[&job_id], receipt);
        receipt["state"] = json!("paused");
        app.store.lock().unwrap().doc.fleet.receipts.insert(job_id.clone(), receipt.clone());
        let _ = archive_batch(State(app.clone()), Path(job_id.clone())).await.unwrap();
        receipt["archived"] = json!(true);
        assert_eq!(app.store.lock().unwrap().doc.fleet.receipts[&job_id], receipt);
        let _ = archive_batch(State(app.clone()), Path(job_id.clone())).await.unwrap();
        assert!(resume_batch(State(app.clone()), Path(job_id.clone())).await.is_err());
        // There is no remote: dispatching anything would fail instead of completing here.
        work_batch(&app).await.unwrap();
        verify_batches(&app).await.unwrap();
        assert_eq!(app.store.lock().unwrap().doc.fleet.receipts[&job_id], receipt);
        assert!(archive_batch(State(app), Path(id())).await.is_err());
    }

    #[tokio::test]
    async fn maintenance_success_retries_verification_without_reinstalling() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task_ids = Arc::new(Mutex::new(Vec::<Value>::new()));
        let recorded_ids = task_ids.clone();
        let mut document = Document::default();
        document.lines.push(serde_json::from_value(json!({
            "id":id(),"name":"fixture broken entry","kind":"vless","hops":[]
        })).unwrap());
        let document = Arc::new(Mutex::new(document));
        let served_document = document.clone();
        let router = Router::new()
            .route("/api/federation/task", post(move |Json(body): Json<Value>| {
                let ids = recorded_ids.clone();
                async move {
                    ids.lock().unwrap().push(body["id"].clone());
                    Json(json!({"state":"succeeded","detail":"components updated"}))
                }
            }))
            .route("/api/federation/state", get(|| async {
                Json(json!({"nodes":[],"monitoring":{"effective":Policy::default()}}))
            }))
            .route("/api/federation/document", get(move || {
                let doc = served_document.clone();
                async move { Json(serde_json::to_value(&*doc.lock().unwrap()).unwrap()) }
            }));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        let remote_id = id();
        let job_id = id();
        let task_id = id();
        {
            let mut store = app.store.lock().unwrap();
            store.doc.fleet.remotes.push(Remote {
                id: remote_id.clone(), name: "fixture".into(), url: format!("http://{address}"),
                token: "test".into(), seen: now(), error: String::new(),
                policy_error: String::new(), snapshot: Value::Null,
            });
            store.doc.fleet.receipts.insert(job_id.clone(), json!({
                "id":job_id,"action":"upgrade","version":"0.2.40","state":"running",
                "targets":[{"id":remote_id,"task_id":task_id,"state":"pending"}]
            }));
        }
        let first = work_batch(&app).await;
        let maintained = app.store.lock().unwrap().doc.fleet.receipts[&job_id].clone();
        let checked = verify_batches(&app).await;
        let failed = app.store.lock().unwrap().doc.fleet.receipts[&job_id].clone();
        let second = work_batch(&app).await;
        verify_batches(&app).await.unwrap();
        let failed_again = app.store.lock().unwrap().doc.fleet.receipts[&job_id].clone();
        document.lock().unwrap().lines.clear();
        app.store.lock().unwrap().doc.fleet.receipts.get_mut(&job_id).unwrap()["targets"][0]["result"]["verification"]["retry_at"] = json!(0);
        let third = verify_batches(&app).await;
        let passed = app.store.lock().unwrap().doc.fleet.receipts[&job_id].clone();
        server.abort();
        let _ = server.await;
        first.unwrap(); checked.unwrap(); second.unwrap(); third.unwrap();
        let _released = std::net::TcpListener::bind(address).unwrap();
        assert_eq!(maintained["state"], "succeeded");
        assert_eq!(maintained["targets"][0]["result"]["verification"]["state"], "pending");
        assert_eq!(failed, failed_again, "未到复查时间，不重复检查");
        for job in [&failed, &failed_again] {
            assert_eq!(job["state"], "succeeded");
            assert_eq!(job["targets"][0]["state"], "succeeded");
            let result = &job["targets"][0]["result"];
            assert_eq!(result["maintenance_state"], "succeeded");
            assert_eq!(result["verification"]["state"], "failed");
            assert!(result["verification"]["detail"].as_str().unwrap().contains("无入口"));
            assert!(result["verification"]["detail"].as_str().unwrap().contains("fixture broken entry"));
        }
        assert_eq!(passed["state"], "succeeded");
        assert_eq!(passed["targets"][0]["result"]["maintenance_state"], "succeeded");
        assert_eq!(passed["targets"][0]["result"]["verification"]["state"], "succeeded");
        assert_eq!(*task_ids.lock().unwrap(), vec![json!(task_id)]);
    }

    #[tokio::test]
    async fn hundred_upgrades_are_bounded_and_one_failure_does_not_stop_the_rest() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
        let operations = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let concurrent = Arc::new(AtomicUsize::new(0));
        let (seen, active, peak, http, parallel) = (calls.clone(), operations.clone(), maximum.clone(), requests.clone(), concurrent.clone());
        let router = Router::new().route("/api/federation/task", post(move |Json(body): Json<Value>| {
            let (seen, active, peak, http, parallel) = (seen.clone(), active.clone(), peak.clone(), http.clone(), parallel.clone());
            async move {
                let id = body["id"].as_str().unwrap().to_string();
                parallel.fetch_max(http.fetch_add(1, SeqCst) + 1, SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                http.fetch_sub(1, SeqCst);
                let mut seen = seen.lock().unwrap();
                let count = seen.entry(id.clone()).or_default();
                *count += 1;
                if *count == 1 {
                    peak.fetch_max(active.fetch_add(1, SeqCst) + 1, SeqCst);
                    Json(json!({"state":"running"}))
                } else {
                    active.fetch_sub(1, SeqCst);
                    Json(json!({"state":if id == "task-0" { "failed" } else { "succeeded" },"error":if id == "task-0" { "fixture install failed" } else { "" }}))
                }
            }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        let job_id = id();
        {
            let mut s = app.store.lock().unwrap();
            let targets: Vec<_> = (0..100).map(|i| {
                let remote_id = format!("remote-{i}");
                s.doc.fleet.remotes.push(Remote { id:remote_id.clone(), name:remote_id.clone(), url:format!("http://{address}"),
                    token:"test".into(), seen:now(), error:String::new(), policy_error:String::new(), snapshot:Value::Null });
                json!({"id":remote_id,"task_id":format!("task-{i}"),"state":"pending"})
            }).collect();
            s.doc.fleet.receipts.insert(job_id.clone(), json!({"id":job_id,"action":"upgrade","version":"0.2.44","state":"running","targets":targets}));
        }
        let completed = async {
            for _ in 0..40 { work_batch(&app).await?; }
            Ok::<_, anyhow::Error>(())
        }.await;
        server.abort(); let _ = server.await;
        let _released = std::net::TcpListener::bind(address).unwrap();
        completed.unwrap();
        assert_eq!(maximum.load(SeqCst), 5, "远端运行的升级不能超过 5 台");
        assert!((2..=5).contains(&concurrent.load(SeqCst)), "实际请求必须并发");
        assert_eq!(operations.load(SeqCst), 0);
        let seen = calls.lock().unwrap();
        assert_eq!(seen.len(), 100);
        assert!(seen.values().all(|count| *count == 2), "完成或失败的任务不能重复下发");
        let s = app.store.lock().unwrap();
        let job = &s.doc.fleet.receipts[&job_id];
        assert_eq!(job["state"], "completed");
        assert_eq!(job["targets"][0]["state"], "failed");
        assert_eq!(job["targets"].as_array().unwrap().iter().filter(|t| t["state"] == "succeeded").count(), 99);
    }

    #[tokio::test]
    async fn legacy_resume_keeps_completed_upgrades_and_original_task_ids() {
        let legacy = json!({"state":"failed","error":"组件维护已完成，但整链验证失败：timeout"});
        assert!(maintenance_succeeded(&legacy));
        assert!(!maintenance_succeeded(&json!({"state":"unknown"})));
        let mut job = json!({"state":"paused","targets":[legacy,{"state":"pending"}]});
        finish_batch(&mut job);
        assert_eq!(job["state"], "paused", "旧任务未执行的升级不能自动恢复");
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        let job_id = id();
        job["targets"][0]["task_id"] = json!("completed-task");
        job["targets"][1]["task_id"] = json!("pending-task");
        app.store.lock().unwrap().doc.fleet.receipts.insert(job_id.clone(), job);
        let _ = resume_batch(State(app.clone()), Path(job_id.clone())).await.unwrap();
        let s = app.store.lock().unwrap();
        let resumed = &s.doc.fleet.receipts[&job_id];
        assert_eq!(resumed["state"], "running");
        assert_eq!(resumed["targets"][0]["state"], "succeeded");
        assert_eq!(resumed["targets"][0]["task_id"], "completed-task");
        assert_eq!(resumed["targets"][1]["state"], "pending");
        assert_eq!(resumed["targets"][1]["task_id"], "pending-task");
    }

    #[tokio::test]
    async fn policy_failure_keeps_fresh_remote_state_and_controller_clock() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/api/federation/state", get(|| async {
                Json(json!({"time":now(),"version":"fixture","monitoring":{"override":{"enabled":true}}}))
            }))
            .route("/api/federation/policy", post(|| async {
                (StatusCode::BAD_REQUEST, Json(json!({"error":"fixture policy rejected"})))
            }));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        let remote = Remote {
            id: id(), name: "fixture".into(), url: format!("http://{address}"),
            token: "test".into(), seen: 1, error: "old connection error".into(),
            policy_error: String::new(), snapshot: Value::Null,
        };
        app.store.lock().unwrap().doc.fleet.remotes.push(remote.clone());
        let result = monitor_api::poll_remote(&app, remote, false).await;
        server.abort();
        let _ = server.await;
        result.unwrap();
        let Json(value) = view(State(app)).await;
        assert!(value["server_now"].as_u64().unwrap() > 1);
        assert!(value["remotes"][0]["seen"].as_u64().unwrap() > 1);
        assert_eq!(value["remotes"][0]["snapshot"]["version"], "fixture");
        assert_eq!(value["remotes"][0]["error"], "");
        assert!(value["remotes"][0]["policy_error"].as_str().unwrap().contains("fixture policy rejected"));
    }

    #[tokio::test]
    async fn local_and_remote_state_expose_the_same_versions() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        let Json(local) = state(State(app.clone())).await;
        let Json(remote) = remote_state(State(app)).await;
        assert_eq!(local["version"], env!("CARGO_PKG_VERSION"));
        assert!(local["agent_version"].is_null());
        for key in ["version", "agent_version", "agent_seen", "cores", "maintenance", "maintenance_actions"] {
            assert_eq!(local[key], remote[key], "{key}");
        }
    }

    #[tokio::test]
    async fn restart_all_requires_remote_capability_and_persists_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        let remote_id = id();
        app.store.lock().unwrap().doc.fleet.remotes.push(Remote {
            id: remote_id.clone(), name: "test".into(), url: "https://unused.invalid".into(),
            token: String::new(), seen: now(), error: String::new(), policy_error: String::new(),
            snapshot: json!({"maintenance":true}),
        });
        let input = || Batch { id: id(), targets: vec![remote_id.clone()],
            action: "restart-all".into(), version: String::new(), lines: BTreeMap::new() };
        assert!(batch(State(app.clone()), Json(input())).await.is_err());
        assert!(app.store.lock().unwrap().doc.fleet.receipts.is_empty());
        app.store.lock().unwrap().doc.fleet.remotes[0].snapshot["maintenance_actions"] = json!(["restart-all"]);
        let Json(result) = batch(State(app.clone()), Json(input())).await.unwrap();
        let store = app.store.lock().unwrap();
        let job = &store.doc.fleet.receipts[result["id"].as_str().unwrap()];
        assert_eq!(job["action"], "restart-all");
        assert_eq!(job["targets"][0]["id"], remote_id);
        assert_eq!(job["targets"][0]["state"], "pending");
    }

    #[tokio::test]
    async fn machine_auth_is_scoped_and_keeps_origin_checks() {
        use axum::body::Body;
        use hyper::service::Service;
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        app.store.lock().unwrap().doc.fleet.controller_hash = digest("machine-token");
        let router = Router::new()
            .route("/api/lines", post(|| async { "ok" }))
            .route("/api/security/token", post(|| async { "must not reach" }))
            .layer(middleware::from_fn_with_state(app.clone(), guard));
        let service = hyper_util::service::TowerToHyperService::new(router);
        for (path, token, marker, origin, expected) in [
            (
                "/api/lines",
                "machine-token",
                true,
                "https://panel.test",
                StatusCode::OK,
            ),
            (
                "/api/lines",
                "wrong",
                true,
                "https://panel.test",
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/security/token",
                "machine-token",
                true,
                "https://panel.test",
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/lines",
                "machine-token",
                false,
                "https://panel.test",
                StatusCode::FORBIDDEN,
            ),
            (
                "/api/lines",
                "machine-token",
                true,
                "https://other.test",
                StatusCode::FORBIDDEN,
            ),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "panel.test")
                .header("origin", origin)
                .header("authorization", format!("Bearer {token}"));
            if marker {
                request = request.header("x-chain-request", "1");
            }
            let response = service
                .call(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{path} {origin}");
        }
    }
    #[tokio::test]
    async fn invitation_and_ticket_are_single_use_and_unbinding_revokes_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        let code = invite(State(app.clone())).await.unwrap().0["code"]
            .as_str()
            .unwrap()
            .to_string();
        let _ = pair(
            State(app.clone()),
            Json(Pair {
                code: code.clone(),
                name: "controller".into(),
                controller_id: id(),
            }),
        )
        .await
        .unwrap();
        assert!(
            pair(
                State(app.clone()),
                Json(Pair {
                    code,
                    name: "another".into(),
                    controller_id: id()
                })
            )
            .await
            .is_err()
        );
        let value = ticket(State(app.clone())).await.unwrap().0["ticket"]
            .as_str()
            .unwrap()
            .to_string();
        let response = exchange(
            State(app.clone()),
            Json(Ticket {
                ticket: value.clone(),
            }),
        )
        .await
        .unwrap();
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("HttpOnly; SameSite=Strict")
        );
        assert!(
            exchange(State(app.clone()), Json(Ticket { ticket: value }))
                .await
                .is_err()
        );
        assert_eq!(app.sessions.lock().unwrap().len(), 1);
        let pending = ticket(State(app.clone())).await.unwrap().0["ticket"]
            .as_str()
            .unwrap()
            .to_string();
        app.sessions
            .lock()
            .unwrap()
            .insert("independent-session".into(), now() + 600);
        let _ = revoke(State(app.clone())).await.unwrap();
        assert_eq!(app.sessions.lock().unwrap().len(), 1);
        assert!(
            app.sessions
                .lock()
                .unwrap()
                .contains_key("independent-session")
        );
        assert!(
            exchange(State(app.clone()), Json(Ticket { ticket: pending }))
                .await
                .is_err()
        );
        assert!(
            app.store
                .lock()
                .unwrap()
                .doc
                .fleet
                .controller_hash
                .is_empty()
        );
    }
    #[tokio::test]
    async fn expired_invitation_and_ticket_cannot_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path());
        {
            let mut s = app.store.lock().unwrap();
            s.doc.fleet.invitation_hash = digest("expired");
            s.doc.fleet.invitation_expires = now() - 1;
        }
        assert!(
            pair(
                State(app.clone()),
                Json(Pair {
                    code: "expired".into(),
                    name: "controller".into(),
                    controller_id: id()
                })
            )
            .await
            .is_err()
        );
        app.store.lock().unwrap().doc.fleet.controller_hash = digest("owner");
        app.tickets
            .lock()
            .unwrap()
            .insert(digest("ticket"), (now() - 1, digest("owner")));
        assert!(
            exchange(
                State(app.clone()),
                Json(Ticket {
                    ticket: "ticket".into()
                })
            )
            .await
            .is_err()
        );
    }
}
