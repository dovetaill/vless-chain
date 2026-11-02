//! Bounded, loopback-only acceptance of original client exports and shared rights.
use super::{
    super::{Options, strings, workspace::Workspace},
    business::{line, node},
    harness::{Harness, map},
    protocols::PROFILES,
    rentals::operate,
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use futures_util::future::join_all;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};

fn now() -> u64 {
    chrono::Utc::now().timestamp() as u64
}
fn number(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or(0)
}
fn sum(v: &Value) -> u64 {
    number(v, "upload") + number(v, "download")
}

async fn connect(port: u16, destination: u16) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(&[5, 1, 0]).await?;
    let mut auth = [0; 2];
    stream.read_exact(&mut auth).await?;
    ensure!(auth == [5, 0], "本地 SOCKS 协商失败");
    let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
    request.extend(destination.to_be_bytes());
    stream.write_all(&request).await?;
    let mut reply = [0; 10];
    stream.read_exact(&mut reply).await?;
    ensure!(reply[..2] == [5, 0] && reply[3] == 1, "代理拒绝 TCP");
    Ok(stream)
}
async fn transfer(
    port: u16,
    destination: u16,
    mode: u8,
    size: usize,
    seconds: u64,
    timeout: u64,
) -> Value {
    let started = Instant::now();
    let (mut sent, mut received, mut chunks, mut left) = (0, 0, 0, size);
    let result = tokio::time::timeout(Duration::from_secs(timeout), async {
        let mut stream = connect(port, destination).await?;
        stream.write_all(&[mode]).await?;
        stream.write_u32(size as u32).await?;
        sent += 5;
        let bytes = vec![b'x'; 65536];
        let mut reply = vec![0; 65536];
        while left > 0 && (seconds == 0 || started.elapsed() < Duration::from_secs(seconds)) {
            let n = left.min(bytes.len());
            if mode != b'D' {
                stream.write_all(&bytes[..n]).await?;
                sent += n;
            }
            if mode != b'U' {
                stream.read_exact(&mut reply[..n]).await?;
                received += n;
                ensure!(reply[..n] == bytes[..n], "载荷被修改或截断");
            }
            left -= n;
            chunks += 1;
        }
        if mode == b'U' {
            let ack = stream.read_u64().await?;
            received += 8;
            ensure!(ack == size as u64, "目标未确认全部上传");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) => Some(e.to_string()),
    };
    json!({"ok":error.is_none(),"sent":sent,"received":received,"chunks":chunks,"elapsed":started.elapsed().as_secs_f64(),"stop":if left==0{"bytes"}else{"time"},"error":error})
}
struct Association {
    _control: TcpStream,
    socket: UdpSocket,
    relay: std::net::SocketAddr,
}
impl Association {
    async fn new(port: u16) -> Result<Self> {
        let mut control = TcpStream::connect(("127.0.0.1", port)).await?;
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        control.write_all(&[5, 1, 0]).await?;
        let mut auth = [0; 2];
        control.read_exact(&mut auth).await?;
        ensure!(auth == [5, 0], "UDP SOCKS 协商失败");
        control.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        let mut reply = [0; 10];
        control.read_exact(&mut reply).await?;
        ensure!(reply[..2] == [5, 0] && reply[3] == 1, "UDP 关联被拒绝");
        Ok(Self {
            _control: control,
            socket,
            relay: ([127, 0, 0, 1], u16::from_be_bytes([reply[8], reply[9]])).into(),
        })
    }
    async fn exchange(&self, destination: u16, payload: &[u8], timeout: u64) -> Result<()> {
        let mut packet = vec![0, 0, 0, 1, 127, 0, 0, 1];
        packet.extend(destination.to_be_bytes());
        packet.extend(payload);
        self.socket.send_to(&packet, self.relay).await?;
        let mut reply = vec![0; 65536];
        let n = tokio::time::timeout(Duration::from_secs(timeout), self.socket.recv(&mut reply))
            .await??;
        ensure!(reply[..n] == packet, "UDP 序号或载荷变化");
        Ok(())
    }
}
async fn udp_request(port: u16, destination: u16, payload: &[u8], timeout: u64) -> Value {
    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(timeout + 2), async {
        Association::new(port)
            .await?
            .exchange(destination, payload, timeout)
            .await
    })
    .await;
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) => Some(e.to_string()),
    };
    json!({"ok":error.is_none(),"sent":payload.len(),"received":if error.is_none(){payload.len()}else{0},"elapsed":start.elapsed().as_secs_f64(),"error":error})
}
fn aggregate(results: Vec<Value>, started: Instant) -> Value {
    let seconds = started.elapsed().as_secs_f64();
    let count = results.len();
    let success = results.iter().filter(|r| r["ok"] == true).count();
    let sent = results.iter().map(|r| number(r, "sent")).sum::<u64>();
    let received = results.iter().map(|r| number(r, "received")).sum::<u64>();
    let errors = results
        .iter()
        .filter_map(|r| r["error"].as_str())
        .collect::<BTreeSet<_>>();
    let mut value = json!({"concurrency":count,"success":success,"failure":count-success,"seconds":seconds,"sent_bytes":sent,"received_bytes":received,"average_mbps":(sent+received)as f64*8.0/seconds/1e6,"upload_mbps":sent as f64*8.0/seconds/1e6,"download_mbps":received as f64*8.0/seconds/1e6,"errors":errors,"stop_counts":{"time":results.iter().filter(|r|r["stop"]=="time").count(),"bytes":results.iter().filter(|r|r["stop"]=="bytes").count()}});
    for key in [
        "attempted_packets",
        "confirmed_packets",
        "unconfirmed_packets",
    ] {
        value[key] = json!(results.iter().map(|r| number(r, key)).sum::<u64>());
    }
    value
}
async fn parallel_tcp(
    port: u16,
    target: u16,
    count: usize,
    mode: u8,
    size: usize,
    seconds: u64,
    timeout: u64,
) -> Value {
    let start = Instant::now();
    aggregate(
        join_all((0..count).map(|_| transfer(port, target, mode, size, seconds, timeout))).await,
        start,
    )
}
type Records = Arc<Mutex<BTreeMap<[u8; 16], (u64, u64, BTreeSet<Vec<u8>>)>>>;
fn token(records: &Records) -> [u8; 16] {
    let token = *uuid::Uuid::new_v4().as_bytes();
    records
        .lock()
        .unwrap()
        .insert(token, (0, 0, BTreeSet::new()));
    token
}
fn observed(records: &Records, token: [u8; 16]) -> Value {
    let guard = records.lock().unwrap();
    let (packets, bytes, sequences) = &guard[&token];
    json!({"received_packets":packets,"received_bytes":bytes,"unique_packets":sequences.len(),"duplicates":packets-sequences.len()as u64})
}
async fn udp_stream(
    association: &Association,
    target: u16,
    token: [u8; 16],
    stream: usize,
    count: usize,
    timeout: u64,
) -> Value {
    let start = Instant::now();
    let (mut confirmed, mut attempted) = (0, 0);
    let result = async {
        for sequence in 0..count {
            ensure!(
                start.elapsed() < Duration::from_secs(20),
                "UDP 流超过 20 秒界限"
            );
            let mut marker = token.to_vec();
            marker.extend((stream as u32).to_be_bytes());
            marker.extend((sequence as u32).to_be_bytes());
            marker.extend(vec![b'x'; 1000]);
            attempted += 1;
            association.exchange(target, &marker, timeout).await?;
            confirmed += 1;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    json!({"ok":result.is_ok(),"sent":confirmed*1024,"received":confirmed*1024,"attempted_packets":attempted,"confirmed_packets":confirmed,"unconfirmed_packets":attempted-confirmed,"elapsed":start.elapsed().as_secs_f64(),"error":result.err().map(|e|e.to_string())})
}
async fn udp_batch(
    port: u16,
    target: u16,
    records: &Records,
    token: [u8; 16],
    count: usize,
    associations: Option<&[Association]>,
    timeout: u64,
) -> Value {
    let start = Instant::now();
    let results=join_all((0..16).map(|index|async move{
        if let Some(associations)=associations {udp_stream(&associations[index],target,token,index,count,timeout).await}
        else {match tokio::time::timeout(Duration::from_secs(5),Association::new(port)).await{
            Ok(Ok(a))=>udp_stream(&a,target,token,index,count,timeout).await,
            result=>json!({"ok":false,"error":format!("UDP 关联失败：{}",match result{Ok(Err(e))=>e.to_string(),Err(e)=>e.to_string(),_=>unreachable!()})}),
        }}
    })).await;
    let mut result = aggregate(results, start);
    result["payload_mbps"] = json!(
        number(&result, "received_bytes") as f64 * 8.0 / result["seconds"].as_f64().unwrap() / 1e6
    );
    result["target"] = observed(records, token);
    result
}
async fn servers(h: &Harness<'_>) -> Result<(u16, u16, Arc<AtomicU64>, Records)> {
    let tcp = TcpListener::bind("127.0.0.1:0").await?;
    let tcp_port = tcp.local_addr()?.port();
    h.remember(tcp_port)?;
    let accepted = Arc::new(AtomicU64::new(0));
    let counter = accepted.clone();
    h.task(tokio::spawn(async move{
        let mut children=tokio::task::JoinSet::new();
        loop{tokio::select!{connection=tcp.accept()=>{let Ok((mut stream,_))=connection else{break};counter.fetch_add(1,Ordering::SeqCst);
            children.spawn(async move{let _=tokio::time::timeout(Duration::from_secs(20),async{
                let mode=stream.read_u8().await?;let mut left=stream.read_u32().await?as usize;let total=left;
                ensure!(left<=8*1024*1024&&[b'U',b'D',b'E'].contains(&mode),"传输夹具头无效");
                let mut buffer=vec![b'x';65536];while left>0{let n=left.min(buffer.len());let n=if mode==b'D'{n}else{let n=stream.read(&mut buffer[..n]).await?;ensure!(n>0,"传输提前关闭");n};
                    if mode!=b'U'{stream.write_all(&buffer[..n]).await?;}left-=n;
                }if mode==b'U'{stream.write_u64(total as u64).await?;}Ok::<_,anyhow::Error>(())
            }).await;});},_=children.join_next(),if !children.is_empty()=>{}}}
    }));
    let udp = UdpSocket::bind("127.0.0.1:0").await?;
    let udp_port = udp.local_addr()?.port();
    h.remember(udp_port)?;
    let records: Records = Arc::new(Mutex::new(BTreeMap::new()));
    let observations = records.clone();
    h.task(tokio::spawn(async move {
        let mut buffer = vec![0; 65536];
        while let Ok((n, peer)) = udp.recv_from(&mut buffer).await {
            if n >= 24 {
                let key: [u8; 16] = buffer[..16].try_into().unwrap();
                if let Some((packets, bytes, sequences)) =
                    observations.lock().unwrap().get_mut(&key)
                {
                    *packets += 1;
                    *bytes += n as u64;
                    sequences.insert(buffer[16..24].to_vec());
                }
            }
            let _ = udp.send_to(&buffer[..n], peer).await;
        }
    }));
    Ok((tcp_port, udp_port, accepted, records))
}
async fn fresh(h: &Harness<'_>, line: &Value, at: u64) -> Result<Value> {
    let state = h
        .wait_state("新的数据面采样", 12, |s| {
            s["lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == line["id"] && number(&v["traffic"], "sampled_at") > at)
        })
        .await?;
    Ok(state["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == line["id"])
        .unwrap()["traffic"]
        .clone())
}
async fn accounted(
    h: &Harness<'_>,
    line: &Value,
    before: &Value,
    result: &mut Value,
) -> Result<Value> {
    let after = fresh(h, line, now()).await?;
    for direction in ["upload", "download"] {
        result[format!("meter_{direction}_bytes")] = json!(
            number(&after, direction)
                .checked_sub(number(before, direction))
                .context("计量发生回退")?
        );
        result[format!("reported_{direction}_bps")] = after[format!("{direction}_bps")].clone();
    }
    Ok(after)
}
async fn limits(h: &Harness<'_>, line: &Value, value: Value) -> Result<()> {
    operate(h, &[line.clone()], "limits", json!({"limits":value})).await?;
    h.synced(&[line.clone()]).await
}
fn row(
    h: &Harness<'_>,
    results: &mut Vec<Value>,
    name: &str,
    stage: &str,
    mut value: Value,
) -> Result<()> {
    value["profile"] = json!(name);
    value["stage"] = json!(stage);
    println!("LOAD_CASE {}", serde_json::to_string(&value)?);
    results.push(value);
    fs::write(
        h.root.join("load-protocols-results.json"),
        serde_json::to_vec_pretty(results)?,
    )?;
    Ok(())
}
fn selected(
    args: &Options,
    key: &str,
    choices: &[&str],
    defaults: &[&str],
) -> Result<BTreeSet<String>> {
    let values = args
        .values
        .get(key)
        .map(|s| s.split(',').collect::<Vec<_>>())
        .unwrap_or_else(|| defaults.to_vec());
    ensure!(
        !values.is_empty() && values.iter().all(|v| choices.contains(v)),
        "{key} 含未知或空选择"
    );
    Ok(values.into_iter().map(String::from).collect())
}
pub async fn run(work: &Workspace, stage: &Path, args: &Options) -> Result<()> {
    let stages = selected(
        args,
        "--stages",
        &["basic", "auth", "load", "rate", "quota", "udp-rights"],
        &["basic", "auth", "load", "rate", "quota"],
    )?;
    let names = PROFILES.iter().map(|p| p.0).collect::<Vec<_>>();
    let profiles = selected(args, "--profiles", &names, &names)?;
    let mut h = Harness::new(work, stage, "load-protocols")?;
    let (tcp, udp, accepted, records) = servers(&h).await?;
    h.allow_loopback(&[tcp, udp])?;
    let (target, cert, key) = h.tls("localhost").await?;
    h.panel(map(&[("SSL_CERT_FILE", cert.display().to_string())]))
        .await?;
    ensure!(
        h.state().await?["lines"] == json!([]),
        "负载验收需要空代理列表"
    );
    let node = node(&h, "有界协议负载验收", Some(target)).await?;
    h.preflight(target, true).await?;
    h.agent(
        0,
        &node,
        map(&[("SSL_CERT_FILE", cert.display().to_string())]),
    )
    .await?;
    h.wait_state("负载 Agent", 30, |s| s["nodes"][0]["online"] == true)
        .await?;
    let tls = json!({"mode":"manual","domain":"localhost","cert":fs::read_to_string(&cert)?,"key":fs::read_to_string(&key)?});
    let mut lines = BTreeMap::new();
    let mut created = vec![];
    for (name, protocol, secure, network) in PROFILES.iter().filter(|p| profiles.contains(p.0)) {
        let value=line(&h,&[node.clone()],&format!("load-verify-{name}"),&[0],json!({"access":{"protocol":protocol,"host":"127.0.0.1","tls":if *secure{tls.clone()}else{json!({"mode":"off"})},"transport":{"network":network,"path":"/load-protocol","mode":"auto"}}}),200).await?;
        created.push(value.clone());
        lines.insert(name.to_string(), value);
    }
    let isolated = line(
        &h,
        &[node.clone()],
        "load-isolation",
        &[0],
        json!({"access":{"protocol":"socks"}}),
        200,
    )
    .await?;
    created.push(isolated.clone());
    ensure!(
        h.state().await?["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| created.iter().any(|c| c["id"] == l["id"])),
        "出现其他会话草稿，拒绝发布"
    );
    h.publish("applied").await?;
    h.synced(&created).await?;
    let (mut proxies, mut invalid) = (vec![], BTreeMap::<String, Vec<String>>::new());
    for (name, line) in lines
        .iter()
        .chain(std::iter::once((&"isolation".to_string(), &isolated)))
    {
        let export = h
            .request(
                &format!("/api/lines/{}/export", line["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        let mut proxy: Value = serde_json::from_str(
            export["node"]
                .as_str()
                .context("缺少原始导出")?
                .strip_prefix("- ")
                .context("导出格式不正确")?,
        )?;
        proxy["name"] = json!(name);
        proxy["server"] = json!("127.0.0.1");
        proxies.push(proxy.clone());
        let fields = match proxy["type"].as_str().context("缺少协议")? {
            "vless" | "vmess" => vec!["uuid"],
            "tuic" => vec!["uuid", "password"],
            "http" | "socks5" => vec!["username", "password"],
            _ => vec!["password"],
        };
        for field in fields {
            let mut bad = proxy.clone();
            let label = format!("{name}-wrong-{field}");
            bad["name"] = json!(label);
            bad[field] = json!(if field == "uuid" {
                uuid::Uuid::new_v4().to_string()
            } else if proxy["type"] == "ss" {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(uuid::Uuid::new_v4().as_bytes())
            } else {
                "incorrect-authentication".into()
            });
            proxies.push(bad);
            invalid.entry(name.clone()).or_default().push(label);
        }
    }
    let (port, api, isolation) = (h.port()?, h.port()?, h.port()?);
    let secret = uuid::Uuid::new_v4().to_string();
    let core = super::protocols::binary(&h)?;
    for (name, listener) in [("load-client", port), ("load-isolation-client", isolation)] {
        let directory = h.root.join(name);
        fs::create_dir(&directory)?;
        let mut config = json!({"mixed-port":listener,"bind-address":"127.0.0.1","allow-lan":false,"mode":"rule","log-level":"warning","proxies":proxies.iter().filter(|p|name=="load-client"||p["name"]=="isolation").collect::<Vec<_>>(),"rules":["MATCH,isolation"]});
        if name == "load-client" {
            config["external-controller"] = json!(format!("127.0.0.1:{api}"));
            config["secret"] = json!(secret);
            config["proxy-groups"] = json!([{"name":"load-selection","type":"select","proxies":proxies.iter().map(|p|p["name"].clone()).collect::<Vec<_>>()}]);
            config["rules"] = json!(["MATCH,load-selection"]);
        }
        let path = directory.join("config.json");
        crate::ctl::upgrade::atomic(&path, &serde_json::to_vec(&config)?, 0o600)?;
        h.launch(
            name,
            &core,
            map(&[("SSL_CERT_FILE", cert.display().to_string())]),
            &strings(&[
                "-f",
                path.to_str().unwrap(),
                "-d",
                directory.to_str().unwrap(),
            ]),
        )?;
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let select = async |name: &str| -> Result<()> {
        let r = client
            .put(format!("http://127.0.0.1:{api}/proxies/load-selection"))
            .bearer_auth(&secret)
            .json(&json!({"name":name}))
            .send()
            .await?;
        ensure!(r.status() == 204, "Mihomo 选择失败");
        Ok(())
    };
    wait("Mihomo 控制器", async || {
        Ok(select(lines.keys().next().unwrap()).await.is_ok())
    })
    .await?;
    wait("隔离客户端", async || {
        Ok(TcpStream::connect(("127.0.0.1", isolation)).await.is_ok())
    })
    .await?;
    let baseline = transfer(isolation, tcp, b'E', 262144, 0, 12).await;
    ensure!(baseline["ok"] == true, "隔离对照客户端未启动：{baseline}");
    let mut results = vec![];
    let udp_baseline = if stages.contains("udp-rights") {
        let value = udp_batch(isolation, udp, &records, token(&records), 16, None, 4).await;
        ensure!(
            value["failure"] == 0 && value["confirmed_packets"] == 256,
            "UDP 对照失败：{value}"
        );
        Some(value)
    } else {
        None
    };
    let checks = async {
        for (name, line) in &lines {
            select(name).await?;
            let protocol = line["access"]["protocol"].as_str().unwrap();
            let mut healthy = true;
            if stages.contains("basic") {
                let before = h.current(line).await?["traffic"].clone();
                let mut value = parallel_tcp(port, tcp, 1, b'E', 16384, 0, 12).await;
                accounted(&h, line, &before, &mut value).await?;
                value["passed"] = json!(
                    value["failure"] == 0
                        && number(&value, "meter_upload_bytes") >= 16389
                        && number(&value, "meter_download_bytes") >= 16384
                );
                healthy = value["passed"] == true;
                row(&h, &mut results, name, "basic-tcp", value)?;
                if protocol == "http" {
                    row(
                        &h,
                        &mut results,
                        name,
                        "basic-udp",
                        json!({"passed":true,"capability":"unsupported"}),
                    )?;
                } else {
                    let before = h.current(line).await?["traffic"].clone();
                    let start = Instant::now();
                    let mut value = aggregate(
                        vec![udp_request(port, udp, b"protocol-udp", 4).await],
                        start,
                    );
                    accounted(&h, line, &before, &mut value).await?;
                    value["passed"] = json!(
                        value["failure"] == 0
                            && number(&value, "meter_upload_bytes") >= 12
                            && number(&value, "meter_download_bytes") >= 12
                    );
                    healthy &= value["passed"] == true;
                    row(&h, &mut results, name, "basic-udp", value)?;
                }
            }
            if stages.contains("auth") {
                for wrong in &invalid[name] {
                    select(wrong).await?;
                    for transport in if protocol == "http" {
                        vec!["tcp"]
                    } else {
                        vec!["tcp", "udp"]
                    } {
                        let before = h.current(line).await?["traffic"].clone();
                        let start = Instant::now();
                        let detail = if transport == "tcp" {
                            transfer(port, tcp, b'E', 64, 0, 5).await
                        } else {
                            udp_request(port, udp, b"invalid-auth", 2).await
                        };
                        let mut value = aggregate(vec![detail], start);
                        accounted(&h, line, &before, &mut value).await?;
                        value["passed"] = json!(
                            value["success"] == 0
                                && value["meter_upload_bytes"] == 0
                                && value["meter_download_bytes"] == 0
                        );
                        value["expected_rejections"] =
                            value.as_object_mut().unwrap().remove("failure").unwrap();
                        row(
                            &h,
                            &mut results,
                            name,
                            &format!("auth-{}-{transport}", wrong.rsplit('-').next().unwrap()),
                            value,
                        )?;
                    }
                }
                select(name).await?;
            }
            if !healthy {
                row(
                    &h,
                    &mut results,
                    name,
                    "dependent-load-and-rate",
                    json!({"passed":false,"skipped":"基础流量或计量失败，不执行负载重试"}),
                )?;
                continue;
            }
            if stages.contains("load") {
                for count in [1, 16, 64] {
                    let before = h.current(line).await?["traffic"].clone();
                    let mut value = parallel_tcp(
                        port,
                        tcp,
                        count,
                        b'E',
                        if count == 64 { 4 * 1024 * 1024 } else { 65536 },
                        if count == 64 { 6 } else { 0 },
                        12,
                    )
                    .await;
                    accounted(&h, line, &before, &mut value).await?;
                    value["passed"] = json!(
                        value["failure"] == 0
                            && number(&value, "meter_upload_bytes") >= number(&value, "sent_bytes")
                            && number(&value, "meter_download_bytes")
                                >= number(&value, "received_bytes")
                    );
                    row(&h, &mut results, name, "load-tcp", value)?;
                }
                if protocol != "http" {
                    for count in [1, 16] {
                        let before = h.current(line).await?["traffic"].clone();
                        let start = Instant::now();
                        let details = join_all((0..count).map(|index| async move {
                            let mut marker = (index as u16).to_be_bytes().to_vec();
                            marker.extend(vec![b'x'; 1022]);
                            udp_request(port, udp, &marker, 4).await
                        }))
                        .await;
                        let mut value = aggregate(details, start);
                        accounted(&h, line, &before, &mut value).await?;
                        value["passed"] = json!(
                            value["failure"] == 0
                                && number(&value, "meter_upload_bytes")
                                    >= number(&value, "sent_bytes")
                                && number(&value, "meter_download_bytes")
                                    >= number(&value, "received_bytes")
                        );
                        row(&h, &mut results, name, "load-udp", value)?;
                    }
                }
            }
            if stages.contains("rate") {
                for (mode, direction) in [(b'U', "upload"), (b'D', "download")] {
                    limits(&h, line, json!({format!("{direction}_mbps"):2})).await?;
                    let before = h.current(line).await?["traffic"].clone();
                    let other = h.current(&isolated).await?["traffic"].clone();
                    let (mut value, control) =
                        tokio::join!(parallel_tcp(port, tcp, 16, mode, 65536, 0, 20), async {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            transfer(isolation, tcp, b'E', 262144, 0, 12).await
                        });
                    accounted(&h, line, &before, &mut value).await?;
                    let actual = if mode == b'U' {
                        number(&value, "sent_bytes").saturating_sub(80)
                    } else {
                        number(&value, "received_bytes")
                    };
                    let elapsed = value["seconds"].as_f64().unwrap();
                    let mbps = actual as f64 * 8.0 / elapsed / 1e6;
                    let mut isolated_result = control.clone();
                    accounted(&h, &isolated, &other, &mut isolated_result).await?;
                    let unthrottled = control["elapsed"].as_f64().unwrap()
                        < (baseline["elapsed"].as_f64().unwrap() * 8.0).max(0.75);
                    isolated_result["unthrottled"] = json!(unthrottled);
                    value["payload_mbps"] = json!(mbps);
                    value["configured_mbps"] = json!(2);
                    value["isolation"] = isolated_result;
                    value["passed"] = json!(
                        value["failure"] == 0
                            && mbps > 0.0
                            && mbps <= 2.4
                            && elapsed >= 1048576.0 * 8.0 / 2e6 * 0.8
                            && control["ok"] == true
                            && unthrottled
                    );
                    row(
                        &h,
                        &mut results,
                        name,
                        &format!("rate-shared-{direction}"),
                        value,
                    )?;
                }
                limits(&h, line, json!({})).await?;
            }
            if stages.contains("quota") {
                let before = fresh(&h, line, now()).await?;
                let cap = sum(&before) + 262144;
                limits(&h, line, json!({"monthly_gib":cap as f64/1073741824.0})).await?;
                let start = Instant::now();
                let mut value = aggregate(
                    join_all((0..16).map(|i| {
                        transfer(
                            port,
                            tcp,
                            if i % 2 == 0 { b'U' } else { b'D' },
                            131072,
                            0,
                            8,
                        )
                    }))
                    .await,
                    start,
                );
                let after = accounted(&h, line, &before, &mut value).await?;
                let control = transfer(isolation, tcp, b'E', 131072, 0, 12).await;
                value["passed"] = json!(
                    sum(&after) == cap
                        && after["blocked"] == true
                        && number(&value, "success") < 16
                        && control["ok"] == true
                );
                value["limit_bytes"] = json!(cap);
                value["remaining_bytes_before"] = json!(262144);
                value["isolation"] = control;
                value["expected_quota_stops"] =
                    value.as_object_mut().unwrap().remove("failure").unwrap();
                row(
                    &h,
                    &mut results,
                    name,
                    "quota-concurrent-upload-download",
                    value,
                )?;
            }
            if stages.contains("udp-rights") {
                if protocol == "http" {
                    row(
                        &h,
                        &mut results,
                        name,
                        "udp-rights",
                        json!({"passed":true,"capability":"unsupported"}),
                    )?;
                } else {
                    udp_rights(
                        &h,
                        line,
                        &isolated,
                        port,
                        isolation,
                        tcp,
                        udp,
                        &records,
                        &accepted,
                        udp_baseline.as_ref().unwrap(),
                        name,
                        &mut results,
                    )
                    .await?;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let cleanup = async {
        operate(&h, &created, "delete", json!({})).await?;
        h.wait_state("负载夹具删除及 Agent 确认", 30, |s| {
            s["lines"] == json!([])
                && s["nodes"][0]["report"]["revision"] == s["nodes"][0]["desired_revision"]
                && s["nodes"][0]["report"]["applied"] == true
        })
        .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    checks?;
    cleanup?;
    let failures = results
        .iter()
        .filter(|v| v["passed"] != true)
        .collect::<Vec<_>>();
    ensure!(
        failures.is_empty(),
        "协议负载验收失败：{}",
        serde_json::to_string(&failures)?
    );
    h.close().await?;
    println!("PASS {} 项有界协议负载及计量验收", results.len());
    Ok(())
}

async fn isolation_result(
    h: &Harness<'_>,
    line: &Value,
    before: &Value,
    mut result: Value,
    baseline: &Value,
) -> Result<Value> {
    accounted(h, line, before, &mut result).await?;
    let unthrottled = result["payload_mbps"].as_f64().unwrap() > 1.2
        && result["seconds"].as_f64().unwrap()
            < (baseline["seconds"].as_f64().unwrap() * 2.0).max(0.75);
    result["unthrottled"] = json!(unthrottled);
    result["baseline_seconds"] = baseline["seconds"].clone();
    result["passed"] = json!(
        result["failure"] == 0
            && result["confirmed_packets"] == 256
            && result["target"]["unique_packets"] == 256
            && result["target"]["duplicates"] == 0
            && result["meter_upload_bytes"] == 262144
            && result["meter_download_bytes"] == 262144
            && unthrottled
    );
    Ok(result)
}
async fn udp_rights(
    h: &Harness<'_>,
    line: &Value,
    isolated: &Value,
    port: u16,
    isolation: u16,
    tcp: u16,
    udp: u16,
    records: &Records,
    accepted: &Arc<AtomicU64>,
    baseline: &Value,
    name: &str,
    results: &mut Vec<Value>,
) -> Result<()> {
    limits(h, line, json!({})).await?;
    for direction in ["upload", "download"] {
        limits(h, line, json!({format!("{direction}_mbps"):1})).await?;
        let before = h.current(line).await?["traffic"].clone();
        let other = h.current(isolated).await?["traffic"].clone();
        let (mut result, control) = tokio::join!(
            udp_batch(port, udp, records, token(records), 64, None, 4),
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                udp_batch(isolation, udp, records, token(records), 16, None, 4).await
            }
        );
        accounted(h, line, &before, &mut result).await?;
        let control = isolation_result(h, isolated, &other, control, baseline).await?;
        result["passed"] = json!(
            result["failure"] == 0
                && result["confirmed_packets"] == 1024
                && result["unconfirmed_packets"] == 0
                && result["payload_mbps"].as_f64().unwrap() > 0.0
                && result["payload_mbps"].as_f64().unwrap() <= 1.2
                && result["seconds"].as_f64().unwrap() >= 1048576.0 * 8.0 / 1e6 * 0.8
                && result["target"]["unique_packets"] == 1024
                && result["target"]["duplicates"] == 0
                && result["target"]["received_bytes"] == 1048576
                && result["meter_upload_bytes"] == 1048576
                && result["meter_download_bytes"] == 1048576
                && control["passed"] == true
        );
        result["configured_mbps"] = json!(1);
        result["isolation"] = control;
        row(
            h,
            results,
            name,
            &format!("udp-rate-shared-{direction}"),
            result,
        )?;
    }
    limits(h, line, json!({})).await?;
    let mut associations = vec![];
    let warmup = token(records);
    for index in 0..16 {
        let a = Association::new(port).await?;
        let value = udp_stream(&a, udp, warmup, index, 1, 4).await;
        ensure!(value["ok"] == true, "UDP 预热失败：{value}");
        associations.push(a);
    }
    let before = fresh(h, line, now()).await?;
    let cap = sum(&before) + 262144;
    limits(h, line, json!({"monthly_gib":cap as f64/1073741824.0})).await?;
    let quota_token = token(records);
    let mut result = udp_batch(port, udp, records, quota_token, 64, Some(&associations), 2).await;
    let exhausted = accounted(h, line, &before, &mut result).await?;
    let probe = token(records);
    let old = udp_stream(&associations[0], udp, probe, 0, 1, 2).await;
    let mut marker = probe.to_vec();
    marker.extend([0; 8]);
    marker.extend(vec![b'x'; 1000]);
    let new = udp_request(port, udp, &marker, 2).await;
    let before_connections = accepted.load(Ordering::SeqCst);
    let stream = transfer(port, tcp, b'E', 1024, 0, 3).await;
    let mut post = json!({"existing_udp":old,"new_udp":new,"tcp":stream,"target_tcp_connections":accepted.load(Ordering::SeqCst)-before_connections});
    let final_sample = accounted(h, line, &exhausted, &mut post).await?;
    post["target_udp"] = observed(records, probe);
    let other = h.current(isolated).await?["traffic"].clone();
    let control = udp_batch(isolation, udp, records, token(records), 16, None, 4).await;
    let control = isolation_result(h, isolated, &other, control, baseline).await?;
    result["target"] = observed(records, quota_token);
    result["passed"] = json!(
        number(&result, "confirmed_packets") > 0
            && number(&result, "meter_upload_bytes") + number(&result, "meter_download_bytes")
                == 262144
            && sum(&exhausted) == cap
            && exhausted["blocked"] == true
            && final_sample["blocked"] == true
            && number(&result["target"], "received_bytes") > 0
            && number(&result["target"], "received_bytes") <= number(&result, "meter_upload_bytes")
            && result["target"]["duplicates"] == 0
            && post["existing_udp"]["ok"] == false
            && post["new_udp"]["ok"] == false
            && post["tcp"]["ok"] == false
            && post["target_udp"]["received_packets"] == 0
            && post["target_tcp_connections"] == 0
            && post["meter_upload_bytes"] == 0
            && post["meter_download_bytes"] == 0
            && control["passed"] == true
    );
    result["after_exhaustion"] = post;
    result["isolation"] = control;
    result["limit_bytes"] = json!(cap);
    result["remaining_bytes_before"] = json!(262144);
    result["expected_quota_stops"] = result.as_object_mut().unwrap().remove("failure").unwrap();
    row(h, results, name, "udp-quota-hard-cap", result)
}
