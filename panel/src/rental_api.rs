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
    ensure!(!ids.is_empty() && ids.len() <= 500, "请选择 1–500 条代理");
    ensure!(
        ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
        "代理选择有重复"
    );
    ensure!(
        ids.iter().all(|id| doc
            .lines
            .iter()
            .any(|l| l.id == *id && l.kind == Kind::Vless)),
        "所选代理不存在"
    );
    Ok(())
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
        let line = doc.lines.iter().find(|l| &l.id == line_id).unwrap();
        let mut lease = doc
            .rentals
            .get(line_id)
            .context("请先设置客户归属和续费报价")?
            .clone();
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
            line_id: line.id.clone(),
            customer_id: customer.id.clone(),
            line_name: line.name.clone(),
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
    Json(input): Json<RenewalInput>,
) -> ApiResult<Json<Value>> {
    let value = edit(&app, |doc| {
        let payments = renew_selected(doc, &input, now())?;
        let mut value = results(doc, &input.line_ids);
        value["payments"] = json!(payments);
        Ok(value)
    })?;
    Ok(Json(value))
}

fn results(doc: &Document, ids: &[String]) -> Value {
    json!({"ok":true,"results":ids.iter().map(|id| {
        json!({"id":id,"sync_status":doc.lines.iter().find(|l| l.id == *id)
            .map(|l| rental::sync_status(doc,l)).unwrap_or("pending")})
    }).collect::<Vec<_>>()})
}

// Operating fields are patched into the deployed configuration; unrelated drafts stay drafts.
fn schedule(doc: &mut Document, ids: &[String]) -> Result<()> {
    let mut changed = BTreeSet::new();
    for id in ids {
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
        spec["lease"] = json!(doc.rentals.get(id).map(Rental::policy));
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
}
