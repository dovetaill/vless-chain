use super::*;
macro_rules! ensure {($condition:expr,$($message:tt)*)=>{if !$condition {return Err(anyhow::anyhow!($($message)*).into());}};}

fn applied(doc: &Document) -> Document {
    Document {
        nodes: doc.applied_nodes.clone(),
        lines: doc.applied_lines.clone(),
        externals: doc.applied_externals.clone(),
        ..Default::default()
    }
}
pub async fn export(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let doc = applied(&app.store.lock().unwrap().doc);
    let line = doc
        .lines
        .iter()
        .find(|l| l.id == id && l.enabled)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "请先应用并启用此线路"))?;
    ensure!(
        line.kind == Kind::Vless,
        "此规则使用入口地址，无代理客户端配置"
    );
    let node = chain_panel::access::mihomo_node(line, &doc)?;
    Ok(Json(
        json!({"share":share(line,&doc)?,"node":format!("- {}\n", serde_json::to_string(&node)?)}),
    ))
}
pub async fn external_export(
    State(app): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let store = app.store.lock().unwrap();
    let proxy = store
        .doc
        .externals
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "外部代理不存在"))?;
    let node = chain_panel::external::mihomo(&proxy.url, &proxy.name)?;
    Ok(Json(
        json!({"share":proxy.url,"node":format!("- {}\n", serde_json::to_string(&node)?)}),
    ))
}
#[derive(Deserialize)]
pub struct SubscriptionInput {
    name: String,
    line_ids: Vec<String>,
}
pub async fn subscribe(
    State(app): State<App>,
    Json(input): Json<SubscriptionInput>,
) -> ApiResult<Json<Value>> {
    ensure!(
        !input.name.trim().is_empty() && input.name.len() <= 120,
        "订阅名称无效"
    );
    ensure!(!input.line_ids.is_empty(), "请至少选择一条代理线路");
    let token = secret();
    let subscription = Subscription {
        id: id(),
        name: input.name,
        token_hash: digest(&token),
        line_ids: input.line_ids,
        created: now(),
    };
    edit(&app, |d| {
        ensure!(d.subscriptions.len() < 100, "最多创建 100 个订阅");
        ensure!(
            subscription
                .line_ids
                .iter()
                .all(|id| d.lines.iter().any(|l| l.id == *id && l.kind == Kind::Vless)),
            "订阅引用了不存在的代理线路"
        );
        d.subscriptions.push(subscription.clone());
        Ok(())
    })?;
    Ok(Json(
        json!({"id":subscription.id,"path":format!("/subscription/{token}")}),
    ))
}
pub async fn revoke(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    edit(&app, |d| {
        d.subscriptions.retain(|s| s.id != id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
pub async fn subscription(
    State(app): State<App>,
    Path(token): Path<String>,
) -> ApiResult<Response> {
    let store = app.store.lock().unwrap();
    let s = store
        .doc
        .subscriptions
        .iter()
        .find(|s| equal(&s.token_hash, &digest(&token)))
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "订阅不存在或已撤销"))?;
    let doc = applied(&store.doc);
    let lines: Vec<_> = doc
        .lines
        .iter()
        .filter(|l| s.line_ids.contains(&l.id) && l.enabled)
        .cloned()
        .collect();
    let yaml = chain_panel::access::mihomo(&lines, &doc)?;
    let mut response = yaml.into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/yaml; charset=utf-8"),
    );
    response.headers_mut().insert(
        "content-disposition",
        HeaderValue::from_static("attachment; filename=chain-panel.yaml"),
    );
    Ok(response)
}
pub fn alerts(doc: &Document) -> Vec<Value> {
    let mut alerts = vec![];
    for node in &doc.nodes {
        if node.report.system["certificate"]["ok"] == false {
            alerts.push(json!({"id":format!("cert-{}",node.id),"level":"error","message":format!("{}：证书续期失败，请检查域名、80 端口和 Agent 日志",node.name)}));
        }
        if now().saturating_sub(node.seen) >= 15 {
            alerts.push(json!({"id":format!("node-{}",node.id),"level":"warning","message":format!("{}：服务器离线或尚未接入",node.name)}));
        } else if !node.report.error.is_empty() {
            alerts.push(json!({"id":format!("node-{}",node.id),"level":"error","message":format!("{}：{}",node.name,node.report.error)}));
        }
    }
    for line in &doc.lines {
        let traffic = line
            .hops
            .first()
            .and_then(|h| doc.nodes.iter().find(|n| n.id == h.node_id))
            .and_then(|n| n.report.traffic.get(&line.id));
        if let (Some(t), Some(q)) = (traffic, line.limits.monthly_gib) {
            let ratio = (t.upload + t.download) as f64 / (q * 1073741824.0);
            if ratio >= 0.8 || t.blocked {
                alerts.push(json!({"id":format!("quota-{}",line.id),"level":if t.blocked {"error"} else {"warning"},"message":format!("{}：{}，本月额度已使用 {:.1}%",line.name,if t.blocked {"额度用尽，已停止传输"} else if ratio>=0.95 {"接近流量上限"} else {"流量提醒"},ratio*100.0)}));
            }
        }
        if let Some(check) = doc.checks.get(&line.id) {
            if check["ok"] == false {
                alerts.push(json!({"id":format!("check-{}",line.id),"level":"warning","message":format!("{}：最近一次连接检查未通过",line.name)}));
            }
        }
    }
    alerts
}
