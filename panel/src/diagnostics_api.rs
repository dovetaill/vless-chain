use super::*;
use chain_panel::diagnostics::LogRequest;
use tokio::sync::watch;

#[derive(Default)]
pub struct Broker(pub Mutex<HashMap<String, Pending>>);
pub struct Pending {
    id: String,
    request: Value,
    expires: std::time::Instant,
    reply: watch::Sender<Option<Value>>,
    delivered: bool,
}
pub fn routes(router: Router<App>) -> Router<App> {
    router.route("/api/diagnostics/{id}/logs", post(logs))
        .route("/api/diagnostics/{id}/reality", post(reality))
        .route("/api/diagnostics/{id}/reality-candidates", post(reality_candidates))
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
async fn reality_candidates(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    {
        let store = app.store.lock().unwrap();
        let node = store.doc.nodes.iter().find(|n| n.id == id).context("节点不存在")?;
        if node.report.system["reality_candidates_version"] != 1 {
            return Err(error(StatusCode::BAD_REQUEST, "请先升级该节点 Agent，以支持 REALITY 目标推荐"));
        }
    }
    Ok(Json(dispatch(&app, &id, json!({"operation":"reality-candidates"}), 55).await?))
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
    app.diagnostics.request(id, request, Duration::from_secs(seconds)).await
}

struct Waiting<'a> {
    broker: &'a Broker,
    node: String,
    job: String,
    reply: watch::Receiver<Option<Value>>,
}
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let mut queue = self.broker.0.lock().unwrap();
        if queue.get(&self.node).is_some_and(|p| p.id == self.job
            && (p.expires <= std::time::Instant::now() || (!p.delivered && p.reply.receiver_count() == 1))) {
            queue.remove(&self.node);
        }
    }
}
impl Broker {
    async fn request(&self, node: &str, request: Value, timeout: Duration) -> Result<Value> {
        let wait_until = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let (mut waiting, shared, expires) = {
                let mut queue = self.0.lock().unwrap();
                queue.retain(|_, p| p.expires > std::time::Instant::now());
                let (job, reply, shared, expires) = if let Some(p) = queue.get(node) {
                    ensure!(p.reply.receiver_count() < 64, "该节点诊断等待人数过多，请稍后重试；这不表示节点离线");
                    (p.id.clone(), p.reply.subscribe(), request["operation"] == "logs" && p.request == request, p.expires)
                } else {
                    ensure!(queue.len() < 64, "诊断任务繁忙，请稍后重试；这不表示节点离线");
                    let (tx, rx) = watch::channel(None);
                    let job = model_id();
                    let expires = std::time::Instant::now() + timeout;
                    queue.insert(node.to_string(), Pending { id: job.clone(), request: request.clone(), expires, reply: tx, delivered: false });
                    (job, rx, true, expires)
                };
                (Waiting { broker: self, node: node.into(), job, reply }, shared, expires)
            };
            let deadline = if shared { expires } else { expires.min(wait_until) };
            let result = tokio::time::timeout_at(deadline.into(), waiting.reply.changed()).await;
            if shared {
                result.context("Agent 诊断响应超时，请检查节点服务和网络；未返回旧日志")?.context("诊断请求已结束，请重试")?;
                let value = waiting.reply.borrow().clone().context("诊断未返回结果")?;
                if let Some(e) = value["error"].as_str() { anyhow::bail!("{e}"); }
                return Ok(value);
            }
            ensure!(std::time::Instant::now() < wait_until, "该节点仍在执行其他诊断，请稍后重试；这不表示节点离线");
        }
    }

    fn exchange(&self, node: &str, reply: Reply) -> Result<Value> {
        let mut queue = self.0.lock().unwrap();
        if let Some(p) = queue.get_mut(node) {
            if p.expires <= std::time::Instant::now() { queue.remove(node); }
            else if reply.id == p.id {
                ensure!(serde_json::to_vec(&reply.result)?.len() <= 300_000, "日志响应过大");
                p.reply.send_replace(Some(reply.result));
                queue.remove(node);
            } else {
                p.delivered = true;
                return Ok(json!({"id":p.id,"request":p.request}));
            }
        }
        Ok(Value::Null)
    }
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
    Ok(Json(app.diagnostics.exchange(&id, reply)?))
}

pub fn targets(config: &Value) -> BTreeSet<(String, String)> {
    let mut result = BTreeSet::new();
    if let Some(inbounds) = config["inbounds"].as_array() {
        for inbound in inbounds {
            if inbound["streamSettings"]["security"] != "reality" { continue; }
            let reality = &inbound["streamSettings"]["realitySettings"];
            if let Some(target) = reality["target"].as_str().or_else(|| reality["dest"].as_str()) {
                let guard_tag = format!("{}-reality-target", inbound["tag"].as_str().unwrap_or(""));
                let target = config["outbounds"].as_array().and_then(|items| items.iter()
                    .find(|item| item["tag"] == guard_tag && item["protocol"] == "freedom"))
                    .and_then(|item| item["settings"]["redirect"].as_str()).unwrap_or(target);
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
fn require_verified(result: &Value) -> Result<()> {
    ensure!(result["ok"] == true && result["cloudflare_checked"] == true
        && result["fallback"]["status"] == "verified",
        "REALITY 检查未全部通过，已保留草稿并阻止发布：{}", result["detail"].as_str().unwrap_or("缺少验证结果"));
    Ok(())
}
pub async fn preflight(app: &App) -> Result<BTreeMap<String,Value>> {
    let (configs, nodes) = { let s = app.store.lock().unwrap(); (compile(&s.doc)?, s.doc.nodes.clone()) };
    for node in nodes {
        let Some(config) = configs.get(&node.id) else { continue; };
        if node.desired.as_ref().is_some_and(|d| &d.config == config) { continue; }
        for (target, server_name) in targets(config) {
            ensure!(node.report.system["reality_preflight_version"] == 1, "{}：请升级 Agent 后再发布 REALITY；草稿已保留", node.name);
            let result = dispatch(app, &node.id, json!({"operation":"reality-preflight","request":{"target":target,"server_name":server_name}}), 70).await
                .with_context(|| format!("{}：REALITY 发布检查未完成，草稿已保留", node.name))?;
            require_verified(&result).with_context(|| format!("{}：{target} / {server_name}", node.name))?;
        }
    }
    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reality_publication_requires_every_check() {
        let verified = json!({"ok":true,"cloudflare_checked":true,"fallback":{"status":"verified"}});
        require_verified(&verified).unwrap();
        for key in ["ok", "cloudflare_checked", "fallback"] {
            let mut missing = verified.clone(); missing.as_object_mut().unwrap().remove(key);
            assert!(require_verified(&missing).is_err());
        }
        assert!(require_verified(&json!({"ok":false,"status":"unverified"})).is_err());
    }
    fn log_request(source: &str) -> Value {
        json!({"operation":"logs","request":{"source":source}})
    }
    fn poll_agent(broker: &Broker) -> Value {
        broker.exchange("node", Reply { id: String::new(), result: Value::Null }).unwrap()
    }
    #[tokio::test]
    async fn diagnostics_identical_logs_share_one_result() {
        let broker = Broker::default();
        let mut first = Box::pin(broker.request("node", log_request("agent"), Duration::from_secs(1)));
        let mut second = Box::pin(broker.request("node", log_request("agent"), Duration::from_secs(1)));
        assert!(futures_util::poll!(&mut first).is_pending());
        assert!(futures_util::poll!(&mut second).is_pending());
        assert_eq!(broker.0.lock().unwrap().len(), 1);
        let task = poll_agent(&broker);
        let result = json!({"lines":["same snapshot"]});
        broker.exchange("node", Reply { id: task["id"].as_str().unwrap().into(), result: result.clone() }).unwrap();
        assert_eq!(first.await.unwrap(), result);
        assert_eq!(second.await.unwrap(), result);
        assert!(broker.0.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn diagnostics_cancel_cleans_unstarted_but_preserves_running_probe() {
        let broker = Broker::default();
        let mut future = Box::pin(broker.request("node", log_request("agent"), Duration::from_secs(1)));
        assert!(futures_util::poll!(&mut future).is_pending());
        drop(future);
        assert!(broker.0.lock().unwrap().is_empty());
        let probe = json!({"operation":"path","request":{"line_id":"line"}});
        let mut future = Box::pin(broker.request("node", probe.clone(), Duration::from_secs(1)));
        assert!(futures_util::poll!(&mut future).is_pending());
        let task = poll_agent(&broker);
        drop(future);
        let mut logs = Box::pin(broker.request("node", log_request("agent"), Duration::from_secs(1)));
        assert!(futures_util::poll!(&mut logs).is_pending());
        assert_eq!(poll_agent(&broker), task);
        broker.exchange("node", Reply { id: task["id"].as_str().unwrap().into(), result: json!({"probe":true}) }).unwrap();
        assert!(futures_util::poll!(&mut logs).is_pending());
        let next = poll_agent(&broker);
        assert_eq!(next["request"], log_request("agent"));
        // A late reply for the previous job must never satisfy the new request.
        let still_waiting = broker.exchange("node", Reply { id: task["id"].as_str().unwrap().into(), result: json!({"wrong":true}) }).unwrap();
        assert_eq!(still_waiting["id"], next["id"]);
        assert!(futures_util::poll!(&mut logs).is_pending());
        broker.exchange("node", Reply { id: next["id"].as_str().unwrap().into(), result: json!({"lines":[]}) }).unwrap();
        assert_eq!(logs.await.unwrap(), json!({"lines":[]}));
        assert!(broker.0.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn diagnostics_different_sources_serialize_without_mixing_results() {
        let broker = Broker::default();
        let mut first = Box::pin(broker.request("node", log_request("agent"), Duration::from_secs(1)));
        let mut second = Box::pin(broker.request("node", log_request("xray"), Duration::from_secs(1)));
        assert!(futures_util::poll!(&mut first).is_pending());
        assert!(futures_util::poll!(&mut second).is_pending());
        let task = poll_agent(&broker);
        broker.exchange("node", Reply { id: task["id"].as_str().unwrap().into(), result: json!({"source":"agent"}) }).unwrap();
        assert_eq!(first.await.unwrap()["source"], "agent");
        assert!(futures_util::poll!(&mut second).is_pending());
        let task = poll_agent(&broker);
        assert_eq!(task["request"], log_request("xray"));
        broker.exchange("node", Reply { id: task["id"].as_str().unwrap().into(), result: json!({"source":"xray"}) }).unwrap();
        assert_eq!(second.await.unwrap()["source"], "xray");
    }
    #[tokio::test]
    async fn diagnostics_timeout_removes_job_and_allows_retry() {
        let broker = Broker::default();
        assert!(broker.request("node", log_request("agent"), Duration::from_millis(1)).await.unwrap_err().to_string().contains("超时"));
        assert!(broker.0.lock().unwrap().is_empty());
        let mut retry = Box::pin(broker.request("node", log_request("agent"), Duration::from_secs(1)));
        assert!(futures_util::poll!(&mut retry).is_pending());
        poll_agent(&broker);
        drop(retry);
        assert_eq!(broker.0.lock().unwrap().len(), 1);
        broker.0.lock().unwrap().get_mut("node").unwrap().expires = std::time::Instant::now();
        assert_eq!(poll_agent(&broker), Value::Null);
        assert!(broker.0.lock().unwrap().is_empty());
    }
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
    #[test]
    fn diagnostics_targets_unwrap_guard_destination() {
        let mut config = chain_panel::compiler::empty_config();
        let mut inbound = json!({"tag":"line-test-in","streamSettings":{"security":"reality","realitySettings":{"target":"cover.example:8443","serverNames":["cover.example"]}}});
        chain_panel::compiler::guard_reality(&mut config, &mut inbound, 61000).unwrap();
        config["inbounds"].as_array_mut().unwrap().push(inbound);
        assert_eq!(targets(&config), BTreeSet::from([("cover.example:8443".into(), "cover.example".into())]));
    }
}
