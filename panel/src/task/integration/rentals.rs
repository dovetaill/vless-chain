use super::{
    super::workspace::Workspace,
    business::{line, node},
    harness::{Harness, map},
    plans::{echo, socks},
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};

pub fn used(l: &Value) -> u64 {
    l["traffic"]["upload"].as_u64().unwrap_or(0) + l["traffic"]["download"].as_u64().unwrap_or(0)
}
pub async fn operate(
    h: &Harness<'_>,
    lines: &[Value],
    action: &str,
    extra: Value,
) -> Result<Value> {
    let mut body = json!({"line_ids":lines.iter().map(|l|l["id"].clone()).collect::<Vec<_>>(),"action":action});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    h.request("/api/lines/operate", "POST", Some(&body), 200)
        .await
}
pub async fn blocked(line: &Value) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut stream = TcpStream::connect((
            "127.0.0.1",
            line["hops"][0]["port"].as_u64().unwrap() as u16,
        ))
        .await?;
        if stream.write_all(&[5, 1, 2]).await.is_ok() {
            let mut bytes = [0; 2];
            let read = stream.read(&mut bytes).await;
            ensure!(read.is_err() || read? == 0, "被阻止的线路接受 SOCKS 握手");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("被阻止的线路没有及时关闭握手")??;
    Ok(())
}
pub async fn closed(stream: &mut TcpStream) -> Result<()> {
    let mut bytes = [0];
    let result = tokio::time::timeout(Duration::from_secs(4), stream.read(&mut bytes)).await?;
    ensure!(result.is_err() || result? == 0, "原有连接未关闭");
    Ok(())
}
async fn public(h: &Harness<'_>, body: Value, status: u16) -> Result<Value> {
    h.anonymous("/api/public/lookup", "POST", Some(&body), status)
        .await
}
async fn rejected(udp: &UdpSocket, packet: &[u8], destination: std::net::SocketAddr) -> Result<()> {
    udp.send_to(packet, destination).await?;
    let mut bytes = [0; 1024];
    ensure!(
        !matches!(
            tokio::time::timeout(Duration::from_millis(400), udp.recv(&mut bytes)).await,
            Ok(Ok(_))
        ),
        "未认证 UDP 被接受"
    );
    Ok(())
}
async fn fresh(h: &Harness<'_>, line: &Value, sampled: &Value) -> Result<()> {
    h.wait_state("新的计量采样", 30, |s| {
        s["lines"].as_array().unwrap().iter().any(|l| {
            l["id"] == line["id"] && l["traffic"]["sampled_at"].as_u64() > sampled.as_u64()
        })
    })
    .await?;
    Ok(())
}
pub async fn run(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "rentals")?;
    let echo_port = h.echo().await?;
    h.allow_loopback(&[echo_port])?;
    let (target, cert, key) = h.tls("localhost").await?;
    h.panel(map(&[
        ("CHAIN_APPLY_TIMEOUT", "9".into()),
        ("SSL_CERT_FILE", cert.display().to_string()),
    ]))
    .await?;
    h.anonymous("/api/state", "GET", None, 401).await?;
    let n = node(&h, "租赁验证节点", Some(target)).await?;
    h.preflight(target, true).await?;
    let (agent, agent_env) = h
        .agent(0, &n, map(&[("SSL_CERT_FILE", cert.display().to_string())]))
        .await?;
    h.wait_state("租赁 Agent", 30, |s| s["nodes"][0]["online"] == true)
        .await?;
    let mut lines = vec![];
    for (i, name) in ["主租赁", "同客户另一代理", "硬额度租赁"]
        .iter()
        .enumerate()
    {
        lines.push(line(&h,&[n.clone()],name,&[0],json!({"access":{"protocol":"socks"},"limits":if i==2{json!({"monthly_gib":8192.0/1073741824.0})}else{json!({})}}),200).await?);
    }
    let main = &lines[0];
    let companion = &lines[1];
    let quota = &lines[2];
    h.publish("applied").await?;
    h.synced(&lines).await?;
    protocol_metering(&h, &n, echo_port, &cert, &key).await?;
    println!("PASS VLESS/Hysteria 2/TUIC 认证及真实双向计量");
    let mut exports = vec![];
    let mut proxies = vec![];
    for l in &lines {
        let export = h
            .request(
                &format!("/api/lines/{}/export", l["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        proxies.push(serde_json::from_str::<Value>(
            export["node"].as_str().unwrap().strip_prefix("- ").unwrap(),
        )?);
        exports.push(export);
    }
    public(&h, json!({"config":exports[0]["share"]}), 404).await?;
    let temporary = h
        .request(
            "/api/customers",
            "POST",
            Some(&json!({"name":"临时客户"})),
            200,
        )
        .await?;
    h.request(
        "/api/customers",
        "POST",
        Some(&json!({"id":temporary["id"],"name":"改名客户","contact":"private-temporary"})),
        200,
    )
    .await?;
    ensure!(
        h.state().await?["customers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == temporary["id"] && c["name"] == "改名客户"),
        "客户编辑失败"
    );
    h.request(
        &format!("/api/customers/{}", temporary["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    ensure!(
        h.state().await?["customers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"] != temporary["id"]),
        "客户删除失败"
    );
    let customer=h.request("/api/customers","POST",Some(&json!({"name":"private-customer-name","contact":"private-contact","notes":"private-notes"})),200).await?;
    let prices = json!({"monthly":1200,"quarterly":3200,"yearly":10000});
    h.request(
        "/api/rentals/contact",
        "POST",
        Some(&json!({"renewal_contact":"运营续费入口"})),
        200,
    )
    .await?;
    operate(&h, &[quota.clone()], "unlimited", json!({})).await?;
    h.synced(&[quota.clone()]).await?;
    let personal = h.current(quota).await?;
    ensure!(
        personal["rental"]["customer_id"] == ""
            && personal["rental"]["started_at"].as_u64().unwrap() > 0
            && personal["rental"]["expires_at"] == 0
            && personal["enabled"] == true
            && personal["limits"] == quota["limits"]
            && h.state().await?["payments"] == json!([]),
        "自用无限期改变额度、身份或产生支付"
    );
    ensure!(
        public(&h, json!({"config":exports[2]["share"]}), 200).await?["lines"][0]["rental"]["permanent"]
            == true,
        "公开查询未显示无限期"
    );
    for l in [main, companion] {
        h.request(
            &format!("/api/rentals/{}", l["id"].as_str().unwrap()),
            "POST",
            Some(&json!({"customer_id":customer["id"],"prices":prices})),
            200,
        )
        .await?;
        h.synced(&[l.clone()]).await?;
        let saved = h.current(l).await?;
        ensure!(
            saved["rental"]["prices"] == prices
                && saved["rental"]["started_at"] == 0
                && saved["rental"]["expires_at"] == 0
                && saved["rental_status"] == "not_started",
            "未开通租赁状态错误"
        );
    }
    h.request(
        &format!("/api/customers/{}", customer["id"].as_str().unwrap()),
        "DELETE",
        None,
        400,
    )
    .await?;
    blocked(main).await?;
    println!("PASS 客户管理、价格与未开通租赁拒绝握手");
    let opening = json!({"line_ids":[main["id"],companion["id"]],"term":"monthly","amounts":{main["id"].as_str().unwrap():900},"note":"人工开通","request_id":"rental-opening"});
    let paid = h
        .request("/api/rentals/renew", "POST", Some(&opening), 200)
        .await?;
    ensure!(
        h.request("/api/rentals/renew", "POST", Some(&opening), 200)
            .await?["payments"]
            == paid["payments"]
            && h.state().await?["payments"].as_array().unwrap().len() == 2,
        "开通不幂等"
    );
    let mut changed = opening;
    changed["note"] = json!("不能复用为另一笔订单");
    h.request("/api/rentals/renew", "POST", Some(&changed), 400)
        .await?;
    h.synced(&lines).await?;
    for l in &lines {
        ensure!(
            h.current(l).await?["rental_status"] == "active",
            "开通未激活"
        );
    }
    let baseline = h.current(main).await?;
    socks(main, echo_port, 1, Some("wrong-rental-secret")).await?;
    let udp = UdpSocket::bind("127.0.0.1:0").await?;
    h.remember(udp.local_addr()?.port())?;
    let mut packet = vec![0, 0, 0, 1, 127, 0, 0, 1];
    packet.extend(echo_port.to_be_bytes());
    packet.extend(b"unauthorized");
    let public_address = format!("127.0.0.1:{}", main["hops"][0]["port"]).parse()?;
    rejected(&udp, &packet, public_address).await?;
    fresh(&h, main, &baseline["traffic"]["sampled_at"]).await?;
    ensure!(
        used(&h.current(main).await?) == used(&baseline),
        "未认证流量被计费"
    );
    let payload = b"rental-tcp-".repeat(512);
    echo(main, echo_port, &payload).await?;
    h.wait_state("认证 TCP 计量", 30, |s| {
        s["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == main["id"] && used(l) >= used(&baseline) + 2 * payload.len() as u64)
    })
    .await?;
    let before = h.current(main).await?;
    packet.truncate(10);
    packet.extend(b"rental-authenticated-udp");
    let other = UdpSocket::bind("127.0.0.1:0").await?;
    h.remember(other.local_addr()?.port())?;
    let (control, relay) = socks(main, udp.local_addr()?.port(), 3, None).await?;
    h.remember(relay.port())?;
    let transfer = async || -> Result<()> {
        udp.send_to(&packet, relay).await?;
        let mut response = [0; 1024];
        let n = tokio::time::timeout(Duration::from_secs(5), udp.recv(&mut response))
            .await
            .context("已认证租赁 UDP 回显超时")??;
        ensure!(
            response[..n].ends_with(b"rental-authenticated-udp"),
            "UDP 回显错误"
        );
        Ok(())
    };
    transfer().await?;
    h.wait_state("认证 UDP 计量", 30, |s| {
        s["lines"].as_array().unwrap().iter().any(|l| {
            l["id"] == main["id"] && used(l) >= used(&before) + 2 * (packet.len() - 10) as u64
        })
    })
    .await?;
    let authorized = h.current(main).await?;
    socks(main, echo_port, 1, Some("wrong-rental-secret")).await?;
    rejected(&other, &packet, public_address).await?;
    rejected(&other, &packet, relay).await?;
    fresh(&h, main, &authorized["traffic"]["sampled_at"]).await?;
    ensure!(
        used(&h.current(main).await?) == used(&authorized),
        "同 IP 的其他客户端借用 UDP 授权计费"
    );
    transfer().await?;
    h.wait_state("原 UDP 授权保持有效", 30, |s| {
        s["lines"].as_array().unwrap().iter().any(|l| {
            l["id"] == main["id"] && used(l) >= used(&authorized) + 2 * (packet.len() - 10) as u64
        })
    })
    .await?;
    let before_close = h.current(main).await?;
    drop(control);
    wait("UDP 关联关闭", async || {
        Ok(std::net::UdpSocket::bind(relay).is_ok())
    })
    .await?;
    rejected(&udp, &packet, relay).await?;
    fresh(&h, main, &before_close["traffic"]["sampled_at"]).await?;
    ensure!(
        used(&h.current(main).await?) == used(&before_close),
        "关闭的 UDP 关联继续收费"
    );
    drop(udp);
    drop(other);
    let before = h.current(main).await?;
    let mut new_prices = prices.clone();
    new_prices["monthly"] = json!(1800);
    h.request(
        &format!("/api/rentals/{}", main["id"].as_str().unwrap()),
        "POST",
        Some(&json!({"customer_id":customer["id"],"prices":new_prices})),
        200,
    )
    .await?;
    let paid = h
        .request(
            "/api/rentals/renew",
            "POST",
            Some(&json!({"line_ids":[main["id"]],"term":"monthly","request_id":"rental-renewal"})),
            200,
        )
        .await?["payments"][0]
        .clone();
    h.synced(&[main.clone()]).await?;
    let after = h.current(main).await?;
    ensure!(
        after["rental"]["started_at"] == before["rental"]["started_at"]
            && after["rental"]["expires_at"].as_u64() > before["rental"]["expires_at"].as_u64()
            && after["traffic"]["cycle_start"] == before["traffic"]["cycle_start"]
            && used(&after) == used(&before)
            && paid["previous_expires_at"] == before["rental"]["expires_at"],
        "续费重置用量或权益起点"
    );
    let amounts = h.state().await?["payments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["line_id"] == main["id"])
        .map(|p| p["amount_fen"].clone())
        .collect::<Vec<_>>();
    ensure!(amounts == vec![json!(900), json!(1800)], "历史金额被改写");
    let mut wrong = proxies[0].clone();
    wrong["password"] = json!("incorrect");
    public(&h, json!({"config":serde_json::to_string(&wrong)?}), 404).await?;
    public(&h,json!({"config":r#"{"proxy-providers":{"remote":{"url":"http://127.0.0.1:1/never-fetch"}}}"#}),404).await?;
    let mut wrong = proxies[1].clone();
    wrong["password"] = json!("incorrect");
    let result = public(
        &h,
        json!({"config":serde_json::to_string(&json!({"proxies":[proxies[0],wrong]}))?}),
        200,
    )
    .await?;
    ensure!(
        result["lines"].as_array().unwrap().len() == 1
            && result["lines"][0]["id"] == main["id"]
            && result["unmatched"] == json!(["第 2 个代理"])
            && result["renewal_contact"] == "运营续费入口",
        "公开查询凭据隔离错误"
    );
    let text = serde_json::to_string(&result)?;
    for forbidden in [
        customer["id"].as_str().unwrap(),
        "private-customer-name",
        "private-contact",
        "private-notes",
        "token_hash",
        "password",
    ] {
        ensure!(!text.contains(forbidden), "公开查询泄漏 {forbidden}");
    }
    ensure!(
        public(
            &h,
            json!({"config":serde_json::to_string(&proxies[..2])?}),
            200
        )
        .await?["lines"]
            .as_array()
            .unwrap()
            .len()
            == 2,
        "多节点查询缺失"
    );
    let query = format!("/api/rentals/{}/query-link", main["id"].as_str().unwrap());
    let old = h.request(&query, "POST", Some(&json!({})), 200).await?["path"]
        .as_str()
        .unwrap()
        .split_once("#token=")
        .context("查询令牌缺失")?
        .1
        .to_string();
    ensure!(
        public(&h, json!({"token":old}), 200).await?["lines"][0]["id"] == main["id"],
        "查询令牌范围错误"
    );
    let token = h.request(&query, "POST", Some(&json!({})), 200).await?["path"]
        .as_str()
        .unwrap()
        .split_once("#token=")
        .unwrap()
        .1
        .to_string();
    public(&h, json!({"token":old}), 404).await?;
    let draft_port = h.port()?;
    let mut draft = companion.clone();
    draft["name"] = json!("未发布的另一代理草稿");
    draft["hops"][0]["port"] = json!(draft_port);
    h.request("/api/lines", "POST", Some(&draft), 200).await?;
    let (mut stream, _) = socks(main, echo_port, 1, None).await?;
    stream.write_all(b"before-disable").await?;
    let mut response = [0; 14];
    stream.read_exact(&mut response).await?;
    ensure!(&response == b"before-disable", "关闭前流量错误");
    operate(&h, &[main.clone()], "disable", json!({})).await?;
    h.synced(&[main.clone()]).await?;
    closed(&mut stream).await?;
    ensure!(
        public(&h, json!({"token":token}), 200).await?["lines"][0]["status"] == "disabled",
        "公开禁用状态错误"
    );
    h.request(
        "/api/rentals/renew",
        "POST",
        Some(&json!({"line_ids":[main["id"]],"term":"monthly","request_id":"disabled-renewal"})),
        200,
    )
    .await?;
    h.synced(&[main.clone()]).await?;
    ensure!(
        h.current(main).await?["enabled"] == false
            && public(&h, json!({"config":exports[0]["share"]}), 200).await?["lines"][0]["status"]
                == "disabled",
        "续费绕过手动禁用"
    );
    let before = h.current(main).await?["rental"].clone();
    let payments = h.state().await?["payments"].clone();
    operate(&h, &[main.clone()], "unlimited", json!({})).await?;
    h.synced(&[main.clone()]).await?;
    let after = h.current(main).await?;
    ensure!(
        after["enabled"] == false
            && after["rental"]["expires_at"] == 0
            && h.state().await?["payments"] == payments,
        "无限期绕过禁用或产生支付"
    );
    for field in ["customer_id", "prices", "started_at"] {
        ensure!(
            after["rental"][field] == before[field],
            "无限期改写 {field}"
        );
    }
    let lookup = public(&h, json!({"token":token}), 200).await?;
    ensure!(
        lookup["lines"][0]["rental"]["permanent"] == true
            && lookup["lines"][0]["status"] == "disabled",
        "无限期公开状态错误"
    );
    blocked(main).await?;
    let active: Value = serde_json::from_slice(&fs::read(h.root.join("agent-0/active.json"))?)?;
    let specs = active["config"]["_chain"].as_array().unwrap();
    ensure!(
        specs
            .iter()
            .any(|s| s["id"] == companion["id"] && s["port"] == companion["hops"][0]["port"])
            && specs.iter().all(|s| s["port"] != draft_port)
            && h.current(companion).await?["published"] == false,
        "租赁操作意外发布其他草稿"
    );
    echo(companion, echo_port, b"companion-alive").await?;
    operate(&h, &[main.clone()], "enable", json!({})).await?;
    h.synced(&[main.clone()]).await?;
    echo(main, echo_port, b"enabled-alive").await?;
    let (mut stream, _) = socks(quota, echo_port, 1, None).await?;
    let _ = stream.write_all(&[b'q'; 16384]).await;
    let mut returned = vec![];
    let _ = tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut returned)).await?;
    ensure!(returned.len() < 16384, "8192 字节硬额度未阻止载荷");
    h.wait_state("精确硬额度", 30, |s| {
        s["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == quota["id"] && l["traffic"]["blocked"] == true && used(l) == 8192)
    })
    .await?;
    ensure!(
        h.current(quota).await?["rental_status"] == "quota_exhausted"
            && public(&h, json!({"config":exports[2]["share"]}), 200).await?["lines"][0]["rental"]
                ["permanent"]
                == true,
        "无限期绕过硬额度"
    );
    blocked(quota).await?;
    work.stop(agent).await?;
    h.wait_state("节点离线", 22, |s| s["nodes"][0]["online"] == false)
        .await?;
    let operation = operate(
        &h,
        &[main.clone()],
        "limits",
        json!({"limits":{"upload_mbps":20,"download_mbps":20}}),
    )
    .await?;
    let lookup = public(&h, json!({"token":token}), 200).await?;
    ensure!(
        operation["results"][0]["sync_status"] == "pending"
            && lookup["lines"][0]["status"] == "offline"
            && lookup["lines"][0]["sync_status"] == "pending",
        "离线操作被提前确认"
    );
    h.launch(
        "agent-reconnected",
        &h.bin.join("chain-agent"),
        agent_env,
        &[],
    )?;
    h.synced(&[main.clone()]).await?;
    echo(main, echo_port, b"reconnected").await?;
    ensure!(
        public(&h, json!({"token":token}), 200).await?["lines"][0]["status"] == "active",
        "重连状态未恢复"
    );
    operate(&h, &[main.clone()], "delete", json!({})).await?;
    public(&h, json!({"token":token}), 404).await?;
    public(&h, json!({"config":exports[0]["share"]}), 404).await?;
    ensure!(
        h.state().await?["payments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["line_id"] == main["id"] && p["amount_fen"] == 900),
        "删除丢失历史支付"
    );
    operate(&h, &[companion.clone(), quota.clone()], "delete", json!({})).await?;
    h.request(
        &format!("/api/customers/{}", customer["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    ensure!(
        h.state().await?["lines"] == json!([]) && h.state().await?["customers"] == json!([]),
        "租赁删除残留"
    );
    h.close().await?;
    println!("PASS 租赁开通/续费幂等、认证计量、UDP 隔离、查询隔离、禁用、精确额度及离线确认");
    Ok(())
}

async fn protocol_metering(
    h: &Harness<'_>,
    n: &Value,
    echo_port: u16,
    cert: &Path,
    key: &Path,
) -> Result<()> {
    let tls = json!({"mode":"manual","domain":"localhost","cert":fs::read_to_string(cert)?,"key":fs::read_to_string(key)?});
    let mut lines = vec![];
    for protocol in ["vless", "hysteria2", "tuic"] {
        lines.push(
            line(
                h,
                &[n.clone()],
                &format!("{protocol}计量验证"),
                &[0],
                json!({"access":{"protocol":protocol,"host":"127.0.0.1","tls":tls}}),
                200,
            )
            .await?,
        );
    }
    h.publish("applied").await?;
    h.remember_ports()?;
    let vless = &lines[0];
    h.wait_state("VLESS 初始计量", 30, |s| {
        s["lines"].as_array().unwrap().iter().any(|l| {
            l["id"] == vless["id"] && l["traffic"]["sampled_at"].as_u64().is_some_and(|v| v > 0)
        })
    })
    .await?;
    let before = h.current(vless).await?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut std::io::BufReader::new(fs::File::open(cert)?)) {
        roots.add(cert?)?;
    }
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
    let stream = TcpStream::connect((
        "127.0.0.1",
        vless["hops"][0]["port"].as_u64().unwrap() as u16,
    ))
    .await?;
    let mut stream = connector.connect("localhost".try_into()?, stream).await?;
    let id = uuid::Uuid::new_v4();
    ensure!(id.to_string() != vless["hops"][0]["uuid"], "测试 UUID 重复");
    let mut packet = vec![0];
    packet.extend(id.as_bytes());
    packet.extend([0, 1]);
    packet.extend(echo_port.to_be_bytes());
    packet.extend([1, 127, 0, 0, 1]);
    packet.extend(b"must-not-charge");
    stream.write_all(&packet).await?;
    let mut response = [0; 64];
    let result = tokio::time::timeout(Duration::from_secs(8), stream.read(&mut response)).await?;
    ensure!(result.is_err() || result? == 0, "错误 VLESS UUID 被接受");
    drop(stream);
    fresh(h, vless, &before["traffic"]["sampled_at"]).await?;
    ensure!(
        used(&h.current(vless).await?) == used(&before),
        "错误 VLESS UUID 消耗额度"
    );
    let binary = super::protocols::binary(h)?;
    for l in &lines {
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
        let directory = h.root.join(format!(
            "meter-client-{}",
            l["access"]["protocol"].as_str().unwrap()
        ));
        fs::create_dir(&directory)?;
        let port = h.port()?;
        let path = directory.join("config.json");
        fs::write(
            &path,
            serde_json::to_vec(
                &json!({"mixed-port":port,"allow-lan":false,"bind-address":"127.0.0.1","mode":"rule","log-level":"warning","proxies":[proxy],"rules":[format!("MATCH,{}",proxy["name"].as_str().unwrap())]}),
            )?,
        )?;
        let pid = h.launch(
            directory.file_name().unwrap().to_str().unwrap(),
            &binary,
            map(&[("SSL_CERT_FILE", cert.display().to_string())]),
            &super::super::strings(&[
                "-f",
                path.to_str().unwrap(),
                "-d",
                directory.to_str().unwrap(),
            ]),
        )?;
        wait("Mihomo 计量客户端", async || {
            Ok(TcpStream::connect(("127.0.0.1", port)).await.is_ok())
        })
        .await?;
        let before = h.current(l).await?;
        let payload = format!("{}-metered-tcp", l["access"]["protocol"].as_str().unwrap())
            .repeat(128)
            .into_bytes();
        plain_echo(port, echo_port, &payload).await?;
        h.wait_state("协议 TCP 双向计量", 30, |s| {
            s["lines"].as_array().unwrap().iter().any(|v| {
                v["id"] == l["id"]
                    && ["upload", "download"].iter().all(|k| {
                        v["traffic"][k].as_u64().unwrap_or(0)
                            >= before["traffic"][k].as_u64().unwrap_or(0) + payload.len() as u64
                    })
            })
        })
        .await?;
        let before = h.current(l).await?;
        let marker = format!("{}-metered-udp", l["access"]["protocol"].as_str().unwrap());
        super::protocols::udp_echo(port, echo_port, marker.as_bytes()).await?;
        h.wait_state("协议 UDP 双向计量", 30, |s| {
            s["lines"].as_array().unwrap().iter().any(|v| {
                v["id"] == l["id"]
                    && ["upload", "download"].iter().all(|k| {
                        v["traffic"][k].as_u64().unwrap_or(0)
                            >= before["traffic"][k].as_u64().unwrap_or(0) + marker.len() as u64
                    })
            })
        })
        .await?;
        h.work.stop(pid).await?;
    }
    operate(h, &lines, "delete", json!({})).await?;
    h.wait_state("移除计量临时线路", 30, |s| {
        s["nodes"].as_array().unwrap().iter().any(|v| {
            v["id"] == n["id"]
                && v["report"]["applied"] == true
                && v["report"]["revision"] == v["desired_revision"]
        })
    })
    .await?;
    Ok(())
}
pub async fn plain_echo(port: u16, target: u16, payload: &[u8]) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
        stream.write_all(&[5, 1, 0]).await?;
        let mut reply = [0; 2];
        stream.read_exact(&mut reply).await?;
        ensure!(reply == [5, 0], "无认证 SOCKS 客户端协商失败");
        let mut packet = vec![5, 1, 0, 1, 127, 0, 0, 1];
        packet.extend(target.to_be_bytes());
        stream.write_all(&packet).await?;
        let mut reply = [0; 10];
        stream.read_exact(&mut reply).await?;
        ensure!(reply[..2] == [5, 0], "无认证 SOCKS 连接失败");
        stream.write_all(payload).await?;
        let mut response = vec![0; payload.len()];
        stream.read_exact(&mut response).await?;
        ensure!(response == payload, "SOCKS 载荷不匹配");
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
