//! Customer entitlements use a separate ledger from infrastructure resource pools.
use crate::{model::*, resources};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub line_ids: Vec<String>,
    pub monthly_gib: Option<f64>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub customer_id: String,
    pub monthly_gib: f64,
    pub members: Vec<Member>,
}
impl Plan {
    pub fn key(&self) -> String { format!("plan:{}", self.id) }
    pub fn lines(&self) -> impl Iterator<Item = &String> { self.members.iter().flat_map(|m| &m.line_ids) }
}
pub fn member_key(plan: &Plan, member: &Member) -> String { format!("member:{}:{}", plan.id, member.id) }
pub fn is_budget(key: &str) -> bool { key.starts_with("plan:") || key.starts_with("member:") }
pub fn for_line<'a>(doc: &'a Document, line: &str) -> Option<&'a Plan> {
    doc.customer_plans.iter().find(|p| p.lines().any(|id| id == line))
}
pub fn lease<'a>(doc: &'a Document, line: &str) -> Option<&'a Rental> {
    if let Some(plan) = for_line(doc, line) { doc.rentals.get(&plan.key()) } else { doc.rentals.get(line) }
}
pub fn budgets(doc: &Document, line: &str) -> Vec<String> {
    let Some(plan) = for_line(doc, line) else { return vec![]; };
    let mut ids = vec![plan.key()];
    for member in &plan.members {
        if member.line_ids.iter().any(|id| id == line) { ids.push(member_key(plan, member)); }
    }
    ids
}
pub fn budget(doc: &Document, key: &str, at: u64) -> Result<(resources::Resource, u64, u64)> {
    let (plan, limit, name) = if let Some(id) = key.strip_prefix("plan:") {
        let plan = doc.customer_plans.iter().find(|p| p.id == id).context("客户套餐不存在")?;
        (plan, plan.monthly_gib, plan.name.clone())
    } else {
        let (id, member_id) = key.strip_prefix("member:").and_then(|s| s.split_once(':')).context("成员额度标识无效")?;
        let plan = doc.customer_plans.iter().find(|p| p.id == id).context("客户套餐不存在")?;
        let member = plan.members.iter().find(|m| m.id == member_id).context("套餐成员不存在")?;
        (plan, member.monthly_gib.unwrap_or(plan.monthly_gib), format!("{} / {}", plan.name, member.name))
    };
    let rental = doc.rentals.get(&plan.key()).context("套餐尚未配置租期")?;
    ensure!(rental.started_at > 0 && at >= rental.started_at && (rental.expires_at == 0 || at < rental.expires_at), "套餐尚未开通或已到期");
    let (cycle, end) = crate::rental::cycle(rental.started_at, at);
    let end = if rental.expires_at > 0 { end.min(rental.expires_at) } else { end };
    Ok((resources::Resource { id: key.into(), name, monthly_gib: Some(limit), ..Default::default() }, cycle, end))
}
pub fn exhausted(doc: &Document, line: &str, at: u64) -> bool {
    budgets(doc, line).iter().any(|key| {
        let Ok((budget, cycle, _)) = budget(doc, key, at) else { return false; };
        doc.customer_ledgers.get(key).is_some_and(|ledger| ledger.cycle == cycle
            && ledger.allocations.values().fold(0u64, |sum, allocation| sum.saturating_add(allocation.used)) >= (budget.monthly_gib.unwrap() * 1073741824.0) as u64)
    })
}

pub fn validate(doc: &Document, plan: &Plan, previous: Option<&Plan>) -> Result<()> {
    uuid::Uuid::parse_str(&plan.id).context("套餐标识无效")?;
    ensure!(!plan.name.trim().is_empty() && plan.name.len() <= 120, "套餐名称须为 1–120 字节");
    ensure!(doc.customers.iter().any(|c| c.id == plan.customer_id), "请选择已有客户");
    ensure!(plan.monthly_gib.is_finite() && plan.monthly_gib > 0.0 && plan.monthly_gib <= 1e9, "套餐共享额度须为正数");
    ensure!(!plan.members.is_empty() && plan.members.len() <= 500, "套餐须有 1–500 名成员");
    if let Some(old) = previous {
        ensure!(old.customer_id == plan.customer_id, "已有套餐不能直接转给另一位客户");
        ensure!(old.members.iter().all(|m| plan.members.iter().any(|n| n.id == m.id && m.line_ids.iter().all(|id| n.line_ids.contains(id)))), "已有成员和线路不能移出套餐；请停用线路，保留历史归属");
    }
    let mut member_ids = BTreeSet::new();
    let mut lines = BTreeSet::new();
    for member in &plan.members {
        uuid::Uuid::parse_str(&member.id).context("成员标识无效")?;
        ensure!(member_ids.insert(&member.id), "成员标识重复");
        ensure!(!member.name.trim().is_empty() && member.name.len() <= 120, "成员名称须为 1–120 字节");
        if let Some(limit) = member.monthly_gib { ensure!(limit.is_finite() && limit > 0.0 && limit <= plan.monthly_gib, "成员上限须为正数且不能超过套餐总额"); }
        ensure!(!member.line_ids.is_empty(), "每名成员至少关联一条线路");
        for id in &member.line_ids {
            ensure!(lines.insert(id), "一条线路不能同时属于多个成员");
            let line = doc.lines.iter().find(|l| l.id == *id && l.kind == Kind::Vless).context("套餐线路不存在")?;
            ensure!(for_line(doc, id).is_none_or(|p| p.id == plan.id), "线路已属于其他套餐");
            if !previous.is_some_and(|p| p.lines().any(|old| old == id)) {
                ensure!(!doc.rentals.contains_key(id) && !doc.applied_lines.iter().any(|l| l.id == *id), "只能加入未出租且未发布的新线路；原独立租赁保持不变");
            }
            ensure!(!doc.lines.iter().any(|l| l.proxy_chain().iter().any(|r| r == &format!("line:{id}"))), "套餐线路不能作为其他线路的链式出口，以免重复记账");
            let node = line.hops.first().and_then(|h| doc.nodes.iter().find(|n| n.id == h.node_id)).context("套餐线路缺少入口节点")?;
            ensure!(node.report.system["customer_plan_version"] == 1, "请先升级套餐线路的入口 Agent 并等待心跳");
        }
    }
    let at = now();
    let anchor = doc.rentals.get(&plan.key()).map(|r| r.started_at).unwrap_or(0);
    let cycle = crate::rental::cycle(anchor, at).0;
    for (key, limit) in std::iter::once((plan.key(), plan.monthly_gib)).chain(plan.members.iter().filter_map(|m| m.monthly_gib.map(|g| (member_key(plan, m), g)))) {
        let reserved = doc.customer_ledgers.get(&key).filter(|l| l.cycle == cycle).map(|l| l.allocations.values().fold(0u64, |sum, a| sum.saturating_add(a.granted))).unwrap_or(0);
        ensure!((limit * 1073741824.0) as u64 >= reserved, "新上限不能低于本周期已授权额度，包含离线入口持有的预留");
    }
    Ok(())
}

pub fn view(doc: &Document, at: u64) -> Vec<Value> {
    doc.customer_plans.iter().map(|plan| {
        let rental = doc.rentals.get(&plan.key());
        let usage = |key: &str| {
            let cycle = rental.map(|r| crate::rental::cycle(r.started_at, at).0).unwrap_or(0);
            let ledger = doc.customer_ledgers.get(key).filter(|l| l.cycle == cycle);
            json!({"used":ledger.map(|l| l.allocations.values().fold(0u64, |sum, a| sum.saturating_add(a.used))).unwrap_or(0),
                "reserved":ledger.map(|l| l.allocations.values().fold(0u64, |sum, a| sum.saturating_add(a.granted))).unwrap_or(0)})
        };
        json!({"id":plan.id,"name":plan.name,"customer_id":plan.customer_id,"monthly_gib":plan.monthly_gib,"usage":usage(&plan.key()),
            "members":plan.members.iter().map(|m| json!({"id":m.id,"name":m.name,"line_ids":m.line_ids,"monthly_gib":m.monthly_gib,"usage":usage(&member_key(plan,m))})).collect::<Vec<_>>(),
            "rental":rental.map(|r| json!({"customer_id":r.customer_id,"prices":r.prices,"started_at":r.started_at,"expires_at":r.expires_at,"paid_months":r.paid_months,
                "permanent":r.started_at>0 && r.expires_at==0,"next_reset_at":if r.started_at>0 {crate::rental::cycle(r.started_at,at).1} else {0},"query_link_exists":!r.query_token_hash.is_empty()}))})
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Document, Plan) {
        let plan = Plan { id: id(), name: "团队".into(), customer_id: "c".into(), monthly_gib: 1.0,
            members: vec![Member { id: id(), name: "成员".into(), line_ids: vec!["a".into(), "b".into()], monthly_gib: Some(0.5) }] };
        let mut doc = Document::default();
        doc.customers.push(Customer { id:"c".into(), name:"客户".into(), contact:String::new(), notes:String::new() });
        for name in ["a", "b"] {
            doc.lines.push(serde_json::from_value(json!({"id":name,"name":name,"kind":"vless","enabled":true,"hops":[{"node_id":name,"port":443,"uuid":"existing-credential"}]})).unwrap());
            doc.nodes.push(serde_json::from_value(json!({"id":name,"name":name,"host":"192.0.2.1","token_hash":"test",
                "reality":{"target":"","server_name":"","private_key":"","public_key":"","short_id":""},
                "report":{"system":{"customer_plan_version":1}},"desired":{"revision":"r","config":{"_chain":[{"resource_ids":[plan.key(),member_key(&plan,&plan.members[0])]}]}}})).unwrap());
        }
        (doc, plan)
    }
    #[test]
    fn existing_entitlements_are_not_merged_and_members_keep_one_owner() {
        let (mut doc, plan) = fixture();
        validate(&doc, &plan, None).unwrap();
        doc.rentals.insert("a".into(), Rental::default());
        assert!(validate(&doc, &plan, None).is_err());
        doc.rentals.clear();
        doc.applied_lines.push(doc.lines[0].clone());
        assert!(validate(&doc, &plan, None).is_err());
        doc.applied_lines.clear();
        let mut repeated = plan.clone();
        repeated.members.push(Member { id:id(), name:"第二人".into(), line_ids:vec!["a".into()], monthly_gib:None });
        assert!(validate(&doc, &repeated, None).is_err());
        doc.nodes[0].report.system = json!({});
        assert!(validate(&doc, &plan, None).is_err(), "旧Agent不能忽略共享扣费约束");
    }
    #[test]
    fn customer_grants_require_explicit_protocol_version() {
        let (mut doc, plan) = fixture();
        let at = 1_800_000_000;
        doc.rentals.insert(plan.key(), Rental { started_at:at-1, expires_at:at+100, ..Default::default() });
        doc.customer_plans.push(plan.clone());
        for resource in [plan.key(), member_key(&plan, &plan.members[0])] {
            let mut request: resources::Request = serde_json::from_value(json!({
                "resource":resource, "epoch":id(), "cycle":0, "used":0, "want":80
            })).unwrap();
            assert_eq!(request.customer_plan_version, 0);
            let before = serde_json::to_value(&doc.customer_ledgers).unwrap();
            for version in [0, 2, u32::MAX] {
                request.customer_plan_version = version;
                let error = resources::allocate(&mut doc, "a", &request, at).err().unwrap();
                assert!(error.to_string().contains("计费协议版本 1"));
                assert_eq!(serde_json::to_value(&doc.customer_ledgers).unwrap(), before);
            }
            request.customer_plan_version = 1;
            assert_eq!(resources::allocate(&mut doc, "a", &request, at).unwrap().cap, 80);
        }
    }
    #[test]
    fn customer_grants_share_one_ceiling_across_nodes_and_restart_epochs() {
        let (mut doc, plan) = fixture();
        let at = 1_800_000_000;
        doc.rentals.insert(plan.key(), Rental { customer_id:"c".into(), started_at:at-1, expires_at:at+100, ..Default::default() });
        doc.customer_plans.push(plan.clone());
        let mut request = resources::Request { customer_plan_version:1, resource:plan.key(), epoch:id(), cycle:0, used:0, want:16*1024*1024 };
        let first = resources::allocate(&mut doc,"a",&request,at).unwrap();
        request.epoch = id();
        let second = resources::allocate(&mut doc,"b",&request,at).unwrap();
        assert_eq!(first.cap + second.cap,32*1024*1024);
        assert_eq!(first.end,at+100);
        let mut restored: Document = serde_json::from_value(serde_json::to_value(&doc).unwrap()).unwrap();
        for _ in 0..100 { request.epoch=id(); resources::allocate(&mut restored,"a",&request,at).unwrap(); }
        assert_eq!(restored.customer_ledgers[&plan.key()].allocations.values().map(|a|a.granted).sum::<u64>(),1073741824);
        request.epoch=id();
        assert_eq!(resources::allocate(&mut restored,"b",&request,at).unwrap().cap,0);
        assert!(resources::allocate(&mut restored,"b",&request,at+100).is_err());
        assert!(restored.resources.ledgers.is_empty(), "客户与供应资源账本分开");
        request.resource=member_key(&plan,&plan.members[0]);
        for _ in 0..100 { request.epoch=id(); resources::allocate(&mut restored,"b",&request,at).unwrap(); }
        assert_eq!(restored.customer_ledgers[&request.resource].allocations.values().map(|a|a.granted).sum::<u64>(),536870912);
    }
    #[test]
    fn reserved_customer_credit_is_not_reported_as_consumed_quota() {
        let (mut doc, plan)=fixture();
        let at=now();
        let anchor=at-1;
        doc.rentals.insert(plan.key(),Rental {started_at:anchor,expires_at:at+100,..Default::default()});
        doc.customer_plans.push(plan.clone());
        doc.customer_ledgers.insert(plan.key(),resources::Ledger {cycle:anchor,end:at+100,
            allocations:[("node".into(),resources::Allocation {granted:1073741824,used:0,..Default::default()})].into(),..Default::default()});
        assert!(!exhausted(&doc,"a",at));
        doc.customer_ledgers.get_mut(&plan.key()).unwrap().allocations.get_mut("node").unwrap().used=1073741824;
        assert!(exhausted(&doc,"a",at));
        assert!(exhausted(&doc,"b",at));
    }
}
