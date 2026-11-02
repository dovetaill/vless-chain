use super::{
    super::{Options, strings, workspace::Workspace},
    business::{line, node},
    harness::{Harness, map},
    protocols::{PROFILES, binary, udp_echo},
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

async fn counters(h: &Harness<'_>, lines: &[Value]) -> Result<BTreeMap<String, u64>> {
    Ok(h.state().await?["lines"]
        .as_array()
        .context("缺少线路")?
        .iter()
        .filter(|l| lines.iter().any(|v| v["id"] == l["id"]))
        .map(|l| {
            (
                l["id"].as_str().unwrap().to_string(),
                l["traffic"]["upload"].as_u64().unwrap_or(0)
                    + l["traffic"]["download"].as_u64().unwrap_or(0),
            )
        })
        .collect())
}
async fn traversed(h: &Harness<'_>, lines: &[Value], before: &BTreeMap<String, u64>) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let after = counters(h, lines).await?;
        if after.len() == lines.len()
            && after
                .iter()
                .all(|(id, value)| *value > before.get(id).copied().unwrap_or(0))
        {
            return Ok(());
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "预期链路未全部计量：before={before:?}, after={after:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn select(
    client: &reqwest::Client,
    controller: u16,
    secret: &str,
    entry: &Value,
) -> Result<()> {
    let response = client
        .put(format!("http://127.0.0.1:{controller}/proxies/matrix"))
        .bearer_auth(secret)
        .json(&json!({"name":entry["name"]}))
        .send()
        .await?;
    ensure!(
        response.status() == 204,
        "无法切换矩阵客户端：{}",
        response.text().await?
    );
    Ok(())
}
struct Matrix<'a, 'w> {
    h: &'a Harness<'w>,
    client: reqwest::Client,
    direct: reqwest::Client,
    port: u16,
    controller: u16,
    secret: String,
    http: u16,
    udp: u16,
    results: Vec<Value>,
    selected: Vec<String>,
}
impl Matrix<'_, '_> {
    fn selected(&self, label: &str) -> bool {
        self.selected.is_empty() || self.selected.iter().any(|v| v == label)
    }
    async fn test(
        &mut self,
        label: String,
        entry: &Value,
        path: &[Value],
        udp: bool,
        bytes: usize,
    ) -> Result<()> {
        select(&self.direct, self.controller, &self.secret, entry).await?;
        if !udp && entry["access"]["protocol"] != "http" {
            let export = self
                .h
                .request(
                    &format!("/api/lines/{}/export", entry["id"].as_str().unwrap()),
                    "GET",
                    None,
                    200,
                )
                .await?;
            let proxy: Value =
                serde_json::from_str(export["node"].as_str().unwrap().strip_prefix("- ").unwrap())?;
            ensure!(
                proxy["udp"] == false && export["share"].as_str().unwrap().contains("udp=false"),
                "TCP 链路错误宣告 UDP"
            );
        }
        let mut lines = vec![entry.clone()];
        lines.extend_from_slice(path);
        let before = counters(self.h, &lines).await?;
        let tcp = async {
            let response = self
                .client
                .get(format!("http://127.0.0.1:{}/", self.http))
                .header("Connection", "close")
                .send()
                .await?;
            ensure!(
                response.status() == 200 && response.text().await? == "chain-integration-ok",
                "矩阵 TCP 内容错误"
            );
            traversed(self.h, &lines, &before).await
        }
        .await;
        let mut errors = vec![];
        if let Err(error) = &tcp {
            errors.push(format!("TCP {error:#}"));
        }
        let udp_result = if udp {
            let result = async {
                let before = counters(self.h, &lines).await?;
                udp_echo(self.port, self.udp, b"matrix-small").await?;
                let payload = (0..bytes).map(|_| rand::random::<u8>()).collect::<Vec<_>>();
                udp_echo(self.port, self.udp, &payload).await?;
                traversed(self.h, &lines, &before).await?;
                if bytes < 4096 {
                    let payload = vec![b'x'; bytes + 1];
                    let result = udp_echo(self.port, self.udp, &payload).await;
                    ensure!(
                        result
                            .as_ref()
                            .err()
                            .is_some_and(|e| e.is::<tokio::time::error::Elapsed>()),
                        "Hysteria 2 UDP 边界行为改变：{result:?}"
                    );
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = &result {
                errors.push(format!("UDP {error:#}"));
            }
            Some(result.is_ok())
        } else {
            None
        };
        let result = json!({"case":label,"tcp":tcp.is_ok(),"udp":udp_result,"hops":tcp.is_ok()&&udp_result.unwrap_or(true),"udp_payload_bytes":if udp{Some(bytes)}else{None},"errors":errors});
        println!("CASE {result}");
        self.results.push(result);
        fs::write(
            self.h.root.join("matrix-results.json"),
            serde_json::to_vec_pretty(&self.results)?,
        )?;
        Ok(())
    }
}
async fn create(
    h: &Harness<'_>,
    n: &Value,
    name: &str,
    index: usize,
    tls: &Value,
    chain: &[String],
) -> Result<Value> {
    let (_, protocol, secure, network) = PROFILES[index];
    let port = loop {
        let port = h.port()?;
        if std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            break port;
        }
    };
    line(h,&[n.clone()],name,&[0],json!({"hops":[{"node_id":n["id"],"port":port}],"chain":chain,"access":{"protocol":protocol,"host":"127.0.0.1","tls":if secure{tls.clone()}else{json!({"mode":"off"})},"transport":{"network":network,"path":"/matrix","host":"","mode":"auto"}}}),200).await
}
async fn chain(h: &Harness<'_>, entry: &Value, path: &[String], status: u16) -> Result<Value> {
    let mut body = entry.clone();
    body["chain"] = json!(path);
    h.request("/api/lines", "POST", Some(&body), status).await
}
pub async fn run(work: &Workspace, stage: &Path, args: &Options) -> Result<()> {
    let mut h = Harness::new(work, stage, "matrix")?;
    let (target, cert, key) = h.tls("localhost").await?;
    let http = h.http(b"chain-integration-ok".to_vec()).await?;
    let udp = h.echo().await?;
    h.allow_loopback(&[http, udp])?;
    h.panel(map(&[("SSL_CERT_FILE", cert.display().to_string())]))
        .await?;
    let n = node(&h, "chain-matrix", Some(target)).await?;
    h.preflight(target, true).await?;
    h.agent(0, &n, map(&[("SSL_CERT_FILE", cert.display().to_string())]))
        .await?;
    h.wait_state("矩阵 Agent", 30, |s| s["nodes"][0]["online"] == true)
        .await?;
    let tls = json!({"mode":"manual","domain":"localhost","cert":fs::read_to_string(&cert)?,"key":fs::read_to_string(key)?});
    let mut exits = vec![];
    let mut middles = vec![];
    let mut entries = vec![];
    let mut drivers = vec![];
    for (i, p) in PROFILES.iter().enumerate() {
        exits.push(create(&h, &n, &format!("exit-{}", p.0), i, &tls, &[]).await?);
        middles.push(create(&h, &n, &format!("middle-{}", p.0), i, &tls, &[]).await?);
    }
    for (i, p) in PROFILES.iter().enumerate() {
        entries.push(
            create(
                &h,
                &n,
                &format!("entry-{}", p.0),
                i,
                &tls,
                &[format!("line:{}", exits[1]["id"].as_str().unwrap())],
            )
            .await?,
        );
        drivers.push(create(&h, &n, &format!("driver-{}", p.0), 8, &tls, &[]).await?);
    }
    h.publish("applied").await?;
    let mut refs = vec![];
    for group in [&middles, &exits] {
        let mut group_refs = vec![];
        for (i, l) in group.iter().enumerate() {
            let export = h
                .request(
                    &format!("/api/lines/{}/export", l["id"].as_str().unwrap()),
                    "GET",
                    None,
                    200,
                )
                .await?;
            let external = h
                .request(
                    "/api/externals",
                    "POST",
                    Some(&json!({"url":export[if i%2==1{"share"}else{"node"}]})),
                    200,
                )
                .await?;
            group_refs.push(format!("external:{}", external["id"].as_str().unwrap()));
        }
        refs.push(group_refs);
    }
    let mut proxies = vec![];
    for l in entries.iter().chain(&drivers) {
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
    }
    let port = h.port()?;
    let controller = h.port()?;
    let secret = uuid::Uuid::new_v4().to_string();
    let dir = h.root.join("client");
    fs::create_dir(&dir)?;
    let path = dir.join("config.json");
    fs::write(
        &path,
        serde_json::to_vec(
            &json!({"mixed-port":port,"bind-address":"127.0.0.1","mode":"rule","external-controller":format!("127.0.0.1:{controller}"),"secret":secret,"proxies":proxies,"proxy-groups":[{"name":"matrix","type":"select","proxies":proxies.iter().map(|p|p["name"].clone()).collect::<Vec<_>>()}],"rules":["MATCH,matrix"],"log-level":"warning"}),
        )?,
    )?;
    let binary = binary(&h)?;
    let env = map(&[("SSL_CERT_FILE", cert.display().to_string())]);
    let check = h.launch(
        "client-check",
        &binary,
        env.clone(),
        &strings(&[
            "-t",
            "-d",
            dir.to_str().unwrap(),
            "-f",
            path.to_str().unwrap(),
        ]),
    )?;
    wait("矩阵客户端检查", async || {
        Ok(work.poll(check)?.is_some())
    })
    .await?;
    ensure!(
        work.poll(check)?.unwrap().success(),
        "矩阵客户端配置检查失败"
    );
    h.launch(
        "client",
        &binary,
        env,
        &strings(&["-d", dir.to_str().unwrap(), "-f", path.to_str().unwrap()]),
    )?;
    let mut m = Matrix {
        h: &h,
        client: reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))?)
            .timeout(Duration::from_secs(12))
            .build()?,
        direct: reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?,
        port,
        controller,
        secret,
        http,
        udp,
        results: vec![],
        selected: args.trailing.clone(),
    };
    wait("矩阵客户端监听", async || {
        Ok(select(&m.direct, controller, &m.secret, &entries[0])
            .await
            .is_ok())
    })
    .await?;
    if m.selected.iter().any(|s| s == "exports and probes") {
        for i in [4, 7, 11, 12] {
            h.probe(&entries[i], &format!("http://127.0.0.1:{http}/"))
                .await?;
        }
        let limited = chain(&h, &drivers[0], &[refs[1][6].clone()], 200).await?;
        let nested = chain(
            &h,
            &drivers[1],
            &[format!("line:{}", limited["id"].as_str().unwrap())],
            200,
        )
        .await?;
        h.publish("applied").await?;
        let exported = h
            .request(
                &format!("/api/lines/{}/export", nested["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        ensure!(
            serde_json::from_str::<Value>(
                exported["node"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("- ")
                    .unwrap()
            )?["udp"]
                == false,
            "嵌套链路 UDP 能力错误"
        );
        let imported = h
            .request(
                "/api/externals",
                "POST",
                Some(&json!({"url":exported["node"]})),
                200,
            )
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
            )?["udp"]
                == false,
            "导入丢失 UDP 限制"
        );
        let sub = h
            .request(
                "/api/subscriptions",
                "POST",
                Some(&json!({"name":"nested only","line_ids":[nested["id"]]})),
                200,
            )
            .await?;
        let yaml = h
            .request(sub["path"].as_str().unwrap(), "GET", None, 200)
            .await?;
        ensure!(
            yaml.as_str()
                .unwrap()
                .contains(nested["name"].as_str().unwrap())
                && yaml.as_str().unwrap().contains("udp: false"),
            "订阅丢失 UDP 限制"
        );
        let rejected = chain(
            &h,
            &drivers[2],
            &[
                format!("line:{}", limited["id"].as_str().unwrap()),
                refs[1][11].clone(),
            ],
            400,
        )
        .await?;
        ensure!(
            rejected["error"].as_str().unwrap().contains("需要 UDP"),
            "QUIC 链路未拒绝 TCP 中继"
        );
        m.results
            .push(json!({"case":"exports and probes","errors":[]}));
    }
    for (i, p) in PROFILES.iter().enumerate() {
        let label = format!("ingress {} -> tls-vision", p.0);
        if m.selected(&label) {
            m.test(label, &entries[i], &[exits[1].clone()], p.1 != "http", 4096)
                .await?;
        }
        let label = format!("single {}", p.0);
        if m.selected.iter().any(|s| s == &label) {
            let driver = chain(&h, &drivers[0], &[refs[1][i].clone()], 200).await?;
            h.publish("applied").await?;
            m.test(label, &driver, &[exits[i].clone()], p.1 != "http", 4096)
                .await?;
        }
    }
    for (b, exit) in exits.iter().enumerate() {
        let mut valid = vec![];
        for (a, p) in PROFILES.iter().enumerate() {
            let label = format!("{} -> {}", p.0, PROFILES[b].0);
            if !m.selected(&label) {
                continue;
            }
            let path = [refs[0][a].clone(), refs[1][b].clone()];
            if p.1 == "http" && matches!(PROFILES[b].1, "hysteria2" | "tuic") {
                let rejected = chain(&h, &drivers[a], &path, 400).await?;
                ensure!(
                    rejected["error"].as_str().unwrap().contains("需要 UDP")
                        && h.current(&drivers[a]).await?["chain"] == drivers[a]["chain"],
                    "拒绝的链路改变了草稿"
                );
                m.results
                    .push(json!({"case":label,"rejected":true,"errors":[]}));
            } else {
                drivers[a] = chain(&h, &drivers[a], &path, 200).await?;
                valid.push(a);
            }
        }
        if !valid.is_empty() {
            h.publish("applied").await?;
        }
        for a in valid {
            let bytes = match (PROFILES[a].1, PROFILES[b].1) {
                ("hysteria2", "shadowsocks") => 4096 - 58,
                ("hysteria2", "socks") => 4096 - 10,
                _ => 4096,
            };
            m.test(
                format!("{} -> {}", PROFILES[a].0, PROFILES[b].0),
                &drivers[a],
                &[middles[a].clone(), exit.clone()],
                PROFILES[b].1 != "http"
                    && !(PROFILES[a].1 == "http"
                        && matches!(PROFILES[b].1, "shadowsocks" | "socks")),
                bytes,
            )
            .await?;
        }
        println!("COLUMN_DONE {}", PROFILES[b].0);
    }
    for order in [
        vec![4, 2, 11, 12],
        vec![12, 11, 3, 5],
        vec![8, 4, 11],
        vec![4, 11, 12],
    ] {
        let label = format!(
            "mixed {}",
            order
                .iter()
                .map(|i| PROFILES[*i].0)
                .collect::<Vec<_>>()
                .join(" -> ")
        );
        if !m.selected(&label) {
            continue;
        }
        let driver = chain(
            &h,
            &drivers[0],
            &order
                .iter()
                .map(|i| refs[0][*i].clone())
                .collect::<Vec<_>>(),
            200,
        )
        .await?;
        h.publish("applied").await?;
        m.test(
            label,
            &driver,
            &order
                .iter()
                .map(|i| middles[*i].clone())
                .collect::<Vec<_>>(),
            true,
            4096,
        )
        .await?;
    }
    ensure!(
        m.selected
            .iter()
            .all(|s| m.results.iter().any(|r| r["case"] == *s)),
        "存在未知或未执行的矩阵用例"
    );
    let failed = m
        .results
        .iter()
        .filter(|r| !r["errors"].as_array().unwrap().is_empty())
        .collect::<Vec<_>>();
    println!(
        "MATRIX_SUMMARY {}",
        json!({"total":m.results.len(),"failed":failed})
    );
    ensure!(failed.is_empty(), "矩阵验收失败");
    h.close().await?;
    Ok(())
}
