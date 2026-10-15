//! Per-line data plane: byte accounting, shared rate budgets and local quota enforcement.
//! No host firewall or qdisc is changed. All listeners/tasks belong to this Agent.
use crate::{model::*, runtime::atomic_write};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Spec {
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

pub struct Book {
    path: PathBuf,
    items: Mutex<BTreeMap<String, Traffic>>,
    sample: Mutex<(Instant, BTreeMap<String, [u64; 2]>)>,
}
impl Book {
    pub fn open(dir: &std::path::Path) -> Result<Arc<Self>> {
        let path = dir.join("traffic.json");
        let mut items: BTreeMap<String, Traffic> = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path)?)?
        } else {
            BTreeMap::new()
        };
        for v in items.values_mut() {
            v.connections = 0;
            v.upload_bps = 0.0;
            v.download_bps = 0.0;
        }
        Ok(Arc::new(Self {
            path,
            items: Mutex::new(items),
            sample: Mutex::new((Instant::now(), BTreeMap::new())),
        }))
    }
    pub fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&*self.items.lock().unwrap())?;
        atomic_write(&self.path, &bytes)
    }
    pub fn snapshot(&self) -> BTreeMap<String, Traffic> {
        let mut sample = self.sample.lock().unwrap();
        let elapsed = sample.0.elapsed().as_secs_f64();
        let mut items = self.items.lock().unwrap();
        for (id, t) in items.iter_mut() {
            rollover(t);
            let old = sample
                .1
                .get(id)
                .copied()
                .unwrap_or([t.total_upload, t.total_download]);
            if elapsed >= 0.5 {
                t.upload_bps = t.total_upload.saturating_sub(old[0]) as f64 / elapsed;
                t.download_bps = t.total_download.saturating_sub(old[1]) as f64 / elapsed;
                sample
                    .1
                    .insert(id.clone(), [t.total_upload, t.total_download]);
            }
            t.sampled_at = now();
        }
        if elapsed >= 0.5 {
            sample.0 = Instant::now();
        }
        items.clone()
    }
}
fn rollover(t: &mut Traffic) {
    let month = Utc::now().format("%Y-%m").to_string();
    if t.month != month {
        t.month = month;
        t.upload = 0;
        t.download = 0;
        t.blocked = false;
    }
    while t.days.len() > 35 {
        let key = t.days.keys().next().unwrap().clone();
        t.days.remove(&key);
    }
}

pub struct Lane {
    pub id: String,
    pub book: Arc<Book>,
    limits: RwLock<Limits>,
    clocks: [tokio::sync::Mutex<Instant>; 2],
}
impl Lane {
    pub fn new(spec: &Spec, book: Arc<Book>) -> Arc<Self> {
        book.items
            .lock()
            .unwrap()
            .entry(spec.id.clone())
            .or_insert_with(|| Traffic {
                epoch: id(),
                ..Default::default()
            });
        Arc::new(Self {
            id: spec.id.clone(),
            book,
            limits: RwLock::new(spec.limits.clone()),
            clocks: [
                tokio::sync::Mutex::new(Instant::now()),
                tokio::sync::Mutex::new(Instant::now()),
            ],
        })
    }
    pub fn update(&self, limits: &Limits) {
        *self.limits.write().unwrap() = limits.clone();
    }
    pub fn available(&self) -> bool {
        let limit = self.limits.read().unwrap().monthly_gib;
        let mut items = self.book.items.lock().unwrap();
        let t = items.get_mut(&self.id).unwrap();
        rollover(t);
        t.blocked =
            limit.is_some_and(|q| t.upload.saturating_add(t.download) >= (q * 1073741824.0) as u64);
        !t.blocked
    }
    pub async fn charge(&self, amount: usize, upload: bool, datagram: bool) -> io::Result<usize> {
        if amount == 0 {
            return Ok(0);
        }
        if !self.available() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "本月线路额度已用尽",
            ));
        }
        let limits = self.limits.read().unwrap().clone();
        let rate = if upload {
            limits.upload_mbps
        } else {
            limits.download_mbps
        };
        if let Some(rate) = rate {
            let mut clock = self.clocks[if upload { 0 } else { 1 }].lock().await;
            // A single shared clock, including all concurrent connections and UDP sessions.
            let start = (*clock).max(Instant::now());
            *clock = start + Duration::from_secs_f64(amount as f64 / (rate * 125000.0));
            let until = *clock;
            drop(clock);
            tokio::time::sleep_until(until.into()).await;
        }
        let limits = self.limits.read().unwrap().clone();
        let mut items = self.book.items.lock().unwrap();
        let t = items.get_mut(&self.id).unwrap();
        rollover(t);
        let left = limits
            .monthly_gib
            .map(|q| {
                ((q * 1073741824.0) as u64).saturating_sub(t.upload.saturating_add(t.download))
            })
            .unwrap_or(u64::MAX);
        let allowed = amount.min(left.min(usize::MAX as u64) as usize);
        if allowed == 0 || datagram && allowed < amount {
            t.blocked = true;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "本月线路额度不足",
            ));
        }
        let day = t
            .days
            .entry(Utc::now().format("%Y-%m-%d").to_string())
            .or_default();
        if upload {
            t.upload = t.upload.saturating_add(allowed as u64);
            t.total_upload = t.total_upload.saturating_add(allowed as u64);
            day[0] = day[0].saturating_add(allowed as u64);
        } else {
            t.download = t.download.saturating_add(allowed as u64);
            t.total_download = t.total_download.saturating_add(allowed as u64);
            day[1] = day[1].saturating_add(allowed as u64);
        }
        t.blocked = limits
            .monthly_gib
            .is_some_and(|q| t.upload.saturating_add(t.download) >= (q * 1073741824.0) as u64);
        Ok(allowed)
    }
    pub fn connection(self: &Arc<Self>) -> Connection {
        self.book
            .items
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .unwrap()
            .connections += 1;
        Connection(self.clone())
    }
}
pub struct Connection(Arc<Lane>);
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(t) = self.0.book.items.lock().unwrap().get_mut(&self.0.id) {
            t.connections = t.connections.saturating_sub(1);
        }
    }
}

pub async fn pump<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut r: R,
    mut w: W,
    lane: Arc<Lane>,
    upload: bool,
) -> io::Result<()> {
    let mut buf = [0; 16384];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(300), r.read(&mut buf)).await??;
        if n == 0 {
            return w.shutdown().await;
        }
        let allowed = lane.charge(n, upload, false).await?;
        w.write_all(&buf[..allowed]).await?;
        if allowed < n {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "本月线路额度已用尽",
            ));
        }
    }
}
pub async fn relay<A: AsyncRead + AsyncWrite + Unpin, B: AsyncRead + AsyncWrite + Unpin>(
    a: A,
    b: B,
    lane: Arc<Lane>,
) -> io::Result<()> {
    let _connection = lane.connection();
    let (ar, aw) = tokio::io::split(a);
    let (br, bw) = tokio::io::split(b);
    tokio::try_join!(pump(ar, bw, lane.clone(), true), pump(br, aw, lane, false))?;
    Ok(())
}

// SOCKS5 UDP replies must advertise the public listener, never the loopback backend.
async fn socks_handshake<A: AsyncRead + AsyncWrite + Unpin>(
    client: &mut A,
    backend: &mut TcpStream,
    public_port: u16,
) -> io::Result<bool> {
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
    backend.write_all(&req).await?;
    let address = read_socks_address(client, req[3]).await?;
    backend.write_all(&address).await?;
    let mut reply = [0u8; 4];
    backend.read_exact(&mut reply).await?;
    let addr = read_socks_address(backend, reply[3]).await?;
    if req[1] == 3 && reply[1] == 0 {
        client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0]).await?;
        client.write_all(&public_port.to_be_bytes()).await?;
    } else {
        client.write_all(&reply).await?;
        client.write_all(&addr).await?;
    }
    Ok(req[1] == 3 && reply[1] == 0)
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
type Peers = Arc<Mutex<BTreeMap<std::net::IpAddr, usize>>>;
struct UdpPermit {
    peers: Peers,
    peer: std::net::IpAddr,
}
impl Drop for UdpPermit {
    fn drop(&mut self) {
        let mut peers = self.peers.lock().unwrap();
        if let Some(count) = peers.get_mut(&self.peer) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                peers.remove(&self.peer);
            }
        }
    }
}
async fn connection<A: AsyncRead + AsyncWrite + Unpin>(
    mut stream: A,
    spec: Spec,
    lane: Arc<Lane>,
    peers: Peers,
    peer: std::net::IpAddr,
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
    let _permit = if spec.protocol == Protocol::Socks
        && spec.kind == Kind::Vless
        && tokio::time::timeout(
            Duration::from_secs(15),
            socks_handshake(&mut stream, &mut backend, spec.port),
        )
        .await??
    {
        *peers.lock().unwrap().entry(peer).or_default() += 1;
        Some(UdpPermit { peers, peer })
    } else {
        None
    };
    relay(stream, backend, lane).await?;
    Ok(())
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
            self.lanes
                .insert(spec.id.clone(), Lane::new(spec, book.clone()));
        }
        for spec in specs.iter().filter(|s| s.kind != Kind::Website) {
            let lane = self.lanes[&spec.id].clone();
            let peers: Peers = Arc::new(Mutex::new(BTreeMap::new()));
            if spec.network.contains("tcp") {
                let listener = TcpListener::bind(("0.0.0.0", spec.port))
                    .await
                    .with_context(|| format!("入口 TCP {} 已占用", spec.port))?;
                let spec = spec.clone();
                let cancel = self.cancel.clone();
                let lane = lane.clone();
                let dir = dir.clone();
                let peers = peers.clone();
                self.tasks.push(tokio::spawn(async move {
                    let mut children=JoinSet::new();
                    loop {tokio::select!{
                        _=cancel.cancelled()=>break,
                        Some(_)=children.join_next(),if !children.is_empty()=>{},
                        result=listener.accept()=>{
                            let Ok((stream,peer))=result else {break};
                            if children.len()>=1024 {continue;}
                            let spec=spec.clone();let lane=lane.clone();let dir=dir.clone();let peers=peers.clone();
                            children.spawn(async move {
                                let _=stream.set_nodelay(true);
                                if matches!(spec.tls.mode.as_str(),"manual"|"auto") && spec.protocol != Protocol::Vless {
                                    if let Ok(config)=crate::website::load_tls(&spec.tls,&dir) {
                                        if let Ok(Ok(tls))=tokio::time::timeout(Duration::from_secs(15),tokio_rustls::TlsAcceptor::from(config).accept(stream)).await {let _=connection(tls,spec,lane,peers,peer.ip()).await;}
                                    }
                                } else {let _=connection(stream,spec,lane,peers,peer.ip()).await;}
                            });
                        }
                    }}
                    children.abort_all();while children.join_next().await.is_some() {}
                }));
            }
            if spec.network.contains("udp") {
                let socket = Arc::new(
                    UdpSocket::bind(("0.0.0.0", spec.port))
                        .await
                        .with_context(|| format!("入口 UDP {} 已占用", spec.port))?,
                );
                let spec = spec.clone();
                let cancel = self.cancel.clone();
                self.tasks.push(tokio::spawn(async move {
                    let mut sessions:BTreeMap<std::net::SocketAddr,mpsc::Sender<Vec<u8>>>=BTreeMap::new();
                    let mut children=JoinSet::new();let mut buf=vec![0u8;65536];
                    loop {tokio::select!{
                        _=cancel.cancelled()=>break,
                        Some(_)=children.join_next(),if !children.is_empty()=>{sessions.retain(|_,tx|!tx.is_closed());},
                        received=socket.recv_from(&mut buf)=>{
                            let Ok((n,peer))=received else {break};
                            if spec.kind==Kind::Vless && spec.protocol==Protocol::Socks && !peers.lock().unwrap().contains_key(&peer.ip()) {continue;}
                            if !lane.available() {continue;}
                            if sessions.get(&peer).is_some_and(|tx|tx.is_closed()) {sessions.remove(&peer);}
                            if !sessions.contains_key(&peer) {
                                if sessions.len()>=1024 {continue;}
                                let (tx,mut rx)=mpsc::channel::<Vec<u8>>(8);sessions.insert(peer,tx);
                                let socket=socket.clone();let lane=lane.clone();let backend_port=spec.backend;
                                let peers=peers.clone();let authenticated=spec.kind==Kind::Vless&&spec.protocol==Protocol::Socks;
                                children.spawn(async move {
                                    let result=async {
                                        let backend=UdpSocket::bind("127.0.0.1:0").await?;backend.connect(("127.0.0.1",backend_port)).await?;
                                        let _connection=lane.connection();let mut out=vec![0u8;65536];
                                        loop {tokio::select!{
                                            packet=tokio::time::timeout(Duration::from_secs(60),rx.recv())=>{
                                                let Ok(Some(packet))=packet else {break};
                                                if authenticated && !peers.lock().unwrap().contains_key(&peer.ip()) {break;}
                                                lane.charge(packet.len(),true,true).await?;backend.send(&packet).await?;
                                            }
                                            response=backend.recv(&mut out)=>{
                                                let n=response?;
                                                if authenticated && !peers.lock().unwrap().contains_key(&peer.ip()) {break;}
                                                lane.charge(n,false,true).await?;socket.send_to(&out[..n],peer).await?;
                                            }
                                        }}Ok::<(),io::Error>(())
                                    }.await;let _=result;
                                });
                            }
                            let _=sessions[&peer].try_send(buf[..n].to_vec());
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
                lane.update(&spec.limits);
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
    }
    Ok(specs)
}
pub fn xray_config(config: &serde_json::Value) -> serde_json::Value {
    let mut v = config.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("_chain");
        m.remove("_mihomo");
    }
    v
}

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
    serde_json::json!({"gateway_version":1,"memory_total":total,"memory_used":total.saturating_sub(available),"load_1m":load,"network_rx":rx,"network_tx":tx,"listening_ports":listening_ports,"sampled_at":now()})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(limits: Limits) -> Spec {
        Spec {
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
        book.items.lock().unwrap().get_mut(&s.id).unwrap().month = "2000-01".into();
        assert!(lane.available());
        assert_eq!(book.snapshot()[&s.id].upload, 0);
        assert_eq!(book.snapshot()[&s.id].total_upload, 10);
    }
}
