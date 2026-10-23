//! Shared resource budgets. Controller grants are durable and never reclaimed on timeout.
//! Rate leases expire; an offline agent can only spend its already granted bytes.
use crate::model::*;
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, Months, TimeZone};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Resource {
    pub id: String,
    pub name: String,
    pub members: Vec<String>,
    pub monthly_gib: Option<f64>,
    pub mbps: Option<f64>,
    pub reset_day: u32,
    pub reset_hour: u32,
    pub timezone: String,
}
impl Default for Resource {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            members: vec![],
            monthly_gib: None,
            mbps: None,
            reset_day: 1,
            reset_hour: 0,
            timezone: "Asia/Shanghai".into(),
        }
    }
}
impl Resource {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 120,
            "资源名称无效"
        );
        ensure!(
            (1..=31).contains(&self.reset_day) && self.reset_hour < 24,
            "重置日期或时间无效"
        );
        self.timezone.parse::<chrono_tz::Tz>().context("时区无效")?;
        for value in [self.monthly_gib, self.mbps].into_iter().flatten() {
            ensure!(
                value.is_finite() && value > 0.0 && value <= 1e9,
                "额度和带宽必须为正数"
            );
        }
        Ok(())
    }
    pub fn cycle(&self, at: u64) -> Result<(u64, u64)> {
        let zone = self.timezone.parse::<chrono_tz::Tz>()?;
        let current = zone
            .timestamp_opt(at as i64, 0)
            .single()
            .context("时间无效")?;
        let first = current.date_naive().with_day(1).unwrap();
        let boundary = |first: chrono::NaiveDate| -> Result<u64> {
            let last = first
                .checked_add_months(Months::new(1))
                .unwrap()
                .pred_opt()
                .unwrap()
                .day();
            let day = first.with_day(self.reset_day.min(last)).unwrap();
            // At a DST gap move to the first valid minute; at a fold use the first occurrence.
            let mut local = day.and_hms_opt(self.reset_hour, 0, 0).unwrap();
            for _ in 0..=180 {
                if let Some(t) = zone.from_local_datetime(&local).earliest() {
                    return Ok(t.timestamp() as u64);
                }
                local += chrono::Duration::minutes(1);
            }
            anyhow::bail!("重置时间无法解析")
        };
        let start_month = if boundary(first)? > at {
            first.checked_sub_months(Months::new(1)).unwrap()
        } else {
            first
        };
        Ok((
            boundary(start_month)?,
            boundary(start_month.checked_add_months(Months::new(1)).unwrap())?,
        ))
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Allocation {
    pub granted: u64,
    pub used: u64,
    pub rate: f64,
    pub expires: u64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub cycle: u64,
    pub end: u64,
    pub allocations: BTreeMap<String, Allocation>,
    pub alerted: u32,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub resources: Vec<Resource>,
    pub ledgers: BTreeMap<String, Ledger>,
}

pub fn paths(doc: &Document, line: &Line) -> Result<Vec<String>> {
    fn walk(
        doc: &Document,
        line: &Line,
        stack: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) -> Result<()> {
        ensure!(stack.insert(line.id.clone()), "代理链存在循环");
        out.extend(line.hops.iter().map(|h| format!("node:{}", h.node_id)));
        for reference in line.proxy_chain() {
            if reference.starts_with("external:") {
                out.push(reference);
            } else {
                walk(
                    doc,
                    doc.lines
                        .iter()
                        .find(|l| reference == format!("line:{}", l.id))
                        .context("引用代理不存在")?,
                    stack,
                    out,
                )?;
            }
        }
        stack.remove(&line.id);
        Ok(())
    }
    let mut out = vec![];
    walk(doc, line, &mut BTreeSet::new(), &mut out)?;
    Ok(out)
}
pub fn multiplier(doc: &Document, line: &Line) -> Result<u32> {
    Ok(paths(doc, line)?.len().max(1).try_into()?)
}
// Only charge the resources traversed by this entry. A referenced managed line accounts
// for its own path, so A -> line:B never debits B's resource twice at A and B.
pub fn pools(doc: &Document, line: &Line) -> Vec<String> {
    let members: BTreeSet<_> = line
        .hops
        .iter()
        .map(|h| format!("node:{}", h.node_id))
        .chain(
            line.proxy_chain()
                .into_iter()
                .filter(|r| r.starts_with("external:")),
        )
        .collect();
    let mut pools: Vec<_> = doc.resources
        .resources
        .iter()
        .filter(|r| r.members.iter().any(|m| members.contains(m)))
        .map(|r| r.id.clone())
        .collect();
    pools.extend(crate::plans::budgets(doc, &line.id));
    pools
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub customer_plan_version: u32,
    pub resource: String,
    pub epoch: String,
    pub cycle: u64,
    pub used: u64,
    pub want: u64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Grant {
    pub cycle: u64,
    pub end: u64,
    pub cap: u64,
    pub rate: Option<f64>,
    pub valid_for: u64,
}

pub fn allocate(doc: &mut Document, node: &str, input: &Request, at: u64) -> Result<Grant> {
    uuid::Uuid::parse_str(&input.epoch).context("账本标识无效")?;
    ensure!(input.want <= 16 * 1024 * 1024, "额度申请过大");
    let customer_budget = crate::plans::is_budget(&input.resource);
    let (resource, cycle, end) = if customer_budget {
        ensure!(input.customer_plan_version == 1, "客户套餐需要计费协议版本 1，请升级 Agent 后重试");
        crate::plans::budget(doc, &input.resource, at)?
    } else {
        let resource = doc
        .resources
        .resources
        .iter()
        .find(|r| r.id == input.resource)
        .context("资源不存在")?
        .clone();
        let (cycle, end) = resource.cycle(at)?;
        (resource, cycle, end)
    };
    let eligible: BTreeSet<_> = doc
        .nodes
        .iter()
        .filter(|n| {
            n.desired.as_ref().is_some_and(|d| {
                d.config["_chain"].as_array().is_some_and(|specs| {
                    specs.iter().any(|s| {
                        s["resource_ids"]
                            .as_array()
                            .is_some_and(|ids| ids.iter().any(|id| id == &input.resource))
                    })
                })
            })
        })
        .map(|n| n.id.clone())
        .collect();
    ensure!(eligible.contains(node), "入口未关联此资源");
    let ledgers = if customer_budget { &mut doc.customer_ledgers } else { &mut doc.resources.ledgers };
    let ledger = ledgers
        .entry(resource.id.clone())
        .or_default();
    if ledger.cycle != cycle {
        *ledger = Ledger {
            cycle,
            end,
            ..Default::default()
        };
    }
    let key = format!("{node}:{}", input.epoch);
    let granted = ledger
        .allocations
        .values()
        .fold(0u64, |a, b| a.saturating_add(b.granted));
    let total = resource
        .monthly_gib
        .map(|g| (g * 1073741824.0) as u64)
        .unwrap_or(u64::MAX);
    let other_rates: f64 = ledger
        .allocations
        .iter()
        .filter(|(k, a)| **k != key && a.expires > at)
        .map(|(_, a)| a.rate)
        .sum();
    let a = ledger.allocations.entry(key).or_default();
    if input.cycle == cycle {
        a.used = a.used.max(input.used.min(a.granted));
    }
    let outstanding = a.granted.saturating_sub(a.used);
    let extra = input
        .want
        .saturating_sub(outstanding)
        .min(total.saturating_sub(granted));
    a.granted = a.granted.saturating_add(extra);
    let rate = resource
        .mbps
        .map(|limit| (limit / eligible.len().max(1) as f64).min((limit - other_rates).max(0.0)));
    a.rate = rate.unwrap_or(0.0);
    a.expires = (at + 10).min(end);
    let result = Grant {
        cycle,
        end,
        cap: a.granted,
        rate,
        valid_for: a.expires.saturating_sub(at),
    };
    if let Some(limit) = resource.monthly_gib {
        let percent =
            (granted.saturating_add(extra) as f64 / (limit * 1073741824.0) * 100.0) as u32;
        for threshold in [80, 95, 100] {
            if percent >= threshold && ledger.alerted < threshold {
                doc.monitoring.notice(
                    format!(
                        "资源 {}：已分配额度达到 {}%（包含入口尚未使用的预留额度）",
                        resource.name, threshold
                    ),
                    at,
                );
                ledger.alerted = threshold;
            }
        }
    }
    Ok(result)
}

pub struct Client {
    database: Mutex<rusqlite::Connection>,
    epoch: String,
    grants: Mutex<BTreeMap<String, (Grant, Instant)>>,
    refresh: tokio::sync::Mutex<()>,
    budgets: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
    http: reqwest::Client,
    endpoint: String,
    token: String,
}
impl Client {
    pub fn open(dir: &std::path::Path, endpoint: String, token: String) -> Result<Arc<Self>> {
        let db = rusqlite::Connection::open(dir.join("resources.db"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS identity(epoch TEXT NOT NULL); CREATE TABLE IF NOT EXISTS consumption(resource TEXT PRIMARY KEY,cycle INTEGER NOT NULL,used INTEGER NOT NULL);")?;
        let epoch = db
            .query_row("SELECT epoch FROM identity LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap_or_else(|_| id());
        db.execute(
            "INSERT INTO identity SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM identity)",
            [&epoch],
        )?;
        Ok(Arc::new(Self {
            database: Mutex::new(db),
            epoch,
            grants: Mutex::new(BTreeMap::new()),
            refresh: tokio::sync::Mutex::new(()),
            budgets: Mutex::new(BTreeMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint,
            token,
        }))
    }
    fn used(&self, resource: &str) -> Result<(u64, u64)> {
        use rusqlite::OptionalExtension;
        Ok(self
            .database
            .lock()
            .unwrap()
            .query_row(
                "SELECT cycle,used FROM consumption WHERE resource=?1",
                [resource],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or_default())
    }
    async fn grant(&self, resource: &str, need: u64) -> Result<Grant> {
        let _refresh = self.refresh.lock().await;
        let (cycle, used) = self.used(resource)?;
        if let Some((grant, expiry)) = self.grants.lock().unwrap().get(resource) {
            if *expiry > Instant::now()
                && grant.end > now()
                && grant.cycle == cycle
                && grant.cap.saturating_sub(used) >= need
            {
                ensure!(grant.rate != Some(0.0), "资源带宽正在重新分配，请稍后重试");
                return Ok(grant.clone());
            }
        }
        let requested = Instant::now();
        let grant = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&Request {
                customer_plan_version: if crate::plans::is_budget(resource) { 1 } else { 0 },
                resource: resource.into(),
                epoch: self.epoch.clone(),
                cycle,
                used,
                want: (16 * 1024 * 1024).max(need),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<Grant>()
            .await?;
        let expiry = requested + Duration::from_secs(grant.valid_for.saturating_sub(1));
        ensure!(expiry > Instant::now(), "资源带宽租约已过期");
        if grant.cycle != cycle
            || self.database.lock().unwrap().query_row(
                "SELECT COUNT(*) FROM consumption WHERE resource=?1",
                [resource],
                |r| r.get::<_, u64>(0),
            )? == 0
        {
            self.database.lock().unwrap().execute("INSERT INTO consumption VALUES(?1,?2,0) ON CONFLICT(resource) DO UPDATE SET cycle=excluded.cycle,used=0",(resource,grant.cycle))?;
        }
        self.grants
            .lock()
            .unwrap()
            .insert(resource.into(), (grant.clone(), expiry));
        ensure!(grant.rate != Some(0.0), "资源带宽正在重新分配，请稍后重试");
        Ok(grant)
    }
    pub async fn charge(&self, resources: &[String], amount: usize, _upload: bool) -> Result<()> {
        self.charge_weighted(resources, amount, 1).await
    }
    pub async fn charge_weighted(&self, resources: &[String], amount: usize, multiplier: u32) -> Result<()> {
        if resources.is_empty() {
            return Ok(());
        }
        // Each forwarded upload or download crosses both the server's ingress and
        // egress. Symmetric provider capacity therefore bounds their combined rate.
        let locks = {
            let mut budgets = self.budgets.lock().unwrap();
            resources
                .iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|id| {
                    budgets
                        .entry(id.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                        .clone()
                })
                .collect::<Vec<_>>()
        };
        let mut permits = Vec::new();
        for lock in locks {
            permits.push(lock.lock_owned().await);
        }

        let debits: BTreeMap<_, _> = resources.iter().map(|resource| {
            let weight = if crate::plans::is_budget(resource) { multiplier.max(1) as u64 } else { 1 };
            (resource, (amount as u64).saturating_mul(weight))
        }).collect();
        ensure!(debits.values().all(|amount| *amount <= 16 * 1024 * 1024), "单次计费块过大");
        let mut grants = vec![];
        let mut remaining = amount as f64;
        while remaining > 0.0 {
            grants.clear();
            let mut rate = f64::INFINITY;
            let mut window = 4.0f64;
            for (resource, debit) in &debits {
                let grant = self.grant(resource, *debit).await?;
                if let Some(r) = grant.rate {
                    rate = rate.min(r * 125000.0);
                    let expiry = self.grants.lock().unwrap()[*resource].1;
                    window = window.min(
                        expiry
                            .saturating_duration_since(Instant::now())
                            .as_secs_f64(),
                    );
                }
                grants.push((*resource, *debit, grant));
            }
            if rate.is_infinite() {
                break;
            }
            if window < 0.01 {
                self.grants
                    .lock()
                    .unwrap()
                    .retain(|_, (g, _)| g.rate.is_none());
                continue;
            }
            let duration = (remaining / rate).min(window);
            tokio::time::sleep(Duration::from_secs_f64(duration)).await;
            remaining = (remaining - duration * rate).max(0.0);
        }
        let mut database = self.database.lock().unwrap();
        let transaction = database.transaction()?;
        for (resource, debit, grant) in grants {
            ensure!(
                grant.end > now()
                    && self
                        .grants
                        .lock()
                        .unwrap()
                        .get(resource)
                        .is_some_and(|(_, expiry)| *expiry > Instant::now()),
                "资源租约已过期"
            );
            let used: u64 = transaction.query_row(
                "SELECT used FROM consumption WHERE resource=?1 AND cycle=?2",
                (resource, grant.cycle),
                |r| r.get(0),
            )?;
            ensure!(
                grant.cap.saturating_sub(used) >= debit,
                "共享资源额度不足"
            );
            transaction.execute(
                "UPDATE consumption SET used=used+?1 WHERE resource=?2",
                (debit, resource),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn customer_debits_use_current_multiplier_and_commit_all_budgets_together() {
        use axum::{Json, Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        eprintln!("owned weighted budget listener={address}");
        let router = Router::new().route("/grant",post(|Json(request):Json<Request>| async move {
            assert_eq!(request.customer_plan_version, if crate::plans::is_budget(&request.resource) { 1 } else { 0 });
            Json(Grant { cycle:1,end:now()+100,cap:if request.resource=="plan:small" { 5 } else { 100_000 },rate:None,valid_for:10 })
        }));
        let stop = tokio_util::sync::CancellationToken::new();
        let stopped = stop.clone();
        let server = tokio::spawn(async move { axum::serve(listener,router).with_graceful_shutdown(stopped.cancelled_owned()).await.unwrap(); });
        let dir=tempfile::tempdir().unwrap();
        let url=format!("http://{address}/grant");
        let client=Client::open(dir.path(),url.clone(),"test".into()).unwrap();
        let budgets=vec!["physical".into(),"plan:team".into(),"member:team:one".into()];
        client.charge_weighted(&budgets,10,3).await.unwrap();
        client.charge_weighted(&budgets,10,2).await.unwrap();
        assert_eq!(client.used("physical").unwrap().1,20);
        assert_eq!(client.used("plan:team").unwrap().1,50);
        assert_eq!(client.used("member:team:one").unwrap().1,50);
        assert!(client.charge_weighted(&["physical".into(),"plan:small".into()],10,1).await.is_err());
        assert_eq!(client.used("physical").unwrap().1,20,"另一预算不足时不得只写入一半扣费");
        drop(client);
        let reopened=Client::open(dir.path(),url,"test".into()).unwrap();
        assert_eq!(reopened.used("plan:team").unwrap().1,50,"重启不能按新倍率重算历史");
        drop(reopened);
        stop.cancel(); server.await.unwrap();
        let _released=std::net::TcpListener::bind(address).unwrap();
    }
    #[tokio::test]
    async fn shared_bandwidth_counts_both_directions_and_usage_survives_reopen() {
        use axum::{Json, Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        eprintln!("owned resource test listener={address}");
        let app = Router::new().route(
            "/grant",
            post(|| async {
                Json(Grant {
                    cycle: 1,
                    end: now() + 100,
                    cap: 100_000,
                    rate: Some(0.08),
                    valid_for: 10,
                })
            }),
        );
        let stop = tokio_util::sync::CancellationToken::new();
        let stopped = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .unwrap()
        });
        let dir = tempfile::tempdir().unwrap();
        let url = format!("http://{address}/grant");
        let client = Client::open(dir.path(), url.clone(), "test".into()).unwrap();
        let start = Instant::now();
        let resources = vec!["pool".into()];
        let (a, b) = tokio::join!(
            client.charge(&resources, 10_000, true),
            client.charge(&resources, 10_000, false)
        );
        assert!(a.is_ok() && b.is_ok(), "{a:?} {b:?}");
        assert!(start.elapsed() >= Duration::from_millis(1900));
        assert_eq!(client.used("pool").unwrap(), (1, 20_000));
        drop(client);
        let client = Client::open(dir.path(), url, "test".into()).unwrap();
        assert_eq!(client.used("pool").unwrap(), (1, 20_000));
        stop.cancel();
        server.await.unwrap();
        assert!(tokio::net::TcpListener::bind(address).await.is_ok());
    }
    fn fixture() -> Document {
        let mut d = Document::default();
        for key in ["a", "b"] {
            d.nodes.push(serde_json::from_value(serde_json::json!({"id":key,"name":key,"host":"example.com","token_hash":"", "reality":{"target":"","server_name":"","private_key":"","public_key":"","short_id":""},"desired":{"revision":"r","config":{"_chain":[{"resource_ids":["pool"]}]}}})).unwrap());
        }
        d.resources.resources.push(Resource {
            id: "pool".into(),
            name: "pool".into(),
            monthly_gib: Some(100.0 / 1073741824.0),
            mbps: Some(10.0),
            ..Default::default()
        });
        d
    }
    #[test]
    fn legacy_supply_requests_remain_compatible() {
        let mut doc = fixture();
        let request: Request = serde_json::from_value(serde_json::json!({
            "resource":"pool", "epoch":id(), "cycle":0, "used":0, "want":80
        })).unwrap();
        assert_eq!(request.customer_plan_version, 0);
        assert_eq!(allocate(&mut doc, "a", &request, 1_800_000_000).unwrap().cap, 80);
        assert!(doc.customer_ledgers.is_empty());
    }
    #[test]
    fn grants_are_idempotent_and_never_overallocate() {
        let mut d = fixture();
        let epoch = id();
        let at = 1_800_000_000;
        let mut request = Request {
            customer_plan_version: 0,
            resource: "pool".into(),
            epoch,
            cycle: 0,
            used: 0,
            want: 80,
        };
        let a = allocate(&mut d, "a", &request, at).unwrap();
        assert_eq!(a.cap, 80);
        assert_eq!(a.rate, Some(5.0));
        assert_eq!(allocate(&mut d, "a", &request, at).unwrap().cap, 80);
        request.epoch = id();
        let b = allocate(&mut d, "b", &request, at).unwrap();
        assert_eq!(b.cap, 20);
        // Lost local state gets a new identity, never a refund of previously issued bytes.
        request.epoch = id();
        assert_eq!(allocate(&mut d, "a", &request, at + 20).unwrap().cap, 0);
        assert_eq!(
            d.resources.ledgers["pool"]
                .allocations
                .values()
                .map(|a| a.granted)
                .sum::<u64>(),
            100
        );
        request.cycle = a.cycle;
        request.used = 80;
        assert_eq!(allocate(&mut d, "b", &request, a.end).unwrap().cap, 80);
    }
    #[test]
    fn nested_paths_count_hops_but_not_group_size() {
        let mut d = fixture();
        let b:Line=serde_json::from_value(serde_json::json!({"id":"b","name":"B","kind":"vless","hops":[{"node_id":"b","port":1234,"uuid":"u"}],"chain":["external:x"]})).unwrap();
        let a:Line=serde_json::from_value(serde_json::json!({"id":"a","name":"A","kind":"vless","hops":[{"node_id":"a","port":1235,"uuid":"u"}],"chain":["line:b"]})).unwrap();
        d.lines = vec![a.clone(), b.clone()];
        d.resources.resources[0].members = vec!["external:x".into(), "external:y".into()];
        assert_eq!(multiplier(&d, &a).unwrap(), 3);
        assert!(pools(&d, &a).is_empty());
        assert_eq!(pools(&d, &b), vec!["pool"]);
    }
    #[test]
    fn month_end_and_dst_cycle() {
        let r = Resource {
            reset_day: 31,
            timezone: "America/New_York".into(),
            ..Default::default()
        };
        let at = chrono::DateTime::parse_from_rfc3339("2026-02-28T06:00:00Z")
            .unwrap()
            .timestamp() as u64;
        let (start, end) = r.cycle(at).unwrap();
        assert_eq!(
            chrono::DateTime::from_timestamp(start as i64, 0)
                .unwrap()
                .to_rfc3339(),
            "2026-02-28T05:00:00+00:00"
        );
        assert_eq!(
            chrono::DateTime::from_timestamp(end as i64, 0)
                .unwrap()
                .to_rfc3339(),
            "2026-03-31T04:00:00+00:00"
        );
    }
}
