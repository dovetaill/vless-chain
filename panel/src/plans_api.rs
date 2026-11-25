use super::*;
use chain_panel::plans;

pub fn routes(router: Router<App>) -> Router<App> {
    router.route("/api/customer-plans", get(view).post(save))
        .route("/api/customer-plans/{id}", delete(remove))
        .route("/api/customer-plans/{id}/query-link", post(plan_link))
        .route("/api/customer-plans/{id}/members/{member}/query-link", post(member_link))
}
async fn plan_link(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> { query_link(app, id, None) }
async fn member_link(State(app): State<App>, Path((id, member)): Path<(String, String)>) -> ApiResult<Json<Value>> { query_link(app, id, Some(member)) }
fn query_link(app: App, id: String, member_id: Option<String>) -> ApiResult<Json<Value>> {
    let token = secret();
    edit(&app, |doc| {
        let plan = doc.customer_plans.iter().find(|p| p.id == id).context("套餐不存在")?;
        let (key, scoped_lines): (String, Vec<&String>) = if let Some(member_id) = member_id {
            let member = plan.members.iter().find(|m| m.id == member_id).context("套餐成员不存在")?;
            (plans::member_key(plan, member), member.line_ids.iter().collect())
        } else {
            (plan.key(), plan.lines().collect())
        };
        ensure!(
            scoped_lines.iter().any(|lid| doc.applied_lines.iter().any(|l| l.id == **lid && l.kind == Kind::Vless)),
            "请先应用此套餐关联的代理线路"
        );
        doc.customer_query_tokens.insert(key, digest(&token));
        Ok(())
    })?;
    Ok(Json(json!({"path":format!("/query#token={token}")})))
}
async fn view(State(app): State<App>) -> Json<Value> {
    Json(json!(plans::view(&app.store.lock().unwrap().doc, now())))
}
#[derive(Deserialize)]
struct Input {
    #[serde(flatten)]
    plan: plans::Plan,
    prices: Prices,
}
async fn save(State(app): State<App>, Json(mut input): Json<Input>) -> ApiResult<Json<Value>> {
    let value = edit(&app, |doc| {
        let creating = input.plan.id.is_empty();
        if creating { input.plan.id = id(); }
        for member in &mut input.plan.members { if member.id.is_empty() { member.id = id(); } }
        let previous = doc.customer_plans.iter().find(|p| p.id == input.plan.id).cloned();
        ensure!(creating || previous.is_some(), "套餐不存在");
        plans::validate(doc, &input.plan, previous.as_ref())?;
        input.prices.validate()?;
        let key = input.plan.key();
        let mut schedule_targets = vec![key.clone()];
        if let Some(old) = &previous {
            for old_member in &old.members {
                if !input.plan.members.iter().any(|m| m.id == old_member.id) {
                    doc.customer_query_tokens.remove(&plans::member_key(old, old_member));
                }
            }
            for old_line in old.lines() {
                if !input.plan.lines().any(|lid| lid == old_line) {
                    if let Some(line) = doc.lines.iter_mut().find(|l| l.id == *old_line) {
                        line.enabled = false;
                        schedule_targets.push(old_line.clone());
                    }
                }
            }
        }
        let rental = doc.rentals.entry(key).or_default();
        rental.customer_id = input.plan.customer_id.clone();
        rental.prices = input.prices;
        doc.customer_plans.retain(|p| p.id != input.plan.id);
        doc.customer_plans.push(input.plan);
        rental_api::schedule(doc, &schedule_targets)?;
        Ok(json!({"ok":true}))
    })?;
    Ok(Json(value))
}
async fn remove(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        let plan = doc.customer_plans.iter().find(|p| p.id == id).context("套餐不存在")?.clone();
        let key = plan.key();
        let at = now();
        let active_lease = doc.rentals.get(&key).is_some_and(|r| r.started_at > 0 && (r.expires_at == 0 || r.expires_at > at));
        let has_enabled_line = plan.lines().any(|lid| doc.lines.iter().any(|l| l.id == *lid && l.enabled));
        ensure!(
            !active_lease || !has_enabled_line,
            "在用套餐仍有启用中的线路；请先停用关联线路或等待到期后再删除套餐"
        );
        ensure!(
            !doc.rental_orders.iter().any(|o| o.confirmed_at == 0 && o.cancelled_at == 0 && o.items.iter().any(|i| i.line_id == key)),
            "套餐仍有待确认订单，请先取消或处理订单"
        );
        let member_lines: Vec<String> = plan
            .lines()
            .filter(|lid| doc.lines.iter().any(|l| l.id == **lid))
            .cloned()
            .collect();
        let prefix = format!("member:{}:", plan.id);
        for line in &mut doc.lines {
            if member_lines.iter().any(|lid| lid == &line.id) {
                line.enabled = false;
            }
        }
        doc.customer_plans.retain(|p| p.id != id);
        doc.rentals.remove(&key);
        doc.customer_query_tokens.retain(|k, _| k != &key && !k.starts_with(&prefix));
        if !member_lines.is_empty() {
            rental_api::schedule(doc, &member_lines)?;
        }
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deleting_expired_plan_disables_lines_instead_of_releasing_free_access() {
        let dir = tempfile::tempdir().unwrap();
        let app = fleet_api::tests::app(dir.path());
        let plan_id = id();
        let member_id = id();
        let customer_id = id();
        {
            let mut store = app.store.lock().unwrap();
            let doc = &mut store.doc;
            doc.customers.push(Customer { id: customer_id.clone(), name: "客户".into(), contact: String::new(), notes: String::new() });
            doc.nodes.push(serde_json::from_value(json!({"id":"node","name":"Node","host":"127.0.0.1","token_hash":"h","reality":Reality::new(String::new(), String::new())})).unwrap());
            let line: Line = serde_json::from_value(json!({"id":"line-1","name":"L1","hops":[{"node_id":"node","port":24443,"uuid":id()}]})).unwrap();
            doc.lines.push(line.clone());
            doc.applied_lines.push(line);
            doc.applied_nodes = doc.nodes.clone();
            doc.nodes[0].desired = Some(Desired { revision: "r1".into(), config: json!({"_chain":[{"id":"line-1","kind":"vless","port":24443,"backend":50001,"enabled":true,"meter":{"port":50002,"backend":50003,"password":"p"}}]}) });
            let plan = plans::Plan {
                id: plan_id.clone(),
                name: "已到期套餐".into(),
                customer_id,
                monthly_gib: 10.0,
                members: vec![plans::Member { id: member_id, name: "成员".into(), line_ids: vec!["line-1".into()], monthly_gib: None }],
            };
            doc.rentals.insert(plan.key(), Rental { customer_id: plan.customer_id.clone(), started_at: 100, expires_at: 200, ..Default::default() });
            doc.customer_plans.push(plan);
            store.save().unwrap();
        }
        let _ = remove(State(app.clone()), Path(plan_id)).await.unwrap();
        let store = app.store.lock().unwrap();
        assert!(store.doc.customer_plans.is_empty());
        assert!(!store.doc.lines[0].enabled, "删除已到期套餐必须停用其关联线路，防止退化为免费自用代理");
        assert_eq!(store.doc.nodes[0].desired.as_ref().unwrap().config["_chain"][0]["enabled"], false);
    }
}
