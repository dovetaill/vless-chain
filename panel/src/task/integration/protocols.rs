use super::{
    super::{Options, strings, workspace::Workspace},
    business::{line, node},
    harness::{Harness, map},
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};

pub const PROFILES: &[(&str, &str, bool, &str)] = &[
    ("reality-vision", "vless", false, "tcp"),
    ("tls-vision", "vless", true, "tcp"),
    ("reality-xhttp", "vless", false, "xhttp"),
    ("tls-xhttp", "vless", true, "xhttp"),
    ("ss2022", "shadowsocks", false, "tcp"),
    ("trojan", "trojan", true, "tcp"),
    ("http", "http", false, "tcp"),
    ("https", "http", true, "tcp"),
    ("socks5", "socks", false, "tcp"),
    ("vmess", "vmess", false, "tcp"),
    ("vmess-tls", "vmess", true, "tcp"),
    ("hysteria2", "hysteria2", true, "tcp"),
    ("tuic5", "tuic", true, "tcp"),
];
pub fn binary(h: &Harness<'_>) -> Result<PathBuf> {
    let path = std::env::var_os("CHAIN_TEST_MIHOMO")
        .map(PathBuf::from)
        .unwrap_or_else(|| h.bin.join("mihomo"));
    path.canonicalize().context("协议验收需要已校验的 Mihomo")
}
pub async fn udp_echo(port: u16, destination: u16, marker: &[u8]) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut control = TcpStream::connect(("127.0.0.1", port)).await?;
        let udp = UdpSocket::bind("127.0.0.1:0").await?;
        control.write_all(&[5, 1, 0]).await?;
        let mut auth = [0; 2];
        control.read_exact(&mut auth).await?;
        ensure!(auth == [5, 0], "Mihomo SOCKS 认证响应错误");
        control.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        let mut reply = [0; 10];
        control.read_exact(&mut reply).await?;
        ensure!(reply[..2] == [5, 0] && reply[3] == 1, "Mihomo UDP 关联失败");
        let relay = u16::from_be_bytes([reply[8], reply[9]]);
        let mut packet = vec![0, 0, 0, 1, 127, 0, 0, 1];
        packet.extend(destination.to_be_bytes());
        packet.extend(marker);
        udp.send_to(&packet, ("127.0.0.1", relay)).await?;
        let mut response = vec![0; 65536];
        let n = udp.recv(&mut response).await?;
        ensure!(
            response[..n] == packet,
            "UDP 载荷不匹配：发送 {} 字节，收到 {} 字节",
            marker.len(),
            n.saturating_sub(10)
        );
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
pub async fn mihomo(
    h: &Harness<'_>,
    lines: &[Value],
    http: u16,
    udp: Option<u16>,
    cert: &Path,
) -> Result<()> {
    mihomo_routes(h, lines, http, udp, cert, None).await
}
pub async fn mihomo_routes(
    h: &Harness<'_>,
    lines: &[Value],
    http: u16,
    udp: Option<u16>,
    cert: &Path,
    shared_port: Option<u16>,
) -> Result<()> {
    let vless = h.state().await?["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| {
            l["kind"] == "vless"
                && l["access"]["protocol"] == "vless"
                && l["enabled"] == true
                && l["published"] == true
        })
        .cloned()
        .context("缺少可验证的 VLESS")?;
    let mut group = vec![vless];
    for l in lines {
        if !group.iter().any(|v| v["id"] == l["id"]) {
            group.push(l.clone());
        }
    }
    let binary = binary(h)?;
    for l in group {
        let export = h
            .request(
                &format!("/api/lines/{}/export", l["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        ensure!(export.get("config").is_none(), "导出泄漏配置");
        let node = export["node"].as_str().context("导出节点缺失")?;
        let mut proxy: Value = serde_json::from_str(
            node.strip_prefix("- ")
                .context("导出不是原始 Mihomo 节点")?,
        )?;
        let rewritten = if let Some(port) = shared_port.filter(|_| proxy["port"] == 443) {
            proxy["port"] = json!(port);
            Some(format!("- {}", serde_json::to_string(&proxy)?))
        } else {
            None
        };
        let node = rewritten.as_deref().unwrap_or(node);
        let directory = h.root.join(format!("mihomo-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let port = h.port()?;
        let config = format!(
            "mixed-port: {port}\nmode: rule\nproxies:\n  {}\nrules:\n  - {}\n",
            node.trim(),
            serde_json::to_string(&format!("MATCH,{}", proxy["name"].as_str().unwrap()))?
        );
        let path = directory.join("config.yaml");
        fs::write(&path, config)?;
        let env = map(&[("SSL_CERT_FILE", cert.display().to_string())]);
        let check = h.launch(
            &format!("mihomo-check-{port}"),
            &binary,
            env.clone(),
            &strings(&[
                "-t",
                "-f",
                path.to_str().unwrap(),
                "-d",
                directory.to_str().unwrap(),
            ]),
        )?;
        wait("Mihomo 校验", async || Ok(h.work.poll(check)?.is_some())).await?;
        ensure!(
            h.work.poll(check)?.unwrap().success(),
            "原始导出未通过 Mihomo 校验"
        );
        let pid = h.launch(
            &format!("mihomo-{port}"),
            &binary,
            env,
            &strings(&[
                "-f",
                path.to_str().unwrap(),
                "-d",
                directory.to_str().unwrap(),
            ]),
        )?;
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))?)
            .timeout(Duration::from_secs(10))
            .build()?;
        wait("原始导出真实客户端流量", async || {
            Ok(
                match client.get(format!("http://127.0.0.1:{http}/")).send().await {
                    Ok(r) => r.text().await.is_ok_and(|s| s == "chain-integration-ok"),
                    Err(_) => false,
                },
            )
        })
        .await?;
        if proxy["type"] != "http" {
            if let Some(udp) = udp {
                udp_echo(port, udp, b"chain-protocol-udp-ok").await?;
            }
        }
        h.work.stop(pid).await?;
    }
    Ok(())
}
pub async fn run(work: &Workspace, stage: &Path, args: &Options) -> Result<()> {
    if args
        .values
        .get("--case")
        .is_some_and(|s| s == "chain-matrix")
    {
        return super::matrix::run(work, stage, args).await;
    }
    let mut h = Harness::new(work, stage, "protocols")?;
    let (target, cert, key) = h.tls("localhost").await?;
    let http = h.http(b"chain-integration-ok".to_vec()).await?;
    let udp = h.echo().await?;
    h.allow_loopback(&[http, udp])?;
    h.panel(map(&[
        ("SSL_CERT_FILE", cert.display().to_string()),
        ("CHAIN_XRAY_LOG_LEVEL", "debug".into()),
    ]))
    .await?;
    let n = node(&h, "protocol-test", Some(target)).await?;
    h.preflight(target, true).await?;
    crate::runtime::private_dir(&h.root.join("agent-0"), "chain-agent")?;
    let live = h.root.join("agent-0/acme/config/live/localhost");
    fs::create_dir_all(&live)?;
    fs::copy(&cert, live.join("fullchain.pem"))?;
    fs::copy(&key, live.join("privkey.pem"))?;
    let (agent, env) = h
        .agent(
            0,
            &n,
            map(&[
                ("SSL_CERT_FILE", cert.display().to_string()),
                (
                    "CHAIN_ACME_WEBROOT",
                    h.root.join("acme-web").display().to_string(),
                ),
            ]),
        )
        .await?;
    h.wait_state("协议 Agent", 30, |s| s["nodes"][0]["online"] == true)
        .await?;
    let occupied = h.state().await?["nodes"][0]["report"]["system"]["listening_ports"].clone();
    for port in occupied
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.as_u64().is_some_and(|p| (50000..65535).contains(&p)))
    {
        h.request("/api/lines","POST",Some(&json!({"name":format!("reserved-{port}"),"kind":"website","enabled":false,"target":format!("http://127.0.0.1:{http}/"),"hops":[{"node_id":n["id"],"port":port}],"access":{"tls":{"mode":"off","domain":format!("reserved-{port}.test")}}})),200).await?;
    }
    let tls = json!({"mode":"manual","domain":"localhost","cert":fs::read_to_string(&cert)?,"key":fs::read_to_string(&key)?});
    let mut lines = vec![];
    for (protocol, secure) in [
        ("vless", false),
        ("shadowsocks", false),
        ("trojan", true),
        ("http", false),
        ("socks", false),
        ("vmess", false),
        ("vmess", true),
        ("hysteria2", true),
        ("tuic", true),
    ] {
        let tls = if protocol == "tuic" {
            json!({"mode":"auto","domain":"localhost"})
        } else if secure {
            tls.clone()
        } else {
            json!({"mode":"off"})
        };
        lines.push(
            line(
                &h,
                &[n.clone()],
                &format!("{protocol}{}", if secure { " TLS" } else { "" }),
                &[0],
                json!({"access":{"protocol":protocol,"host":"127.0.0.1","tls":tls}}),
                200,
            )
            .await?,
        );
    }
    for protocol in ["trojan", "hysteria2", "tuic"] {
        line(
            &h,
            &[n.clone()],
            "invalid TLS",
            &[0],
            json!({"access":{"protocol":protocol}}),
            400,
        )
        .await?;
    }
    h.publish("applied").await?;
    let url = format!("http://127.0.0.1:{http}/");
    for l in &lines {
        h.probe(l, &url).await?;
        let export = h
            .request(
                &format!("/api/lines/{}/export", l["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        let proxy: Value =
            serde_json::from_str(export["node"].as_str().unwrap().strip_prefix("- ").unwrap())?;
        ensure!(proxy["port"] == l["hops"][0]["port"], "导出端口改变");
        if proxy["type"] == "vless" {
            ensure!(
                proxy["reality-opts"]["public-key"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    && proxy["reality-opts"]["short-id"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                    && proxy["servername"] == "localhost"
                    && proxy["flow"] == "xtls-rprx-vision",
                "REALITY 导出参数错误"
            );
        }
        for source in [&export["share"], &export["node"]] {
            let imported = h
                .request("/api/externals", "POST", Some(&json!({"url":source})), 200)
                .await?;
            let again = h
                .request(
                    &format!("/api/externals/{}/export", imported["id"].as_str().unwrap()),
                    "GET",
                    None,
                    200,
                )
                .await?;
            ensure!(
                serde_json::from_str::<Value>(
                    again["node"].as_str().unwrap().strip_prefix("- ").unwrap()
                )?["type"]
                    == proxy["type"],
                "节点导入往返丢失类型"
            );
        }
    }
    mihomo(&h, &lines, http, Some(udp), &cert).await?;
    let tuic = lines.last().unwrap();
    let vless = &lines[0];
    let mut chained = vec![];
    for path in [vec![tuic], vec![vless, tuic], vec![tuic, vless]] {
        chained.push(line(&h,&[n.clone()],&format!("chain-{}",uuid::Uuid::new_v4()),&[0],json!({"access":{"protocol":"socks","host":"127.0.0.1"},"chain":path.iter().map(|l|format!("line:{}",l["id"].as_str().unwrap())).collect::<Vec<_>>()}),200).await?);
    }
    h.publish("applied").await?;
    h.probe(vless, &url).await?;
    for l in &chained {
        h.probe(l, &url).await?;
    }
    mihomo(&h, &chained, http, Some(udp), &cert).await?;
    let active: Value = serde_json::from_slice(&fs::read(h.root.join("agent-0/active.json"))?)?;
    let backend = active["config"]["_mihomo"]["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["type"] == "tuic")
        .context("TUIC 后端缺失")?["port"]
        .as_u64()
        .unwrap() as u16;
    work.stop(agent).await?;
    h.remember(backend)?;
    let occupied = UdpSocket::bind(("127.0.0.1", backend)).await?;
    let pid = h.launch("agent-occupied", &h.bin.join("chain-agent"), env, &[])?;
    wait("TUIC 监听失败退出", async || {
        Ok(work.poll(pid)?.is_some())
    })
    .await?;
    ensure!(
        !work.poll(pid)?.unwrap().success()
            && fs::read_to_string(h.root.join("agent-occupied.log"))?
                .contains("Mihomo 监听启动失败"),
        "TUIC 监听冲突被报告为正常"
    );
    drop(occupied);
    h.close().await?;
    println!("PASS 原始协议导出真实 TCP/UDP、往返导入、TUIC 各链位置及后端监听冲突");
    Ok(())
}
