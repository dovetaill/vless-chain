use super::{
    super::{Options, repository, strings, workspace::Workspace},
    harness::{Harness, map},
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub async fn run(work: &Workspace, stage: &Path, case: &str, args: &Options) -> Result<()> {
    match case {
        "smoke" => smoke(work, stage).await,
        "local-web" => local_web(work, stage).await,
        "proxy-flow" => proxy_flow(work, stage).await,
        "sharing" => super::sharing::run(work, stage).await,
        "rentals" => super::rentals::run(work, stage).await,
        "resilience" => super::resilience::run(work, stage).await,
        "fronting" => super::fronting::run(work, stage, args).await,
        "fleet-two-panels" | "fleet-sharing" => super::fleet::run(work, stage, args).await,
        "protocols" | "chain-matrix" => super::protocols::run(work, stage, args).await,
        "diagnostics-browser" => diagnostics(work, stage).await,
        _ => anyhow::bail!("验收 {case} 迁移尚未完成"),
    }
}

pub async fn node(h: &Harness<'_>, name: &str, target: Option<u16>) -> Result<Value> {
    let mut value = json!({"name":name,"host":"127.0.0.1"});
    if let Some(target) = target {
        value["target"] = json!(format!("127.0.0.1:{target}"));
        value["server_name"] = json!("localhost");
    }
    let mut result = h.request("/api/nodes", "POST", Some(&value), 200).await?;
    if target.is_some() {
        result["target"] = value["target"].clone();
        result["server_name"] = value["server_name"].clone();
    }
    Ok(result)
}
pub async fn line(
    h: &Harness<'_>,
    nodes: &[Value],
    name: &str,
    path: &[usize],
    extra: Value,
    status: u16,
) -> Result<Value> {
    let hops = path
        .iter()
        .map(|i| Ok(json!({"node_id":nodes[*i]["id"],"port":h.port()?})))
        .collect::<Result<Vec<_>>>()?;
    let mut input = json!({"name":name,"kind":"vless","hops":hops});
    input
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().context("线路附加配置无效")?.clone());
    if input["kind"] == "vless" && input["access"].get("reality").is_none() {
        let node = &nodes[*path.first().context("线路路径为空")?];
        if node["target"].as_str().is_some_and(|v| !v.is_empty()) {
            if input["access"].is_null() {
                input["access"] = json!({});
            }
            input["access"]["reality"] =
                json!({"target":node["target"],"server_name":node["server_name"]});
        }
    }
    h.request("/api/lines", "POST", Some(&input), status).await
}
async fn restart(
    h: &Harness<'_>,
    pid: u32,
    env: &BTreeMap<String, String>,
    name: &str,
) -> Result<u32> {
    h.work.stop(pid).await?;
    let pid = h.launch(name, &h.bin.join("chain-panel"), env.clone(), &[])?;
    h.ready().await?;
    h.login(&h.root.join("control")).await?;
    Ok(pid)
}
pub async fn smoke(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "smoke")?;
    let (tls, cert, key) = h.tls("localhost").await?;
    let http = h.http(b"chain-integration-ok".to_vec()).await?;
    let echo = h.echo().await?;
    h.allow_loopback(&[http, echo])?;
    let mut env = map(&[
        ("CHAIN_APPLY_TIMEOUT", "9".into()),
        ("CHAIN_XRAY_LOG_LEVEL", "debug".into()),
        ("SSL_CERT_FILE", cert.display().to_string()),
    ]);
    let mut controller = h.panel(env.clone()).await?;
    env.extend(map(&[
        (
            "CHAIN_DATA_DIR",
            h.root.join("control").display().to_string(),
        ),
        ("CHAIN_LISTEN", h.base.trim_start_matches("http://").into()),
        ("CHAIN_XRAY_BIN", h.xray.display().to_string()),
    ]));
    h.anonymous("/api/state", "GET", None, 401).await?;
    h.anonymous("/api/login", "POST", Some(&json!({"token":"wrong"})), 401)
        .await?;
    h.request_headers(
        "/api/preview",
        "POST",
        Some(&json!({})),
        403,
        &[("Origin", "https://untrusted.example")],
    )
    .await?;
    h.preflight(tls, true).await?;
    let mut nodes = vec![];
    let mut agents = vec![];
    let mut agent_envs = vec![];
    for (i, name) in ["入口 A", "中转 B", "出口 C"].iter().enumerate() {
        let n = node(&h, name, Some(tls)).await?;
        let (pid, env) = h.agent(i, &n, Default::default()).await?;
        nodes.push(n);
        agents.push(pid);
        agent_envs.push(env);
    }
    h.wait_state("三节点注册", 30, |s| {
        s["nodes"]
            .as_array()
            .is_some_and(|a| a.len() == 3 && a.iter().all(|n| n["online"] == true))
    })
    .await?;
    h.request_headers(
        &format!("/api/agent/{}/sync", nodes[1]["id"].as_str().unwrap()),
        "POST",
        Some(&json!({})),
        401,
        &[
            ("Cookie", ""),
            (
                "Authorization",
                &format!("Bearer {}", nodes[0]["token"].as_str().unwrap()),
            ),
        ],
    )
    .await?;
    ensure!(
        !serde_json::to_string(&h.state().await?)?.contains("private_key"),
        "面板泄漏服务端私钥"
    );
    let reality = json!({"access":{"reality":{"target":format!("127.0.0.1:{tls}"),"server_name":"localhost"}}});
    let mut main = line(&h, &nodes, "三跳代理", &[0, 1, 2], reality.clone(), 200).await?;
    let shared = line(&h, &nodes, "共享入口出口", &[0, 2], reality.clone(), 200).await?;
    let mut removed = reality.clone();
    removed["kind"] = json!("forward");
    removed["target"] = json!(format!("127.0.0.1:{echo}"));
    line(&h, &nodes, "已移除转发", &[0], removed, 400).await?;
    line(&h, &nodes, "重复节点", &[0, 0], reality.clone(), 400).await?;
    let mut conflict = main.clone();
    conflict["id"] = json!("");
    conflict["name"] = json!("冲突端口");
    conflict["hops"] = json!([main["hops"][0]]);
    h.request("/api/lines", "POST", Some(&conflict), 400)
        .await?;
    ensure!(
        h.state().await?["lines"].as_array().unwrap().len() == 2
            && h.request("/api/preview", "POST", Some(&json!({})), 200)
                .await?["nodes"]
                .as_array()
                .unwrap()
                .len()
                == 3,
        "预览线路或节点数量错误"
    );
    h.publish("applied").await?;
    let url = format!("http://127.0.0.1:{http}/");
    h.probe(&main, &url).await?;
    h.probe(&shared, &url).await?;
    let before = h.current(&main).await?["share"].clone();
    main["hops"] = json!([main["hops"][0], main["hops"][2]]);
    main = h.request("/api/lines", "POST", Some(&main), 200).await?;
    ensure!(
        h.current(&main).await?["share"] == before,
        "路径编辑改变入口分享链接"
    );
    h.publish("applied").await?;
    h.probe(&main, &url).await?;
    let share = h.current(&shared).await?["share"].clone();
    let external = h
        .request(
            "/api/externals",
            "POST",
            Some(&json!({"name":"外部 VLESS 落地","url":share})),
            200,
        )
        .await?;
    let mut extra = reality.clone();
    extra["external_id"] = external["id"].clone();
    let via = line(&h, &nodes, "外部落地线路", &[1], extra, 200).await?;
    h.publish("applied").await?;
    h.probe(&via, &url).await?;
    let mut occupied = reality.clone();
    occupied["hops"] = json!([{"node_id":nodes[1]["id"],"port":echo}]);
    let rejected = line(&h, &nodes, "真实监听冲突", &[1], occupied, 200).await?;
    h.publish("rolled_back").await?;
    let mut stream = TcpStream::connect(("127.0.0.1", echo)).await?;
    stream.write_all(b"untouched").await?;
    let mut reply = [0; 9];
    stream.read_exact(&mut reply).await?;
    ensure!(&reply == b"untouched", "发布冲突影响外来监听");
    drop(stream);
    h.probe(&via, &url).await?;
    h.request(
        &format!("/api/lines/{}", rejected["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    controller = restart(&h, controller, &env, "control-restart").await?;
    ensure!(
        h.state().await?["lines"].as_array().unwrap().len() == 3,
        "重启丢失业务线路"
    );
    h.probe(&main, &url).await?;
    *h.preflight.as_ref().unwrap().disconnect.lock().unwrap() = Some((
        format!("Bearer {}", nodes[2]["token"].as_str().unwrap()),
        agents[2],
    ));
    let interrupted = line(&h, &nodes, "中断发布测试", &[0, 2], reality, 200).await?;
    let job = h
        .request("/api/publish", "POST", Some(&json!({})), 200)
        .await?["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    h.wait_state("下游待应用配置", 30, |s| {
        s["nodes"].as_array().unwrap().iter().any(|n| {
            n["id"] == nodes[2]["id"]
                && n["desired_revision"]
                    .as_str()
                    .is_some_and(|r| r.starts_with(&job))
        })
    })
    .await?;
    controller = restart(&h, controller, &env, "control-recover").await?;
    h.wait_state("离线恢复状态", 45, |s| {
        s["jobs"][0]["state"] == "degraded"
    })
    .await?;
    agents[2] = h.launch(
        "agent-reconnect",
        &h.bin.join("chain-agent"),
        agent_envs[2].clone(),
        &[],
    )?;
    h.wait_state("延迟恢复确认", 30, |s| {
        s["jobs"][0]["state"] == "rolled_back"
    })
    .await?;
    h.request(
        &format!("/api/lines/{}", interrupted["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    h.probe(&main, &url).await?;
    super::features::exercise(
        &mut h,
        &nodes,
        echo,
        http,
        &cert,
        &key,
        &mut agents,
        &agent_envs,
    )
    .await?;
    if std::env::var_os("CHAIN_TEST_BROWSER").is_some() {
        work.run(
            &repository(),
            &strings(&[
                "node",
                repository()
                    .join("panel/tests/browser.cjs")
                    .to_str()
                    .unwrap(),
                &h.base,
                h.root.join("control/admin-token").to_str().unwrap(),
                work.root.to_str().unwrap(),
                &h.port()?.to_string(),
            ]),
        )
        .await?;
    }
    let _ = controller;
    h.close().await?;
    println!("PASS 认证/跨域、三跳/共享链、外部落地、端口冲突回滚、重启及中断发布恢复");
    Ok(())
}

fn children(pid: i32) -> Result<Vec<i32>> {
    #[cfg(target_os = "linux")]
    {
        let mut result = vec![];
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            if let Ok(child) = entry.file_name().to_string_lossy().parse::<i32>() {
                if fs::read_to_string(entry.path().join("stat"))
                    .ok()
                    .and_then(|s| {
                        s.rsplit_once(')')
                            .map(|(_, v)| v.split_whitespace().nth(1).unwrap_or("").to_string())
                    })
                    .is_some_and(|s| s == pid.to_string())
                {
                    result.push(child);
                }
            }
        }
        Ok(result)
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-axo", "pid=,ppid="])
            .output()?;
        Ok(String::from_utf8(out.stdout)?
            .lines()
            .filter_map(|l| {
                let mut a = l.split_whitespace();
                let child = a.next()?.parse().ok()?;
                let parent: i32 = a.next()?.parse().ok()?;
                (parent == pid).then_some(child)
            })
            .collect())
    }
}
fn exited(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |s| {
            s.rsplit_once(')')
                .is_some_and(|(_, v)| v.split_whitespace().next() == Some("Z"))
        })
    }
    #[cfg(target_os = "macos")]
    {
        (unsafe { libc::kill(pid, 0) }) != 0
    }
}
async fn local_web(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "local-web")?;
    let controller = h.panel(Default::default()).await?;
    ensure!(
        h.state().await?["local_node_id"].is_null(),
        "新控制台已有本机节点"
    );
    let node = h
        .request(
            "/api/local-node",
            "POST",
            Some(&json!({"host":"127.0.0.1"})),
            200,
        )
        .await?["id"]
        .clone();
    h.wait_state("本机 Agent", 30, |s| {
        s["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == node && n["online"] == true)
    })
    .await?;
    ensure!(
        h.request(
            "/api/local-node",
            "POST",
            Some(&json!({"host":"127.0.0.1"})),
            200
        )
        .await?["id"]
            == node,
        "重复注册改变节点 ID"
    );
    let state = h.state().await?;
    ensure!(
        state["nodes"].as_array().unwrap().len() == 1
            && state["local_node_id"] == node
            && state["nodes"][0]["target"] == ""
            && state["nodes"][0]["server_name"] == "",
        "本机注册默认值错误"
    );
    h.request(
        &format!("/api/nodes/{}", node.as_str().unwrap()),
        "DELETE",
        None,
        409,
    )
    .await?;
    h.request(
        &format!("/api/nodes/{}/token", node.as_str().unwrap()),
        "POST",
        Some(&json!({})),
        409,
    )
    .await?;
    h.request("/api/lines","POST",Some(&json!({"name":"local SS","kind":"vless","hops":[{"node_id":node,"port":h.port()?}],"access":{"protocol":"shadowsocks"}})),200).await?;
    h.publish("applied").await?;
    let owned = children(controller as i32)?;
    ensure!(!owned.is_empty(), "控制台未拥有本机 Agent");
    let mut descendants = owned.clone();
    for pid in owned {
        descendants.extend(children(pid)?);
    }
    let seen = h.state().await?["nodes"][0]["seen"].as_u64().unwrap();
    unsafe {
        libc::kill(controller as i32, libc::SIGKILL);
    }
    wait("控制台强制退出后回收 Agent/Xray", async || {
        Ok(descendants.iter().all(|p| exited(*p)))
    })
    .await?;
    let env = map(&[
        (
            "CHAIN_DATA_DIR",
            h.root.join("control").display().to_string(),
        ),
        ("CHAIN_LISTEN", h.base.trim_start_matches("http://").into()),
        ("CHAIN_XRAY_BIN", h.xray.display().to_string()),
    ]);
    h.launch("panel-restarted", &h.bin.join("chain-panel"), env, &[])?;
    h.ready().await?;
    h.login(&h.root.join("control")).await?;
    h.wait_state("本机 Agent 恢复身份", 30, |s| {
        s["nodes"].as_array().unwrap().iter().any(|n| {
            n["id"] == node && n["online"] == true && n["seen"].as_u64().is_some_and(|v| v > seen)
        })
    })
    .await?;
    ensure!(
        h.state().await?["local_node_id"] == node,
        "重启改变本机身份"
    );
    h.close().await?;
    Ok(())
}

async fn proxy_flow(work: &Workspace, stage: &Path) -> Result<()> {
    super::proxy_flow::run(work, stage).await
}
async fn diagnostics(work: &Workspace, stage: &Path) -> Result<()> {
    use std::io::Write;
    let mut h = Harness::new(work, stage, "diagnostics-browser")?;
    h.panel(Default::default()).await?;
    let n = node(&h, "浏览器测试节点", None).await?;
    h.agent(0, &n, Default::default()).await?;
    h.wait_state("诊断能力", 30, |s| {
        s["nodes"][0]["report"]["system"]["diagnostics_version"] == 1
    })
    .await?;
    let l = line(
        &h,
        &[n],
        "浏览器诊断代理",
        &[0],
        json!({"access":{"protocol":"socks"}}),
        200,
    )
    .await?;
    let tag = format!(
        "{} line-{}-in ",
        chrono::Utc::now().timestamp(),
        l["id"].as_str().unwrap()
    );
    let agent = h.root.join("agent-0");
    fs::write(
        agent.join("xray.log"),
        format!("{tag}fixture-live\n{tag}password=fixture-password\n"),
    )?;
    let mut gz = flate2::write::GzEncoder::new(
        fs::File::create(agent.join("xray.log.123.gz"))?,
        flate2::Compression::default(),
    );
    writeln!(gz, "{tag}fixture-archive")?;
    gz.finish()?;
    work.run(
        &repository(),
        &strings(&[
            "node",
            repository()
                .join("panel/tests/diagnostics-ui.cjs")
                .to_str()
                .unwrap(),
            &h.base,
            h.root.join("control/admin-token").to_str().unwrap(),
            h.root.to_str().unwrap(),
            agent.to_str().unwrap(),
            l["id"].as_str().unwrap(),
        ]),
    )
    .await?;
    if let Some(directory) = std::env::var_os("CHAIN_TEST_SCREENSHOTS") {
        for entry in fs::read_dir(&h.root)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|v| v == "png") {
                fs::copy(entry.path(), Path::new(&directory).join(entry.file_name()))?;
            }
        }
    }
    h.close().await
}
