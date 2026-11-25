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
        let action = match request["operation"].as_str() {
            Some("logs") => "读取实时日志（历史日志仍保留在该 VPS）",
            Some("reality") => "执行 REALITY 目标检查",
            Some("reality-candidates") => "获取 REALITY 推荐目标",
            Some("path") => "执行链路分段检查",
            _ => "执行节点诊断",
        };
        ensure!(now().saturating_sub(node.seen) < 60, "Agent 离线，暂时无法{action}");
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
                    let job = id();
                    let expires = std::time::Instant::now() + timeout;
                    queue.insert(node.to_string(), Pending { id: job.clone(), request: request.clone(), expires, reply: tx, delivered: false });
                    (job, rx, true, expires)
                };
                (Waiting { broker: self, node: node.into(), job, reply }, shared, expires)
            };
            let deadline = if shared { expires } else { expires.min(wait_until) };
            let result = tokio::time::timeout_at(deadline.into(), waiting.reply.changed()).await;
            if shared {
                result.context("Agent 诊断响应超时，请检查节点服务和网络")?.context("诊断请求已结束，请重试")?;
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
                ensure!(serde_json::to_vec(&reply.result)?.len() <= 300_000, "诊断响应过大");
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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(!chain_panel::management::machine_route("POST","/api/diagnostics/../../logs"));
    }
}
