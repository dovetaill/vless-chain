use super::{
    business::{line, node},
    harness::Harness,
    plans::{echo, socks},
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};

fn total(value: &Value) -> u64 {
    value["traffic"]["upload"].as_u64().unwrap_or(0)
        + value["traffic"]["download"].as_u64().unwrap_or(0)
}
fn core_ids(directory: &Path) -> Result<std::collections::BTreeSet<String>> {
    #[cfg(target_os = "linux")]
    {
        let mut ids = std::collections::BTreeSet::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            if let Ok(command) = fs::read(entry.path().join("cmdline")) {
                let text = String::from_utf8_lossy(&command);
                if text.contains(directory.to_str().unwrap()) && text.contains("xray") {
                    ids.insert(entry.file_name().to_string_lossy().into());
                }
            }
        }
        Ok(ids)
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()?;
        Ok(String::from_utf8(output.stdout)?
            .lines()
            .filter(|l| l.contains(directory.to_str().unwrap()) && l.contains("xray"))
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .collect())
    }
}

pub async fn exercise(
    h: &mut Harness<'_>,
    nodes: &[Value],
    echo_port: u16,
    http: u16,
    cert: &Path,
    key: &Path,
    agents: &mut [u32],
    envs: &[BTreeMap<String, String>],
) -> Result<()> {
    let reality = h.state().await?["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|l| {
            l["access"]["reality"]["target"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|_| l["access"]["reality"].clone())
        })
        .context("REALITY 夹具缺失")?;
    let old = node(h, "旧版本能力检查", None).await?;
    h.request_headers(
        &format!("/api/agent/{}/sync", old["id"].as_str().unwrap()),
        "POST",
        Some(&json!({"running":true,"applied":true,"revision":"","error":""})),
        200,
        &[
            ("Cookie", ""),
            (
                "Authorization",
                &format!("Bearer {}", old["token"].as_str().unwrap()),
            ),
        ],
    )
    .await?;
    let legacy = line(
        h,
        &[old.clone()],
        "旧 Agent 保护",
        &[0],
        json!({"access":{"reality":reality}}),
        200,
    )
    .await?;
    ensure!(
        h.request("/api/publish", "POST", Some(&json!({})), 400)
            .await?["error"]
            .as_str()
            .unwrap()
            .contains("Agent"),
        "旧 Agent 未被拒绝"
    );
    h.request(
        &format!("/api/lines/{}", legacy["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    h.request(
        &format!("/api/nodes/{}", old["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    let previous = h.state().await?;
    for l in previous["lines"].as_array().unwrap() {
        h.request(
            &format!("/api/lines/{}", l["id"].as_str().unwrap()),
            "DELETE",
            None,
            200,
        )
        .await?;
    }
    h.publish("applied").await?;
    let mut lines = vec![];
    for (protocol, name) in [
        ("http", "HTTP 代理"),
        ("socks", "SOCKS5 代理"),
        ("shadowsocks", "Shadowsocks 2022"),
    ] {
        lines.push(
            line(
                h,
                nodes,
                name,
                &[0],
                json!({"access":{"protocol":protocol}}),
                200,
            )
            .await?,
        );
    }
    let tls = json!({"mode":"manual","domain":"localhost","cert":fs::read_to_string(cert)?,"key":fs::read_to_string(key)?});
    let trojan = line(
        h,
        nodes,
        "Trojan TLS",
        &[0],
        json!({"access":{"protocol":"trojan","tls":tls}}),
        200,
    )
    .await?;
    let vless = line(
        h,
        nodes,
        "VLESS TLS Vision",
        &[0],
        json!({"access":{"protocol":"vless","tls":tls}}),
        200,
    )
    .await?;
    let backend = h.website().await?;
    h.allow_loopback(&[echo_port, http, backend])?;
    let site=line(h,nodes,"演示网站",&[0],json!({"kind":"website","target":format!("http://127.0.0.1:{backend}/app"),"access":{"tls":{"mode":"off","domain":"app.example.test"}}}),200).await?;
    h.request("/api/lines","POST",Some(&json!({"name":"共享端口网站","kind":"website","hops":site["hops"],"target":format!("http://127.0.0.1:{backend}/other"),"access":{"tls":{"mode":"off","domain":"other.example.test"}}})),200).await?;
    let secure=line(h,nodes,"HTTPS 网站",&[0],json!({"kind":"website","target":format!("http://127.0.0.1:{backend}"),"access":{"tls":tls}}),200).await?;
    let mut limited = line(
        h,
        nodes,
        "共享带宽",
        &[0],
        json!({"access":{"protocol":"socks"},"limits":{"upload_mbps":0.8,"download_mbps":0.8}}),
        200,
    )
    .await?;
    let plain = line(
        h,
        nodes,
        "普通 SOCKS",
        &[0],
        json!({"access":{"protocol":"socks"}}),
        200,
    )
    .await?;
    let quota = line(
        h,
        nodes,
        "月额度",
        &[0],
        json!({"access":{"protocol":"socks"},"limits":{"monthly_gib":8192.0/1073741824.0}}),
        200,
    )
    .await?;
    let occupied = h.state().await?["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == nodes[0]["id"])
        .unwrap()["report"]["system"]["listening_ports"]
        .clone();
    let automatic=h.request("/api/lines","POST",Some(&json!({"name":"自动端口","kind":"vless","access":{"protocol":"socks"},"target":format!("127.0.0.1:{echo_port}"),"hops":[{"node_id":nodes[0]["id"]}]})),200).await?;
    let port = automatic["hops"][0]["port"].as_u64().unwrap() as u16;
    ensure!(
        !occupied.as_array().unwrap().contains(&json!(port)),
        "自动端口与监听冲突"
    );
    h.remember(port)?;
    h.publish("applied").await?;
    let url = format!("http://127.0.0.1:{http}/");
    for l in lines.iter().chain([&automatic, &vless]) {
        h.probe(l, &url).await?;
    }
    let certificate = rustls_pemfile::certs(&mut std::io::BufReader::new(fs::File::open(cert)?))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut roots = rustls::RootCertStore::empty();
    for c in certificate {
        roots.add(c)?;
    }
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let stream = TcpStream::connect((
        "127.0.0.1",
        trojan["hops"][0]["port"].as_u64().unwrap() as u16,
    ))
    .await?;
    let mut stream = connector.connect("localhost".try_into()?, stream).await?;
    let hash = openssl::hash::hash(
        openssl::hash::MessageDigest::sha224(),
        trojan["access"]["password"].as_str().unwrap().as_bytes(),
    )?;
    let mut packet = hex::encode(hash).into_bytes();
    packet.extend(b"\r\n\x01\x01\x7f\x00\x00\x01");
    packet.extend(http.to_be_bytes());
    packet.extend(b"\r\nGET / HTTP/1.0\r\nHost: localhost\r\n\r\n");
    stream.write_all(&packet).await?;
    let mut body = vec![];
    let _ = stream.read_to_end(&mut body).await;
    ensure!(
        String::from_utf8_lossy(&body).contains("chain-integration-ok"),
        "Trojan TLS 流量错误"
    );
    drop(stream);
    let website = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&fs::read(cert)?)?)
        .build()?;
    for (host, expected, status) in [
        ("app.example.test", "/app/hello?q=1", 200),
        ("other.example.test", "/other/hello?q=1", 200),
        ("missing.example.test", "未配置", 404),
    ] {
        let response = website
            .get(format!(
                "http://127.0.0.1:{}/hello?q=1",
                site["hops"][0]["port"].as_u64().unwrap()
            ))
            .header("Host", host)
            .send()
            .await?;
        ensure!(
            response.status().as_u16() == status && response.text().await?.contains(expected),
            "网站虚拟主机转发错误"
        );
    }
    ensure!(
        website
            .get(format!(
                "https://localhost:{}/secure",
                secure["hops"][0]["port"].as_u64().unwrap()
            ))
            .send()
            .await?
            .text()
            .await?
            == "/secure",
        "HTTPS 网站转发错误"
    );
    let mut stream = TcpStream::connect((
        "127.0.0.1",
        site["hops"][0]["port"].as_u64().unwrap() as u16,
    ))
    .await?;
    stream.write_all(b"GET /socket HTTP/1.1\r\nHost: app.example.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").await?;
    let mut header = vec![];
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).await?;
        header.push(byte[0]);
        ensure!(header.len() < 8192, "WebSocket 响应头过大");
    }
    ensure!(
        String::from_utf8_lossy(&header).contains("101"),
        "WebSocket 未升级"
    );
    let mut frame = vec![0x81, 0x84, 1, 2, 3, 4];
    frame.extend(
        b"ping"
            .iter()
            .enumerate()
            .map(|(i, v)| v ^ [1, 2, 3, 4][i % 4]),
    );
    stream.write_all(&frame).await?;
    let mut response = [0; 6];
    stream.read_exact(&mut response).await?;
    ensure!(&response == b"\x81\x04ping", "WebSocket 帧不匹配");
    drop(stream);
    udp_lifetime(h, &lines[1], echo_port).await?;
    let started = Instant::now();
    let results =
        futures_util::future::join_all((0..8).map(|_| echo(&limited, echo_port, &[b'x'; 25000])))
            .await;
    for result in results {
        result?;
    }
    let elapsed = started.elapsed().as_secs_f64();
    ensure!(
        elapsed > 1.9 && elapsed < 8.0,
        "8 连接共享带宽时间错误：{elapsed}"
    );
    let started = Instant::now();
    echo(&plain, echo_port, &[b'x'; 200000]).await?;
    ensure!(
        started.elapsed().as_secs_f64() < 1.5,
        "普通线路受到无关带宽限制"
    );
    h.probe(&limited, &url).await?;
    for _ in 0..6 {
        if echo(&quota, echo_port, &[0; 1024]).await.is_err() {
            break;
        }
    }
    h.wait_state("额度用尽", 30, |s| {
        s["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == quota["id"] && total(l) >= 8192)
    })
    .await?;
    ensure!(
        echo(&quota, echo_port, b"blocked").await.is_err(),
        "额度用尽后仍转发"
    );
    let directory = Path::new(&envs[0]["CHAIN_AGENT_DIR"]);
    let before = core_ids(directory)?;
    limited["limits"] = json!({"upload_mbps":1.6,"download_mbps":1.6});
    h.request("/api/lines", "POST", Some(&limited), 200).await?;
    ensure!(
        h.request("/api/preview", "POST", Some(&json!({})), 200)
            .await?["affected_line_ids"]
            == json!([]),
        "仅限速变更触发监听重建"
    );
    h.publish("applied").await?;
    ensure!(core_ids(directory)? == before, "限速更新重启 Xray");
    h.work.stop(agents[0]).await?;
    let stopped = chrono::Utc::now().timestamp();
    agents[0] = h.launch(
        "agent-counter-restart",
        &h.bin.join("chain-agent"),
        envs[0].clone(),
        &[],
    )?;
    h.wait_state("计数 Agent 重启", 30, |s| {
        s["nodes"].as_array().unwrap().iter().any(|n| {
            n["id"] == nodes[0]["id"]
                && n["seen"].as_i64().is_some_and(|v| v > stopped)
                && n["report"]["running"] == true
        })
    })
    .await?;
    ensure!(total(&h.current(&quota).await?) >= 8192, "重启重置额度");
    h.probe(&plain, &url).await?;
    for l in lines.iter().chain([&trojan, &vless]) {
        let export = h
            .request(
                &format!("/api/lines/{}/export", l["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        ensure!(
            export["share"].as_str().is_some_and(|v| !v.is_empty())
                && export.get("config").is_none(),
            "导出缺失分享链接或泄漏配置"
        );
        let proxy: Value = serde_json::from_str(
            export["node"]
                .as_str()
                .unwrap()
                .strip_prefix("- ")
                .context("不是原始 Mihomo 节点")?,
        )?;
        ensure!(
            proxy["name"] == l["name"] && proxy["port"] == l["hops"][0]["port"],
            "导出名称或端口错误"
        );
    }
    let sub=h.request("/api/subscriptions","POST",Some(&json!({"name":"测试订阅","line_ids":lines.iter().map(|l|l["id"].clone()).collect::<Vec<_>>()})),200).await?;
    ensure!(
        h.anonymous(sub["path"].as_str().unwrap(), "GET", None, 200)
            .await?
            .as_str()
            .unwrap()
            .contains("proxies:"),
        "订阅未生成代理列表"
    );
    h.request(
        &format!("/api/subscriptions/{}", sub["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    h.anonymous(sub["path"].as_str().unwrap(), "GET", None, 404)
        .await?;
    let export = h
        .request(
            &format!("/api/lines/{}/export", lines[2]["id"].as_str().unwrap()),
            "GET",
            None,
            200,
        )
        .await?;
    let external = h
        .request(
            "/api/externals",
            "POST",
            Some(&json!({"url":export["node"]})),
            200,
        )
        .await?;
    let imported = h
        .request(
            &format!("/api/externals/{}/export", external["id"].as_str().unwrap()),
            "GET",
            None,
            200,
        )
        .await?;
    ensure!(
        serde_json::from_str::<Value>(
            imported["node"]
                .as_str()
                .unwrap()
                .strip_prefix("- ")
                .unwrap()
        )?["type"]
            == "ss",
        "SS 节点往返导入失败"
    );
    if std::env::var_os("CHAIN_TEST_MIHOMO").is_some() {
        let mut group = lines.clone();
        group.extend([trojan, vless]);
        super::protocols::mihomo(h, &group, http, None, cert).await?;
    }
    security(h).await?;
    h.remember_ports()?;
    println!("PASS 协议、虚拟主机/WebSocket、UDP 生命周期、共享限速、额度持久性、导出/订阅及 TOTP");
    Ok(())
}

async fn udp_lifetime(h: &Harness<'_>, line: &Value, target: u16) -> Result<()> {
    let udp = UdpSocket::bind("127.0.0.1:0").await?;
    h.remember(udp.local_addr()?.port())?;
    let mut packet = vec![0, 0, 0, 1, 127, 0, 0, 1];
    packet.extend(target.to_be_bytes());
    packet.extend(b"udp-auth");
    let public = (
        "127.0.0.1",
        line["hops"][0]["port"].as_u64().unwrap() as u16,
    );
    udp.send_to(&packet, public).await?;
    let mut response = [0; 65536];
    ensure!(
        !matches!(
            tokio::time::timeout(Duration::from_millis(400), udp.recv(&mut response)).await,
            Ok(Ok(_))
        ),
        "未认证 UDP 被接受"
    );
    let (control, relay) = socks(line, 0, 3, None).await?;
    ensure!(relay.port() > 0, "UDP relay 端口为空");
    h.remember(relay.port())?;
    udp.send_to(&packet, relay).await?;
    let n = tokio::time::timeout(Duration::from_secs(5), udp.recv(&mut response)).await??;
    ensure!(response[..n].ends_with(b"udp-auth"), "认证 UDP 回显错误");
    drop(control);
    wait("UDP relay 关闭", async || {
        Ok(std::net::UdpSocket::bind(relay).is_ok())
    })
    .await?;
    udp.send_to(&packet, relay).await?;
    ensure!(
        !matches!(
            tokio::time::timeout(Duration::from_millis(400), udp.recv(&mut response)).await,
            Ok(Ok(_))
        ),
        "关联关闭后仍允许 UDP"
    );
    Ok(())
}
async fn security(h: &Harness<'_>) -> Result<()> {
    let token = fs::read_to_string(h.root.join("control/admin-token"))?;
    let setup = h
        .request(
            "/api/security/totp/start",
            "POST",
            Some(&json!({"token":token.trim()})),
            200,
        )
        .await?;
    let otp = crate::security::totp(setup["secret"].as_str().unwrap())?;
    let now = chrono::Utc::now().timestamp() as u64;
    let codes = h
        .request(
            "/api/security/totp/finish",
            "POST",
            Some(&json!({"code":otp.generate(now-30)})),
            200,
        )
        .await?["recovery_codes"]
        .clone();
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":token.trim()})),
        401,
    )
    .await?;
    let login = json!({"token":token.trim(),"code":otp.generate(now)});
    h.anonymous("/api/login", "POST", Some(&login), 200).await?;
    h.anonymous("/api/login", "POST", Some(&login), 401).await?;
    let recovery = json!({"token":token.trim(),"code":codes[0]});
    h.anonymous("/api/login", "POST", Some(&recovery), 200)
        .await?;
    h.anonymous("/api/login", "POST", Some(&recovery), 401)
        .await?;
    h.request(
        "/api/security/totp/disable",
        "POST",
        Some(&json!({"token":token.trim(),"code":codes[1]})),
        200,
    )
    .await?;
    h.login(&h.root.join("control")).await?;
    let new = h
        .request(
            "/api/security/token",
            "POST",
            Some(&json!({"token":token.trim()})),
            200,
        )
        .await?["token"]
        .as_str()
        .unwrap()
        .to_string();
    h.request("/api/state", "GET", None, 401).await?;
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":token.trim()})),
        401,
    )
    .await?;
    ensure!(
        fs::read_to_string(h.root.join("control/admin-token"))? == new,
        "管理员密钥未持久化"
    );
    h.login(&h.root.join("control")).await?;
    ensure!(
        h.state().await?["security"]["totp_enabled"] == false,
        "TOTP 未停用"
    );
    Ok(())
}
