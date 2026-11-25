//! Shared resource budgets. Controller grants are durable and never reclaimed on timeout.
//! Rate leases expire; an offline agent can only spend its already granted bytes.
use crate::model::*;
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, Months, TimeZone};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio::time::Instant;

pub const MIN_GRANT_WANT: u64 = 16 * 1024 * 1024;
pub const MAX_GRANT_WANT: u64 = 128 * 1024 * 1024;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const PACE_COALESCE: Duration = Duration::from_millis(2);
const PACE_IDLE_RESET: Duration = Duration::from_millis(20);

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
        ensure!(
            stack.len() < 16 && stack.insert(line.id.clone()),
            "代理链存在循环或超过 16 层"
        );
        out.extend(line.hops.iter().map(|h| format!("node:{}", h.node_id)));
        for reference in line.proxy_chain() {
            if reference.starts_with("external:") {
                out.push(reference.clone());
            } else {
                walk(
                    doc,
                    doc.lines
                        .iter()
                        .find(|l| *reference == format!("line:{}", l.id))
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
                .iter()
                .filter(|r| r.starts_with("external:"))
                .cloned(),
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

fn one() -> u32 {
    1
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub resource: String,
    pub epoch: String,
    pub cycle: u64,
    pub used: u64,
    pub want: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_want: Option<u64>,
    #[serde(default = "one")]
    pub customer_plan_version: u32,
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
    let want = input.want.max(input.preferred_want.unwrap_or(0));
    ensure!(want <= MAX_GRANT_WANT, "额度申请过大");
    let customer_budget = crate::plans::is_budget(&input.resource);
    let (resource, cycle, end) = if customer_budget {
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
    // Count distinct active entry nodes (not stale epochs on the same node) so a single active
    // entry can use 100% of the pool while newly active peers immediately receive their fair share.
    let mut active_nodes: BTreeSet<&str> = ledger
        .allocations
        .iter()
        .filter(|(k, a)| **k != key && a.expires > at)
        .filter_map(|(k, _)| k.split_once(':').map(|(n, _)| n))
        .collect();
    active_nodes.insert(node);
    let active_count = active_nodes.len().max(1);
    let a = ledger.allocations.entry(key).or_default();
    if input.cycle == cycle {
        a.used = a.used.max(input.used.min(a.granted));
    }
    let outstanding = a.granted.saturating_sub(a.used);
    let extra = want
        .saturating_sub(outstanding)
        .min(total.saturating_sub(granted));
    a.granted = a.granted.saturating_add(extra);
    let (rate, lease_secs) = match resource.mbps {
        Some(limit) => {
            let floor = limit / eligible.len().max(1) as f64;
            let assigned = (limit / active_count as f64).max(floor).min(limit);
            let secs = if assigned > floor + 1e-9 && end > at + 2 { 5 } else { 10 };
            (Some(assigned), secs)
        }
        None => (None, 10),
    };
    a.rate = rate.unwrap_or(0.0);
    a.expires = (at + lease_secs).min(end);
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

#[derive(Clone, Copy, Default)]
struct UsageRow {
    cycle: u64,
    used: u64,
    dirty: bool,
}

#[derive(Clone)]
struct GrantEntry {
    grant: Grant,
    expiry: Instant,
    window: u64,
    window_used: u64,
    granted_delta: u64,
}

enum FlushCommand {
    Sync(mpsc::SyncSender<Result<()>>),
}

fn flush_usage_rows(
    db: &mut rusqlite::Connection,
    usage: &Mutex<BTreeMap<String, UsageRow>>,
    truncate: bool,
) -> Result<()> {
    let dirty: Vec<(String, u64, u64)> = {
        let state = usage.lock().unwrap();
        state
            .iter()
            .filter(|(_, row)| row.dirty)
            .map(|(k, row)| (k.clone(), row.cycle, row.used))
            .collect()
    };
    if !dirty.is_empty() {
        let tx = db.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO consumption(resource,cycle,used) VALUES(?1,?2,?3) \
                 ON CONFLICT(resource) DO UPDATE SET cycle=excluded.cycle,used=excluded.used",
            )?;
            for (resource, cycle, used) in &dirty {
                stmt.execute((resource, cycle, used))?;
            }
        }
        tx.commit()?;
        let mut state = usage.lock().unwrap();
        for (resource, cycle, used) in dirty {
            if let Some(row) = state.get_mut(&resource) {
                if row.cycle == cycle && row.used == used {
                    row.dirty = false;
                }
            }
        }
    }
    let mode = if truncate { "TRUNCATE" } else { "PASSIVE" };
    let _ = db.execute_batch(&format!("PRAGMA wal_checkpoint({mode});"));
    Ok(())
}

pub struct Client {
    epoch: String,
    quota: Arc<crate::quota::Journal>,
    usage: Arc<Mutex<BTreeMap<String, UsageRow>>>,
    grants: Mutex<BTreeMap<String, GrantEntry>>,
    refreshing: Mutex<BTreeSet<String>>,
    budgets: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<Option<Instant>>>>>,
    http: reqwest::Client,
    endpoint: String,
    token: String,
    flusher: Option<mpsc::Sender<FlushCommand>>,
    writer: Option<std::thread::JoinHandle<()>>,
}
impl Client {
    pub fn open(dir: &std::path::Path, endpoint: String, token: String) -> Result<Arc<Self>> {
        let quota = crate::quota::Journal::open(&dir.join("resource-quota.db"));
        let mut db = rusqlite::Connection::open(dir.join("resources.db"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE IF NOT EXISTS identity(epoch TEXT NOT NULL); \
             CREATE TABLE IF NOT EXISTS consumption(resource TEXT PRIMARY KEY,cycle INTEGER NOT NULL,used INTEGER NOT NULL);",
        )?;
        use rusqlite::OptionalExtension;
        let epoch = db
            .query_row("SELECT epoch FROM identity LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?.unwrap_or_else(id);
        db.execute(
            "INSERT INTO identity SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM identity)",
            [&epoch],
        )?;
        let mut initial = BTreeMap::new();
        {
            let mut stmt = db.prepare("SELECT resource,cycle,used FROM consumption")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?, r.get::<_, u64>(2)?))
            })?;
            for row in rows {
                let (resource, cycle, used) = row?;
                let recovered = quota.recovered(&resource, cycle);
                initial.insert(
                    resource,
                    UsageRow {
                        cycle,
                        used: used.max(recovered),
                        dirty: recovered > used,
                    },
                );
            }
        }
        let usage = Arc::new(Mutex::new(initial));
        let writer_usage = usage.clone();
        let (flusher, rx) = mpsc::channel::<FlushCommand>();
        let writer = std::thread::Builder::new()
            .name("chain-resources".into())
            .spawn(move || {
                loop {
                    match rx.recv_timeout(FLUSH_INTERVAL) {
                        Ok(FlushCommand::Sync(done)) => {
                            let _ = done.send(flush_usage_rows(&mut db, &writer_usage, false));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Err(e) = flush_usage_rows(&mut db, &writer_usage, false) {
                                eprintln!("资源账本定期刷盘失败: {e}");
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            let _ = flush_usage_rows(&mut db, &writer_usage, true);
                            break;
                        }
                    }
                }
            })?;
        Ok(Arc::new(Self {
            epoch,
            quota,
            usage,
            grants: Mutex::new(BTreeMap::new()),
            refreshing: Mutex::new(BTreeSet::new()),
            budgets: Mutex::new(BTreeMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint,
            token,
            flusher: Some(flusher),
            writer: Some(writer),
        }))
    }
    pub fn flush(&self) -> Result<()> {
        let Some(flusher) = &self.flusher else {
            return Ok(());
        };
        let (tx, rx) = mpsc::sync_channel(1);
        flusher
            .send(FlushCommand::Sync(tx))
            .map_err(|_| anyhow::anyhow!("资源账本写入线程已退出"))?;
        rx.recv()
            .map_err(|_| anyhow::anyhow!("资源账本写入线程未响应"))?
    }
    pub(crate) fn settle_quota(&self) -> Result<()> {
        let rows = self.usage.lock().unwrap().iter()
            .map(|(id, row)| (id.clone(), row.cycle, row.used)).collect();
        self.quota.settle(rows)
    }
    fn used(&self, resource: &str) -> Result<(u64, u64)> {
        let mut usage = self.usage.lock().unwrap();
        let Some(row) = usage.get_mut(resource) else { return Ok((0, 0)) };
        let recovered = self.quota.recovered(resource, row.cycle);
        if recovered > row.used {
            row.used = recovered;
            row.dirty = true;
        }
        Ok((row.cycle, row.used))
    }
    pub(crate) fn cached_rate(&self, resources: &[String]) -> Option<f64> {
        if resources.is_empty() {
            return None;
        }
        let grants = self.grants.lock().unwrap();
        resources
            .iter()
            .filter_map(|id| match grants.get(id) {
                // Keep the last known rate across lease renewals to avoid a 125 B/s fragmentation cliff.
                Some(entry) => entry.grant.rate,
                // Use a small initial probe block only before the very first grant is fetched.
                None => Some(0.008),
            })
            .reduce(f64::min)
    }
    async fn fetch_grant(&self, resource: &str, cycle: u64, used: u64, want: u64) -> Result<(Grant, Instant)> {
        let requested = Instant::now();
        let want = want.clamp(MIN_GRANT_WANT, MAX_GRANT_WANT);
        let grant = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&Request {
                resource: resource.into(),
                epoch: self.epoch.clone(),
                cycle,
                used,
                // Older panels accept at most 16 MiB and ignore the optional larger window.
                want: MIN_GRANT_WANT,
                preferred_want: (want > MIN_GRANT_WANT).then_some(want),
                customer_plan_version: 1,
            })
            .send()
            .await?
            .error_for_status()?
            .json::<Grant>()
            .await?;
        let expiry = requested + Duration::from_secs(grant.valid_for.saturating_sub(1));
        Ok((grant, expiry))
    }
    fn apply_grant(&self, resource: &str, grant: Grant, expiry: Instant, requested_window: u64,
        expected_cycle: Option<u64>) {
        if grant.end <= now() {
            return;
        }
        let mut grants = self.grants.lock().unwrap();
        if grants.get(resource).is_some_and(|old| {
            old.grant.cycle == grant.cycle && old.expiry > expiry && old.grant.cap >= grant.cap
        }) {
            return;
        }
        let base_used = {
            let mut usage = self.usage.lock().unwrap();
            let entry = usage.entry(resource.into()).or_default();
            if expected_cycle.is_some_and(|cycle| cycle != entry.cycle) {
                return;
            }
            if entry.cycle != grant.cycle {
                *entry = UsageRow {
                    cycle: grant.cycle,
                    used: self.quota.ceiling(resource, grant.cycle),
                    dirty: true,
                };
            }
            entry.used
        };
        let next_window = match grants.get(resource) {
            Some(old) if old.grant.cycle == grant.cycle => {
                if old.window_used >= old.window / 2 {
                    (old.window * 2).min(MAX_GRANT_WANT)
                } else if old.window_used < old.window / 8 {
                    (old.window / 2).max(MIN_GRANT_WANT)
                } else {
                    old.window
                }
            }
            _ => requested_window.clamp(MIN_GRANT_WANT, MAX_GRANT_WANT),
        };
        grants.insert(
            resource.into(),
            GrantEntry {
                granted_delta: grant.cap.saturating_sub(base_used),
                grant,
                expiry,
                window: next_window,
                window_used: 0,
            },
        );
    }
    fn maybe_prefetch(self: &Arc<Self>, resource: &str) {
        let now = Instant::now();
        let prefetch_args = {
            let grants = self.grants.lock().unwrap();
            let Some(entry) = grants.get(resource) else {
                return;
            };
            let (_, used) = self.used(resource).unwrap_or_default();
            if entry.grant.cycle == 0 || entry.grant.end <= crate::model::now() + 3 {
                return;
            }
            let low_bytes = entry.granted_delta >= 1024 * 1024
                && entry.window_used >= (entry.granted_delta * 3) / 4
                && entry.grant.cap.saturating_sub(used) <= entry.granted_delta / 4;
            let low_time = entry.expiry.saturating_duration_since(now) <= Duration::from_secs(2)
                && (entry.window_used > 0 || now >= entry.expiry);
            if !low_bytes && !low_time {
                return;
            }
            let next_want = if entry.window_used >= entry.window / 2 {
                (entry.window * 2).min(MAX_GRANT_WANT)
            } else {
                entry.window
            };
            (entry.grant.cycle, used, next_want)
        };
        if !self.refreshing.lock().unwrap().insert(resource.to_owned()) {
            return;
        }
        let client = self.clone();
        let name = resource.to_owned();
        tokio::spawn(async move {
            let (cycle, used, want) = prefetch_args;
            if let Ok((grant, expiry)) = client.fetch_grant(&name, cycle, used, want).await {
                // A delayed prefetch must never change the charging cycle seen by a foreground debit.
                if grant.cycle == cycle && expiry > Instant::now() && grant.rate != Some(0.0) {
                    client.apply_grant(&name, grant, expiry, want, Some(cycle));
                }
            }
            client.refreshing.lock().unwrap().remove(&name);
        });
    }
    async fn grant(self: &Arc<Self>, resource: &str, need: u64) -> Result<Grant> {
        // charge_weighted already holds the budget locks for the requested resources.
        self.quota.ready().await?;
        let (cycle, used) = self.used(resource)?;
        let mut desired_window = MIN_GRANT_WANT;
        let cached = self.grants.lock().unwrap().get(resource).cloned();
        if let Some(entry) = cached {
            desired_window = if entry.window_used >= entry.window / 2 {
                (entry.window * 2).min(MAX_GRANT_WANT)
            } else {
                entry.window
            };
            if (entry.expiry > Instant::now() || entry.grant.rate.is_none())
                && entry.grant.end > now()
                && entry.grant.cycle == cycle
                && entry.grant.cap.saturating_sub(used) >= need
            {
                ensure!(entry.grant.rate != Some(0.0), "资源带宽正在重新分配，请稍后重试");
                self.maybe_prefetch(resource);
                return Ok(entry.grant);
            }
        }
        let want = desired_window.max(need);
        let (grant, expiry) = self.fetch_grant(resource, cycle, used, want).await?;
        ensure!(expiry > Instant::now() && grant.end > now(), "资源带宽租约已过期");
        self.apply_grant(resource, grant.clone(), expiry, want, None);
        ensure!(grant.rate != Some(0.0), "资源带宽正在重新分配，请稍后重试");
        Ok(grant)
    }
    pub async fn charge_weighted(self: &Arc<Self>, resources: &[String], amount: usize, multiplier: u32) -> Result<()> {
        if resources.is_empty() || amount == 0 {
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
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
                        .clone()
                })
                .collect::<Vec<_>>()
        };
        let mut permits = Vec::new();
        for lock in locks {
            permits.push(lock.lock_owned().await);
        }

        let debits: BTreeMap<_, _> = resources
            .iter()
            .map(|resource| {
                let weight = if crate::plans::is_budget(resource) {
                    multiplier.max(1) as u64
                } else {
                    1
                };
                (resource, (amount as u64).saturating_mul(weight))
            })
            .collect();
        ensure!(
            debits.values().all(|amount| *amount <= MAX_GRANT_WANT),
            "单次计费块过大"
        );
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
                    let expiry = self.grants.lock().unwrap()[*resource].expiry;
                    window = window.min(
                        expiry
                            .saturating_duration_since(Instant::now())
                            .as_secs_f64(),
                    );
                }
                grants.push((*resource, *debit, grant));
            }
            if rate.is_infinite() {
                for slot in &mut permits {
                    **slot = None;
                }
                break;
            }
            if window < 0.01 {
                let mut cached = self.grants.lock().unwrap();
                for (resource, _, _) in &grants {
                    if cached.get(*resource).is_some_and(|e| {
                        e.grant.rate.is_some()
                            && e.expiry.saturating_duration_since(Instant::now())
                                < Duration::from_millis(10)
                    }) {
                        cached.remove(*resource);
                    }
                }
                for slot in &mut permits {
                    **slot = None;
                }
                continue;
            }
            let now = Instant::now();
            let base = permits
                .iter()
                .filter_map(|slot| **slot)
                .max()
                .filter(|prev| *prev >= now || now.duration_since(*prev) <= PACE_IDLE_RESET)
                .unwrap_or(now);
            let needed = remaining / rate;
            if needed <= window {
                let target = base + Duration::from_secs_f64(needed);
                if target.saturating_duration_since(now) >= PACE_COALESCE {
                    tokio::time::sleep_until(target).await;
                }
                for slot in &mut permits {
                    **slot = Some(target);
                }
                remaining = 0.0;
            } else {
                tokio::time::sleep(Duration::from_secs_f64(window)).await;
                for slot in &mut permits {
                    **slot = None;
                }
                remaining = (remaining - window * rate).max(0.0);
            }
        }
        let reservations = {
            let usage = self.usage.lock().unwrap();
            let mut reservations = Vec::with_capacity(grants.len());
            for (resource, debit, grant) in &grants {
                let row = usage.get(*resource).context("资源账期不存在")?;
                ensure!(row.cycle == grant.cycle && grant.cap.saturating_sub(row.used) >= *debit,
                    "共享资源额度不足");
                reservations.push((resource.as_str(), row.cycle, row.used + debit, grant.cap));
            }
            reservations
        };
        self.quota.cover(&reservations).await?;
        let current_unix = now();
        {
            let mut grants_guard = self.grants.lock().unwrap();
            let mut usage_guard = self.usage.lock().unwrap();
            for (resource, debit, grant) in &grants {
                ensure!(grant.end > current_unix, "资源账期已结束");
                let row = usage_guard.get(*resource).copied().unwrap_or_default();
                ensure!(
                    row.cycle == grant.cycle && grant.cap.saturating_sub(row.used) >= *debit,
                    "共享资源额度不足"
                );
            }
            for (resource, debit, grant) in &grants {
                let row = usage_guard.entry((*resource).clone()).or_insert(UsageRow {
                    cycle: grant.cycle,
                    used: 0,
                    dirty: true,
                });
                row.used = row.used.saturating_add(*debit);
                row.dirty = true;
                if let Some(entry) = grants_guard.get_mut(*resource) {
                    entry.window_used = entry.window_used.saturating_add(*debit);
                }
            }
        }
        Ok(())
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Err(e) = self.flush().and_then(|()| self.settle_quota()) {
            eprintln!("资源账本退出结算失败，保留已预留额度: {e:#}");
        }
        self.flusher.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn delayed_prefetch_and_a_revisited_cycle_preserve_durable_debits() {
        let dir = tempfile::tempdir().unwrap();
        let c = Client::open(dir.path(), "http://127.0.0.1:1".into(), "fixture".into()).unwrap();
        let pool = vec!["plan:fixture".to_owned()];
        let apply = |cycle, expected| {
            c.apply_grant(&pool[0], Grant { cycle, end: now()+120, cap: 128, rate: None, valid_for: 60 },
                Instant::now()+Duration::from_secs(60), MIN_GRANT_WANT, expected);
        };
        apply(10,None);
        c.charge_weighted(&pool,40,2).await.unwrap();
        apply(20,None);
        c.charge_weighted(&pool,5,2).await.unwrap();
        apply(10,Some(10));
        assert_eq!(c.used(&pool[0]).unwrap(),(20,10));
        apply(10,None);
        assert_eq!(c.used(&pool[0]).unwrap(),(10,82));
        c.charge_weighted(&pool,23,2).await.unwrap();
        assert_eq!(c.used(&pool[0]).unwrap(),(10,128));
    }
    #[tokio::test]
    async fn reservation_failure_keeps_multi_budget_debits_atomic_and_can_recover() {
        let dir = tempfile::tempdir().unwrap();
        let c = Client::open(dir.path(), "http://127.0.0.1:1".into(), "fixture".into()).unwrap();
        c.quota.ready().await.unwrap();
        let ids = vec!["a".to_owned(),"b".to_owned(),"other".to_owned()];
        for id in &ids {
            c.apply_grant(id, Grant {cycle:1, end:now()+120, cap:128, rate:None, valid_for:60},
                Instant::now()+Duration::from_secs(60), MIN_GRANT_WANT, None);
        }
        let db = rusqlite::Connection::open(dir.path().join("resource-quota.db")).unwrap();
        db.execute_batch("CREATE TRIGGER reject_b BEFORE INSERT ON reservation WHEN NEW.id='b'
            BEGIN SELECT RAISE(ABORT, 'injected'); END").unwrap();
        assert!(c.charge_weighted(&ids[..2],32,1).await.is_err());
        assert_eq!(c.quota.ceiling("a",1),0);
        assert_eq!(c.used("a").unwrap(),(1,0));
        assert_eq!(c.used("b").unwrap(),(1,0));
        c.charge_weighted(&ids[2..],10,1).await.unwrap();
        db.execute_batch("DROP TRIGGER reject_b").unwrap();
        c.charge_weighted(&ids[..2],32,1).await.unwrap();
        assert_eq!(c.used("b").unwrap(),(1,32));
    }
    #[tokio::test]
    async fn killed_before_statistics_flush_cannot_respend_pool_or_customer_quota() {
        const CASE: &str = "CHAIN_TEST_RESOURCE_CRASH_DIR";
        let ids = vec!["pool".to_owned(), "plan:fixture".to_owned()];
        let cap = 128;
        let open = |dir: &std::path::Path| {
            let c = Client::open(dir, "http://127.0.0.1:1".into(), "fixture".into()).unwrap();
            for id in &ids {
                c.apply_grant(id, Grant { cycle: 10, end: now()+120, cap, rate: None, valid_for: 60 },
                    Instant::now()+Duration::from_secs(60), MIN_GRANT_WANT, None);
            }
            c
        };
        if let Some(dir) = std::env::var_os(CASE) {
            let dir = std::path::Path::new(&dir);
            let c = open(dir);
            let locked = rusqlite::Connection::open(dir.join("resources.db")).unwrap();
            locked.execute_batch("BEGIN IMMEDIATE").unwrap();
            c.charge_weighted(&ids, 80, 1).await.unwrap();
            unsafe { libc::_exit(73) }
        }
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "resources::tests::killed_before_statistics_flush_cannot_respend_pool_or_customer_quota"])
            .env(CASE, dir.path()).status().unwrap();
        assert_eq!(status.code(), Some(73));
        for _ in 0..2 {
            let c = open(dir.path());
            for id in &ids { assert_eq!(c.used(id).unwrap(), (10,82)); }
        }
        let c = open(dir.path());
        c.charge_weighted(&ids,46,1).await.unwrap();
        assert!(c.charge_weighted(&ids,1,1).await.is_err());
        for id in &ids { assert_eq!(c.used(id).unwrap(), (10,128)); }
    }
    #[tokio::test]
    async fn delayed_grant_does_not_block_a_cached_unrelated_resource() {
        use axum::{Json, Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let waiting = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler = {
            let (waiting, release, calls) = (waiting.clone(), release.clone(), calls.clone());
            move |Json(request): Json<Request>| {
                let (waiting, release, calls) = (waiting.clone(), release.clone(), calls.clone());
                async move {
                    if request.resource == "slow" {
                        waiting.notify_one();
                        release.notified().await;
                    } else {
                        calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Json(Grant { cycle: 1, end: now()+120, cap: 1000, rate: None, valid_for: 60 })
                }
            }
        };
        let server = tokio::spawn(async move { axum::serve(listener, Router::new().route("/", post(handler))).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let client = Client::open(dir.path(), format!("http://{address}/"), "test".into()).unwrap();
        client.charge_weighted(&["cached".into()], 7, 1).await.unwrap();
        let other = client.clone();
        let slow = tokio::spawn(async move { other.charge_weighted(&["slow".into()], 5, 1).await });
        tokio::time::timeout(Duration::from_secs(2), waiting.notified()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), client.charge_weighted(&["cached".into()], 9, 1)).await.unwrap().unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        release.notify_one();
        slow.await.unwrap().unwrap();
        assert_eq!((client.used("cached").unwrap().1, client.used("slow").unwrap().1), (16, 5));
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn customer_debits_use_current_multiplier_and_commit_all_budgets_together() {
        use axum::{Json, Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        eprintln!("owned weighted budget listener={address}");
        let router = Router::new().route("/grant",post(|Json(request):Json<Request>| async move {
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
            client.charge_weighted(&resources, 10_000, 1),
            client.charge_weighted(&resources, 10_000, 1)
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
    #[tokio::test]
    async fn pure_quota_grant_survives_offline_controller_until_granted_bytes_exhaust() {
        let dir = tempfile::tempdir().unwrap();
        let client = Client::open(dir.path(), "http://127.0.0.1:1/grant".into(), "fixture".into()).unwrap();
        let pool = vec!["plan:offline".to_owned()];
        client.apply_grant(
            &pool[0],
            Grant { cycle: 1, end: now() + 120, cap: 64, rate: None, valid_for: 0 },
            Instant::now() - Duration::from_secs(5),
            MIN_GRANT_WANT,
            None,
        );
        client.charge_weighted(&pool, 40, 1).await.unwrap();
        assert_eq!(client.used(&pool[0]).unwrap(), (1, 40));
        assert!(client.charge_weighted(&pool, 25, 1).await.is_err());
    }
    #[test]
    fn extended_grant_uses_one_window_and_rejects_oversized_requests_before_allocation() {
        let mut doc = fixture();
        doc.resources.resources[0].monthly_gib = Some(1.0);
        let at = 1_800_000_000;
        let mut request: Request = serde_json::from_value(serde_json::json!({
            "resource":"pool", "epoch":id(), "cycle":0, "used":0,
            "want":MIN_GRANT_WANT, "preferred_want":MAX_GRANT_WANT + 1
        })).unwrap();
        assert!(allocate(&mut doc, "a", &request, at).is_err());
        assert!(doc.resources.ledgers.is_empty());
        request.preferred_want = Some(MAX_GRANT_WANT);
        assert_eq!(allocate(&mut doc, "a", &request, at).unwrap().cap, MAX_GRANT_WANT);
        assert_eq!(allocate(&mut doc, "a", &request, at).unwrap().cap, MAX_GRANT_WANT);
    }
    #[test]
    fn grants_are_idempotent_and_never_overallocate() {
        let mut d = fixture();
        let epoch_a = id();
        let at = 1_800_000_000;
        let mut request = Request {
            resource: "pool".into(),
            epoch: epoch_a.clone(),
            cycle: 0,
            used: 0,
            want: 80,
            preferred_want: None,
            customer_plan_version: 1,
        };
        let a = allocate(&mut d, "a", &request, at).unwrap();
        assert_eq!(a.cap, 80);
        // Sole active entry borrows the full 10 Mbps pool with a shorter 5s elastic lease.
        assert_eq!(a.rate, Some(10.0));
        assert_eq!(a.valid_for, 5);
        assert_eq!(allocate(&mut d, "a", &request, at).unwrap().cap, 80);
        request.epoch = id();
        let b = allocate(&mut d, "b", &request, at).unwrap();
        assert_eq!(b.cap, 20);
        // Once peer node "b" becomes active, it immediately gets its 5 Mbps fair share.
        assert_eq!(b.rate, Some(5.0));
        request.epoch = epoch_a;
        assert_eq!(allocate(&mut d, "a", &request, at + 1).unwrap().rate, Some(5.0));
        // Lost local state gets a new identity, never a refund of previously issued bytes.
        request.epoch = id();
        let reclaimed = allocate(&mut d, "a", &request, at + 20).unwrap();
        assert_eq!(reclaimed.cap, 0);
        // After "b"'s lease expires, "a" reclaims the full 10 Mbps pool.
        assert_eq!(reclaimed.rate, Some(10.0));
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
    #[tokio::test]
    async fn high_bandwidth_shared_resource_is_not_capped_by_1ms_timer_and_preserves_cached_rate() {
        use axum::{Json, Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/grant",
            post(|| async {
                Json(Grant {
                    cycle: 1,
                    end: now() + 120,
                    cap: 100 * 1024 * 1024,
                    rate: Some(500.0), // 500 Mbps = 62.5 MB/s
                    valid_for: 10,
                })
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let client = Client::open(dir.path(), format!("http://{address}/grant"), "test".into()).unwrap();
        let pool = vec!["pool".into()];
        assert_eq!(client.cached_rate(&pool), Some(0.008));
        // Transfer 25 MiB in 64 KiB chunks at 500 Mbps (~420 ms expected; old 1ms sleep took > 1.6s).
        let started = Instant::now();
        for _ in 0..400 {
            client.charge_weighted(&pool, 65_536, 1).await.unwrap();
        }
        let elapsed = started.elapsed().as_secs_f64();
        let mbps = (400.0 * 65_536.0 * 8.0) / elapsed / 1e6;
        assert!(
            (380.0..=560.0).contains(&mbps),
            "500 Mbps 资源池速率不准确: {mbps:.1} Mbps ({elapsed:.3}s)"
        );
        assert_eq!(client.cached_rate(&pool), Some(500.0));
        client.flush().unwrap();
        server.abort();
        let _ = server.await;
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
