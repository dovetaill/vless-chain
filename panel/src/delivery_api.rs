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
    let at = now();
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
        let traffic = chain_panel::rental::safe_traffic(
            chain_panel::plans::lease(doc, &line.id), chain_panel::rental::traffic(doc, line), at);
        if let (Some(t), Some(q)) = (traffic, line.limits.monthly_gib) {
            let ratio = t.used() as f64 / (q * 1073741824.0);
            if ratio >= 0.8 {
                alerts.push(json!({"id":format!("quota-{}",line.id),"level":if ratio>=1.0 {"error"} else {"warning"},"message":format!("{}：{}，本周期额度已使用 {:.1}%",line.name,if ratio>=1.0 {"额度用尽"} else if ratio>=0.95 {"接近流量上限"} else {"流量提醒"},ratio*100.0)}));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_alerts_use_charged_current_cycle_and_applied_entry() {
        let at = now();
        let gib = 1_u64 << 30;
        let applied: Line = serde_json::from_value(json!({"id":"line","name":"proxy","kind":"vless",
            "hops":[{"node_id":"node","port":443,"uuid":"fixture"}],"limits":{"monthly_gib":1.0}})).unwrap();
        let mut node: Node = serde_json::from_value(json!({"id":"node","name":"node","host":"192.0.2.1",
            "token_hash":"","seen":at,"reality":{"target":"","server_name":"","private_key":"","public_key":"","short_id":""}})).unwrap();
        node.report.traffic.insert("line".into(), Traffic { upload: 1, charged: Some(gib), ..Default::default() });
        let mut draft = applied.clone();
        draft.hops[0].node_id = "unsaved-entry".into();
        let mut doc = Document { nodes: vec![node], lines: vec![draft], applied_lines: vec![applied], ..Default::default() };
        let quota = |doc: &Document| alerts(doc).into_iter().filter(|a| a["id"] == "quota-line").collect::<Vec<_>>();
        let messages = quota(&doc);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["level"], "error");
        assert!(messages[0]["message"].as_str().unwrap().contains("100.0%"));

        let traffic = doc.nodes[0].report.traffic.get_mut("line").unwrap();
        traffic.charged = Some(0);
        traffic.upload = gib;
        traffic.blocked = true;
        assert!(quota(&doc).is_empty(), "停用或到期不能当作额度耗尽，也不能覆盖权威扣量");
        doc.nodes[0].report.traffic.get_mut("line").unwrap().charged = None;
        assert_eq!(quota(&doc).len(), 1, "旧 Agent 没有扣量字段时仍使用上下行合计");

        doc.rentals.insert("line".into(), Rental { started_at: at - 86400, expires_at: at + 86400, ..Default::default() });
        assert!(quota(&doc).is_empty(), "旧周期上报不能产生本周期额度告警");
    }
}
