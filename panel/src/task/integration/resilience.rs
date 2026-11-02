use super::{
    super::workspace::Workspace,
    business::{line, node},
    harness::{Harness, map},
    plans::{echo, socks},
    rentals::{blocked, closed, operate},
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, TimeZone};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};

fn now() -> u64 {
    chrono::Utc::now().timestamp() as u64
}
fn total(t: &Value) -> u64 {
    t["total_upload"].as_u64().unwrap_or(0) + t["total_download"].as_u64().unwrap_or(0)
}
fn period(t: &Value) -> u64 {
    t["upload"].as_u64().unwrap_or(0) + t["download"].as_u64().unwrap_or(0)
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn disk(h: &Harness<'_>, l: &Value) -> Result<Value> {
    Ok(read(&h.root.join("agent-0/traffic.json"))?[l["id"].as_str().unwrap()].clone())
}
fn durable(h: &Harness<'_>, l: &Value) -> Result<Value> {
    let db = rusqlite::Connection::open_with_flags(
        h.root.join("agent-0/traffic.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let body: String = db.query_row(
        "SELECT body FROM traffic WHERE line_id=?1",
        [l["id"].as_str().unwrap()],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&body)?)
}
async fn metered(h: &Harness<'_>, l: &Value, expected: u64) -> Result<Value> {
    h.wait_state("确认全部载荷已计量", 30, |s| {
        s["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == l["id"] && total(&v["traffic"]) == expected)
    })
    .await?;
    Ok(h.current(l).await?["traffic"].clone())
}
async fn exchange(s: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(6), async {
        s.write_all(bytes).await?;
        let mut received = vec![0; bytes.len()];
        s.read_exact(&mut received).await?;
        ensure!(received == bytes, "崩溃验收回显不一致");
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
async fn udp(
    h: &Harness<'_>,
    l: &Value,
    target: u16,
) -> Result<(TcpStream, UdpSocket, std::net::SocketAddr, Vec<u8>)> {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    h.remember(socket.local_addr()?.port())?;
    let (control, relay) = socks(l, socket.local_addr()?.port(), 3, None).await?;
    h.remember(relay.port())?;
    let mut packet = vec![0, 0, 0, 1, 127, 0, 0, 1];
    packet.extend(target.to_be_bytes());
    Ok((control, socket, relay, packet))
}
async fn datagram(
    socket: &UdpSocket,
    relay: std::net::SocketAddr,
    header: &[u8],
    payload: &[u8],
) -> Result<()> {
    let mut packet = header.to_vec();
    packet.extend(payload);
    socket.send_to(&packet, relay).await?;
    let mut response = [0; 65536];
    let n = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut response)).await??;
    ensure!(response[..n] == packet, "崩溃验收 UDP 回显不一致");
    Ok(())
}
async fn udp_closed(relay: std::net::SocketAddr) -> Result<()> {
    wait("UDP 关联释放", async || {
        Ok(std::net::UdpSocket::bind(relay).is_ok())
    })
    .await
}

#[cfg(target_os = "linux")]
struct Reaper {
    previous: libc::c_int,
    cores: BTreeMap<i32, String>,
}
#[cfg(target_os = "linux")]
impl Reaper {
    fn new() -> Result<Self> {
        let mut previous = 0;
        ensure!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous, 0, 0, 0) } == 0,
            "读取 subreaper 失败"
        );
        ensure!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == 0,
            "设置 subreaper 失败"
        );
        Ok(Self {
            previous,
            cores: BTreeMap::new(),
        })
    }
    fn identity(pid: i32) -> Option<(i32, i32, String)> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .collect::<Vec<_>>();
        Some((
            fields.get(1)?.parse().ok()?,
            fields.get(2)?.parse().ok()?,
            fields.get(19)?.to_string(),
        ))
    }
    fn register(&mut self, h: &Harness<'_>, agent: u32) -> Result<Vec<i32>> {
        let mut found = vec![];
        for task in fs::read_dir(format!("/proc/{agent}/task"))? {
            let path = task?.path().join("children");
            for pid in fs::read_to_string(path)
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|p| p.parse::<i32>().ok())
            {
                if let Some((parent, group, start)) = Self::identity(pid) {
                    if parent == agent as i32 && group == agent as i32 {
                        let cmd = fs::read(format!("/proc/{pid}/cmdline"))?;
                        ensure!(
                            String::from_utf8_lossy(&cmd).contains(h.root.to_str().unwrap()),
                            "核心不属于任务目录"
                        );
                        self.cores.insert(pid, start);
                        found.push(pid);
                    }
                }
            }
        }
        found.sort();
        found.dedup();
        fs::write(
            h.root.join("crash-children.json"),
            serde_json::to_vec(&self.cores)?,
        )?;
        Ok(found)
    }
    fn reap(&self, pids: &[i32]) -> bool {
        for pid in pids {
            unsafe {
                libc::waitpid(*pid, std::ptr::null_mut(), libc::WNOHANG);
            }
        }
        pids.iter().all(|p| Self::identity(*p).is_none())
    }
    fn kill_core(&self, pid: i32) -> Result<()> {
        let (_, _, start) = Self::identity(pid).context("核心已退出")?;
        ensure!(self.cores.get(&pid) == Some(&start), "核心 PID 已复用");
        ensure!(
            unsafe { libc::kill(pid, libc::SIGKILL) } == 0,
            "核心终止失败"
        );
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Drop for Reaper {
    fn drop(&mut self) {
        for (pid, start) in &self.cores {
            if Self::identity(*pid).is_some_and(|(_, _, s)| s == *start) {
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                    libc::waitpid(*pid, std::ptr::null_mut(), 0);
                }
            }
        }
        unsafe {
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.previous, 0, 0, 0);
        }
    }
}

async fn kill(h: &Harness<'_>, pid: u32) -> Result<()> {
    ensure!(h.work.poll(pid)?.is_none(), "拒绝终止已退出进程");
    ensure!(
        unsafe { libc::kill(pid as i32, libc::SIGKILL) } == 0,
        "终止任务进程失败"
    );
    wait("SIGKILL 进程退出", async || {
        Ok(h.work.poll(pid)?.is_some())
    })
    .await
}
async fn restart_agent(
    h: &Harness<'_>,
    env: &BTreeMap<String, String>,
    line: &Value,
    index: usize,
) -> Result<u32> {
    let pid = h.launch(
        &format!("agent-restart-{index}"),
        &h.bin.join("chain-agent"),
        env.clone(),
        &[],
    )?;
    wait("恢复入口监听", async || {
        Ok(TcpStream::connect((
            "127.0.0.1",
            line["hops"][0]["port"].as_u64().unwrap() as u16,
        ))
        .await
        .is_ok())
    })
    .await?;
    h.synced(&[line.clone()]).await?;
    Ok(pid)
}
async fn restart_panel(
    h: &Harness<'_>,
    env: &BTreeMap<String, String>,
    index: usize,
) -> Result<u32> {
    let pid = h.launch(
        &format!("panel-restart-{index}"),
        &h.bin.join("chain-panel"),
        env.clone(),
        &[],
    )?;
    h.ready().await?;
    h.login(&h.root.join("control")).await?;
    Ok(pid)
}

#[cfg(not(target_os = "linux"))]
pub async fn run(_: &Workspace, _: &Path) -> Result<()> {
    anyhow::bail!("进程崩溃与 subreaper 验收需要 Linux")
}
#[cfg(target_os = "linux")]
pub async fn run(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "resilience")?;
    let mut reaper = Reaper::new()?;
    let target = h.echo().await?;
    h.allow_loopback(&[target])?;
    let mut panel_env = map(&[("CHAIN_APPLY_TIMEOUT", "12".into())]);
    let mut control = h.panel(panel_env.clone()).await?;
    panel_env.extend(map(&[
        ("CHAIN_LISTEN", h.base.trim_start_matches("http://").into()),
        (
            "CHAIN_DATA_DIR",
            h.root.join("control").display().to_string(),
        ),
        ("CHAIN_XRAY_BIN", h.xray.display().to_string()),
    ]));
    let n = node(&h, "异常恢复验证节点", None).await?;
    let (mut agent, agent_env) = h.agent(0, &n, Default::default()).await?;
    h.wait_state("崩溃验收 Agent", 30, |s| {
        s["nodes"][0]["online"] == true
    })
    .await?;
    let mut lines = vec![];
    for name in ["崩溃计量", "过期断开", "跨周期保活", "周期恢复额度"] {
        lines.push(
            line(
                &h,
                &[n.clone()],
                name,
                &[0],
                json!({"access":{"protocol":"socks"}}),
                200,
            )
            .await?,
        );
    }
    h.publish("applied").await?;
    let (main, expiring, cycling, exhausted) = (&lines[0], &lines[1], &lines[2], &lines[3]);
    let customer = h
        .request(
            "/api/customers",
            "POST",
            Some(&json!({"name":"恢复验证客户","contact":"test-contact"})),
            200,
        )
        .await?;
    for l in &lines {
        h.request(&format!("/api/rentals/{}",l["id"].as_str().unwrap()),"POST",Some(&json!({"customer_id":customer["id"],"prices":{"monthly":1200,"quarterly":3300,"yearly":12000}})),200).await?;
    }
    h.request("/api/rentals/renew","POST",Some(&json!({"line_ids":lines.iter().map(|l|l["id"].clone()).collect::<Vec<_>>(),"term":"monthly","request_id":"resilience-opening"})),200).await?;
    h.synced(&lines).await?;
    echo(main, target, b"durable-baseline").await?;
    wait("基线持久化", async || {
        Ok(total(&disk(&h, main)?) >= 2 * b"durable-baseline".len() as u64)
    })
    .await?;
    let before = durable(&h, main)?;
    let payload = b"crash-accounting-".repeat(16384);
    let cores = reaper.register(&h, agent)?;
    ensure!(!cores.is_empty(), "未登记核心");
    let (mut stream, _) = socks(main, target, 1, None).await?;
    for _ in 0..8 {
        exchange(&mut stream, &payload).await?;
    }
    let delivered = 16 * payload.len() as u64;
    kill(&h, agent).await?;
    closed(&mut stream).await?;
    wait("Agent 死亡回收全部核心", async || {
        Ok(reaper.reap(&cores))
    })
    .await?;
    let saved = durable(&h, main)?;
    ensure!(
        total(&saved) == total(&before) + delivered
            && saved["total_upload"].as_u64().unwrap()
                == before["total_upload"].as_u64().unwrap() + delivered / 2
            && saved["total_download"].as_u64().unwrap()
                == before["total_download"].as_u64().unwrap() + delivered / 2,
        "SIGKILL 丢失已确认载荷计量：before={before} durable={saved} confirmed={delivered}"
    );
    let mut results = json!({"agent_sigkill_accounting":{"client_confirmed_payload_bytes":delivered,"expected_lifetime_bytes":total(&before)+delivered,"last_durable_lifetime_bytes":total(&saved),"lost_bytes":0,"durable_source":"traffic.db WAL synchronous=FULL","scope":"SIGKILL process crash; does not emulate hardware power loss"}});
    agent = restart_agent(&h, &agent_env, main, 1).await?;
    let restored = metered(&h, main, total(&saved)).await?;
    ensure!(restored["epoch"] == saved["epoch"], "恢复改变计量 epoch");
    echo(main, target, b"after-agent-restart").await?;
    metered(
        &h,
        main,
        total(&saved) + 2 * b"after-agent-restart".len() as u64,
    )
    .await?;
    operate(&h, &[cycling.clone()], "disable", json!({})).await?;
    h.synced(&[cycling.clone()]).await?;
    ensure!(period(&durable(&h, exhausted)?) == 0, "额度夹具已有用量");
    operate(
        &h,
        &[exhausted.clone()],
        "limits",
        json!({"limits":{"monthly_gib":1.0/1073741824.0}}),
    )
    .await?;
    h.synced(&[exhausted.clone()]).await?;
    ensure!(
        (h.current(exhausted).await?["limits"]["monthly_gib"]
            .as_f64()
            .unwrap()
            * 1073741824.0) as u64
            == 1,
        "面板一字节额度改变"
    );
    let (mut quota, _) = socks(exhausted, target, 1, None).await?;
    quota.write_all(b"quota-before-core-kill").await?;
    closed(&mut quota).await?;
    wait("用尽额度落盘", async || {
        Ok(period(&durable(&h, exhausted)?) == 1)
    })
    .await?;
    let active = read(&h.root.join("agent-0/active.json"))?;
    let rights = h.state().await?["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["id"].as_str().unwrap().to_string(),
                json!({"enabled":l["enabled"],"limits":l["limits"],"rental":l["rental"]}),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let cores = reaper.register(&h, agent)?;
    ensure!(cores.len() == 1, "SOCKS 夹具应只有一个核心：{cores:?}");
    let (mut stream, _) = socks(main, target, 1, None).await?;
    let (udp_control, socket, relay, header) = udp(&h, main, target).await?;
    exchange(&mut stream, b"before-core-kill").await?;
    datagram(&socket, relay, &header, b"before-core-kill").await?;
    reaper.kill_core(cores[0])?;
    closed(&mut stream).await?;
    udp_closed(relay).await?;
    drop(udp_control);
    drop(socket);
    wait("核心失败令 Agent 非零退出", async || {
        Ok(work.poll(agent)?.is_some())
    })
    .await?;
    ensure!(
        !work.poll(agent)?.unwrap().success(),
        "核心失败 Agent 未失败"
    );
    wait("回收故障核心", async || Ok(reaper.reap(&cores))).await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    agent = restart_agent(&h, &agent_env, main, 2).await?;
    ensure!(
        read(&h.root.join("agent-0/active.json"))? == active,
        "监督重启改变配置"
    );
    for l in h.state().await?["lines"].as_array().unwrap() {
        ensure!(
            rights[l["id"].as_str().unwrap()]
                == json!({"enabled":l["enabled"],"limits":l["limits"],"rental":l["rental"]}),
            "重启改变权益"
        );
    }
    blocked(cycling).await?;
    blocked(exhausted).await?;
    echo(main, target, b"after-supervisor-restart").await?;
    let (c, s, r, p) = udp(&h, main, target).await?;
    datagram(&s, r, &p, b"after-supervisor-restart").await?;
    drop((c, s));
    operate(&h, &[cycling.clone()], "enable", json!({})).await?;
    h.synced(&[cycling.clone()]).await?;
    results["core_sigkill"] = json!({"agent_failed_for_supervisor":true,"supervisor_restart_delay_seconds":5,"active_revision_preserved":true,"rights_preserved":true});
    let request = json!({"line_ids":[main["id"]],"term":"quarterly","amounts":{main["id"].as_str().unwrap():3000},"request_id":"persisted-renewal","note":"SIGKILL后重试同一收款"});
    let payment = h
        .request("/api/rentals/renew", "POST", Some(&request), 200)
        .await?["payments"]
        .clone();
    h.synced(&[main.clone()]).await?;
    let before = h.state().await?;
    let lease = h.current(main).await?["rental"].clone();
    let (mut stream, _) = socks(main, target, 1, None).await?;
    let (c, s, r, p) = udp(&h, main, target).await?;
    kill(&h, control).await?;
    exchange(&mut stream, b"panel-is-offline").await?;
    datagram(&s, r, &p, b"panel-is-offline").await?;
    {
        let db = rusqlite::Connection::open_with_flags(
            h.root.join("control/panel.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        ensure!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? == "ok",
            "面板 SQLite 损坏"
        );
        let doc: Value =
            serde_json::from_str(&db.query_row("SELECT body FROM state WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })?)?;
        ensure!(
            doc["payments"] == before["payments"] && doc["customers"] == before["customers"],
            "面板退出丢失业务数据"
        );
    }
    control = restart_panel(&h, &panel_env, 1).await?;
    ensure!(
        h.current(main).await?["rental"] == lease
            && h.state().await?["payments"] == before["payments"]
            && h.request("/api/rentals/renew", "POST", Some(&request), 200)
                .await?["payments"]
                == payment
            && h.current(main).await?["rental"] == lease,
        "重启后的重试改变权益或订单"
    );
    let mut changed = request.clone();
    changed["note"] = json!("changed");
    h.request("/api/rentals/renew", "POST", Some(&changed), 400)
        .await?;
    drop((stream, c, s));
    let mut concurrent = request;
    concurrent["request_id"] = json!("concurrent-renewal");
    let responses = futures_util::future::try_join_all(
        (0..6).map(|_| h.request("/api/rentals/renew", "POST", Some(&concurrent), 200)),
    )
    .await?;
    ensure!(
        responses
            .iter()
            .all(|v| v["payments"] == responses[0]["payments"])
            && h.state().await?["payments"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["request_id"] == "concurrent-renewal")
                .count()
                == 1,
        "六个并发重试重复收款"
    );
    results["panel_sigkill"] = json!({"sqlite_integrity":"ok","durable_orders_preserved":true,"concurrent_retries":6,"created_payments":1});
    work.stop(agent).await?;
    work.stop(control).await?;
    let boundary = now() + 20;
    let expiry = boundary + 10;
    install_boundaries(&h, &n, expiring, cycling, exhausted, boundary, expiry)?;
    control = restart_panel(&h, &panel_env, 2).await?;
    agent = restart_agent(&h, &agent_env, main, 3).await?;
    ensure!(now() < boundary - 3, "周期夹具未及时启动");
    let (mut live, _) = socks(cycling, target, 1, None).await?;
    let (mut dying, _) = socks(expiring, target, 1, None).await?;
    let (cc, cs, cr, cp) = udp(&h, cycling, target).await?;
    let (ec, es, er, ep) = udp(&h, expiring, target).await?;
    exchange(&mut live, b"before-reset").await?;
    exchange(&mut dying, b"before-expiry").await?;
    datagram(&cs, cr, &cp, b"before-reset").await?;
    datagram(&es, er, &ep, b"before-expiry").await?;
    blocked(exhausted).await?;
    kill(&h, control).await?;
    wait("面板离线时 Agent 自主跨周期", async || {
        Ok(disk(&h, cycling)?["cycle_start"] == boundary)
    })
    .await?;
    let reset = disk(&h, cycling)?;
    ensure!(
        period(&reset) == 0 && total(&reset) == 64 + 4 * b"before-reset".len() as u64,
        "跨周期计数错误：{reset}"
    );
    exchange(&mut live, b"after-reset").await?;
    datagram(&cs, cr, &cp, b"after-reset").await?;
    ensure!(std::net::UdpSocket::bind(cr).is_err(), "跨周期关闭有效 UDP");
    echo(exhausted, target, b"reset-ok").await?;
    wait("租期到期", async || Ok(now() >= expiry)).await?;
    closed(&mut dying).await?;
    udp_closed(er).await?;
    blocked(expiring).await?;
    exchange(&mut live, b"permanent-alive").await?;
    drop((live, dying, cc, cs, ec, es));
    let recovery = now();
    control = restart_panel(&h, &panel_env, 3).await?;
    h.wait_state("恢复后新计量采样", 30, |s| {
        s["nodes"][0]["report"]["traffic"][exhausted["id"].as_str().unwrap()]["sampled_at"]
            .as_u64()
            .unwrap_or(0)
            >= recovery
            && s["lines"].as_array().unwrap().iter().any(|l| {
                l["id"] == exhausted["id"]
                    && l["traffic"]["sampled_at"].as_u64().unwrap_or(0) >= recovery
            })
    })
    .await?;
    for t in [
        disk(&h, exhausted)?,
        durable(&h, exhausted)?,
        h.current(exhausted).await?["traffic"].clone(),
        h.state().await?["nodes"][0]["report"]["traffic"][exhausted["id"].as_str().unwrap()]
            .clone(),
    ] {
        ensure!(
            t["cycle_start"] == boundary && period(&t) == 16 && total(&t) == 80,
            "恢复后的周期、用量或累计不一致：{t}"
        );
    }
    ensure!(
        h.current(expiring).await?["rental_status"] == "expired",
        "到期状态未恢复"
    );
    let expiry_total = total(&h.current(expiring).await?["traffic"]);
    h.request(
        "/api/rentals/renew",
        "POST",
        Some(
            &json!({"line_ids":[expiring["id"]],"term":"monthly","request_id":"expired-reopening"}),
        ),
        200,
    )
    .await?;
    h.synced(&[expiring.clone()]).await?;
    h.wait_state("过期重开周期", 30, |s| {
        s["lines"].as_array().unwrap().iter().any(|l| {
            l["id"] == expiring["id"] && l["traffic"]["cycle_start"] == l["rental"]["started_at"]
        })
    })
    .await?;
    let reopened = h.current(expiring).await?;
    ensure!(
        period(&reopened["traffic"]) == 0
            && total(&reopened["traffic"]) == expiry_total
            && reopened["rental"]["started_at"].as_u64().unwrap() >= expiry,
        "重开清除了累计或未重置周期"
    );
    echo(expiring, target, b"reopened").await?;
    results["calendar_boundary"] = json!({"boundary_unix":boundary,"expiry_unix":expiry,"panel_offline":true,"open_tcp_udp_survived_reset":true,"exhausted_quota_restored":true,"expired_tcp_udp_closed":true,"expired_renewal_reset_period_only":true});
    ensure!(
        h.current(cycling).await?["rental"]["permanent"] == true,
        "跨周期永久权益丢失"
    );
    let (mut live, _) = socks(cycling, target, 1, None).await?;
    let (c, s, r, p) = udp(&h, cycling, target).await?;
    exchange(&mut live, b"manual-disable").await?;
    datagram(&s, r, &p, b"manual-disable").await?;
    operate(&h, &[cycling.clone()], "disable", json!({})).await?;
    h.synced(&[cycling.clone()]).await?;
    closed(&mut live).await?;
    udp_closed(r).await?;
    drop((c, s));
    let payments = h.state().await?["payments"].clone();
    operate(&h, &[cycling.clone()], "unlimited", json!({})).await?;
    h.synced(&[cycling.clone()]).await?;
    ensure!(
        h.current(cycling).await?["enabled"] == false
            && h.current(cycling).await?["rental"]["permanent"] == true
            && h.state().await?["payments"] == payments,
        "永久权益绕过禁用或产生收款"
    );
    blocked(cycling).await?;
    operate(&h, &[cycling.clone()], "enable", json!({})).await?;
    h.synced(&[cycling.clone()]).await?;
    let baseline = total(&h.current(cycling).await?["traffic"]);
    echo(cycling, target, b"unlimited-enabled").await?;
    let t = metered(
        &h,
        cycling,
        baseline + 2 * b"unlimited-enabled".len() as u64,
    )
    .await?;
    let used = period(&t);
    ensure!(used > 0, "永久权益未计量");
    operate(
        &h,
        &[cycling.clone()],
        "limits",
        json!({"limits":{"monthly_gib":used as f64/1073741824.0}}),
    )
    .await?;
    h.synced(&[cycling.clone()]).await?;
    blocked(cycling).await?;
    ensure!(
        period(&h.current(cycling).await?["traffic"]) == used,
        "降低额度清除了已用量"
    );
    operate(
        &h,
        &[cycling.clone()],
        "limits",
        json!({"limits":{"monthly_gib":(used+4096) as f64/1073741824.0}}),
    )
    .await?;
    h.synced(&[cycling.clone()]).await?;
    ensure!(
        period(&h.current(cycling).await?["traffic"]) == used,
        "提高额度清除了已用量"
    );
    echo(cycling, target, b"quota-raised").await?;
    h.wait_state("提高额度后继续累计", 30, |s| {
        s["lines"].as_array().unwrap().iter().any(|l| {
            l["id"] == cycling["id"]
                && period(&l["traffic"]) == used + 2 * b"quota-raised".len() as u64
        })
    })
    .await?;
    results["permanent_policy"] = json!({"manual_disable_preserved":true,"no_payment_created":true,"quota_increase_preserved_used_bytes":used});
    let _ = (control, agent);
    h.close().await?;
    println!("RESULT {}", serde_json::to_string(&results)?);
    println!(
        "PASS Agent/核心/面板 SIGKILL、零已确认载荷计量丢失、并发收款幂等、离线跨周期与租期断开"
    );
    Ok(())
}

fn install_boundaries(
    h: &Harness<'_>,
    node: &Value,
    expiring: &Value,
    cycling: &Value,
    exhausted: &Value,
    boundary: u64,
    expiry: u64,
) -> Result<()> {
    let zone = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let date = zone
        .timestamp_opt(boundary as i64, 0)
        .single()
        .context("周期日期无效")?;
    let mut year = date.year() - 1;
    let anchor = loop {
        if let Some(d) = date.with_year(year) {
            break d.timestamp() as u64;
        }
        year -= 1;
    };
    let (year, month) = if date.month() == 1 {
        (date.year() - 1, 12)
    } else {
        (date.year(), date.month() - 1)
    };
    let mut day = date.day();
    let previous = loop {
        if let Some(d) = date
            .with_day(1)
            .and_then(|d| d.with_year(year))
            .and_then(|d| d.with_month(month))
            .and_then(|d| d.with_day(day))
        {
            break d;
        }
        day -= 1;
    };
    let db = rusqlite::Connection::open(h.root.join("control/panel.db"))?;
    let mut doc: Value =
        serde_json::from_str(&db.query_row("SELECT body FROM state WHERE id=1", [], |r| {
            r.get::<_, String>(0)
        })?)?;
    let desired = doc["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|n| n["id"] == node["id"])
        .context("周期节点缺失")?
        .get_mut("desired")
        .context("周期配置缺失")?;
    desired["revision"] = json!("resilience-calendar-fixture");
    for (l, start, end) in [
        (expiring, boundary - 60, expiry),
        (cycling, anchor, 0),
        (exhausted, anchor, 0),
    ] {
        let spec = desired["config"]["_chain"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["id"] == l["id"])
            .context("周期线路缺失")?;
        spec["lease"] = json!({"cycle_anchor":start,"expires_at":end});
        if l["id"] == exhausted["id"] {
            spec["limits"] =
                json!({"monthly_gib":64.0/1073741824.0,"upload_mbps":null,"download_mbps":null});
        }
    }
    let desired = desired.clone();
    for (l, start, end) in [
        (expiring, boundary - 60, expiry),
        (cycling, anchor, 0),
        (exhausted, anchor, 0),
    ] {
        let rental = &mut doc["rentals"][l["id"].as_str().unwrap()];
        rental["started_at"] = json!(start);
        rental["expires_at"] = json!(end);
        rental["paid_months"] = json!(0);
    }
    for collection in ["lines", "applied_lines"] {
        doc[collection]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|l| l["id"] == exhausted["id"])
            .context("周期额度线路缺失")?["limits"] =
            json!({"monthly_gib":64.0/1073741824.0,"upload_mbps":null,"download_mbps":null});
    }
    db.execute(
        "UPDATE state SET body=?1 WHERE id=1",
        [serde_json::to_string(&doc)?],
    )?;
    drop(db);
    fs::write(
        h.root.join("agent-0/active.json"),
        serde_json::to_vec(&desired)?,
    )?;
    let path = h.root.join("agent-0/traffic.json");
    let mut book = read(&path)?;
    let ledger = rusqlite::Connection::open(h.root.join("agent-0/traffic.db"))?;
    for l in [cycling, exhausted] {
        let id = l["id"].as_str().unwrap();
        let value = &mut book[id];
        value.as_object_mut().unwrap().extend(json!({"cycle_start":previous.timestamp(),"cycle_end":boundary,"upload":64,"download":0,"total_upload":64,"total_download":0,"upload_bps":0,"download_bps":0,"connections":0,"days":{previous.format("%Y-%m-%d").to_string():[64,0]}}).as_object().unwrap().clone());
        ledger.execute("INSERT INTO traffic(line_id,body) VALUES(?1,?2) ON CONFLICT(line_id) DO UPDATE SET body=excluded.body",rusqlite::params![id,serde_json::to_string(value)?])?;
    }
    fs::write(path, serde_json::to_vec(&book)?)?;
    Ok(())
}
