use super::*;
use chain_panel::diagnostics::LogRequest;
use tokio::sync::oneshot;

#[derive(Default)]
pub struct Broker(pub Mutex<HashMap<String, Pending>>);
pub struct Pending {
    id: String,
    request: Value,
    expires: std::time::Instant,
    reply: Option<oneshot::Sender<Value>>,
}
pub fn routes(router: Router<App>) -> Router<App> {
    router.route("/api/diagnostics/{id}/logs", post(logs))
        .route("/api/diagnostics/{id}/reality", post(reality))
        .route("/api/diagnostics/{id}/path", post(path_probe))
        .route("/api/agent/{id}/diagnostics", post(exchange))
}
async fn logs(State(app): State<App>, Path(id): Path<String>, Json(request): Json<LogRequest>) -> ApiResult<Json<Value>> {
    request.validate()?;
    if request.source == "panel" {
        return Ok(Json(chain_panel::diagnostics::read(app.token_file.parent().unwrap(), &request)?));
    }
    Ok(Json(dispatch(&app, &id, json!({"operation":"logs","request":request}), 10).await?))
}
async fn reality(State(app): State<App>, Path(id): Path<String>, Json(input): Json<RealityTarget>) -> ApiResult<Json<Value>> {
    target(&input.target)?; if !host_valid(&input.server_name) { return Err(error(StatusCode::BAD_REQUEST,"SNI 无效")); }
    Ok(Json(dispatch(&app, &id, json!({"operation":"reality","request":input}), 55).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathProbe { line_id: String, url: String }
async fn path_probe(State(app): State<App>, Path(id): Path<String>, Json(input): Json<PathProbe>) -> ApiResult<Json<Value>> {
    uuid::Uuid::parse_str(&input.line_id).map_err(|_| error(StatusCode::BAD_REQUEST,"代理 ID 无效"))?;
    Ok(Json(dispatch(&app,&id,json!({"operation":"path","request":{"line_id":input.line_id,"url":input.url}}),40).await?))
}
pub async fn dispatch(app: &App, id: &str, request: Value, seconds: u64) -> Result<Value> {
    {
        let s = app.store.lock().unwrap();
        let node = s.doc.nodes.iter().find(|n| n.id == id).context("节点不存在")?;
        ensure!(now().saturating_sub(node.seen) < 30, "Agent 离线，无法读取新日志；历史日志保留在该 VPS");
        ensure!(node.report.system["diagnostics_version"] == 1, "请先升级该 Agent，以支持 Web 日志");
    }
    let (tx, rx) = oneshot::channel();
    let job_id = model_id();
    {
        let mut queue = app.diagnostics.0.lock().unwrap();
        queue.retain(|_, p| p.expires > std::time::Instant::now() && p.reply.as_ref().is_some_and(|r| !r.is_closed()));
        ensure!(queue.len() < 64 && !queue.contains_key(id), "该节点已有日志请求，请稍后刷新");
        queue.insert(id.to_string(), Pending {id:job_id.clone(),request,expires:std::time::Instant::now()+Duration::from_secs(seconds),reply:Some(tx)});
    }
    let result = tokio::time::timeout(Duration::from_secs(seconds), rx).await;
    {
        let mut queue = app.diagnostics.0.lock().unwrap();
        if queue.get(id).is_some_and(|p| p.id == job_id) { queue.remove(id); }
    }
    let value = result.context("Agent 日志读取超时，未返回旧日志")?.context("日志请求已结束")?;
    if let Some(e) = value["error"].as_str() { return Err(anyhow::anyhow!("{e}")); }
    Ok(value)
}
fn model_id() -> String { chain_panel::model::id() }
#[derive(Deserialize)]
struct Reply { #[serde(default)] id: String, #[serde(default)] result: Value }
async fn exchange(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap, Json(reply): Json<Reply>) -> ApiResult<Json<Value>> {
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    {
        let s = app.store.lock().unwrap();
        let n = s.doc.nodes.iter().find(|n| n.id == id).ok_or_else(|| error(StatusCode::UNAUTHORIZED,"节点身份无效"))?;
        if !equal(&digest(token), &n.token_hash) { return Err(error(StatusCode::UNAUTHORIZED,"节点身份无效")); }
    }
    let mut queue = app.diagnostics.0.lock().unwrap();
    if let Some(p) = queue.get_mut(&id) {
        if p.expires <= std::time::Instant::now() { queue.remove(&id); }
        else if reply.id == p.id {
            if serde_json::to_vec(&reply.result)?.len() > 300_000 { return Err(error(StatusCode::BAD_REQUEST,"日志响应过大")); }
            if let Some(tx) = p.reply.take() { let _ = tx.send(reply.result); }
            queue.remove(&id);
        } else { return Ok(Json(json!({"id":p.id,"request":p.request}))); }
    }
    Ok(Json(Value::Null))
}

pub fn targets(config: &Value) -> BTreeSet<(String, String)> {
    let mut result = BTreeSet::new();
    if let Some(inbounds) = config["inbounds"].as_array() {
        for inbound in inbounds {
            if inbound["streamSettings"]["security"] != "reality" { continue; }
            let reality = &inbound["streamSettings"]["realitySettings"];
            if let Some(target) = reality["target"].as_str().or_else(|| reality["dest"].as_str()) {
                if let Some(names) = reality["serverNames"].as_array() {
                    for name in names.iter().filter_map(Value::as_str) { result.insert((target.into(),name.into())); }
                }
            }
        }
    }
    if let Some(profiles) = config["_legacy"].as_array() {
        for profile in profiles { result.extend(targets(&profile["config"])); }
    }
    result
}
pub async fn preflight(app: &App, allow_unverified: bool) -> Result<BTreeMap<String,Value>> {
    let (configs, nodes) = { let s = app.store.lock().unwrap(); (compile(&s.doc)?, s.doc.nodes.clone()) };
    for node in nodes {
        let Some(config) = configs.get(&node.id) else { continue; };
        let old = node.desired.as_ref().map(|d| targets(&d.config)).unwrap_or_default();
        for (target, server_name) in targets(config).difference(&old) {
            let result = dispatch(app, &node.id, json!({"operation":"reality","request":{"target":target,"server_name":server_name}}), 55).await;
            match result {
                Ok(v) if v["ok"] == true => (),
                Ok(v) if v["status"] == "failed" => anyhow::bail!("{}：{} / {} 的 REALITY 握手未通过，已阻止下发。{}",node.name,target,server_name,v["detail"].as_str().unwrap_or("")),
                other if !allow_unverified => anyhow::bail!("REALITY_UNVERIFIED：{}：{} / {} 尚未验证：{}。可以保留草稿；确认接受连接失败风险后可显式继续下发。",node.name,target,server_name,match other {Ok(v)=>v["detail"].as_str().unwrap_or("未知结果").to_string(),Err(e)=>format!("{e:#}")}),
                _ => (),
            }
        }
    }
    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_targets_keep_each_hop_sni_separate() {
        let config = json!({"inbounds":[
            {"streamSettings":{"security":"reality","realitySettings":{"target":"one.example:443","serverNames":["one.example"]}}},
            {"streamSettings":{"security":"reality","realitySettings":{"target":"two.example:443","serverNames":["two.example"]}}}
        ]});
        let targets = targets(&config);
        assert_eq!(targets.len(),2);
        assert!(targets.contains(&("two.example:443".into(),"two.example".into())));
        assert!(!chain_panel::management::machine_route("POST","/api/diagnostics/../../logs"));
    }
}
