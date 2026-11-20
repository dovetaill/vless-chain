//! Per-line data plane: byte accounting, shared rate budgets and local quota enforcement.
//! No host firewall or qdisc is changed. All listeners/tasks belong to this Agent.
use crate::{model::*, runtime::atomic_write};
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, FixedOffset, TimeZone, Utc};
use rusqlite::Connection as Database;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc as std_mpsc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
use tokio_util::io::{InspectReader, InspectWriter};

pub mod capacity;
use capacity::ProxyCapacity;

const TCP_IDLE: Duration = Duration::from_secs(300);
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const PACE_COALESCE: Duration = Duration::from_millis(2);
const PACE_IDLE_RESET: Duration = Duration::from_millis(20);
const INITIAL_TCP_BUF: usize = 8192;
const MAX_TCP_BUF: usize = 65536;
const GATEWAY_COPY_BUF: usize = 32768;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Spec {
    #[serde(default = "one")]
    pub multiplier: u32,
    #[serde(default)]
    pub access_policy: crate::rules::CompiledPolicy,
    #[serde(default)]
    pub resource_ids: Vec<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub lease: Option<LeasePolicy>,
    #[serde(default)]
    pub meter: Option<crate::meter::Spec>,
    #[serde(default)]
    pub proxy_protocol: bool,
    #[serde(default = "all_interfaces")]
    pub listen: String,
    #[serde(default)]
    pub fallback_port: Option<u16>,
    #[serde(default)]
    pub fallback_target: String,
    pub id: String,
    pub port: u16,
    pub backend: u16,
    pub kind: Kind,
    pub protocol: Protocol,
    pub network: String,
    pub limits: Limits,
    pub tls: Tls,
    pub target: String,
}
fn one() -> u32 {
    1
}
fn enabled() -> bool {
    true
}
fn all_interfaces() -> String {
    "0.0.0.0".into()
}
async fn proxy_peer(stream: &mut TcpStream) -> Result<std::net::IpAddr> {
    let mut header = Vec::new();
    while header.len() < 108 && !header.ends_with(b"\r\n") {
        header.push(stream.read_u8().await?);
    }
    ensure!(header.ends_with(b"\r\n"), "PROXY 头过长");
    let text = std::str::from_utf8(&header)?;
    let fields: Vec<_> = text.split_whitespace().collect();
    ensure!(
        fields.len() == 6 && fields[0] == "PROXY" && matches!(fields[1], "TCP4" | "TCP6"),
        "PROXY 头无效"
    );
    let source = fields[2].parse::<std::net::IpAddr>()?;
    fields[3].parse::<std::net::IpAddr>()?;
    fields[4].parse::<u16>()?;
    fields[5].parse::<u16>()?;
    Ok(source)
}

pub struct Book {
    pub resources: RwLock<Option<Arc<crate::resources::Client>>>,
    pub listener_errors: Mutex<BTreeMap<String, String>>,
    pub proxy_capacity: ProxyCapacity,
    path: PathBuf,
    quota: Arc<crate::quota::Journal>,
    ledger: Arc<Mutex<Ledger>>,
    sample: Mutex<(Instant, BTreeMap<String, [u64; 2]>)>,
    snapshot_write: Mutex<()>,
    flusher: Option<std_mpsc::Sender<FlushRequest>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Ledger {
    items: BTreeMap<String, Traffic>,
    dirty: BTreeMap<String, u64>,
    seq: u64,
    day_cache: Option<(i64, String, String)>,
    error: Option<String>,
}

impl Ledger {
    fn billing_row(&mut self, id: &str, lease: Option<&LeasePolicy>, at: u64, month: &str,
        quota: &crate::quota::Journal) -> (&mut Traffic, bool) {
        let row = self.items.get_mut(id).unwrap();
        let rolled = rollover(row, lease, at, month);
        let floor = if rolled { quota.ceiling(id, row.cycle_start) } else { quota.recovered(id, row.cycle_start) };
        let recovered = floor > row.used();
        if recovered {
            row.charged = Some(floor);
        }
        (row, rolled || recovered)
    }

    fn day_labels(&mut self, at: u64) -> (String, String) {
        let utc8_secs = (at as i64).saturating_add(8 * 3600);
        let bucket = utc8_secs.div_euclid(86400);
        if let Some((cached, day, month)) = &self.day_cache {
            if *cached == bucket {
                return (day.clone(), month.clone());
            }
        }
        let local = Utc
            .timestamp_opt(at as i64, 0)
            .single()
            .unwrap()
            .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
        let day = local.format("%Y-%m-%d").to_string();
        let month = local.format("%Y-%m").to_string();
        self.day_cache = Some((bucket, day.clone(), month.clone()));
        (day, month)
    }

    fn mark_dirty(&mut self, id: &str) {
        self.seq = self.seq.wrapping_add(1);
        self.dirty.insert(id.to_owned(), self.seq);
    }

    fn confirmed(&self) -> BTreeMap<String, Traffic> {
        self.items.clone()
    }
}

enum FlushRequest {
    Notify,
    Sync(std_mpsc::SyncSender<io::Result<()>>),
}

const WRITE_USAGE: &str = "INSERT INTO traffic(line_id,body) VALUES(?1,?2) ON CONFLICT(line_id) DO UPDATE SET body=excluded.body";

fn flush_ledger_rows(
    database: &mut Database,
    ledger: &Mutex<Ledger>,
    truncate: bool,
) -> io::Result<()> {
    let pending: Vec<(String, u64, Traffic)> = {
        let state = ledger.lock().unwrap();
        state
            .dirty
            .iter()
            .filter_map(|(id, seq)| state.items.get(id).map(|row| (id.clone(), *seq, row.clone())))
            .collect()
    };
    if pending.is_empty() && ledger.lock().unwrap().error.is_none() {
        if truncate {
            let _ = database.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        }
        return Ok(());
    }
    let outcome = (|| -> Result<()> {
        let tx = database.transaction()?;
        {
            let mut write = tx.prepare_cached(WRITE_USAGE)?;
            for (id, _, row) in &pending {
                write.execute((id, serde_json::to_string(row)?))?;
            }
        }
        tx.commit()?;
        let mode = if truncate { "TRUNCATE" } else { "PASSIVE" };
        let _ = database.execute_batch(&format!("PRAGMA wal_checkpoint({mode});"));
        Ok(())
    })();
    let mut state = ledger.lock().unwrap();
    match outcome {
        Ok(()) => {
            state.error = None;
            for (id, seq, _) in pending {
                if state.dirty.get(&id).copied() == Some(seq) {
                    state.dirty.remove(&id);
                }
            }
            Ok(())
        }
        Err(e) => {
            let msg = format!("流量账本刷盘失败: {e:#}");
            state.error = Some(msg.clone());
            Err(io::Error::other(msg))
        }
    }
}

fn flush_loop(
    mut database: Database,
    requests: std_mpsc::Receiver<FlushRequest>,
    ledger: Arc<Mutex<Ledger>>,
) {
    loop {
        match requests.recv_timeout(FLUSH_INTERVAL) {
            Ok(FlushRequest::Notify) => {
                while let Ok(msg) = requests.try_recv() {
                    if let FlushRequest::Sync(done) = msg {
                        let _ = done.send(flush_ledger_rows(&mut database, &ledger, false));
                    }
                }
                let _ = flush_ledger_rows(&mut database, &ledger, false);
            }
            Ok(FlushRequest::Sync(done)) => {
                let _ = done.send(flush_ledger_rows(&mut database, &ledger, false));
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                let _ = flush_ledger_rows(&mut database, &ledger, false);
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                let _ = flush_ledger_rows(&mut database, &ledger, true);
                break;
            }
        }
    }
}

impl Book {
    pub fn open(dir: &std::path::Path) -> Result<Arc<Self>> {
        let path = dir.join("traffic.json");
        let mut database = Database::open(dir.join("traffic.db"))?;
        database.busy_timeout(Duration::from_secs(5))?;
        database.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0; \
             CREATE TABLE IF NOT EXISTS traffic(line_id TEXT PRIMARY KEY,body TEXT NOT NULL); \
             CREATE TABLE IF NOT EXISTS meta(name TEXT PRIMARY KEY,value INTEGER NOT NULL);",
        )?;
        let initialized: bool = database.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE name='initialized' AND value=1)",
            [],
            |row| row.get(0),
        )?;
        if !initialized {
            let old: BTreeMap<String, Traffic> = if path.exists() {
                serde_json::from_slice(&std::fs::read(&path)?)?
            } else {
                BTreeMap::new()
            };
            // Import and the completion marker commit together. A crash during
            // initialization cannot turn an old JSON ledger into a fresh quota.
            let transaction = database.transaction()?;
            for (id, traffic) in old {
                transaction.execute(
                    "INSERT INTO traffic(line_id,body) VALUES(?1,?2)",
                    (id, serde_json::to_string(&traffic)?),
                )?;
            }
            transaction.execute("INSERT INTO meta(name,value) VALUES('initialized',1)", [])?;
            transaction.commit()?;
        }
        let mut items = BTreeMap::new();
        {
            let mut statement = database.prepare("SELECT line_id,body FROM traffic")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, body) = row?;
                items.insert(id, serde_json::from_str::<Traffic>(&body)?);
            }
        }
        for v in items.values_mut() {
            v.connections = 0;
            v.upload_bps = 0.0;
            v.download_bps = 0.0;
        }
        let quota = crate::quota::Journal::open(&dir.join("line-quota.db"));
        let ledger = Arc::new(Mutex::new(Ledger { items, ..Default::default() }));
        let (flusher, receiver) = std_mpsc::channel();
        let writer_ledger = ledger.clone();
        let writer = std::thread::Builder::new()
            .name("chain-traffic".into())
            .spawn(move || flush_loop(database, receiver, writer_ledger))?;
        Ok(Arc::new(Self {
            resources: RwLock::new(None),
            listener_errors: Mutex::default(),
            proxy_capacity: ProxyCapacity::for_node(),
            path,
            quota,
            ledger,
            sample: Mutex::new((Instant::now(), BTreeMap::new())),
            snapshot_write: Mutex::new(()),
            flusher: Some(flusher),
            writer: Some(writer),
        }))
    }
    pub(crate) fn notify_flush(&self) {
        if let Some(flusher) = &self.flusher {
            let _ = flusher.send(FlushRequest::Notify);
        }
    }
    pub fn flush(&self) -> Result<()> {
        if let Some(resources) = self.resources.read().unwrap().clone() {
            resources.flush()?;
        }
        if let Some(flusher) = &self.flusher {
            let (tx, rx) = std_mpsc::sync_channel(1);
            flusher
                .send(FlushRequest::Sync(tx))
                .map_err(|_| anyhow::anyhow!("流量账本写入器已停止"))?;
            rx.recv()
                .map_err(|_| anyhow::anyhow!("流量账本写入器未确认刷盘"))??;
        }
        Ok(())
    }
    pub fn check_ledger(&self) -> Result<()> {
        let error = self.ledger.lock().unwrap().error.clone();
        if let Some(error) = error {
            self.notify_flush();
            anyhow::bail!("{error}");
        }
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        self.flush()?;
        let _writer = self.snapshot_write.lock().unwrap();
        let rows = self.ledger.lock().unwrap().confirmed();
        let bytes = serde_json::to_vec(&rows)?;
        atomic_write(&self.path, &bytes)
    }
    fn settle_quota(&self) -> Result<()> {
        let rows = self.ledger.lock().unwrap().items.iter()
            .map(|(id, row)| (id.clone(), row.cycle_start, row.used())).collect();
        self.quota.settle(rows)
    }
    pub(crate) fn finish(&self) -> Result<()> {
        self.save()?;
        self.settle_quota()?;
        if let Some(client) = self.resources.read().unwrap().clone() {
            client.settle_quota()?;
        }
        Ok(())
    }
    pub fn snapshot(&self) -> BTreeMap<String, Traffic> {
        let mut sample = self.sample.lock().unwrap();
        let elapsed = sample.0.elapsed().as_secs_f64();
        let mut ledger = self.ledger.lock().unwrap();
        let mut items = ledger.confirmed();
        for (id, t) in items.iter_mut() {
            let old = sample
                .1
                .get(id)
                .copied()
                .unwrap_or([t.total_upload, t.total_download]);
            if elapsed >= 0.5 {
                t.upload_bps = t.total_upload.saturating_sub(old[0]) as f64 / elapsed;
                t.download_bps = t.total_download.saturating_sub(old[1]) as f64 / elapsed;
                let live = ledger.items.get_mut(id).unwrap();
                live.upload_bps = t.upload_bps;
                live.download_bps = t.download_bps;
                sample
                    .1
                    .insert(id.clone(), [t.total_upload, t.total_download]);
            }
            t.sampled_at = now();
        }
        if elapsed >= 0.5 {
            sample.0 = Instant::now();
        }
        items
    }
}
impl Drop for Book {
    fn drop(&mut self) {
        if let Err(e) = self.flush().and_then(|()| self.settle_quota()) {
            eprintln!("流量账本退出结算失败，保留已预留额度: {e:#}");
        }
        self.flusher.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}
fn rollover(t: &mut Traffic, lease: Option<&LeasePolicy>, at: u64, month: &str) -> bool {
    let anchor = lease.map(|p| p.cycle_anchor).unwrap_or_else(|| {
        let local = Utc
            .timestamp_opt(at as i64, 0)
            .single()
            .unwrap()
            .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
        local
            .timezone()
            .with_ymd_and_hms(local.year(), local.month(), 1, 0, 0, 0)
            .single()
            .unwrap()
            .timestamp() as u64
    });
    let (start, end) = if anchor == 0 {
        (0, 0)
    } else {
        crate::rental::cycle(anchor, at)
    };
    let mut rolled = false;
    if t.cycle_start != start {
        t.cycle_start = start;
        t.upload = 0;
        t.download = 0;
        t.charged = Some(0);
        t.blocked = false;
        rolled = true;
    }
    t.cycle_end = end;
    if t.month != month {
        t.month.clear();
        t.month.push_str(month);
    }
    while t.days.len() > 35 {
        let key = t.days.keys().next().unwrap().clone();
        t.days.remove(&key);
    }
    rolled
}

#[derive(Clone)]
struct Policy {
    multiplier: u32,
    access_policy: crate::rules::CompiledPolicy,
    resource_ids: Vec<String>,
    enabled: bool,
    lease: Option<LeasePolicy>,
    limits: Limits,
}
impl Policy {
    fn open(&self, at: u64) -> bool {
        self.enabled
            && self.lease.as_ref().is_none_or(|p| {
                p.cycle_anchor > 0
                    && at >= p.cycle_anchor
                    && (p.expires_at == 0 || at < p.expires_at)
            })
    }
}
pub struct Lane {
    pub id: String,
    pub book: Arc<Book>,
    policy: RwLock<Policy>,
    directions: [tokio::sync::Mutex<Option<tokio::time::Instant>>; 2],
    customer_charge: tokio::sync::Mutex<()>,
    rates: tokio::sync::watch::Sender<[Option<f64>; 2]>,
    stopped: Mutex<CancellationToken>,
    changed: tokio::sync::Notify,
    admissions: AtomicUsize,
    transferring: AtomicUsize,
    drained: tokio::sync::Notify,
    quota_deadline: Mutex<Option<Instant>>,
}
impl Lane {
    pub fn new(spec: &Spec, book: Arc<Book>) -> Arc<Self> {
        book.ledger
            .lock()
            .unwrap()
            .items
            .entry(spec.id.clone())
            .or_insert_with(|| Traffic {
                epoch: id(),
                ..Default::default()
            });
        Arc::new(Self {
            id: spec.id.clone(),
            book,
            policy: RwLock::new(Policy {
                multiplier: spec.multiplier.max(1),
                access_policy: spec.access_policy.clone(),
                resource_ids: spec.resource_ids.clone(),
                enabled: spec.enabled,
                lease: spec.lease.clone(),
                limits: spec.limits.clone(),
            }),
            directions: [tokio::sync::Mutex::new(None), tokio::sync::Mutex::new(None)],
            customer_charge: tokio::sync::Mutex::new(()),
            rates: tokio::sync::watch::channel([
                spec.limits.upload_mbps,
                spec.limits.download_mbps,
            ])
            .0,
            stopped: Mutex::new(CancellationToken::new()),
            changed: tokio::sync::Notify::new(),
            admissions: AtomicUsize::new(0),
            transferring: AtomicUsize::new(0),
            drained: tokio::sync::Notify::new(),
            quota_deadline: Mutex::new(None),
        })
    }
    pub fn update(&self, spec: &Spec) {
        *self.policy.write().unwrap() = Policy {
            multiplier: spec.multiplier.max(1),
            access_policy: spec.access_policy.clone(),
            resource_ids: spec.resource_ids.clone(),
            enabled: spec.enabled,
            lease: spec.lease.clone(),
            limits: spec.limits.clone(),
        };
        let rates = [spec.limits.upload_mbps, spec.limits.download_mbps];
        self.rates.send_if_modified(|current| {
            if *current == rates {
                return false;
            }
            *current = rates;
            true
        });
        self.available();
        self.changed.notify_one();
    }
    pub fn available(&self) -> bool {
        self.available_at(now())
    }
    pub(crate) fn has_headroom(&self, min_bytes: usize) -> bool {
        let policy = self.policy.read().unwrap();
        let Some(limit_gib) = policy.limits.monthly_gib else {
            return policy.resource_ids.is_empty();
        };
        if !policy.resource_ids.is_empty() {
            return false;
        }
        let state = self.book.ledger.lock().unwrap();
        let Some(t) = state.items.get(&self.id) else {
            return false;
        };
        let left = ((limit_gib * 1073741824.0) as u64).saturating_sub(t.used())
            / policy.multiplier.max(1) as u64;
        left >= min_bytes as u64
    }
    fn should_stop(&self, forced: bool, exhausted: bool) -> bool {
        let mut deadline = self.quota_deadline.lock().unwrap();
        if forced || !exhausted || self.transferring.load(Ordering::Acquire) == 0 {
            *deadline = None;
            return forced || exhausted;
        }
        if deadline.is_none() {
            *deadline = Some(Instant::now() + Duration::from_secs(5));
            self.changed.notify_one();
        }
        if Instant::now() >= deadline.unwrap() {
            *deadline = None;
            return true;
        }
        false
    }
    fn available_at(&self, at: u64) -> bool {
        let policy = self.policy.read().unwrap();
        let mut flush_now = false;
        let (forced, exhausted, blocked) = {
            let mut state = self.book.ledger.lock().unwrap();
            let forced = !policy.open(at);
            let (_, month) = state.day_labels(at);
            let (t, rolled) = state.billing_row(&self.id, policy.lease.as_ref(), at, &month, &self.book.quota);
            let exhausted = policy.limits.monthly_gib.is_some_and(|q| {
                t.used() >= (q * 1073741824.0) as u64
                    || ((q * 1073741824.0) as u64).saturating_sub(t.used())
                        < policy.multiplier as u64
            });
            let blocked = forced || exhausted;
            let newly_blocked = !t.blocked && blocked;
            t.blocked = blocked;
            if rolled || newly_blocked {
                state.mark_dirty(&self.id);
                flush_now = true;
            }
            (forced, exhausted, blocked)
        };
        if flush_now {
            self.book.notify_flush();
        }
        if self.should_stop(forced, exhausted) {
            self.stopped.lock().unwrap().cancel();
        }
        !blocked
    }
    pub fn cancellation(&self) -> CancellationToken {
        let at = now();
        let policy = self.policy.read().unwrap();
        let mut flush_now = false;
        let (forced, exhausted, blocked) = {
            let mut state = self.book.ledger.lock().unwrap();
            let forced = !policy.open(at);
            let (_, month) = state.day_labels(at);
            let (t, rolled) = state.billing_row(&self.id, policy.lease.as_ref(), at, &month, &self.book.quota);
            let exhausted = policy.limits.monthly_gib.is_some_and(|q| {
                t.used() >= (q * 1073741824.0) as u64
                    || ((q * 1073741824.0) as u64).saturating_sub(t.used())
                        < policy.multiplier as u64
            });
            let blocked = forced || exhausted;
            let newly_blocked = !t.blocked && blocked;
            t.blocked = blocked;
            if rolled || newly_blocked {
                state.mark_dirty(&self.id);
                flush_now = true;
            }
            (forced, exhausted, blocked)
        };
        if flush_now {
            self.book.notify_flush();
        }
        let mut stopped = self.stopped.lock().unwrap();
        if self.should_stop(forced, exhausted) {
            stopped.cancel();
        } else if !blocked && stopped.is_cancelled() {
            *stopped = CancellationToken::new();
        }
        stopped.clone()
    }
    async fn supervise(self: Arc<Self>, cancel: CancellationToken) {
        loop {
            let at = now();
            self.available_at(at);
            let next = {
                let policy = self.policy.read().unwrap();
                let end = self.book.ledger.lock().unwrap().items[&self.id].cycle_end;
                [
                    Some(end),
                    policy.lease.as_ref().map(|p| p.expires_at),
                    policy.lease.as_ref().map(|p| p.cycle_anchor),
                ]
                .into_iter()
                .flatten()
                .filter(|t| *t > at)
                .min()
                .unwrap_or(at + 86400)
            };
            let mut wait = Duration::from_secs(next).saturating_sub(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default(),
            );
            if let Some(deadline) = *self.quota_deadline.lock().unwrap() {
                wait = wait.min(deadline.saturating_duration_since(Instant::now()));
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = self.changed.notified() => {},
                _ = tokio::time::sleep(wait) => {},
            }
        }
    }
    async fn drain_transfers(&self) {
        loop {
            let drained = self.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            let stopped = self.cancellation();
            if self.transferring.load(Ordering::Acquire) == 0 {
                return;
            }
            tokio::select! {
                _ = stopped.cancelled() => return,
                _ = drained => {},
            }
        }
    }
    pub async fn charge(
        self: &Arc<Self>,
        amount: usize,
        upload: bool,
        datagram: bool,
    ) -> io::Result<usize> {
        Ok(self.transfer(amount, upload, datagram).await?.amount)
    }
    pub async fn transfer(
        self: &Arc<Self>,
        amount: usize,
        upload: bool,
        datagram: bool,
    ) -> io::Result<Transfer> {
        self.transfer_active(amount, upload, datagram, None).await
    }

    pub(crate) async fn transfer_active(
        self: &Arc<Self>,
        amount: usize,
        upload: bool,
        datagram: bool,
        activity: Option<&TcpActivity>,
    ) -> io::Result<Transfer> {
        let mut transfer = Transfer {
            amount: 0,
            lane: self.clone(),
            counted: false,
        };
        if amount == 0 {
            return Ok(transfer);
        }
        if self.policy.read().unwrap().limits.monthly_gib.is_some() {
            self.book.quota.ready().await.map_err(io::Error::other)?;
        }
        if !self.available() {
            self.drain_transfers().await;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "本月线路额度已用尽",
            ));
        }
        let index = if upload { 0 } else { 1 };
        let stopped = self.cancellation();
        let pacing = activity.map(TcpActivity::waiting);
        // Fast-skip the per-direction async Mutex completely when no rate limit is configured.
        let _direction = if self.rates.borrow()[index].is_some() {
            let mut next_ready = tokio::select! {
                _ = stopped.cancelled() => return Err(io::ErrorKind::PermissionDenied.into()),
                permit = self.directions[index].lock() => permit,
            };
            let mut rates = self.rates.subscribe();
            let mut unpaced = amount as f64;
            while unpaced > 0.0 {
                let Some(rate) = rates.borrow_and_update()[index] else {
                    *next_ready = None;
                    break;
                };
                let bps = rate * 125000.0;
                let now = tokio::time::Instant::now();
                let base = next_ready
                    .filter(|prev| *prev >= now || now.duration_since(*prev) <= PACE_IDLE_RESET)
                    .unwrap_or(now);
                let target = base + Duration::from_secs_f64(unpaced / bps);
                if target.saturating_duration_since(now) < PACE_COALESCE {
                    *next_ready = Some(target);
                    break;
                }
                tokio::select! {
                    _ = stopped.cancelled() => {
                        // Leave no future reservations behind on cancellation.
                        *next_ready = None;
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    _ = rates.changed() => {
                        let elapsed = tokio::time::Instant::now()
                            .saturating_duration_since(base)
                            .as_secs_f64();
                        unpaced = (unpaced - elapsed * bps).max(0.0);
                        *next_ready = None;
                        continue;
                    }
                    _ = tokio::time::sleep_until(target) => {
                        *next_ready = Some(target);
                        break;
                    }
                }
            }
            Some(next_ready)
        } else {
            None
        };
        let (has_resources, customer_budget, local_limit) = {
            let p = self.policy.read().unwrap();
            (!p.resource_ids.is_empty(), p.resource_ids.iter().any(|id| crate::plans::is_budget(id)),
                p.limits.monthly_gib.is_some())
        };
        let (customer_permit, mut effective_amount, cached_multiplier) = if has_resources || local_limit {
            // Both directions must agree on the local remainder before charging the shared customer ledger.
            let customer_permit = if customer_budget || local_limit {
                Some(tokio::select! {
                    _ = stopped.cancelled() => return Err(io::ErrorKind::PermissionDenied.into()),
                    permit = self.customer_charge.lock() => permit,
                })
            } else {
                None
            };
            let (resource_ids, multiplier, used, cycle, limit, flush_rollover) = {
                let p = self.policy.read().unwrap();
                let mut state = self.book.ledger.lock().unwrap();
                let at = now();
                let (_, month) = state.day_labels(at);
                let (row, rolled) = state.billing_row(&self.id, p.lease.as_ref(), at, &month, &self.book.quota);
                let (used, cycle) = (row.used(), row.cycle_start);
                if rolled {
                    state.mark_dirty(&self.id);
                }
                (
                    p.resource_ids.clone(),
                    p.multiplier,
                    used, cycle, p.limits.monthly_gib.map(|q| (q * 1073741824.0) as u64),
                    rolled,
                )
            };
            if flush_rollover {
                self.book.notify_flush();
            }
            let quota_left = limit.map(|cap| cap.saturating_sub(used) / multiplier as u64)
                .unwrap_or(u64::MAX);
            if datagram && amount as u64 > quota_left {
                drop(customer_permit);
                drop(_direction);
                self.drain_transfers().await;
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let capped = amount.min(quota_left.min(usize::MAX as u64) as usize);
            if capped == 0 {
                drop(customer_permit);
                drop(_direction);
                self.drain_transfers().await;
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            if let Some(cap) = limit {
                self.book.quota.ready().await.map_err(io::Error::other)?;
                let end = used + capped as u64 * multiplier as u64;
                if used < self.book.quota.recovered(&self.id, cycle) {
                    return Err(io::Error::other("线路账本刚刚恢复，需重新核对余额"));
                }
                self.book.quota.cover(&[(&self.id, cycle, end, cap)]).await.map_err(io::Error::other)?;
            }
            if !resource_ids.is_empty() {
                let client = self
                    .book
                    .resources
                    .read()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| io::Error::other("Agent 尚未连接资源额度服务"))?;
                tokio::select! {
                    _ = stopped.cancelled() => return Err(io::ErrorKind::PermissionDenied.into()),
                    result = client.charge_weighted(&resource_ids, capped, multiplier) => result.map_err(io::Error::other)?,
                }
            }
            (customer_permit, capped, customer_budget.then_some(multiplier))
        } else {
            (None, amount, None)
        };
        drop(pacing);
        let mut flush_now = false;
        let reserved: io::Result<()> = (|| {
            let policy = self.policy.read().unwrap();
            let billing_multiplier = cached_multiplier.unwrap_or(policy.multiplier);
            let at = now();
            if stopped.is_cancelled() || !policy.open(at) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let limits = &policy.limits;
            let mut state = self.book.ledger.lock().unwrap();
            let (day_key, month_key) = state.day_labels(at);
            let (t, rolled) = state.billing_row(&self.id, policy.lease.as_ref(), at, &month_key, &self.book.quota);
            let left = limits
                .monthly_gib
                .map(|q| ((q * 1073741824.0) as u64).saturating_sub(t.used()))
                .unwrap_or(u64::MAX);
            effective_amount = effective_amount
                .min((left / billing_multiplier as u64).min(usize::MAX as u64) as usize);
            if effective_amount == 0 || datagram && effective_amount < amount {
                let was_blocked = t.blocked;
                t.blocked = left < policy.multiplier as u64;
                if t.blocked {
                    self.changed.notify_one();
                }
                if rolled || (!was_blocked && t.blocked) {
                    state.mark_dirty(&self.id);
                    flush_now = true;
                }
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "本月线路额度不足",
                ));
            }
            let debit_end = t.used().saturating_add((effective_amount as u64).saturating_mul(billing_multiplier as u64));
            if limits.monthly_gib.is_some() && self.book.quota.ceiling(&self.id, t.cycle_start) < debit_end {
                return Err(io::Error::other("线路额度或账期已切换，需重新预留"));
            }
            t.charged = Some(debit_end);
            let day = t.days.entry(day_key).or_default();
            if upload {
                t.upload = t.upload.saturating_add(effective_amount as u64);
                t.total_upload = t.total_upload.saturating_add(effective_amount as u64);
                day[0] = day[0].saturating_add(effective_amount as u64);
            } else {
                t.download = t.download.saturating_add(effective_amount as u64);
                t.total_download = t.total_download.saturating_add(effective_amount as u64);
                day[1] = day[1].saturating_add(effective_amount as u64);
            }
            let was_blocked = t.blocked;
            t.blocked = limits.monthly_gib.is_some_and(|q| {
                t.used() >= (q * 1073741824.0) as u64
                    || ((q * 1073741824.0) as u64).saturating_sub(t.used())
                        < policy.multiplier as u64
            });
            if t.blocked {
                self.changed.notify_one();
            }
            let now_blocked = t.blocked;
            state.mark_dirty(&self.id);
            if rolled || (!was_blocked && now_blocked) {
                flush_now = true;
            }
            self.transferring.fetch_add(1, Ordering::AcqRel);
            transfer.counted = true;
            transfer.amount = effective_amount;
            Ok(())
        })();
        drop(customer_permit);
        drop(_direction);
        if flush_now {
            self.book.notify_flush();
        }
        if let Err(error) = reserved {
            drop(transfer);
            self.drain_transfers().await;
            return Err(error);
        }
        Ok(transfer)
    }
    fn connection_limit(&self) -> usize {
        self.policy
            .read()
            .unwrap()
            .access_policy
            .policy
            .max_connections
            .unwrap_or(crate::rules::DEFAULT_PROXY_CONNECTIONS)
            .min(crate::rules::MAX_PROXY_CONNECTIONS)
    }
    pub(crate) fn transport_limit(&self) -> usize {
        // An explicit business limit applies at the meter, after core authentication.
        self.connection_limit().max(crate::rules::DEFAULT_PROXY_CONNECTIONS)
    }
    pub fn admit(self: &Arc<Self>) -> io::Result<Admission> {
        let limit = self.connection_limit();
        let mut current = self.admissions.load(Ordering::Acquire);
        loop {
            if current >= limit {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "代理并发连接达到上限",
                ));
            }
            match self.admissions.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Admission(self.clone())),
                Err(actual) => current = actual,
            }
        }
    }
    pub fn access_policy(&self) -> crate::rules::CompiledPolicy { self.policy.read().unwrap().access_policy.clone() }
    pub fn connection(self: &Arc<Self>) -> Connection {
        self.book
            .ledger
            .lock()
            .unwrap().items
            .get_mut(&self.id)
            .unwrap()
            .connections += 1;
        Connection(self.clone())
    }
}
pub struct Admission(Arc<Lane>);
impl Drop for Admission { fn drop(&mut self) { self.0.admissions.fetch_sub(1,Ordering::AcqRel); } }
pub struct Transfer {
    pub amount: usize,
    lane: Arc<Lane>,
    counted: bool,
}
impl Drop for Transfer {
    fn drop(&mut self) {
        if self.counted && self.lane.transferring.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.lane.available();
            self.lane.drained.notify_waiters();
        }
    }
}
pub struct Connection(Arc<Lane>);
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(t) = self.0.book.ledger.lock().unwrap().items.get_mut(&self.0.id) {
            t.connections = t.connections.saturating_sub(1);
        }
    }
}

pub(crate) struct TcpActivity {
    epoch: tokio::time::Instant,
    last_ms: AtomicU64,
    waiting: AtomicUsize,
}
pub(crate) struct Pacing<'a>(&'a TcpActivity);

impl TcpActivity {
    pub(crate) fn new() -> Self {
        Self {
            epoch: tokio::time::Instant::now(),
            last_ms: AtomicU64::new(0),
            waiting: AtomicUsize::new(0),
        }
    }
    pub(crate) fn mark_progress(&self) {
        let ms = tokio::time::Instant::now()
            .saturating_duration_since(self.epoch)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        self.last_ms.store(ms, Ordering::Release);
    }
    fn progress(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.mark_progress();
        }
    }
    pub(crate) fn waiting(&self) -> Pacing<'_> {
        self.waiting.fetch_add(1, Ordering::AcqRel);
        Pacing(self)
    }
    pub(crate) async fn idle(&self) -> io::Result<()> {
        loop {
            let waiting = self.waiting.load(Ordering::Acquire);
            let last = self.epoch + Duration::from_millis(self.last_ms.load(Ordering::Acquire));
            let deadline = if waiting > 0 {
                tokio::time::Instant::now() + TCP_IDLE
            } else {
                last + TCP_IDLE
            };
            tokio::time::sleep_until(deadline).await;
            let waiting = self.waiting.load(Ordering::Acquire);
            let last = self.epoch + Duration::from_millis(self.last_ms.load(Ordering::Acquire));
            if waiting == 0 && tokio::time::Instant::now().saturating_duration_since(last) >= TCP_IDLE {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
    }
}
impl Drop for Pacing<'_> {
    fn drop(&mut self) {
        self.0.mark_progress();
        self.0.waiting.fetch_sub(1, Ordering::AcqRel);
    }
}

fn configure_tcp_keepalive(stream: &TcpStream) {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let enable: libc::c_int = 1;
    let idle: libc::c_int = 60;
    let interval: libc::c_int = 15;
    let count: libc::c_int = 4;
    let size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    unsafe {
        let _ = libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, (&enable as *const libc::c_int).cast(), size);
        #[cfg(target_os = "linux")]
        {
            let _ = libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, (&idle as *const libc::c_int).cast(), size);
            let _ = libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, (&interval as *const libc::c_int).cast(), size);
            let _ = libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, (&count as *const libc::c_int).cast(), size);
        }
        #[cfg(target_os = "macos")]
        {
            let _ = libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPALIVE, (&idle as *const libc::c_int).cast(), size);
            let _ = libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, (&interval as *const libc::c_int).cast(), size);
            let _ = libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, (&count as *const libc::c_int).cast(), size);
        }
    }
}

pub async fn pump<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    r: R,
    w: W,
    lane: Arc<Lane>,
    upload: bool,
) -> io::Result<()> {
    pump_tcp(r, w, lane, upload, None).await
}

async fn pump_tcp<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut r: R,
    mut w: W,
    lane: Arc<Lane>,
    upload: bool,
    activity: Option<&TcpActivity>,
) -> io::Result<()> {
    // Start with a compact 8 KiB buffer for idle/interactive connections and expand to 64 KiB
    // as soon as a bulk stream fills the initial read buffer.
    let mut buf = vec![0u8; INITIAL_TCP_BUF];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            return w.shutdown().await;
        }
        let filled = n == buf.len() && buf.len() < MAX_TCP_BUF;
        let index = usize::from(!upload);
        let rate = lane.rates.borrow()[index];
        let shared_rate = {
            let policy = lane.policy.read().unwrap();
            if policy.resource_ids.is_empty() {
                None
            } else {
                lane.book
                    .resources
                    .read()
                    .unwrap()
                    .as_ref()
                    .and_then(|client| client.cached_rate(&policy.resource_ids))
            }
        };
        let combined_rate = rate.into_iter().chain(shared_rate).reduce(f64::min);
        let mut offset = 0;
        while offset < n {
            let mut quantum = n - offset;
            if let Some(rate) = combined_rate {
                // At low rates share roughly one second of budget across the active streams.
                let streams = lane.book.ledger.lock().unwrap().items[&lane.id]
                    .connections
                    .max(1);
                quantum = quantum
                    .min((rate * 125000.0 / streams as f64) as usize)
                    .max(1);
            }
            let transfer = lane
                .transfer_active(quantum, upload, false, activity)
                .await?;
            w.write_all(&buf[offset..offset + transfer.amount]).await?;
            offset += transfer.amount;
            let truncated = transfer.amount < quantum;
            drop(transfer);
            if truncated {
                lane.drain_transfers().await;
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "本月线路额度已用尽",
                ));
            }
        }
        if filled {
            buf.resize(MAX_TCP_BUF, 0);
        }
    }
}

pub async fn relay_tcp(a: TcpStream, b: TcpStream, lane: Arc<Lane>) -> io::Result<()> {
    let stopped = lane.cancellation();
    let _connection = lane.connection();
    let activity = TcpActivity::new();
    let progress = |bytes: &[u8]| activity.progress(bytes);
    let (ar, aw) = a.into_split();
    let (br, bw) = b.into_split();
    let ar = InspectReader::new(ar, progress);
    let aw = InspectWriter::new(aw, progress);
    let br = InspectReader::new(br, progress);
    let bw = InspectWriter::new(bw, progress);
    let copying = async {
        tokio::try_join!(
            pump_tcp(ar, bw, lane.clone(), true, Some(&activity)),
            pump_tcp(br, aw, lane, false, Some(&activity))
        )?;
        Ok(())
    };
    tokio::select! {
        _ = stopped.cancelled() => Err(io::ErrorKind::PermissionDenied.into()),
        result = async {
            tokio::select! {
                biased;
                res = copying => res,
                res = activity.idle() => res,
            }
        } => result,
    }
}

pub async fn relay<A: AsyncRead + AsyncWrite + Unpin, B: AsyncRead + AsyncWrite + Unpin>(
    a: A,
    b: B,
    lane: Arc<Lane>,
) -> io::Result<()> {
    let stopped = lane.cancellation();
    let _connection = lane.connection();
    tokio::select! {
        _ = stopped.cancelled() => Err(io::ErrorKind::PermissionDenied.into()),
        result = copy_tcp(a, b, Some(lane)) => result,
    }
}

async fn copy_tcp<A: AsyncRead + AsyncWrite + Unpin, B: AsyncRead + AsyncWrite + Unpin>(
    a: A,
    b: B,
    meter: Option<Arc<Lane>>,
) -> io::Result<()> {
    let activity = TcpActivity::new();
    let progress = |bytes: &[u8]| activity.progress(bytes);
    let mut a = InspectWriter::new(InspectReader::new(a, progress), progress);
    let mut b = InspectWriter::new(InspectReader::new(b, progress), progress);
    let copying = async {
        if let Some(lane) = meter {
            let (ar, aw) = tokio::io::split(a);
            let (br, bw) = tokio::io::split(b);
            tokio::try_join!(
                pump_tcp(ar, bw, lane.clone(), true, Some(&activity)),
                pump_tcp(br, aw, lane, false, Some(&activity))
            )?;
        } else {
            tokio::io::copy_bidirectional_with_sizes(&mut a, &mut b, GATEWAY_COPY_BUF, GATEWAY_COPY_BUF).await?;
        }
        Ok(())
    };
    tokio::select! {
        biased;
        result = copying => result,
        result = activity.idle() => result,
    }
}

// Each authenticated SOCKS5 association owns a public UDP relay. Sharing the
// fixed listener would let an unauthenticated client behind the same NAT use it.
struct SocksUdp {
    public: UdpSocket,
    backend: UdpSocket,
    client_port: u16,
}
async fn socks_handshake<A: AsyncRead + AsyncWrite + Unpin>(
    client: &mut A,
    backend: &mut TcpStream,
    local: std::net::SocketAddr,
    metered_lane: Option<&Lane>,
) -> io::Result<Option<SocksUdp>> {
    let mut head = [0u8; 2];
    client.read_exact(&mut head).await?;
    if head[0] != 5 || head[1] == 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods).await?;
    backend.write_all(&head).await?;
    backend.write_all(&methods).await?;
    backend.read_exact(&mut head).await?;
    client.write_all(&head).await?;
    if head[1] == 2 {
        client.read_exact(&mut head).await?;
        backend.write_all(&head).await?;
        let mut name = vec![0u8; head[1] as usize];
        client.read_exact(&mut name).await?;
        backend.write_all(&name).await?;
        let count = client.read_u8().await?;
        backend.write_u8(count).await?;
        let mut password = vec![0u8; count as usize];
        client.read_exact(&mut password).await?;
        backend.write_all(&password).await?;
        backend.read_exact(&mut head).await?;
        client.write_all(&head).await?;
        if head[1] != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    } else if head[1] != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut req = [0u8; 4];
    client.read_exact(&mut req).await?;
    let address = read_socks_address(client, req[3]).await?;
    // Xray acknowledges CONNECT before dialing the meter. Keep its atomic admission at the meter.
    if req[..3] == [5, 1, 0] && metered_lane.is_some_and(|lane| {
        lane.admissions.load(Ordering::Acquire) >= lane.connection_limit()
            || lane.book.proxy_capacity.meter.available_permits() == 0
    }) {
        client.write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        client.flush().await?;
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let udp_backend = if req[..3] == [5, 3, 0] {
        // Xray validates the source port of the loopback socket that sends the datagrams.
        let socket = UdpSocket::bind(("127.0.0.1", 0)).await?;
        backend.write_all(&[5, 3, 0, 1, 127, 0, 0, 1]).await?;
        backend.write_all(&socket.local_addr()?.port().to_be_bytes()).await?;
        Some(socket)
    } else {
        backend.write_all(&req).await?;
        backend.write_all(&address).await?;
        None
    };
    let mut reply = [0u8; 4];
    backend.read_exact(&mut reply).await?;
    let addr = read_socks_address(backend, reply[3]).await?;
    if let Some(udp_backend) = udp_backend.filter(|_| reply[1] == 0) {
        let port = u16::from_be_bytes(addr[addr.len() - 2..].try_into().unwrap());
        if port == 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bind = local;
        bind.set_port(0);
        if local.ip().to_canonical().is_ipv4() {
            // Retain the IPv4 wildcard reply for clients reaching the server through NAT.
            bind = ([0, 0, 0, 0], 0).into();
        }
        let public = UdpSocket::bind(bind).await?;
        udp_backend.connect(("127.0.0.1", port)).await?;
        match bind.ip() {
            std::net::IpAddr::V4(ip) => {
                client.write_all(&[5, 0, 0, 1]).await?;
                client.write_all(&ip.octets()).await?;
            }
            std::net::IpAddr::V6(ip) => {
                client.write_all(&[5, 0, 0, 4]).await?;
                client.write_all(&ip.octets()).await?;
            }
        }
        client
            .write_all(&public.local_addr()?.port().to_be_bytes())
            .await?;
        Ok(Some(SocksUdp {
            public,
            backend: udp_backend,
            client_port: u16::from_be_bytes(address[address.len() - 2..].try_into().unwrap()),
        }))
    } else {
        client.write_all(&reply).await?;
        client.write_all(&addr).await?;
        Ok(None)
    }
}
async fn read_socks_address<R: AsyncRead + Unpin>(r: &mut R, kind: u8) -> io::Result<Vec<u8>> {
    let mut out = vec![];
    let count = match kind {
        1 => 4,
        4 => 16,
        3 => {
            let n = r.read_u8().await?;
            out.push(n);
            n as usize
        }
        _ => return Err(io::ErrorKind::InvalidData.into()),
    };
    let mut rest = vec![0u8; count + 2];
    r.read_exact(&mut rest).await?;
    out.extend(rest);
    Ok(out)
}
async fn socks_udp<A: AsyncRead + Unpin>(
    client: &mut A,
    backend: &mut TcpStream,
    relay: SocksUdp,
    peer: std::net::IpAddr,
) -> io::Result<()> {
    let activity = TcpActivity::new();
    let mut source = None;
    let mut input = vec![0; 65536];
    let mut output = vec![0; 65536];
    let mut client_control = [0];
    let mut backend_control = [0];
    let pumping = async {
        loop {
            tokio::select! {
                _ = client.read(&mut client_control) => return Ok(()),
                _ = backend.read(&mut backend_control) => return Ok(()),
                received = relay.public.recv_from(&mut input) => {
                    let (n, address) = received?;
                    if address.ip().to_canonical() != peer.to_canonical()
                        || (relay.client_port != 0 && address.port() != relay.client_port)
                        || source.is_some_and(|p| p != address)
                        || crate::meter::payload_size(&input[..n]).is_err() { continue }
                    source = Some(address);
                    activity.mark_progress();
                    relay.backend.send(&input[..n]).await?;
                },
                received = relay.backend.recv(&mut output) => {
                    let n = received?;
                    if let Some(address) = source {
                        activity.mark_progress();
                        relay.public.send_to(&output[..n], address).await?;
                    }
                },
            }
        }
    };
    tokio::select! {
        biased;
        res = pumping => res,
        res = activity.idle() => res,
    }
}
async fn connection<A: AsyncRead + AsyncWrite + Unpin>(
    mut stream: A,
    spec: Spec,
    lane: Arc<Lane>,
    peer: std::net::IpAddr,
    local: std::net::SocketAddr,
) -> Result<()> {
    if !lane.available() {
        return Ok(());
    }
    let mut backend = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect(("127.0.0.1", spec.backend)),
    )
    .await??;
    backend.set_nodelay(true)?;
    if spec.protocol == Protocol::Socks && spec.kind == Kind::Vless {
        if let Some(relay) = tokio::time::timeout(
            Duration::from_secs(15),
            socks_handshake(
                &mut stream,
                &mut backend,
                local,
                spec.meter.as_ref().map(|_| lane.as_ref()),
            ),
        )
        .await??
        {
            return Ok(socks_udp(&mut stream, &mut backend, relay, peer).await?);
        }
    }
    if spec.kind == Kind::Vless {
        if let Err(error) = copy_tcp(stream, &mut backend, None).await {
            if error.kind() == io::ErrorKind::ConnectionReset {
                // A reset client cannot consume a delayed reply. Pass the abort to the local core.
                let _ = backend.set_zero_linger();
            }
            return Err(error.into());
        }
    } else {
        relay(stream, backend, lane).await?;
    }
    Ok(())
}

pub(crate) struct Listener<S> {
    socket: Option<S>,
    address: std::net::SocketAddr,
    book: Arc<Book>,
    label: String,
    retry_at: Option<tokio::time::Instant>,
    delay: Duration,
}
impl<S> Listener<S> {
    pub(crate) fn new(
        socket: S,
        address: std::net::SocketAddr,
        book: Arc<Book>,
        label: String,
    ) -> Self {
        Self {
            socket: Some(socket),
            address,
            book,
            label,
            retry_at: None,
            delay: Duration::from_millis(20),
        }
    }
    async fn ready(&self) {
        if let Some(at) = self.retry_at {
            tokio::time::sleep_until(at).await;
        }
    }
    fn failure(&mut self, error: io::Error) {
        if matches!(
            error.raw_os_error(),
            Some(libc::EBADF | libc::ENOTSOCK | libc::EINVAL)
        ) {
            self.socket.take();
        }
        let detail = error.to_string();
        if self
            .book
            .listener_errors
            .lock()
            .unwrap()
            .insert(self.label.clone(), detail.clone())
            .as_ref()
            != Some(&detail)
        {
            eprintln!("监听 {} 出错，正在重试: {detail}", self.label);
        }
        self.retry_at = Some(tokio::time::Instant::now() + self.delay);
        self.delay = (self.delay * 2).min(Duration::from_secs(2));
    }
    fn recovered(&mut self) {
        if self.retry_at.take().is_some() {
            self.book
                .listener_errors
                .lock()
                .unwrap()
                .remove(&self.label);
            self.delay = Duration::from_millis(20);
        }
    }
}
impl<S> Drop for Listener<S> {
    fn drop(&mut self) {
        self.book
            .listener_errors
            .lock()
            .unwrap()
            .remove(&self.label);
    }
}
fn wildcard_socket(port: u16, kind: socket2::Type) -> io::Result<socket2::Socket> {
    use socket2::{Domain, Socket, Type};
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    let bind = |domain, address: SocketAddr| -> io::Result<Socket> {
        let socket = Socket::new(domain, kind, None)?;
        if domain == Domain::IPV6 {
            socket.set_only_v6(false)?;
        }
        if kind == Type::STREAM {
            socket.set_reuse_address(true)?;
        }
        socket.set_nonblocking(true)?;
        socket.bind(&address.into())?;
        if kind == Type::STREAM {
            socket.listen(1024)?;
        }
        Ok(socket)
    };
    match bind(Domain::IPV6, (Ipv6Addr::UNSPECIFIED, port).into()) {
        Err(e) if matches!(e.raw_os_error(), Some(libc::EAFNOSUPPORT | libc::EPROTONOSUPPORT | libc::EADDRNOTAVAIL | libc::ENOPROTOOPT)) =>
            bind(Domain::IPV4, (Ipv4Addr::UNSPECIFIED, port).into()),
        result => result,
    }
}
async fn bind_tcp(host: &str, port: u16) -> io::Result<TcpListener> {
    if matches!(host, "::" | "0.0.0.0") {
        TcpListener::from_std(wildcard_socket(port, socket2::Type::STREAM)?.into())
    } else {
        TcpListener::bind((host, port)).await
    }
}
async fn bind_udp(host: &str, port: u16) -> io::Result<UdpSocket> {
    if matches!(host, "::" | "0.0.0.0") {
        UdpSocket::from_std(wildcard_socket(port, socket2::Type::DGRAM)?.into())
    } else {
        UdpSocket::bind((host, port)).await
    }
}
impl Listener<TcpListener> {
    pub(crate) async fn accept(&mut self) -> (TcpStream, std::net::SocketAddr) {
        loop {
            self.ready().await;
            if let Some(socket) = &self.socket {
                match socket.accept().await {
                    Ok(connection) => {
                        self.recovered();
                        return connection;
                    }
                    Err(e) => self.failure(e),
                }
            } else {
                match bind_tcp(&self.address.ip().to_string(), self.address.port()).await {
                    Ok(socket) => {
                        self.socket = Some(socket);
                        self.recovered();
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
    }
}
impl Listener<Arc<UdpSocket>> {
    async fn receive(&mut self, buf: &mut [u8]) -> (usize, std::net::SocketAddr, Arc<UdpSocket>) {
        loop {
            self.ready().await;
            if let Some(socket) = self.socket.clone() {
                match socket.recv_from(buf).await {
                    Ok((n, peer)) => {
                        self.recovered();
                        return (n, peer, socket);
                    }
                    Err(e) => self.failure(e),
                }
            } else {
                match bind_udp(&self.address.ip().to_string(), self.address.port()).await {
                    Ok(socket) => {
                        self.socket = Some(Arc::new(socket));
                        self.recovered();
                    }
                    Err(e) => self.failure(e),
                }
            }
        }
    }
}

pub struct Gateway {
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    pub lanes: BTreeMap<String, Arc<Lane>>,
    specs: Vec<Spec>,
}
impl Gateway {
    pub async fn start(specs: Vec<Spec>, book: Arc<Book>, dir: PathBuf) -> Result<Self> {
        let mut gateway = Self {
            cancel: CancellationToken::new(),
            tasks: vec![],
            lanes: BTreeMap::new(),
            specs: specs.clone(),
        };
        let result = gateway.bind(specs, book, dir).await;
        if let Err(e) = result {
            gateway.close().await;
            return Err(e);
        }
        Ok(gateway)
    }
    async fn bind(&mut self, specs: Vec<Spec>, book: Arc<Book>, dir: PathBuf) -> Result<()> {
        for spec in &specs {
            let lane = Lane::new(spec, book.clone());
            self.tasks
                .push(tokio::spawn(lane.clone().supervise(self.cancel.clone())));
            if let Some(meter) = &spec.meter {
                self.tasks.push(
                    crate::meter::serve(
                        meter.clone(),
                        spec.id.clone(),
                        lane.clone(),
                        self.cancel.clone(),
                    )
                    .await?,
                );
            }
            self.lanes.insert(spec.id.clone(), lane);
        }
        for spec in specs.iter().filter(|s| s.kind != Kind::Website) {
            let lane = self.lanes[&spec.id].clone();
            if spec.network.contains("tcp") {
                let listener = bind_tcp(&spec.listen, spec.port)
                    .await
                    .with_context(|| format!("入口 TCP {} 已占用", spec.port))?;
                let address = listener.local_addr()?;
                let mut listener = Listener::new(listener, address, book.clone(), format!("{} TCP {address}", spec.id));
                let spec = spec.clone();
                let cancel = self.cancel.clone();
                let lane = lane.clone();
                let dir = dir.clone();
                self.tasks.push(tokio::spawn(async move {
                    let mut children=JoinSet::new();
                    loop {tokio::select!{
                        biased;
                        _=cancel.cancelled()=>break,
                        Some(_)=children.join_next(),if !children.is_empty()=>{},
                        (stream,peer)=listener.accept()=>{
                            if children.len() >= lane.transport_limit() { continue; }
                            let Some(slot) = lane.book.proxy_capacity.acquire_ingress(&spec.id) else { continue };
                            let spec=spec.clone();let lane=lane.clone();let dir=dir.clone();
                            children.spawn(async move {
                                let _slot = slot;
                                let stopped = lane.cancellation();
                                let transfer = async {
                                let mut stream=stream;
                                let Ok(local) = stream.local_addr() else { return; };
                                let _=stream.set_nodelay(true);
                                configure_tcp_keepalive(&stream);
                                let peer = if spec.proxy_protocol {
                                    let Ok(Ok(peer)) = tokio::time::timeout(Duration::from_secs(3),proxy_peer(&mut stream)).await else { return; };
                                    peer
                                } else { peer.ip() };
                                if matches!(spec.tls.mode.as_str(),"manual"|"auto") && spec.protocol != Protocol::Vless {
                                    if let Ok(config)=crate::website::load_tls(&spec.tls,&dir) {
                                        if let Ok(Ok(tls))=tokio::time::timeout(Duration::from_secs(15),tokio_rustls::TlsAcceptor::from(config).accept(stream)).await {let _=connection(tls,spec,lane,peer,local).await;}
                                    }
                                } else {let _=connection(stream,spec,lane,peer,local).await;}
                                };
                                tokio::select! {_=stopped.cancelled()=>{},_=transfer=>{}}
                            });
                        }
                    }}
                    children.abort_all();while children.join_next().await.is_some() {}
                }));
            }
            if spec.network.contains("udp")
                && !(spec.kind == Kind::Vless && spec.protocol == Protocol::Socks)
            {
                let socket = Arc::new(
                    bind_udp(&spec.listen, spec.port)
                        .await
                        .with_context(|| format!("入口 UDP {} 已占用", spec.port))?,
                );
                let address = socket.local_addr()?;
                let mut listener = Listener::new(socket, address, book.clone(), format!("{} UDP {address}", spec.id));
                let spec = spec.clone();
                let cancel = self.cancel.clone();
                self.tasks.push(tokio::spawn(async move {
                    let mut sessions:BTreeMap<std::net::SocketAddr,mpsc::Sender<bytes::Bytes>>=BTreeMap::new();
                    let mut children=JoinSet::new();let mut buf=vec![0u8;65536];
                    loop {tokio::select!{
                        _=cancel.cancelled()=>break,
                        Some(_)=children.join_next(),if !children.is_empty()=>{sessions.retain(|_,tx|!tx.is_closed());},
                        (n,peer,socket)=listener.receive(&mut buf)=>{
                            if !lane.available() {continue;}
                            if sessions.get(&peer).is_some_and(|tx|tx.is_closed()) {sessions.remove(&peer);}
                            if !sessions.contains_key(&peer) {
                                if sessions.len() >= lane.transport_limit() { continue; }
                                let Some(slot) = lane.book.proxy_capacity.acquire_ingress(&spec.id) else { continue };
                                let (tx,mut rx)=mpsc::channel::<bytes::Bytes>(512);sessions.insert(peer,tx);
                                let socket=socket.clone();let lane=lane.clone();let backend_port=spec.backend;
                                let account=spec.kind != Kind::Vless;
                                children.spawn(async move {
                                    let _slot = slot;
                                    let stopped = lane.cancellation();
                                    let result=async {
                                        let backend=UdpSocket::bind("127.0.0.1:0").await?;backend.connect(("127.0.0.1",backend_port)).await?;
                                        let _connection=account.then(|| lane.connection());let mut out=vec![0u8;65536];
                                        loop {tokio::select!{
                                            packet=tokio::time::timeout(Duration::from_secs(60),rx.recv())=>{
                                                let Ok(Some(packet))=packet else {break};
                                                let transfer=if account {Some(lane.transfer(packet.len(),true,true).await?)} else {None};backend.send(&packet).await?;drop(transfer);
                                            }
                                            response=backend.recv(&mut out)=>{
                                                let n=response?;
                                                let transfer=if account {Some(lane.transfer(n,false,true).await?)} else {None};socket.send_to(&out[..n],peer).await?;drop(transfer);
                                            }
                                        }}Ok::<(),io::Error>(())
                                    };tokio::select! {_=stopped.cancelled()=>{},_=result=>{}}
                                });
                            }
                            let _=sessions[&peer].try_send(bytes::Bytes::copy_from_slice(&buf[..n]));
                        }
                    }}
                    children.abort_all();while children.join_next().await.is_some() {}
                }));
            }
        }
        self.tasks
            .extend(crate::website::serve(&specs, &self.lanes, &dir, self.cancel.clone()).await?);
        Ok(())
    }
    pub fn limits_only(&self, specs: &[Spec]) -> bool {
        same_listeners(specs, &self.specs)
    }
    pub fn update_limits(&mut self, specs: &[Spec]) {
        for spec in specs {
            if let Some(lane) = self.lanes.get(&spec.id) {
                lane.update(spec);
                lane.available();
            }
        }
        self.specs = specs.to_vec();
    }
    pub fn running(&self) -> bool {
        self.tasks.iter().all(|t| !t.is_finished())
    }
    pub async fn close(&mut self) {
        self.cancel.cancel();
        for mut task in self.tasks.drain(..) {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.cancel.cancel();
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub fn same_listeners(a: &[Spec], b: &[Spec]) -> bool {
    let normalize = |s: &[Spec]| {
        let mut s = s.to_vec();
        for item in &mut s {
            item.limits = Limits::default();
            item.enabled = true;
            item.lease = None;
        }
        s
    };
    normalize(a) == normalize(b)
}
pub fn specs(config: &serde_json::Value) -> Result<Vec<Spec>> {
    let specs: Vec<Spec> = serde_json::from_value(
        config
            .get("_chain")
            .cloned()
            .unwrap_or(serde_json::json!([])),
    )?;
    for s in &specs {
        ensure!(s.port > 0 && s.backend > 0, "接入端口无效");
        ensure!(
            s.kind != Kind::Vless || s.meter.is_some(),
            "代理缺少鉴权后计量桥，请更新控制台并重新应用"
        );
        if let Some(meter) = &s.meter {
            ensure!(
                meter.port > 0 && meter.backend > 0 && meter.port != meter.backend,
                "计量桥端口无效"
            );
            ensure!(
                !s.id.is_empty()
                    && s.id.len() <= 255
                    && !meter.password.is_empty()
                    && meter.password.len() <= 255,
                "计量桥身份无效"
            );
        }
    }
    Ok(specs)
}
pub fn xray_config(config: &serde_json::Value) -> serde_json::Value {
    let mut v = config.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("_chain");
        m.remove("_ingress");
        m.remove("_mihomo");
        m.remove("_legacy");
    }
    v
}

struct SystemSample {
    at: Instant,
    cpu: Option<(u64, u64)>,
    rx: u64,
    tx: u64,
}
fn metric_rates(
    old: Option<&SystemSample>,
    current: &SystemSample,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let Some(old) = old else {
        return (None, None, None);
    };
    let seconds = current.at.duration_since(old.at).as_secs_f64();
    if seconds <= 0.0 {
        return (None, None, None);
    }
    let cpu = old
        .cpu
        .zip(current.cpu)
        .and_then(|((total, idle), (new_total, new_idle))| {
            let total = new_total.checked_sub(total)?;
            let idle = new_idle.checked_sub(idle)?;
            (total > 0).then(|| 100.0 * total.saturating_sub(idle) as f64 / total as f64)
        });
    (
        cpu,
        current.rx.checked_sub(old.rx).map(|n| n as f64 / seconds),
        current.tx.checked_sub(old.tx).map(|n| n as f64 / seconds),
    )
}
#[cfg(target_os = "linux")]
pub fn system_metrics() -> serde_json::Value {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let value = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    let total = value("MemTotal:");
    let available = value("MemAvailable:");
    let load = std::fs::read_to_string("/proc/loadavg")
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok());
    let net = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in net.lines().skip(2) {
        if let Some((name, values)) = line.split_once(':') {
            if name.trim() == "lo" {
                continue;
            }
            let v: Vec<_> = values.split_whitespace().collect();
            rx = rx.saturating_add(v.first().and_then(|v| v.parse().ok()).unwrap_or(0));
            tx = tx.saturating_add(v.get(8).and_then(|v| v.parse().ok()).unwrap_or(0));
        }
    }
    let cpu = std::fs::read_to_string("/proc/stat").ok().and_then(|text| {
        let counters: Vec<u64> = text
            .lines()
            .next()?
            .split_whitespace()
            .skip(1)
            .take(8)
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        (counters.len() >= 4).then(|| {
            (
                counters.iter().sum(),
                counters[3] + counters.get(4).copied().unwrap_or(0),
            )
        })
    });
    let current = SystemSample {
        at: Instant::now(),
        cpu,
        rx,
        tx,
    };
    static PREVIOUS: std::sync::OnceLock<Mutex<Option<SystemSample>>> = std::sync::OnceLock::new();
    let mut previous = PREVIOUS.get_or_init(|| Mutex::new(None)).lock().unwrap();
    let (cpu_percent, network_rx_bps, network_tx_bps) = metric_rates(previous.as_ref(), &current);
    *previous = Some(current);
    drop(previous);
    let disk = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(c"/".as_ptr(), &mut stat) == 0).then(|| {
            (
                stat.f_blocks.saturating_mul(stat.f_frsize),
                stat.f_blocks
                    .saturating_sub(stat.f_bfree)
                    .saturating_mul(stat.f_frsize),
            )
        })
    };
    let mut listening_ports = std::collections::BTreeSet::new();
    for table in ["tcp", "tcp6", "udp", "udp6"] {
        let text = std::fs::read_to_string(format!("/proc/net/{table}")).unwrap_or_default();
        for line in text.lines().skip(1) {
            let columns: Vec<_> = line.split_whitespace().collect();
            if table.starts_with("tcp") && columns.get(3) != Some(&"0A") {
                continue;
            }
            if let Some(port) = columns
                .get(1)
                .and_then(|s| s.rsplit(':').next())
                .and_then(|s| u16::from_str_radix(s, 16).ok())
            {
                if port > 0 {
                    listening_ports.insert(port);
                }
            }
        }
    }
    let ephemeral: Vec<u16> = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    serde_json::json!({"agent_version":env!("CARGO_PKG_VERSION"),"gateway_version":2,"sharing_version":1,"customer_plan_version":1,"legacy_config_version":1,"cpu_percent":cpu_percent,"disk_total":disk.map(|d|d.0),"disk_used":disk.map(|d|d.1),"memory_total":total,"memory_used":total.saturating_sub(available),"load_1m":load,"network_rx":rx,"network_tx":tx,"network_rx_bps":network_rx_bps,"network_tx_bps":network_tx_bps,"listening_ports":listening_ports,"ephemeral_port_range":ephemeral,"sampled_at":now()})
}

#[cfg(target_os = "macos")]
pub fn system_metrics() -> serde_json::Value {
    unsafe extern "C" {
        fn mach_host_self() -> libc::mach_port_t;
        static mach_task_self_: libc::mach_port_t;
        fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> libc::kern_return_t;
    }
    fn sysctl<T: Default>(name: &std::ffi::CStr) -> Option<T> {
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>();
        (unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut T).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == 0
            && size == std::mem::size_of::<T>())
        .then_some(value)
    }
    let total = sysctl::<u64>(c"hw.memsize");
    let (cpu, available) = unsafe {
        let host = mach_host_self();
        let mut cpu: libc::host_cpu_load_info = std::mem::zeroed();
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        let cpu = (libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            (&mut cpu as *mut libc::host_cpu_load_info).cast(),
            &mut count,
        ) == 0)
            .then(|| {
                (
                    cpu.cpu_ticks.iter().map(|n| u64::from(*n)).sum(),
                    u64::from(cpu.cpu_ticks[libc::CPU_STATE_IDLE as usize]),
                )
            });
        let mut vm: libc::vm_statistics64 = std::mem::zeroed();
        let mut count =
            (std::mem::size_of_val(&vm) / std::mem::size_of::<libc::integer_t>()) as u32;
        let available = (libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut vm as *mut libc::vm_statistics64).cast(),
            &mut count,
        ) == 0)
            .then(|| {
                (u64::from(vm.free_count) + u64::from(vm.inactive_count))
                    * libc::sysconf(libc::_SC_PAGESIZE) as u64
            });
        mach_port_deallocate(mach_task_self_, host);
        (cpu, available)
    };
    let (mut rx, mut tx) = (0u64, 0u64);
    unsafe {
        let mut interfaces = std::ptr::null_mut();
        if libc::getifaddrs(&mut interfaces) == 0 {
            let mut current = interfaces;
            while !current.is_null() {
                let interface = &*current;
                if !interface.ifa_addr.is_null()
                    && (*interface.ifa_addr).sa_family as i32 == libc::AF_LINK
                    && interface.ifa_flags & libc::IFF_LOOPBACK as u32 == 0
                    && !interface.ifa_data.is_null()
                {
                    let mut mib = [
                        libc::CTL_NET,
                        libc::PF_LINK,
                        libc::NETLINK_GENERIC,
                        libc::IFMIB_IFDATA,
                        libc::if_nametoindex(interface.ifa_name) as i32,
                        libc::IFDATA_GENERAL,
                    ];
                    let mut data: libc::ifmibdata = std::mem::zeroed();
                    let mut size = std::mem::size_of_val(&data);
                    if libc::sysctl(
                        mib.as_mut_ptr(),
                        mib.len() as u32,
                        (&mut data as *mut libc::ifmibdata).cast(),
                        &mut size,
                        std::ptr::null_mut(),
                        0,
                    ) == 0
                    {
                        rx = rx.saturating_add(data.ifmd_data.ifi_ibytes);
                        tx = tx.saturating_add(data.ifmd_data.ifi_obytes);
                    }
                }
                current = interface.ifa_next;
            }
            libc::freeifaddrs(interfaces);
        }
    }
    let current = SystemSample {
        at: Instant::now(),
        cpu,
        rx,
        tx,
    };
    static PREVIOUS: std::sync::OnceLock<Mutex<Option<SystemSample>>> = std::sync::OnceLock::new();
    let mut previous = PREVIOUS.get_or_init(|| Mutex::new(None)).lock().unwrap();
    let (cpu_percent, network_rx_bps, network_tx_bps) = metric_rates(previous.as_ref(), &current);
    *previous = Some(current);
    drop(previous);
    let (disk, load) = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        let disk = (libc::statvfs(c"/".as_ptr(), &mut stat) == 0).then(|| {
            (
                u64::from(stat.f_blocks).saturating_mul(stat.f_frsize as u64),
                u64::from(stat.f_blocks.saturating_sub(stat.f_bfree))
                    .saturating_mul(stat.f_frsize as u64),
            )
        });
        let mut load = 0.0;
        (disk, (libc::getloadavg(&mut load, 1) == 1).then_some(load))
    };
    let mut listening_ports = std::collections::BTreeSet::new();
    for protocol in ["tcp", "udp"] {
        if let Ok(result) = std::process::Command::new("/usr/sbin/netstat")
            .args(["-an", "-p", protocol])
            .output()
        {
            for line in String::from_utf8_lossy(&result.stdout).lines() {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields
                    .first()
                    .is_some_and(|name| name.starts_with(protocol))
                    && (protocol == "udp" || fields.contains(&"LISTEN"))
                {
                    if let Some(port) = fields
                        .get(3)
                        .and_then(|s| s.rsplit('.').next())
                        .and_then(|s| s.parse::<u16>().ok())
                    {
                        if port > 0 {
                            listening_ports.insert(port);
                        }
                    }
                }
            }
        }
    }
    let ephemeral = sysctl::<u32>(c"net.inet.ip.portrange.first")
        .zip(sysctl::<u32>(c"net.inet.ip.portrange.last"))
        .map(|(first, last)| [first, last]);
    serde_json::json!({"agent_version":env!("CARGO_PKG_VERSION"),"gateway_version":2,"sharing_version":1,"customer_plan_version":1,"legacy_config_version":1,"cpu_percent":cpu_percent,"disk_total":disk.map(|d|d.0),"disk_used":disk.map(|d|d.1),"memory_total":total,"memory_used":total.zip(available).map(|(t,a)|t.saturating_sub(a)),"load_1m":load,"network_rx":rx,"network_tx":tx,"network_rx_bps":network_rx_bps,"network_tx_bps":network_tx_bps,"listening_ports":listening_ports,"ephemeral_port_range":ephemeral,"sampled_at":now()})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn concurrent_customer_directions_do_not_charge_past_local_quota() {
        use axum::{Json, Router, routing::post};
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        eprintln!("owned customer direction listener={address}");
        let router=Router::new().route("/grant",post(|| async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Json(crate::resources::Grant {cycle:1,end:now()+100,cap:1000,rate:None,valid_for:10})
        }));
        let stop=CancellationToken::new(); let stopped=stop.clone();
        let server=tokio::spawn(async move { axum::serve(listener,router).with_graceful_shutdown(stopped.cancelled_owned()).await.unwrap(); });
        let dir=tempfile::tempdir().unwrap();
        let book=Book::open(dir.path()).unwrap();
        *book.resources.write().unwrap()=Some(crate::resources::Client::open(dir.path(),format!("http://{address}/grant"),"test".into()).unwrap());
        let mut spec=spec(Limits {monthly_gib:Some(20.0/1073741824.0),..Default::default()});
        spec.multiplier=2; spec.resource_ids=vec!["plan:customer".into()];
        let lane=Lane::new(&spec,book.clone());
        let (a,b)=tokio::join!(lane.charge(10,true,false),lane.charge(10,false,false));
        assert_eq!(a.unwrap_or(0)+b.unwrap_or(0),10);
        assert_eq!(book.snapshot()[&spec.id].used(),20);
        book.flush().unwrap();
        let db=Database::open_with_flags(dir.path().join("resources.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(db.query_row("SELECT used FROM consumption WHERE resource='plan:customer'",[],|row| row.get::<_,u64>(0)).unwrap(),20);
        drop(db); drop(lane); drop(book);
        stop.cancel(); server.await.unwrap();
        let _released=std::net::TcpListener::bind(address).unwrap();
    }
    #[tokio::test]
    async fn multiplier_is_charged_at_transfer_time_and_persisted() {
        let dir=tempfile::tempdir().unwrap();
        let book=Book::open(dir.path()).unwrap();
        let mut s=spec(Limits {monthly_gib:Some(10.0/1073741824.0),..Default::default()});
        s.multiplier=2;
        let lane=Lane::new(&s,book.clone());
        assert_eq!(lane.charge(2,true,false).await.unwrap(),2);
        assert_eq!(book.snapshot()[&s.id].used(),4);
        s.multiplier=3;lane.update(&s);
        assert!(lane.charge(3,false,true).await.is_err());
        assert_eq!(lane.charge(8,false,false).await.unwrap(),2);
        let t=book.snapshot()[&s.id].clone();
        assert_eq!((t.upload,t.download,t.used()),(2,2,10));
        assert!(!lane.available());drop(lane);drop(book);
        let reopened=Book::open(dir.path()).unwrap();assert_eq!(reopened.snapshot()[&s.id].used(),10);
    }
    #[test]
    fn connection_admission_is_atomic_and_released() {
        let dir=tempfile::tempdir().unwrap();let mut s=spec(Limits::default());
        s.access_policy.policy.max_connections=Some(1);
        let lane=Lane::new(&s,Book::open(dir.path()).unwrap());
        let first=lane.admit().unwrap();assert!(lane.admit().is_err());drop(first);assert!(lane.admit().is_ok());
    }

    async fn advance_tcp_clock(seconds: u64) {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(seconds)).await;
        tokio::task::yield_now().await;
        tokio::time::resume();
    }

    #[tokio::test]
    async fn metered_one_way_upload_and_download_remain_active_past_300_seconds() {
        for upload in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let lane = Lane::new(&spec(Limits::default()), Book::open(dir.path()).unwrap());
            let (mut client, inbound) = tokio::io::duplex(32);
            let (outbound, mut server) = tokio::io::duplex(32);
            let job = tokio::spawn(relay(inbound, outbound, lane.clone()));
            let (sender, receiver) = if upload { (&mut client, &mut server) } else { (&mut server, &mut client) };
            for byte in 1..=4 {
                sender.write_u8(byte).await.unwrap();
                assert_eq!(tokio::time::timeout(Duration::from_secs(2), receiver.read_u8()).await.unwrap().unwrap(), byte);
                advance_tcp_clock(150).await;
            }
            assert!(!job.is_finished());
            let usage = &lane.book.snapshot()[&lane.id];
            assert_eq!((usage.upload, usage.download), if upload { (4, 0) } else { (0, 4) });
            advance_tcp_clock(151).await;
            let result = tokio::time::timeout(Duration::from_secs(2), job).await.unwrap().unwrap();
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert_eq!(lane.book.snapshot()[&lane.id].connections, 0);
        }
    }

    #[tokio::test]
    async fn slow_shared_uploads_each_get_progress_and_release_their_connection() {
        let dir = tempfile::tempdir().unwrap();
        let lane = Lane::new(
            &spec(Limits {
                upload_mbps: Some(0.001),
                ..Default::default()
            }),
            Book::open(dir.path()).unwrap(),
        );
        let mut peers = Vec::new();
        let mut jobs = Vec::new();
        for value in 1..=3 {
            let (mut client, inbound) = tokio::io::duplex(32768);
            let (outbound, mut target) = tokio::io::duplex(32768);
            jobs.push(tokio::spawn(relay(inbound, outbound, lane.clone())));
            target.shutdown().await.unwrap();
            client.write_all(&[value; 16384]).await.unwrap();
            client.shutdown().await.unwrap();
            peers.push((client, target, value));
        }
        for tick in 1..=1800 {
            advance_tcp_clock(1).await;
            tokio::time::sleep(Duration::from_millis(1)).await; // Let the real SQLite writer acknowledge.
            if tick == 12 {
                for (_, target, value) in &mut peers {
                    assert_eq!(
                        tokio::time::timeout(Duration::from_millis(100), target.read_u8())
                            .await
                            .unwrap()
                            .unwrap(),
                        *value
                    );
                }
            }
            if jobs.iter().all(|j| j.is_finished()) {
                assert!(tick >= 390, "the shared 125 byte/s budget was exceeded");
                break;
            }
        }
        for job in jobs {
            tokio::time::timeout(Duration::from_secs(1), job)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        for (_, mut target, value) in peers {
            let mut bytes = vec![];
            target.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, vec![value; 16383]);
        }
        let row = &lane.book.snapshot()[&lane.id];
        assert_eq!((row.total_upload, row.connections), (3 * 16384, 0));
    }

    #[tokio::test]
    async fn listener_retry_keeps_existing_flow_and_rebinds_only_its_port() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path()).unwrap();
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let mut listener = Listener::new(socket, address, book.clone(), "test".into());
        let mut client = TcpStream::connect(address).await.unwrap();
        let (mut established, _) = listener.accept().await;
        listener.failure(io::Error::from_raw_os_error(libc::EMFILE));
        let next = TcpStream::connect(address).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), listener.accept())
                .await
                .is_err()
        );
        client.write_all(b"a").await.unwrap();
        assert_eq!(established.read_u8().await.unwrap(), b'a');
        let _ = listener.accept().await;
        drop(next);
        assert!(book.listener_errors.lock().unwrap().is_empty());
        listener.failure(io::Error::from_raw_os_error(libc::EINVAL));
        let occupied = TcpListener::bind(address).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(80), listener.accept())
                .await
                .is_err()
        );
        assert!(book.listener_errors.lock().unwrap().contains_key("test"));
        client.write_all(b"b").await.unwrap();
        assert_eq!(established.read_u8().await.unwrap(), b'b');
        drop(occupied);
        let ((), _) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                async {
                    loop {
                        if TcpStream::connect(address).await.is_ok() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                },
                listener.accept()
            )
        })
        .await
        .unwrap();
        assert!(book.listener_errors.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn public_gateway_drains_half_close_then_reuses_the_node_slot() {
        let dir = tempfile::tempdir().unwrap();
        let mut book = Book::open(dir.path()).unwrap();
        Arc::get_mut(&mut book).unwrap().proxy_capacity = ProxyCapacity::new(1);
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut s = spec(Limits::default());
        s.kind = Kind::Vless;
        s.network = "tcp".into();
        s.listen = "127.0.0.1".into();
        s.backend = backend.local_addr().unwrap().port();
        let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let second = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addresses = [first.local_addr().unwrap(), second.local_addr().unwrap()];
        drop((first, second));
        let mut other = s.clone();
        s.port = addresses[0].port();
        other.id = id();
        other.port = addresses[1].port();
        let mut gateway = Gateway::start(vec![s, other], book.clone(), dir.path().into()).await.unwrap();
        for (address, client_fin) in addresses.into_iter().zip([true, false]) {
            let mut client = TcpStream::connect(address).await.unwrap();
            let (mut server, _) = tokio::time::timeout(Duration::from_secs(2), backend.accept()).await.unwrap().unwrap();
            assert_eq!(book.proxy_capacity.ingress.available_permits(), 0);
            assert_eq!(book.proxy_capacity.meter.available_permits(), 1);
            if client_fin {
                let mut at_capacity = TcpStream::connect(addresses[1]).await.unwrap();
                assert_eq!(tokio::time::timeout(Duration::from_secs(2), at_capacity.read(&mut [0])).await.unwrap().unwrap(), 0);
            }
            let (closed, open) = if client_fin { (&mut client, &mut server) } else { (&mut server, &mut client) };
            closed.write_all(b"request").await.unwrap();
            closed.shutdown().await.unwrap();
            let mut request = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), open.read_to_end(&mut request)).await.unwrap().unwrap();
            assert_eq!(request, b"request");
            for byte in 1..=3 {
                advance_tcp_clock(200).await;
                open.write_u8(byte).await.unwrap();
                assert_eq!(tokio::time::timeout(Duration::from_secs(2), closed.read_u8()).await.unwrap().unwrap(), byte);
            }
            advance_tcp_clock(299).await;
            assert_eq!(book.proxy_capacity.ingress.available_permits(), 0);
            advance_tcp_clock(2).await;
            assert_eq!(tokio::time::timeout(Duration::from_secs(2), closed.read(&mut [0])).await.unwrap().unwrap(), 0);
            assert_eq!(book.proxy_capacity.ingress.available_permits(), 1);
        }
        let mut aborted = TcpStream::connect(addresses[0]).await.unwrap();
        let (mut server, _) = backend.accept().await.unwrap();
        aborted.write_u8(42).await.unwrap();
        assert_eq!(server.read_u8().await.unwrap(), 42);
        aborted.set_zero_linger().unwrap();
        drop(aborted);
        let end = tokio::time::timeout(Duration::from_secs(2), server.read(&mut [0])).await.unwrap();
        assert_eq!(end.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
        gateway.close().await;
    }

    #[tokio::test]
    async fn socks_connect_reports_full_meter_after_authentication_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let mut book = Book::open(dir.path()).unwrap();
        Arc::get_mut(&mut book).unwrap().proxy_capacity = ProxyCapacity::new(1);
        let mut s = spec(Limits::default());
        s.access_policy.policy.max_connections = Some(1);
        let lane = Lane::new(&s, book.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let request = [5, 1, 0, 1, 127, 0, 0, 1, 0, 80];
        for (line_full, node_full, valid_auth) in [
            (true, false, false), (true, false, true), (false, true, true), (false, false, true),
        ] {
            let _admission = line_full.then(|| lane.admit().unwrap());
            let _slot = node_full.then(|| book.proxy_capacity.acquire_meter(&s.id).unwrap());
            let (mut client, mut gateway) = tokio::io::duplex(64);
            let mut backend = TcpStream::connect(local).await.unwrap();
            let (mut core, _) = listener.accept().await.unwrap();
            let full = line_full || node_full;
            let proxy = async {
                let result = socks_handshake(&mut gateway, &mut backend, local, Some(&lane)).await;
                drop(backend);
                match (valid_auth, full) {
                    (false, _) => assert_eq!(result.err().unwrap().kind(), io::ErrorKind::PermissionDenied),
                    (true, true) => assert_eq!(result.err().unwrap().kind(), io::ErrorKind::WouldBlock),
                    (true, false) => assert!(result.unwrap().is_none()),
                }
            };
            let server = async {
                let mut method = [0; 3];
                core.read_exact(&mut method).await.unwrap();
                assert_eq!(method, [5, 1, 2]);
                core.write_all(&[5, 2]).await.unwrap();
                let mut auth = [0; 5];
                core.read_exact(&mut auth).await.unwrap();
                assert_eq!(auth, [1, 1, b'u', 1, b'p']);
                core.write_all(&[1, u8::from(!valid_auth)]).await.unwrap();
                if valid_auth && !full {
                    let mut received = [0; 10];
                    core.read_exact(&mut received).await.unwrap();
                    assert_eq!(received, request);
                    core.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                } else {
                    assert_eq!(core.read(&mut [0]).await.unwrap(), 0);
                }
            };
            let user = async {
                client.write_all(&[5, 1, 2, 1, 1, b'u', 1, b'p']).await.unwrap();
                let mut auth = [0; 4];
                client.read_exact(&mut auth).await.unwrap();
                assert_eq!(auth, [5, 2, 1, u8::from(!valid_auth)]);
                if valid_auth {
                    client.write_all(&request).await.unwrap();
                    let mut reply = [0; 10];
                    client.read_exact(&mut reply).await.unwrap();
                    assert_eq!(reply, [5, u8::from(full), 0, 1, 0, 0, 0, 0, 0, 0]);
                }
            };
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(proxy, server, user); }).await.unwrap();
        }
    }

    #[test]
    fn default_and_configured_business_limits_release_admissions() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = spec(Limits::default());
        let lane = Lane::new(&s, Book::open(dir.path()).unwrap());
        for (setting, expected) in [(None, 4096), (Some(20_480), 20_480), (Some(1024), 1024)] {
            s.access_policy.policy.max_connections = setting;
            lane.update(&s);
            let admissions: Vec<_> = (0..expected).map(|_| lane.admit().unwrap()).collect();
            assert!(lane.admit().is_err());
            drop(admissions);
            assert!(lane.admit().is_ok());
        }
    }

    fn spec(limits: Limits) -> Spec {
        Spec {
            multiplier: 1,
            access_policy: Default::default(),
            resource_ids: vec![],
            enabled: true,
            lease: None,
            meter: None,
            proxy_protocol: false,
            listen: all_interfaces(),
            fallback_port: None,
            fallback_target: String::new(),
            id: id(),
            port: 20000,
            backend: 50000,
            kind: Kind::Forward,
            protocol: Protocol::Vless,
            network: "tcp,udp".into(),
            limits,
            tls: Tls::default(),
            target: "localhost:80".into(),
        }
    }
    #[test]
    fn json_roundtrips_preserve_one_byte_quota_and_decimal_rates() {
        let mut encoded = String::from(
            r#"{"monthly_gib":9.313225746154785e-10,"upload_mbps":0.3,"download_mbps":1.7}"#,
        );
        for _ in 0..8 {
            let limits: Limits = serde_json::from_str(&encoded).unwrap();
            assert_eq!((limits.monthly_gib.unwrap() * 1073741824.0) as u64, 1);
            assert_eq!(limits.upload_mbps.unwrap().to_bits(), 0.3_f64.to_bits());
            assert_eq!(limits.download_mbps.unwrap().to_bits(), 1.7_f64.to_bits());
            encoded = serde_json::to_string(&limits).unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_connections_share_rate_and_other_lines_are_independent() {
        let temp = tempfile::tempdir().unwrap();
        let book = Book::open(temp.path()).unwrap();
        let lane = Lane::new(
            &spec(Limits {
                upload_mbps: Some(0.4),
                ..Default::default()
            }),
            book.clone(),
        );
        let independent = Lane::new(&spec(Limits::default()), book);
        let started = Instant::now();
        let (a, b, c) = tokio::join!(
            lane.charge(25000, true, false),
            lane.charge(25000, true, false),
            async {
                let t = Instant::now();
                independent.charge(25000, true, false).await.unwrap();
                t.elapsed()
            }
        );
        assert_eq!(a.unwrap() + b.unwrap(), 50000);
        assert!(started.elapsed() >= Duration::from_millis(950));
        assert!(c < Duration::from_millis(200));
    }
    #[tokio::test]
    async fn canceled_rate_wait_does_not_delay_reopening_and_rate_updates_wake_waiters() {
        let temp = tempfile::tempdir().unwrap();
        let mut s = spec(Limits {
            upload_mbps: Some(0.001),
            ..Default::default()
        });
        let lane = Lane::new(&s, Book::open(temp.path()).unwrap());
        let pending =
            |lane: Arc<Lane>| tokio::spawn(async move { lane.charge(1000, true, false).await });
        let waiting = async |lane: &Lane| {
            tokio::time::timeout(Duration::from_secs(1), async {
                while lane.directions[0].try_lock().is_ok() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        };
        let job = pending(lane.clone());
        waiting(&lane).await;
        s.enabled = false;
        lane.update(&s);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), job)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        s.enabled = true;
        lane.update(&s);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), lane.charge(1, true, false))
                .await
                .unwrap()
                .unwrap(),
            1
        );

        let job = pending(lane.clone());
        waiting(&lane).await;
        s.limits.upload_mbps = Some(1.0);
        lane.update(&s);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), job)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            1000
        );
        assert_eq!(lane.book.snapshot()[&s.id].total_upload, 1001);
    }

    #[tokio::test]
    async fn legacy_and_current_public_wildcards_accept_both_families() {
        // IPv4-only kernels still exercise the fallback and IPv4 exchange.
        let ipv6 = TcpListener::bind("[::1]:0").await.is_ok();
        for host in ["0.0.0.0", "::"] {
            let tcp = bind_tcp(host, 0).await.unwrap();
            let udp = bind_udp(host, 0).await.unwrap();
            for ip in ["127.0.0.1", "::1"].into_iter().filter(|ip| *ip != "::1" || ipv6) {
                let stream = TcpStream::connect((ip, tcp.local_addr().unwrap().port())).await.unwrap();
                let (_, peer) = tcp.accept().await.unwrap();
                assert_eq!(peer.ip().to_canonical(), stream.local_addr().unwrap().ip());
                let client = UdpSocket::bind((ip, 0)).await.unwrap();
                client.send_to(b"dual", (ip, udp.local_addr().unwrap().port())).await.unwrap();
                let mut buf = [0;4];
                let (n, peer) = udp.recv_from(&mut buf).await.unwrap();
                udp.send_to(&buf[..n], peer).await.unwrap();
                assert_eq!(client.recv(&mut buf).await.unwrap(), 4);
                assert_eq!(&buf,b"dual");
            }
        }
    }
    #[tokio::test]
    async fn unavailable_quota_storage_only_stops_debits_needing_a_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("line-quota.db");
        std::fs::create_dir(&journal).unwrap();
        let book = Book::open(dir.path()).unwrap();
        let paid = Lane::new(&spec(Limits {monthly_gib:Some(1.0), ..Default::default()}),book.clone());
        let free = Lane::new(&spec(Limits::default()),book);
        assert!(paid.charge(32,true,false).await.is_err());
        assert_eq!(free.charge(32,true,false).await.unwrap(),32);
        std::fs::remove_dir(journal).unwrap();
        assert_eq!(paid.charge(32,true,false).await.unwrap(),32);
    }
    #[tokio::test]
    async fn abrupt_restart_enforces_monthly_quota_before_statistics_flush() {
        const CASE: &str = "CHAIN_TEST_LINE_CRASH_DIR";
        let mut s = spec(Limits { monthly_gib: Some(128.0 / 1073741824.0), ..Default::default() });
        s.id = "restart-line".into();
        if let Some(dir) = std::env::var_os(CASE) {
            let dir = std::path::Path::new(&dir);
            let book = Book::open(dir).unwrap();
            let lane = Lane::new(&s, book);
            let locked = Database::open(dir.join("traffic.db")).unwrap();
            locked.execute_batch("BEGIN IMMEDIATE").unwrap();
            assert_eq!(lane.charge(80, true, false).await.unwrap(), 80);
            unsafe { libc::_exit(73) }
        }
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","gateway::tests::abrupt_restart_enforces_monthly_quota_before_statistics_flush"])
            .env(CASE, dir.path()).status().unwrap();
        assert_eq!(status.code(), Some(73));
        let book = Book::open(dir.path()).unwrap();
        let lane = Lane::new(&s, book.clone());
        assert!(lane.available());
        assert_eq!(book.snapshot()[&s.id].used(), 82);
        assert_eq!(book.snapshot()[&s.id].total_upload, 0);
        assert_eq!(lane.charge(80, false, false).await.unwrap(), 46);
        assert!(!lane.available());
    }
    #[tokio::test]
    async fn quota_is_shared_between_directions_and_survives_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let book = Book::open(temp.path()).unwrap();
        let s = spec(Limits {
            monthly_gib: Some(100.0 / 1073741824.0),
            ..Default::default()
        });
        let lane = Lane::new(&s, book.clone());
        assert_eq!(lane.charge(70, true, false).await.unwrap(), 70);
        assert_eq!(lane.charge(70, false, false).await.unwrap(), 30);
        assert!(!lane.available());
        book.save().unwrap();
        let reopened = Book::open(temp.path()).unwrap();
        let lane = Lane::new(&s, reopened.clone());
        assert!(!lane.available());
        assert!(lane.charge(1, true, false).await.is_err());
        assert_eq!(reopened.snapshot()[&s.id].total_download, 30);
    }
    #[tokio::test]
    async fn quota_drains_final_payload_before_closing_both_directions() {
        let temp = tempfile::tempdir().unwrap();
        let s = spec(Limits {
            monthly_gib: Some(10.0 / 1073741824.0),
            ..Default::default()
        });
        let lane = Lane::new(&s, Book::open(temp.path()).unwrap());
        let stop = CancellationToken::new();
        let monitor = tokio::spawn(lane.clone().supervise(stop.clone()));
        let token = lane.cancellation();
        let (mut client, inbound) = tokio::io::duplex(64);
        // A 1-byte outbound buffer holds the admitted 10-byte write in `transferring == 1`
        // until the target reads, verifying reverse traffic is denied before closing.
        let (outbound, mut target) = tokio::io::duplex(1);
        let job = tokio::spawn(relay(inbound, outbound, lane.clone()));
        client.write_all(b"0123456789").await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while lane.transferring.load(Ordering::Acquire) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!lane.available());
        assert!(!token.is_cancelled());
        // One byte fits the duplex buffer even while the denied reverse pump is draining.
        target.write_all(b"x").await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let mut received = [0; 10];
        tokio::time::timeout(Duration::from_secs(1), target.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&received, b"0123456789");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), job)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(token.is_cancelled());
        assert_eq!(client.read(&mut [0; 64]).await.unwrap(), 0);
        let traffic = &lane.book.snapshot()[&s.id];
        assert_eq!((traffic.upload, traffic.download), (10, 0));
        stop.cancel();
        monitor.await.unwrap();
    }

    #[tokio::test]
    async fn locked_sqlite_never_stalls_forwarding_and_flushes_exact_quota_once_unlocked() {
        let temp = tempfile::tempdir().unwrap();
        let s = spec(Limits {
            monthly_gib: Some(100.0 / 1073741824.0),
            ..Default::default()
        });
        let book = Book::open(temp.path()).unwrap();
        let lane = Lane::new(&s, book.clone());
        let database = Database::open(temp.path().join("traffic.db")).unwrap();
        database.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let stop = CancellationToken::new();
        let monitor = tokio::spawn(lane.clone().supervise(stop.clone()));
        let mut clients = Vec::new();
        let mut targets = Vec::new();
        let mut jobs = JoinSet::new();
        for _ in 0..5 {
            let (mut client, inbound) = tokio::io::duplex(64);
            let (outbound, target) = tokio::io::duplex(64);
            jobs.spawn(relay(inbound, outbound, lane.clone()));
            client.write_all(&[1; 23]).await.unwrap();
            client.shutdown().await.unwrap();
            clients.push(client);
            targets.push(target);
        }
        // Payloads forward immediately at memory speed even while SQLite holds a write lock.
        let mut received = 0;
        for target in &mut targets {
            let mut bytes = Vec::new();
            received +=
                tokio::time::timeout(Duration::from_millis(500), target.read_to_end(&mut bytes))
                    .await
                    .expect("forwarding must not block on SQLite lock")
                    .unwrap();
            drop(target.shutdown().await);
        }
        assert_eq!(received, 100);
        assert_eq!(book.snapshot()[&s.id].upload, 100);
        assert!(!lane.available());
        database.execute_batch("COMMIT;").unwrap();
        drop(database);
        while jobs.join_next().await.is_some() {}
        drop(clients);
        drop(targets);
        stop.cancel();
        monitor.await.unwrap();
        drop(lane);
        drop(book);
        let reopened = Book::open(temp.path()).unwrap();
        assert_eq!(reopened.snapshot()[&s.id].total_upload, 100);
        assert_eq!(reopened.snapshot()[&s.id].total_download, 0);
    }

    #[tokio::test]
    async fn quota_drain_is_bounded_when_destination_does_not_read() {
        let temp = tempfile::tempdir().unwrap();
        let s = spec(Limits {
            monthly_gib: Some(10.0 / 1073741824.0),
            ..Default::default()
        });
        let lane = Lane::new(&s, Book::open(temp.path()).unwrap());
        let stop = CancellationToken::new();
        let monitor = tokio::spawn(lane.clone().supervise(stop.clone()));
        let (mut client, inbound) = tokio::io::duplex(64);
        let (outbound, _unread_target) = tokio::io::duplex(1);
        let job = tokio::spawn(relay(inbound, outbound, lane.clone()));
        client.write_all(b"0123456789").await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while lane.transferring.load(Ordering::Acquire) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!lane.cancellation().is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_secs(6), job)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(lane.cancellation().is_cancelled());
        assert_eq!(lane.transferring.load(Ordering::Acquire), 0);
        stop.cancel();
        monitor.await.unwrap();
    }

    #[tokio::test]
    async fn acknowledged_bytes_survive_reopen_without_a_json_save() {
        let temp = tempfile::tempdir().unwrap();
        let s = spec(Limits::default());
        let book = Book::open(temp.path()).unwrap();
        let lane = Lane::new(&s, book.clone());
        lane.charge(13, true, false).await.unwrap();
        lane.charge(17, false, true).await.unwrap();
        assert!(!temp.path().join("traffic.json").exists());
        drop(lane);
        drop(book);
        // A stale compatibility snapshot must never replace the committed rows.
        std::fs::write(temp.path().join("traffic.json"), b"{}").unwrap();
        let book = Book::open(temp.path()).unwrap();
        let traffic = &book.snapshot()[&s.id];
        assert_eq!((traffic.upload, traffic.download), (13, 17));
        assert_eq!((traffic.total_upload, traffic.total_download), (13, 17));
    }

    #[test]
    fn legacy_import_and_marker_are_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let old = BTreeMap::from([
            (
                "a",
                Traffic {
                    total_upload: 13,
                    ..Default::default()
                },
            ),
            (
                "z",
                Traffic {
                    total_download: 17,
                    ..Default::default()
                },
            ),
        ]);
        std::fs::write(
            temp.path().join("traffic.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        let database = Database::open(temp.path().join("traffic.db")).unwrap();
        database.execute_batch("CREATE TABLE traffic(line_id TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TRIGGER fail_import BEFORE INSERT ON traffic WHEN NEW.line_id='z' BEGIN SELECT RAISE(FAIL,'injected import failure'); END;").unwrap();
        assert!(Book::open(temp.path()).is_err());
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM traffic", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        database.execute_batch("DROP TRIGGER fail_import;").unwrap();
        let book = Book::open(temp.path()).unwrap();
        assert_eq!(book.snapshot()["a"].total_upload, 13);
        assert_eq!(book.snapshot()["z"].total_download, 17);
        drop(book);
        std::fs::write(temp.path().join("traffic.json"), b"{}").unwrap();
        let book = Book::open(temp.path()).unwrap();
        assert_eq!(book.snapshot()["a"].total_upload, 13);
    }

    #[tokio::test]
    async fn failed_ledger_flush_keeps_live_forwarding_reports_error_and_recovers_all_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let s = spec(Limits::default());
        let book = Book::open(temp.path()).unwrap();
        let lane = Lane::new(&s, book.clone());
        lane.charge(7, true, false).await.unwrap();
        book.flush().unwrap();
        let database = Database::open(temp.path().join("traffic.db")).unwrap();
        database.execute_batch("CREATE TRIGGER fail_write BEFORE UPDATE ON traffic BEGIN SELECT RAISE(FAIL,'injected storage failure'); END;").unwrap();
        let (mut client, inbound) = tokio::io::duplex(64);
        let (outbound, mut target) = tokio::io::duplex(64);
        let job = tokio::spawn(relay(inbound, outbound, lane.clone()));
        client.write_all(b"still-forwarding").await.unwrap();
        client.shutdown().await.unwrap();
        let mut received = [0; 16];
        tokio::time::timeout(Duration::from_secs(1), target.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&received, b"still-forwarding");
        target.shutdown().await.unwrap();
        job.await.unwrap().unwrap();
        // Flush fails and surfaces through check_ledger/save, while live traffic stays online.
        assert!(book.save().is_err());
        assert!(book.check_ledger().is_err());
        assert!(lane.available());
        assert_eq!(book.snapshot()[&s.id].total_upload, 23);
        database.execute_batch("DROP TRIGGER fail_write;").unwrap();
        drop(database);
        book.flush().unwrap();
        assert!(book.check_ledger().is_ok());
        assert_eq!(lane.charge(3, false, true).await.unwrap(), 3);
        drop(lane);
        drop(book);
        let book = Book::open(temp.path()).unwrap();
        assert_eq!(book.snapshot()[&s.id].total_upload, 23);
        assert_eq!(book.snapshot()[&s.id].total_download, 3);
    }

    #[tokio::test]
    async fn high_bandwidth_rate_limit_exceeds_old_131mbps_timer_ceiling_accurately() {
        let temp = tempfile::tempdir().unwrap();
        let book = Book::open(temp.path()).unwrap();
        let lane = Lane::new(
            &spec(Limits {
                download_mbps: Some(500.0), // 500 Mbps = 62.5 MB/s
                ..Default::default()
            }),
            book,
        );
        let started = Instant::now();
        for _ in 0..400 {
            lane.charge(65_536, false, false).await.unwrap();
        }
        let elapsed = started.elapsed().as_secs_f64();
        let mbps = (400.0 * 65_536.0 * 8.0) / elapsed / 1e6;
        assert!(
            (380.0..=560.0).contains(&mbps),
            "500 Mbps 本地限速不准确: {mbps:.1} Mbps ({elapsed:.3}s)"
        );
    }
    #[tokio::test]
    async fn udp_is_never_partially_forwarded_and_new_month_resets_only_period_usage() {
        let temp = tempfile::tempdir().unwrap();
        let book = Book::open(temp.path()).unwrap();
        let s = spec(Limits {
            monthly_gib: Some(10.0 / 1073741824.0),
            ..Default::default()
        });
        let lane = Lane::new(&s, book.clone());
        assert!(lane.charge(11, true, true).await.is_err());
        assert_eq!(book.snapshot()[&s.id].total_upload, 0);
        lane.charge(10, true, true).await.unwrap();
        assert!(lane.available_at(now() + 32 * 86400));
        assert_eq!(book.snapshot()[&s.id].upload, 0);
        assert_eq!(book.snapshot()[&s.id].total_upload, 10);
        assert!(!lane.available(), "revisiting the spent period must not reopen the quota");
    }

    #[tokio::test]
    async fn concurrent_directions_cannot_overspend_one_quota() {
        let temp = tempfile::tempdir().unwrap();
        let book = Book::open(temp.path()).unwrap();
        let s = spec(Limits {
            monthly_gib: Some(100.0 / 1073741824.0),
            ..Default::default()
        });
        let lane = Lane::new(&s, book.clone());
        let mut jobs = JoinSet::new();
        for i in 0..20 {
            let lane = lane.clone();
            jobs.spawn(async move { lane.charge(23, i % 2 == 0, false).await.unwrap_or(0) });
        }
        let mut charged = 0;
        while let Some(n) = jobs.join_next().await {
            charged += n.unwrap();
        }
        assert_eq!(charged, 100);
        let t = &book.snapshot()[&s.id];
        assert_eq!(t.upload + t.download, 100);
        // This assertion compares exact directional statistics at a confirmed checkpoint.
        // The abrupt-restart test above checks the separate hard bound before that checkpoint.
        book.flush().unwrap();
        let reopened = Book::open(temp.path()).unwrap();
        let durable = &reopened.snapshot()[&s.id];
        assert_eq!(durable.upload + durable.download, 100);
        assert_eq!(durable.total_upload + durable.total_download, 100);
    }

    #[tokio::test]
    async fn disabling_closes_idle_connections_and_reopening_uses_new_token() {
        let temp = tempfile::tempdir().unwrap();
        let mut s = spec(Limits::default());
        let lane = Lane::new(&s, Book::open(temp.path()).unwrap());
        let old = lane.cancellation();
        let (mut client, inbound) = tokio::io::duplex(64);
        let (outbound, mut server) = tokio::io::duplex(64);
        let job = tokio::spawn(relay(inbound, outbound, lane.clone()));
        tokio::task::yield_now().await;
        s.enabled = false;
        lane.update(&s);
        assert!(old.is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), job)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(client.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
        s.enabled = true;
        lane.update(&s);
        assert!(!lane.cancellation().is_cancelled());
        assert!(old.is_cancelled());
    }

    #[tokio::test]
    async fn expiry_is_enforced_without_client_traffic_or_panel_heartbeat() {
        let temp = tempfile::tempdir().unwrap();
        let mut s = spec(Limits::default());
        s.lease = Some(LeasePolicy {
            cycle_anchor: now() - 1,
            expires_at: now() + 1,
        });
        let lane = Lane::new(&s, Book::open(temp.path()).unwrap());
        let token = lane.cancellation();
        let stop = CancellationToken::new();
        let monitor = tokio::spawn(lane.clone().supervise(stop.clone()));
        tokio::time::timeout(Duration::from_secs(3), token.cancelled())
            .await
            .unwrap();
        assert!(!lane.available());
        stop.cancel();
        monitor.await.unwrap();
    }

    #[tokio::test]
    async fn permanent_lease_keeps_quota_and_manual_disable() {
        let temp = tempfile::tempdir().unwrap();
        let mut s = spec(Limits {
            monthly_gib: Some(10.0 / 1073741824.0),
            ..Default::default()
        });
        s.lease = Some(LeasePolicy {
            cycle_anchor: now() - 1,
            expires_at: 0,
        });
        let lane = Lane::new(&s, Book::open(temp.path()).unwrap());
        assert!(lane.available_at(now() + 400 * 86400));
        assert_eq!(lane.charge(10, true, false).await.unwrap(), 10);
        assert!(!lane.available());
        s.enabled = false;
        s.limits.monthly_gib = Some(20.0 / 1073741824.0);
        lane.update(&s);
        assert!(!lane.available());
        s.enabled = true;
        lane.update(&s);
        assert!(lane.available());
    }

    #[test]
    fn rental_cycle_resets_period_usage_and_keeps_lifetime_bytes() {
        let date = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp() as u64;
        let temp = tempfile::tempdir().unwrap();
        let mut s = spec(Limits::default());
        s.lease = Some(LeasePolicy {
            cycle_anchor: date("2028-01-31T23:30:00+08:00"),
            expires_at: date("2028-04-30T23:30:00+08:00"),
        });
        let book = Book::open(temp.path()).unwrap();
        let lane = Lane::new(&s, book.clone());
        assert!(lane.available_at(date("2028-02-29T23:29:59+08:00")));
        {
            let mut state = book.ledger.lock().unwrap();
            let t = state.items.get_mut(&s.id).unwrap();
            t.upload = 20;
            t.total_upload = 20;
        }
        assert!(lane.available_at(date("2028-02-29T23:30:00+08:00")));
        let t = &book.snapshot()[&s.id];
        assert_eq!((t.upload, t.total_upload), (0, 20));
        assert_eq!(t.cycle_end, date("2028-03-31T23:30:00+08:00"));
        assert!(!lane.available_at(date("2028-04-30T23:30:00+08:00")));
        s.lease = Some(LeasePolicy {
            cycle_anchor: 0,
            expires_at: 0,
        });
        lane.update(&s);
        assert!(!lane.available());
    }

    #[test]
    fn metric_rates_are_unknown_initially_and_after_counter_reset() {
        let old = SystemSample {
            at: Instant::now(),
            cpu: Some((100, 20)),
            rx: 100,
            tx: 200,
        };
        let next = SystemSample {
            at: old.at + Duration::from_secs(2),
            cpu: Some((200, 60)),
            rx: 300,
            tx: 600,
        };
        assert_eq!(metric_rates(None, &old), (None, None, None));
        assert_eq!(
            metric_rates(Some(&old), &next),
            (Some(60.0), Some(100.0), Some(200.0))
        );
        let reset = SystemSample {
            at: next.at + Duration::from_secs(1),
            cpu: Some((10, 2)),
            rx: 1,
            tx: 1,
        };
        assert_eq!(metric_rates(Some(&next), &reset), (None, None, None));
    }
}
