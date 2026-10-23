use super::*;
use chain_panel::plans;

pub fn routes(router: Router<App>) -> Router<App> {
    router.route("/api/customer-plans", get(view).post(save))
        .route("/api/customer-plans/{id}/query-link", post(plan_link))
        .route("/api/customer-plans/{id}/members/{member}/query-link", post(member_link))
}
async fn plan_link(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> { query_link(app, id, None) }
async fn member_link(State(app): State<App>, Path((id, member)): Path<(String, String)>) -> ApiResult<Json<Value>> { query_link(app, id, Some(member)) }
fn query_link(app: App, id: String, member_id: Option<String>) -> ApiResult<Json<Value>> {
    let token = secret();
    edit(&app, |doc| {
        let plan = doc.customer_plans.iter().find(|p| p.id == id).context("套餐不存在")?;
        let key = if let Some(member_id) = member_id {
            let member = plan.members.iter().find(|m| m.id == member_id).context("套餐成员不存在")?;
            plans::member_key(plan, member)
        } else { plan.key() };
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
        let previous = doc.customer_plans.iter().find(|p| p.id == input.plan.id);
        anyhow::ensure!(creating || previous.is_some(), "套餐不存在");
        plans::validate(doc, &input.plan, previous)?;
        anyhow::ensure!([input.prices.monthly, input.prices.quarterly, input.prices.yearly].iter().flatten().all(|p| *p <= chain_panel::rental::MAX_PRICE), "套餐报价超出有效范围");
        let key = input.plan.key();
        let rental = doc.rentals.entry(key.clone()).or_default();
        rental.customer_id = input.plan.customer_id.clone();
        rental.prices = input.prices;
        doc.customer_plans.retain(|p| p.id != input.plan.id);
        doc.customer_plans.push(input.plan);
        rental_api::schedule(doc, &[key])?;
        Ok(json!({"ok":true}))
    })?;
    Ok(Json(value))
}
