use super::*;
use chain_panel::rental;
use serde::Serialize;

macro_rules! ensure {($condition:expr,$($message:tt)*)=>{if !$condition {return Err(anyhow::anyhow!($($message)*).into());}};}

#[derive(Deserialize)]
pub struct CustomerInput {
    id: Option<String>,
    name: String,
    #[serde(default)]
    contact: String,
    #[serde(default)]
    notes: String,
}
pub async fn save_customer(
    State(app): State<App>,
    Json(input): Json<CustomerInput>,
) -> ApiResult<Json<Value>> {
    let name = input.name.trim();
    ensure!(
        !name.is_empty() && name.len() <= 120,
        "客户称呼须为 1–120 字节"
    );
    ensure!(
        input.contact.len() <= 500 && input.notes.len() <= 4000,
        "客户联系方式或备注过长"
    );
    let customer = edit(&app, |doc| {
        ensure!(
            input
                .id
                .as_ref()
                .is_none_or(|id| doc.customers.iter().any(|c| &c.id == id)),
            "客户不存在"
        );
        let customer = Customer {
            id: input.id.unwrap_or_else(id),
            name: name.into(),
            contact: input.contact,
            notes: input.notes,
        };
        doc.customers.retain(|c| c.id != customer.id);
        doc.customers.push(customer.clone());
        Ok(customer)
    })?;
    Ok(Json(json!(customer)))
}
pub async fn delete_customer(
    State(app): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        ensure!(doc.customers.iter().any(|c| c.id == id), "客户不存在");
        ensure!(
            !doc.rentals.values().any(|r| r.customer_id == id),
            "客户仍有关联代理，请先处理这些代理"
        );
        doc.customers.retain(|c| c.id != id);
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize)]
pub struct RentalInput {
    customer_id: String,
    prices: Prices,
}
pub async fn save_rental(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<RentalInput>,
) -> ApiResult<Json<Value>> {
    let value = edit(&app, |doc| {
        ensure!(
            doc.lines
                .iter()
                .any(|l| l.id == id && l.kind == Kind::Vless),
            "代理不存在"
        );
        ensure!(chain_panel::plans::for_line(doc, &id).is_none(), "该线路属于共享套餐，请在客户套餐中管理权益和报价");
        ensure!(
            doc.customers.iter().any(|c| c.id == input.customer_id),
            "请选择已有客户"
        );
        ensure!(
            [
                input.prices.monthly,
                input.prices.quarterly,
                input.prices.yearly
            ]
            .iter()
            .flatten()
            .all(|v| *v <= rental::MAX_PRICE),
            "报价超出有效金额范围"
        );
        let r = doc.rentals.entry(id.clone()).or_default();
        ensure!(
            r.started_at == 0 || r.customer_id.is_empty() || r.customer_id == input.customer_id,
            "已开通代理不能直接更换客户，请为新客户创建代理"
        );
        r.customer_id = input.customer_id;
        r.prices = input.prices;
        schedule(doc, std::slice::from_ref(&id))?;
        Ok(results(doc, std::slice::from_ref(&id)))
    })?;
    Ok(Json(value))
}

#[derive(Deserialize)]
pub struct ContactInput {
    renewal_contact: String,
}
pub async fn save_contact(
    State(app): State<App>,
    Json(input): Json<ContactInput>,
) -> ApiResult<Json<Value>> {
    ensure!(input.renewal_contact.len() <= 2000, "续费联系方式过长");
    edit(&app, |doc| {
        doc.renewal_contact = input.renewal_contact.trim().into();
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize, Serialize)]
pub struct RenewalInput {
    line_ids: Vec<String>,
    term: String,
    #[serde(default)]
    amounts: BTreeMap<String, u64>,
    #[serde(default)]
    note: String,
    request_id: String,
}
fn selection(doc: &Document, ids: &[String]) -> Result<()> {
    ensure!(!ids.is_empty() && ids.len() <= 500, "请选择 1–500 项代理或套餐");
    ensure!(
        ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
        "代理选择有重复"
    );
    ensure!(
        ids.iter().all(|id| doc.lines.iter().any(|l| l.id == *id && l.kind == Kind::Vless)
            || doc.customer_plans.iter().any(|plan| plan.key() == *id)),
        "所选代理或套餐不存在"
    );
    Ok(())
}

fn billing_target<'a>(doc: &'a Document, id: &str) -> Result<(&'a str, &'a Rental)> {
    ensure!(id.starts_with("plan:") || chain_panel::plans::for_line(doc, id).is_none(),
        "套餐成员线路不能独立收款或续费，请选择所属共享套餐");
    let lease = doc.rentals.get(id).context("请先设置客户归属和续费报价")?;
    if id.starts_with("plan:") {
        let plan = doc.customer_plans.iter().find(|p| p.key() == id).context("共享套餐不存在")?;
        ensure!(plan.customer_id == lease.customer_id, "套餐客户与租赁归属不一致，请先核对");
        Ok((&plan.name, lease))
    } else {
        let line = doc.lines.iter().find(|l| l.id == id && l.kind == Kind::Vless).context("代理不存在")?;
        Ok((&line.name, lease))
    }
}

fn target_lines(doc: &Document, ids: &[String]) -> Result<Vec<String>> {
    let mut lines = BTreeSet::new();
    for id in ids {
        if id.starts_with("plan:") {
            let plan = doc.customer_plans.iter().find(|p| p.key() == *id).context("共享套餐不存在")?;
            lines.extend(plan.lines().cloned());
        } else { lines.insert(id.clone()); }
    }
    ensure!(lines.iter().all(|id| doc.lines.iter().any(|line| line.id == *id)), "关联代理已删除，请核对服务范围");
    Ok(lines.into_iter().collect())
}
fn renew_selected(doc: &mut Document, input: &RenewalInput, at: u64) -> Result<Vec<Payment>> {
    ensure!(
        !input.request_id.is_empty() && input.request_id.len() <= 120,
        "续费请求标识无效"
    );
    ensure!(input.note.len() <= 2000, "续费备注过长");
    let hash = digest(&serde_json::to_string(input)?);
    let old: Vec<_> = doc
        .payments
        .iter()
        .filter(|p| p.request_id == input.request_id)
        .cloned()
        .collect();
    if !old.is_empty() {
        ensure!(
            old.iter().all(|p| p.request_hash == hash),
            "该续费请求标识已用于另一笔操作"
        );
        return Ok(old);
    }
    selection(doc, &input.line_ids)?;
    ensure!(
        input.amounts.keys().all(|id| input.line_ids.contains(id)),
        "实收金额包含未选中的代理"
    );
    let months = rental::term_months(&input.term)?;
    let mut records = vec![];
    let mut updates = vec![];
    for line_id in &input.line_ids {
        let (name, current) = billing_target(doc, line_id)?;
        let mut lease = current.clone();
        let customer = doc
            .customers
            .iter()
            .find(|c| c.id == lease.customer_id)
            .context("客户不存在")?;
        let quoted = rental::price(&lease.prices, &input.term)?;
        let amount = input.amounts.get(line_id).copied().unwrap_or(quoted);
        ensure!(amount <= rental::MAX_PRICE, "实收金额超出有效范围");
        let previous = rental::renew(&mut lease, &input.term, at)?;
        records.push(Payment {
            id: id(),
            request_id: input.request_id.clone(),
            request_hash: hash.clone(),
            line_id: line_id.clone(),
            customer_id: customer.id.clone(),
            line_name: name.into(),
            customer_name: customer.name.clone(),
            term: input.term.clone(),
            months,
            amount_fen: amount,
            created: at,
            previous_expires_at: previous,
            expires_at: lease.expires_at,
            note: input.note.clone(),
        });
        updates.push((line_id.clone(), lease));
    }
    for (id, lease) in updates {
        doc.rentals.insert(id, lease);
    }
    doc.payments.extend(records.clone());
    schedule(doc, &input.line_ids)?;
    Ok(records)
}
pub async fn renew(
    State(app): State<App>,
    headers: HeaderMap,
    axum::Extension(actor): axum::Extension<chain_panel::security::Actor>,
    Json(input): Json<RenewalInput>,
) -> ApiResult<Json<Value>> {
    let value = edit(&app, |doc| {
        let payments = renew_ordered(doc, &input, now(), &billing_session(&headers), Some(&actor))?;
        let mut value = results(doc, &input.line_ids);
        value["payments"] = json!(payments);
        Ok(value)
    })?;
    Ok(Json(value))
}

fn billing_session(headers: &HeaderMap) -> String {
    digest(&format!("rental-ui:{}", cookie(headers)))
}

pub async fn context(headers: HeaderMap) -> Json<Value> {
    Json(json!({"scope":billing_session(&headers)}))
}

fn pending_order(doc: &mut Document, input: &RenewalInput, at: u64, session: &str, actor: Option<&chain_panel::security::Actor>) -> Result<RentalOrder> {
    ensure!(!input.request_id.is_empty() && input.request_id.len() <= 120, "订单请求标识无效");
    ensure!(input.note.len() <= 2000, "订单备注过长");
    let hash = digest(&serde_json::to_string(input)?);
    if let Some(order) = doc.rental_orders.iter().find(|o| o.request_id == input.request_id) {
        ensure!(order.request_hash == hash, "该请求已用于另一张订单，请核对原订单");
        return Ok(order.clone());
    }
    ensure!(!doc.payments.iter().any(|p| p.request_id == input.request_id), "该请求已有历史收款，请核对收款记录");
    selection(doc, &input.line_ids)?;
    ensure!(input.amounts.keys().all(|id| input.line_ids.contains(id)), "金额包含未选中的代理");
    let mut items = Vec::new();
    for line_id in &input.line_ids {
        let (name, lease) = billing_target(doc, line_id)?;
        let customer = doc.customers.iter().find(|c| c.id == lease.customer_id).context("客户不存在")?;
        let quoted_fen = rental::price(&lease.prices, &input.term)?;
        let amount_fen = input.amounts.get(line_id).copied().unwrap_or(quoted_fen);
        ensure!(amount_fen <= rental::MAX_PRICE, "订单金额超出有效范围");
        items.push(RentalOrderItem { line_id: line_id.clone(), customer_id: customer.id.clone(),
            line_name: name.into(), customer_name: customer.name.clone(), quoted_fen, amount_fen,
            started_at: lease.started_at, expires_at: lease.expires_at });
    }
    let order = RentalOrder { id: id(), request_id: input.request_id.clone(), request_hash: hash,
        created: at, confirmed_at: 0, cancelled_at: 0, term: input.term.clone(), amounts: input.amounts.clone(),
        note: input.note.clone(), items, created_session: session.into(), confirmed_session: String::new(), cancelled_session: String::new(),
        created_by: actor.cloned(), confirmed_by: None, cancelled_by: None };
    doc.rental_orders.push(order.clone());
    Ok(order)
}

fn renew_ordered(doc: &mut Document, input: &RenewalInput, at: u64, session: &str, actor: Option<&chain_panel::security::Actor>) -> Result<Vec<Payment>> {
    // Historical receipts keep their original identity; a retry must not invent an old order.
    if doc.payments.iter().any(|p| p.request_id == input.request_id) {
        return renew_selected(doc, input, at);
    }
    let order = pending_order(doc, input, at, session, actor)?;
    ensure!(order.cancelled_at == 0, "订单已取消，不能确认收款");
    for item in &order.items {
        let (_, lease) = billing_target(doc, &item.line_id)?;
        ensure!(lease.customer_id == item.customer_id, "订单客户归属已变化，请取消订单后重新核对");
        ensure!(rental::price(&lease.prices, &order.term)? == item.quoted_fen, "订单报价已变化，请取消订单后重新核对");
        ensure!(lease.started_at == item.started_at && lease.expires_at == item.expires_at,
            "订单权益已变化，请先核对是否已收款或续费，再取消重建");
    }
    let payments = renew_selected(doc, input, at)?;
    let saved = doc.rental_orders.iter_mut().find(|o| o.id == order.id).unwrap();
    saved.confirmed_at = at;
    saved.confirmed_session = session.into();
    saved.confirmed_by = actor.cloned();
    Ok(payments)
}

pub async fn create_order(State(app): State<App>, headers: HeaderMap, axum::Extension(actor): axum::Extension<chain_panel::security::Actor>, Json(input): Json<RenewalInput>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(edit(&app, |doc| pending_order(doc, &input, now(), &billing_session(&headers), Some(&actor)))?)))
}

pub async fn order_by_request(State(app): State<App>, Path(request_id): Path<String>) -> ApiResult<Json<Value>> {
    let store = app.store.lock().unwrap();
    let order = store.doc.rental_orders.iter().find(|o| o.request_id == request_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "尚未找到该请求的订单，请核对订单列表"))?;
    Ok(Json(json!(order)))
}

pub async fn confirm_order(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap, axum::Extension(actor): axum::Extension<chain_panel::security::Actor>) -> ApiResult<Json<Value>> {
    let value = edit(&app, |doc| {
        let order = doc.rental_orders.iter().find(|o| o.id == id).context("订单不存在")?.clone();
        let input = RenewalInput { line_ids: order.items.iter().map(|i| i.line_id.clone()).collect(),
            term: order.term, amounts: order.amounts, note: order.note, request_id: order.request_id };
        let payments = renew_ordered(doc, &input, now(), &billing_session(&headers), Some(&actor))?;
        Ok(json!({"ok":true,"payments":payments}))
    })?;
    Ok(Json(value))
}

pub async fn cancel_order(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap, axum::Extension(actor): axum::Extension<chain_panel::security::Actor>) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        let order = doc.rental_orders.iter_mut().find(|o| o.id == id).context("订单不存在")?;
        ensure!(order.confirmed_at == 0, "已收款订单不能取消，请登记退款或金额纠错");
        if order.cancelled_at == 0 { order.cancelled_at = now(); order.cancelled_session = billing_session(&headers); order.cancelled_by = Some(actor.clone()); }
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize, Serialize)]
pub struct AdjustmentInput {
    payment_id: String,
    kind: String,
    amount_fen: u64,
    expected_net_fen: u64,
    note: String,
    #[serde(default)]
    disable_line: bool,
    #[serde(default)]
    expected_line_ids: Option<Vec<String>>,
    request_id: String,
}

fn adjust_payment(doc: &mut Document, input: &AdjustmentInput, at: u64, session: &str, actor: Option<&chain_panel::security::Actor>) -> Result<PaymentAdjustment> {
    ensure!(!input.request_id.is_empty() && input.request_id.len() <= 120, "调整请求标识无效");
    ensure!(!input.note.trim().is_empty() && input.note.len() <= 2000, "请填写不超过 2000 字节的原因或凭据");
    let hash = digest(&serde_json::to_string(input)?);
    if let Some(old) = doc.payment_adjustments.iter().find(|a| a.request_id == input.request_id) {
        ensure!(old.request_hash == hash, "该请求已登记另一笔调整，请核对记录");
        return Ok(old.clone());
    }
    let payment = doc.payments.iter().find(|p| p.id == input.payment_id).context("收款记录不存在")?.clone();
    let mut gross = i64::try_from(payment.amount_fen)?;
    let mut net = gross;
    for adjustment in doc.payment_adjustments.iter().filter(|a| a.payment_id == payment.id) {
        net = net.checked_add(adjustment.delta_fen).context("累计金额溢出")?;
        if adjustment.kind == "correction" { gross = gross.checked_add(adjustment.delta_fen).context("累计金额溢出")?; }
    }
    ensure!(net >= 0 && net as u64 == input.expected_net_fen, "可退金额已变化，请刷新并核对最新记录");
    ensure!(input.amount_fen <= rental::MAX_PRICE, "调整金额超出有效范围");
    let amount = i64::try_from(input.amount_fen)?;
    let delta = match input.kind.as_str() {
        "refund" => {
            ensure!(amount > 0 && amount <= net, "退款须大于零且不能超过可退金额");
            -amount
        }
        "correction" => {
            ensure!(!input.disable_line, "金额纠错不调整代理权益");
            let delta = amount.checked_sub(gross).context("纠错金额溢出")?;
            ensure!(delta != 0, "正确实收金额与当前记录相同");
            ensure!(net.checked_add(delta).is_some_and(|v| v >= 0), "正确实收不能低于已退款总额");
            delta
        }
        _ => anyhow::bail!("请选择人工退款或金额纠错"),
    };
    let affected_line_ids = if input.disable_line { target_lines(doc, std::slice::from_ref(&payment.line_id))? } else { vec![] };
    if input.disable_line {
        if payment.line_id.starts_with("plan:") || input.expected_line_ids.is_some() {
            let expected: BTreeSet<_> = input.expected_line_ids.as_ref().context("请明确确认套餐全部成员的停用范围")?.iter().cloned().collect();
            ensure!(expected == affected_line_ids.iter().cloned().collect(), "停用范围已变化，请重新核对套餐成员和线路");
        }
        for line in doc.lines.iter_mut().filter(|l| affected_line_ids.contains(&l.id)) { line.enabled = false; }
        schedule(doc, std::slice::from_ref(&payment.line_id))?;
    }
    let adjustment = PaymentAdjustment { id: id(), payment_id: payment.id, request_id: input.request_id.clone(),
        request_hash: hash, kind: input.kind.clone(), delta_fen: delta, created: at, note: input.note.trim().into(),
        disable_line: input.disable_line, affected_line_ids, session: session.into(), operator: actor.cloned() };
    doc.payment_adjustments.push(adjustment.clone());
    Ok(adjustment)
}

pub async fn adjust(State(app): State<App>, headers: HeaderMap, axum::Extension(actor): axum::Extension<chain_panel::security::Actor>, Json(input): Json<AdjustmentInput>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(edit(&app, |doc| adjust_payment(doc, &input, now(), &billing_session(&headers), Some(&actor)))?)))
}

pub async fn adjustment_by_request(State(app): State<App>, Path(request_id): Path<String>) -> ApiResult<Json<Value>> {
    let store = app.store.lock().unwrap();
    let adjustment = store.doc.payment_adjustments.iter().find(|a| a.request_id == request_id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "尚未找到该请求的调整，请核对流水后再操作"))?;
    Ok(Json(json!(adjustment)))
}

fn results(doc: &Document, ids: &[String]) -> Value {
    json!({"ok":true,"results":ids.iter().map(|id| {
        let status = if let Some(plan) = doc.customer_plans.iter().find(|p| p.key() == *id) {
            let states: Vec<_> = plan.lines().map(|line| doc.lines.iter().find(|l| l.id == *line)
                .map(|l| rental::sync_status(doc,l)).unwrap_or("pending")).collect();
            if states.contains(&"error") { "error" } else if states.contains(&"unpublished") { "unpublished" }
            else if states.is_empty() || states.contains(&"pending") { "pending" } else { "synced" }
        } else { doc.lines.iter().find(|l| l.id == *id).map(|l| rental::sync_status(doc,l)).unwrap_or("pending") };
        json!({"id":id,"sync_status":status})
    }).collect::<Vec<_>>()})
}

// Operating fields are patched into the deployed configuration; unrelated drafts stay drafts.
pub(super) fn schedule(doc: &mut Document, ids: &[String]) -> Result<()> {
    let mut changed = BTreeSet::new();
    for id in &target_lines(doc, ids)? {
        let line = doc
            .lines
            .iter()
            .find(|l| l.id == *id)
            .context("代理不存在")?
            .clone();
        let Some(applied) = doc.applied_lines.iter_mut().find(|l| l.id == *id) else {
            continue;
        };
        applied.enabled = line.enabled;
        applied.limits = line.limits.clone();
        let node_id = applied
            .hops
            .first()
            .context("已应用代理缺少入口")?
            .node_id
            .clone();
        let active_line = applied.clone();
        let lease = chain_panel::plans::lease(doc, id).map(Rental::policy);
        let resource_ids = chain_panel::resources::pools(doc, &active_line);
        let node = doc
            .nodes
            .iter_mut()
            .find(|n| n.id == node_id)
            .context("入口节点不存在")?;
        let desired = node.desired.as_mut().context("入口节点没有可更新的配置")?;
        let spec = desired.config["_chain"]
            .as_array_mut()
            .context("入口节点配置缺少运营控制")?
            .iter_mut()
            .find(|s| s["id"] == *id)
            .context("入口节点未部署所选代理")?;
        let before = spec.clone();
        spec["enabled"] = json!(line.enabled);
        spec["limits"] = json!(line.limits);
        spec["lease"] = json!(lease);
        spec["resource_ids"] = json!(resource_ids);
        if *spec != before {
            changed.insert(node_id);
        }
    }
    for node in &mut doc.nodes {
        if changed.contains(&node.id) {
            node.desired.as_mut().unwrap().revision = format!("operating-{}", id());
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct OperationInput {
    line_ids: Vec<String>,
    action: String,
    limits: Option<Limits>,
}
fn remove_selected(doc: &mut Document, ids: &[String]) -> Result<()> {
    selection(doc, ids)?;
    ensure!(ids.iter().all(|id| !id.starts_with("plan:") && chain_panel::plans::for_line(doc, id).is_none()),
        "套餐及成员线路需保留历史归属，请使用手动停用");
    ensure!(
        !doc.lines
            .iter()
            .chain(doc.applied_lines.iter())
            .any(|line| !ids.contains(&line.id)
                && line.proxy_chain().iter().any(|key| key
                    .strip_prefix("line:")
                    .is_some_and(|id| ids.iter().any(|selected| selected == id)))),
        "所选代理仍被其他链式代理引用，请先移除引用并应用"
    );
    let mut active = Document {
        nodes: doc.applied_nodes.clone(),
        lines: doc
            .applied_lines
            .iter()
            .filter(|l| !ids.contains(&l.id))
            .cloned()
            .collect(),
        externals: doc.applied_externals.clone(),
        rentals: doc.rentals.clone(),
        customer_plans: doc.customer_plans.clone(),
        customer_ledgers: doc.customer_ledgers.clone(),
        resources: doc.resources.clone(),
        rule_sources: doc.rule_sources.clone(),
        access_policies: doc.access_policies.clone(),
        ..Default::default()
    };
    for id in ids {
        active.rentals.remove(id);
    }
    let configs = compile(&active)?;
    for node in &mut doc.nodes {
        if let Some(config) = configs.get(&node.id) {
            if node
                .desired
                .as_ref()
                .is_some_and(|old| old.config != *config)
            {
                node.desired = Some(Desired {
                    revision: format!("remove-{}", id()),
                    config: config.clone(),
                });
            }
        }
    }
    doc.lines.retain(|l| !ids.contains(&l.id));
    doc.applied_lines.retain(|l| !ids.contains(&l.id));
    for id in ids {
        doc.rentals.remove(id);
        doc.checks.remove(id);
    }
    for s in &mut doc.subscriptions {
        s.line_ids.retain(|id| !ids.contains(id));
    }
    Ok(())
}

fn unlimited(doc: &mut Document, ids: &[String], at: u64) -> Result<()> {
    ensure!(ids.iter().all(|id| !id.starts_with("plan:") && chain_panel::plans::for_line(doc, id).is_none()),
        "套餐成员线路不能单独取消到期限制，请在共享套餐中管理权益");
    for id in ids {
        let lease = doc.rentals.entry(id.clone()).or_default();
        if lease.started_at == 0 {
            lease.started_at = at;
        }
        lease.expires_at = 0;
        lease.paid_months = 0;
    }
    schedule(doc, ids)
}
pub async fn operate(
    State(app): State<App>,
    Json(input): Json<OperationInput>,
) -> ApiResult<Json<Value>> {
    let value = edit(&app, |doc| {
        selection(doc, &input.line_ids)?;
        ensure!(input.line_ids.iter().all(|id| !id.starts_with("plan:")), "此操作只接受代理线路，请使用套餐管理入口");
        match input.action.as_str() {
            "enable" | "disable" => {
                for line in doc
                    .lines
                    .iter_mut()
                    .filter(|l| input.line_ids.contains(&l.id))
                {
                    line.enabled = input.action == "enable";
                }
                schedule(doc, &input.line_ids)?;
            }
            "limits" => {
                let limits = input.limits.as_ref().context("请填写额度和限速")?;
                for line in doc
                    .lines
                    .iter_mut()
                    .filter(|l| input.line_ids.contains(&l.id))
                {
                    line.limits = limits.clone();
                    chain_panel::access::validate_line(line)?;
                }
                schedule(doc, &input.line_ids)?;
            }
            "delete" => remove_selected(doc, &input.line_ids)?,
            "unlimited" => unlimited(doc, &input.line_ids, now())?,
            _ => anyhow::bail!("不支持的批量操作"),
        }
        Ok(results(doc, &input.line_ids))
    })?;
    Ok(Json(value))
}

pub async fn delete_one(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    edit(&app, |doc| remove_selected(doc, &[id]))?;
    Ok(Json(json!({"ok":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Document {
        let mut doc = Document::default();
        doc.customers.push(Customer {
            id: "customer".into(),
            name: "客户".into(),
            contact: "private".into(),
            notes: "internal".into(),
        });
        doc.lines.push(
            serde_json::from_value(json!({"id":"a","name":"代理 A","kind":"vless","hops":[]}))
                .unwrap(),
        );
        doc.rentals.insert(
            "a".into(),
            Rental {
                customer_id: "customer".into(),
                prices: Prices {
                    monthly: Some(2000),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        doc
    }
    fn input() -> RenewalInput {
        RenewalInput {
            line_ids: vec!["a".into()],
            term: "monthly".into(),
            amounts: BTreeMap::new(),
            note: "线下收款".into(),
            request_id: "request".into(),
        }
    }
    #[test]
    fn renewal_retries_do_not_duplicate_money_or_time() {
        let mut doc = fixture();
        let p = renew_selected(&mut doc, &input(), 1_800_000_000).unwrap();
        let expires = doc.rentals["a"].expires_at;
        let again = renew_selected(&mut doc, &input(), 1_800_000_100).unwrap();
        assert_eq!(p[0].id, again[0].id);
        assert_eq!(doc.payments.len(), 1);
        assert_eq!(doc.rentals["a"].expires_at, expires);
        let mut wrong = input();
        wrong.note = "another".into();
        assert!(renew_selected(&mut doc, &wrong, 1_800_000_100).is_err());
    }
    #[test]
    fn invalid_batch_cannot_partly_renew() {
        let mut doc = fixture();
        let mut second = doc.lines[0].clone();
        second.id = "b".into();
        doc.lines.push(second);
        let mut input = input();
        input.line_ids.push("b".into());
        assert!(renew_selected(&mut doc, &input, 1_800_000_000).is_err());
        assert_eq!(doc.rentals["a"].started_at, 0);
        assert!(doc.payments.is_empty());
    }
    #[test]
    fn disabled_price_and_invalid_amount_are_rejected() {
        let mut doc = fixture();
        let mut value = input();
        value.term = "yearly".into();
        assert!(renew_selected(&mut doc, &value, 1_800_000_000).is_err());
        value.term = "monthly".into();
        value.amounts.insert("a".into(), rental::MAX_PRICE + 1);
        assert!(renew_selected(&mut doc, &value, 1_800_000_000).is_err());
        assert!(doc.payments.is_empty());
    }

    #[test]
    fn unlimited_keeps_quotas_and_disabled_state_without_payment() {
        let mut doc = fixture();
        doc.rentals.clear();
        doc.lines[0].enabled = false;
        doc.lines[0].limits.monthly_gib = Some(10.0);
        unlimited(&mut doc, &["a".into()], 1_800_000_000).unwrap();
        let lease = &doc.rentals["a"];
        assert_eq!((lease.started_at, lease.expires_at), (1_800_000_000, 0));
        assert!(!doc.lines[0].enabled);
        assert_eq!(doc.lines[0].limits.monthly_gib, Some(10.0));
        assert!(doc.payments.is_empty());
    }

    #[test]
    fn pending_order_does_not_grant_service_and_confirmation_is_idempotent() {
        let mut doc = fixture();
        let actor = chain_panel::security::Actor { id: "operator".into(), name: "财务".into(), role: chain_panel::security::Role::Finance };
        let order = pending_order(&mut doc, &input(), 1_800_000_000, "session-a", Some(&actor)).unwrap();
        let again = pending_order(&mut doc, &input(), 1_800_000_001, "session-a", Some(&actor)).unwrap();
        assert_eq!(order.id, again.id);
        assert_eq!(doc.rental_orders.len(), 1);
        assert_eq!(doc.rentals["a"].started_at, 0);
        assert!(doc.payments.is_empty());
        let paid = renew_ordered(&mut doc, &input(), 1_800_000_100, "session-b", Some(&actor)).unwrap();
        let lease = serde_json::to_value(&doc.rentals["a"]).unwrap();
        let retried = renew_ordered(&mut doc, &input(), 1_800_000_200, "session-c", None).unwrap();
        assert_eq!(paid[0].id, retried[0].id);
        assert_eq!(doc.payments.len(), 1);
        assert_eq!(serde_json::to_value(&doc.rentals["a"]).unwrap(), lease);
        assert_eq!(doc.rental_orders[0].confirmed_at, 1_800_000_100);
        assert_eq!(doc.rental_orders[0].confirmed_session, "session-b");
        assert_eq!(doc.rental_orders[0].confirmed_by.as_ref().unwrap().id, actor.id);
        assert_eq!(doc.rental_orders[0].created_by.as_ref().unwrap().name, actor.name);
    }

    #[test]
    fn pending_order_rejects_customer_price_and_entitlement_changes() {
        for change in ["customer", "price", "term", "cancel"] {
            let mut doc = fixture();
            pending_order(&mut doc, &input(), 1_800_000_000, "session", None).unwrap();
            match change {
                "customer" => doc.rentals.get_mut("a").unwrap().customer_id = "another".into(),
                "price" => doc.rentals.get_mut("a").unwrap().prices.monthly = Some(3000),
                "term" => doc.rentals.get_mut("a").unwrap().expires_at = 1_900_000_000,
                _ => doc.rental_orders[0].cancelled_at = 1_800_000_001,
            }
            let before = serde_json::to_value(&doc.rentals).unwrap();
            assert!(renew_ordered(&mut doc, &input(), 1_800_000_100, "session", None).is_err(), "{change}");
            assert!(doc.payments.is_empty());
            assert_eq!(serde_json::to_value(&doc.rentals).unwrap(), before);
        }
    }

    #[test]
    fn legacy_data_keeps_receipts_credentials_and_rental_terms_without_invented_orders() {
        let mut doc = fixture();
        doc.rentals.get_mut("a").unwrap().query_token_hash = "original-query-hash".into();
        renew_selected(&mut doc, &input(), 1_800_000_000).unwrap();
        doc.lines[0].enabled = false;
        doc.lines[0].limits = Limits { monthly_gib: Some(100.0), upload_mbps: Some(20.0), download_mbps: Some(80.0) };
        doc.lines[0].access.password = "original-proxy-password".into();
        doc.lines[0].hops = serde_json::from_value(json!([{"node_id":"node","port":24443,"uuid":"00000000-0000-4000-8000-000000000001"}])).unwrap();
        let mut node: Node = serde_json::from_value(json!({"id":"node","name":"原节点","host":"proxy.example",
            "token_hash":"original-agent-hash","reality":{"target":"origin.example:443","server_name":"origin.example",
                "private_key":"original-private","public_key":"original-public","short_id":"abcd"}})).unwrap();
        node.report.traffic.insert("a".into(), Traffic { upload: 111, download: 222, charged: Some(777),
            cycle_start: doc.rentals["a"].started_at, epoch: "original-epoch".into(), blocked: true, ..Default::default() });
        doc.nodes.push(node.clone());
        doc.applied_nodes.push(node);
        doc.applied_lines = doc.lines.clone();
        doc.subscriptions.push(Subscription { id: "original-subscription".into(), name: "旧订阅".into(),
            token_hash: "original-subscription-hash".into(), line_ids: vec!["a".into()], created: 1_800_000_000 });
        let before = serde_json::to_value(&doc).unwrap();
        let mut legacy = before.clone();
        legacy.as_object_mut().unwrap().remove("rental_orders");
        legacy.as_object_mut().unwrap().remove("payment_adjustments");
        legacy.as_object_mut().unwrap().remove("customer_plans");
        legacy.as_object_mut().unwrap().remove("customer_ledgers");
        legacy.as_object_mut().unwrap().remove("customer_query_tokens");
        legacy.as_object_mut().unwrap().remove("operator_members");
        legacy.as_object_mut().unwrap().remove("operator_audit");
        let mut loaded: Document = serde_json::from_value(legacy).unwrap();
        assert!(loaded.rental_orders.is_empty());
        assert!(loaded.payment_adjustments.is_empty());
        assert!(loaded.customer_plans.is_empty());
        assert!(loaded.customer_ledgers.is_empty());
        assert!(loaded.customer_query_tokens.is_empty());
        assert!(loaded.operator_members.is_empty());
        assert!(loaded.operator_audit.is_empty());
        renew_ordered(&mut loaded, &input(), 1_800_000_100, "new-session", None).unwrap();
        assert!(loaded.rental_orders.is_empty());
        let after = serde_json::to_value(&loaded).unwrap();
        for key in ["payments", "rentals", "lines", "applied_lines", "nodes", "applied_nodes", "subscriptions", "customers"] {
            assert_eq!(before[key], after[key], "{key}");
        }
        assert_eq!(loaded.nodes[0].report.traffic["a"].charged, Some(777));
        assert!(!loaded.lines[0].enabled);
        assert_eq!(before, after);
    }

    fn refund(payment: &Payment) -> AdjustmentInput {
        AdjustmentInput { payment_id: payment.id.clone(), kind: "refund".into(), amount_fen: 500,
            expected_net_fen: payment.amount_fen, note: "线下退款凭据".into(), disable_line: false, expected_line_ids: None, request_id: "refund-1".into() }
    }

    #[test]
    fn refunds_are_idempotent_bounded_and_preserve_original_receipts_and_rights() {
        let mut doc = fixture();
        let payment = renew_ordered(&mut doc, &input(), 1_800_000_000, "session", None).unwrap().remove(0);
        let before = serde_json::to_value(&doc.rentals).unwrap();
        let request = refund(&payment);
        let first = adjust_payment(&mut doc, &request, 1_800_000_100, "session", None).unwrap();
        let retry = adjust_payment(&mut doc, &request, 1_800_000_200, "session", None).unwrap();
        assert_eq!(first.id, retry.id);
        assert_eq!(doc.payment_adjustments.len(), 1);
        assert_eq!(doc.payments[0].amount_fen, 2000);
        assert_eq!(serde_json::to_value(&doc.rentals).unwrap(), before);
        let mut excess = refund(&payment);
        excess.request_id = "refund-2".into();
        excess.expected_net_fen = 1500;
        excess.amount_fen = 1501;
        assert!(adjust_payment(&mut doc, &excess, 1_800_000_300, "session", None).is_err());
        excess.amount_fen = 1500;
        excess.expected_net_fen = 2000;
        assert!(adjust_payment(&mut doc, &excess, 1_800_000_300, "session", None).is_err());
        assert_eq!(doc.payment_adjustments.len(), 1);
    }

    #[test]
    fn corrections_are_not_new_receipts_and_cannot_erase_refunded_money() {
        let mut doc = fixture();
        let payment = renew_ordered(&mut doc, &input(), 1_800_000_000, "session", None).unwrap().remove(0);
        let before = serde_json::to_value(&doc.rentals).unwrap();
        adjust_payment(&mut doc, &refund(&payment), 1_800_000_100, "session", None).unwrap();
        let mut correction = AdjustmentInput { payment_id: payment.id, kind: "correction".into(), amount_fen: 3000,
            expected_net_fen: 1500, note: "实际到账30元，原录入20元".into(), disable_line: false, expected_line_ids: None, request_id: "correction-1".into() };
        let record = adjust_payment(&mut doc, &correction, 1_800_000_200, "session", None).unwrap();
        assert_eq!(record.delta_fen, 1000);
        assert_eq!(doc.payments.len(), 1);
        assert_eq!(doc.payments[0].amount_fen, 2000);
        assert_eq!(serde_json::to_value(&doc.rentals).unwrap(), before);
        correction.request_id = "correction-2".into();
        correction.expected_net_fen = 2500;
        correction.amount_fen = 499;
        assert!(adjust_payment(&mut doc, &correction, 1_800_000_300, "session", None).is_err());
        assert_eq!(doc.payment_adjustments.len(), 2);
    }

    #[test]
    fn refund_disable_requires_explicit_choice_without_rewriting_lease_or_quota() {
        let mut doc = fixture();
        doc.lines[0].enabled = true;
        doc.lines[0].limits.monthly_gib = Some(100.0);
        let payment = renew_ordered(&mut doc, &input(), 1_800_000_000, "session", None).unwrap().remove(0);
        let before = serde_json::to_value(&doc.rentals).unwrap();
        let mut request = refund(&payment);
        request.disable_line = true;
        adjust_payment(&mut doc, &request, 1_800_000_100, "session", None).unwrap();
        assert!(!doc.lines[0].enabled);
        assert_eq!(doc.lines[0].limits.monthly_gib, Some(100.0));
        assert_eq!(serde_json::to_value(&doc.rentals).unwrap(), before);
        assert!(doc.payment_adjustments[0].disable_line);
    }

    fn plan_fixture() -> (Document, RenewalInput) {
        let mut doc = fixture();
        let mut second = doc.lines[0].clone();
        second.id = "b".into();
        doc.lines.push(second);
        for (i, line) in doc.lines.iter_mut().enumerate() {
            line.enabled = i == 0;
            let node_id = format!("node-{i}");
            line.hops = serde_json::from_value(json!([{"node_id":node_id,"port":24443,"uuid":format!("credential-{i}")}])).unwrap();
            let node: Node = serde_json::from_value(json!({"id":node_id,"name":"入口","host":"proxy.example","token_hash":"test",
                "reality":{"target":"origin.example:443","server_name":"origin.example","private_key":"private","public_key":"public","short_id":"abcd"},
                "desired":{"revision":"before","config":{"_chain":[{"id":line.id,"lease":null,"resource_ids":[],"unchanged":"original-path"}]}}})).unwrap();
            doc.nodes.push(node);
        }
        doc.applied_lines = doc.lines.clone();
        doc.applied_nodes = doc.nodes.clone();
        let plan: chain_panel::plans::Plan = serde_json::from_value(json!({"id":id(),"name":"共享团队套餐","customer_id":"customer","monthly_gib":100,
            "members":[{"id":id(),"name":"成员","line_ids":["a","b"],"monthly_gib":null}]})).unwrap();
        let key = plan.key();
        let lease = doc.rentals.remove("a").unwrap();
        doc.rentals.insert(key.clone(), lease);
        doc.customer_ledgers.insert(key.clone(), serde_json::from_value(json!({"cycle":1_800_000_000,"end":1_900_000_000,
            "allocations":{"node-0":{"granted":1000,"used":500,"rate":0,"expires":1_900_000_000}},"alerted":0})).unwrap());
        doc.customer_plans.push(plan);
        let mut request = input();
        request.line_ids = vec![key];
        (doc, request)
    }

    #[test]
    fn plan_payment_updates_each_entry_without_individual_leases_or_draft_paths() {
        let (mut doc, request) = plan_fixture();
        let ledger = serde_json::to_value(&doc.customer_ledgers).unwrap();
        doc.lines[0].chain = vec!["external:draft".into()];
        doc.resources.resources.push(chain_panel::resources::Resource { id: "draft-only".into(), members: vec!["external:draft".into()], ..Default::default() });
        let receipts = renew_ordered(&mut doc, &request, 1_800_000_000, "session", None).unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].line_id, request.line_ids[0]);
        assert_eq!(receipts[0].line_name, "共享团队套餐");
        assert!(!doc.rentals.contains_key("a") && !doc.rentals.contains_key("b"));
        for node in &doc.nodes {
            let spec = &node.desired.as_ref().unwrap().config["_chain"][0];
            assert_eq!(spec["lease"], json!(doc.rentals[&request.line_ids[0]].policy()));
            assert!(spec["resource_ids"].as_array().unwrap().contains(&json!(request.line_ids[0])));
            assert!(!spec["resource_ids"].as_array().unwrap().contains(&json!("draft-only")));
            assert_eq!(spec["unchanged"], "original-path");
        }
        assert!(!doc.lines[1].enabled);
        assert_eq!(serde_json::to_value(&doc.customer_ledgers).unwrap(), ledger);
    }

    #[test]
    fn plan_member_cannot_bypass_shared_lease_or_remove_historical_membership() {
        let (mut doc, _) = plan_fixture();
        let before = serde_json::to_value(&doc).unwrap();
        assert!(renew_selected(&mut doc, &input(), 1_800_000_000).is_err());
        assert!(pending_order(&mut doc, &input(), 1_800_000_000, "session", None).is_err());
        assert!(unlimited(&mut doc, &["a".into()], 1_800_000_000).is_err());
        assert!(remove_selected(&mut doc, &["a".into()]).is_err());
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }

    #[test]
    fn plan_refund_requires_complete_scope_and_keeps_shared_lease_and_ledger() {
        let (mut doc, request) = plan_fixture();
        let payment = renew_ordered(&mut doc, &request, 1_800_000_000, "session", None).unwrap().remove(0);
        let lease = serde_json::to_value(&doc.rentals).unwrap();
        let ledger = serde_json::to_value(&doc.customer_ledgers).unwrap();
        let mut adjustment = refund(&payment);
        adjustment.disable_line = true;
        assert!(adjust_payment(&mut doc, &adjustment, 1_800_000_100, "session", None).is_err());
        adjustment.expected_line_ids = Some(vec!["a".into()]);
        assert!(adjust_payment(&mut doc, &adjustment, 1_800_000_100, "session", None).is_err());
        assert!(doc.lines[0].enabled);
        adjustment.expected_line_ids = Some(vec!["b".into(), "a".into()]);
        let record = adjust_payment(&mut doc, &adjustment, 1_800_000_100, "session", None).unwrap();
        assert_eq!(record.affected_line_ids, vec!["a", "b"]);
        assert!(doc.lines.iter().all(|line| !line.enabled));
        assert_eq!(serde_json::to_value(&doc.rentals).unwrap(), lease);
        assert_eq!(serde_json::to_value(&doc.customer_ledgers).unwrap(), ledger);
        assert_eq!(doc.payments[0].amount_fen, payment.amount_fen);
    }
}
