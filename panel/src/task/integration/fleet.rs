use super::{
    super::{
        Options, repository, strings,
        workspace::{Workspace, copy_tree},
    },
    harness::{Harness, map},
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

pub fn proxy(args: &Options) -> Result<()> {
    let root = Path::new(args.value("--input")?).canonicalize()?;
    let config: Value = serde_json::from_slice(&fs::read(root.join("fleet-proxy.json"))?)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            use axum::{
                Router,
                body::{Body, to_bytes},
                extract::{Request, State},
                http::StatusCode,
                routing::any,
            };
            use hyper_util::{
                rt::{TokioExecutor, TokioIo},
                server::conn::auto::Builder,
                service::TowerToHyperService,
            };
            #[derive(Clone)]
            struct Proxy {
                base: String,
                client: reqwest::Client,
            }
            async fn forward(
                State(state): State<Proxy>,
                request: Request,
            ) -> Result<axum::response::Response, StatusCode> {
                let (method, path, headers) = (
                    request.method().clone(),
                    request.uri().to_string(),
                    request.headers().clone(),
                );
                let body = to_bytes(request.into_body(), 2 * 1024 * 1024)
                    .await
                    .map_err(|_| StatusCode::BAD_REQUEST)?;
                let r = state
                    .client
                    .request(method, format!("{}{path}", state.base))
                    .headers(headers)
                    .body(body)
                    .send()
                    .await
                    .map_err(|_| StatusCode::BAD_GATEWAY)?;
                let status = r.status();
                let headers = r.headers().clone();
                let bytes = r.bytes().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
                let mut out = axum::response::Response::new(Body::from(bytes));
                *out.status_mut() = status;
                *out.headers_mut() = headers;
                Ok(out)
            }
            let tls = crate::website::tls_config(
                &fs::read_to_string(root.join("localhost-cert.pem"))?,
                &fs::read_to_string(root.join("localhost-key.pem"))?,
            )?;
            let acceptor = tokio_rustls::TlsAcceptor::from(tls);
            let listener = tokio::net::TcpListener::bind((
                "127.0.0.1",
                config["port"].as_u64().context("代理端口无效")? as u16,
            ))
            .await?;
            let app = Router::new().fallback(any(forward)).with_state(Proxy {
                base: config["base"].as_str().context("代理目标缺失")?.into(),
                client: reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(30))
                    .build()?,
            });
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await?;
                let acceptor = acceptor.clone();
                let app = app.clone();
                connections.spawn(async move {
                    if let Ok(Ok(stream)) =
                        tokio::time::timeout(Duration::from_secs(6), acceptor.accept(stream)).await
                    {
                        let _ = Builder::new(TokioExecutor::new())
                            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
                            .await;
                    }
                });
                while connections.try_join_next().is_some() {}
            }
            #[allow(unreachable_code)]
            Ok::<_, anyhow::Error>(())
        })
}
async fn forward(
    h: &Harness<'_>,
    remote: &str,
    path: &str,
    method: &str,
    body: Option<Value>,
    status: u16,
) -> Result<Value> {
    h.request(
        &format!("/api/fleet/{remote}/forward"),
        "POST",
        Some(&json!({"path":path,"method":method,"body":body})),
        status,
    )
    .await
}
async fn delegated(master: &Harness<'_>, slave: &Harness<'_>, remote: &str) -> Result<String> {
    let value = master
        .request(
            &format!("/api/fleet/{remote}/login"),
            "POST",
            Some(&json!({})),
            200,
        )
        .await?;
    let u = url::Url::parse(value["url"].as_str().context("委托 URL 缺失")?)?;
    let ticket = url::form_urlencoded::parse(u.fragment().context("委托票据缺失")?.as_bytes())
        .find(|(k, _)| k == "fleet-ticket")
        .map(|(_, v)| v.into_owned())
        .context("委托票据缺失")?;
    let admin = slave.save_cookie();
    slave
        .anonymous(
            "/api/federation/login",
            "POST",
            Some(&json!({"ticket":ticket})),
            200,
        )
        .await?;
    let cookie = slave.save_cookie();
    slave
        .anonymous(
            "/api/federation/login",
            "POST",
            Some(&json!({"ticket":ticket})),
            401,
        )
        .await?;
    slave.restore_cookie(cookie.clone());
    slave.state().await?;
    slave.restore_cookie(admin);
    Ok(cookie)
}
async fn monitor(h: &Harness<'_>, remote: &str, active: bool) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(80);
    loop {
        let view = h.request("/api/monitoring", "GET", None, 200).await?;
        if view["conditions"][format!("panel:{remote}")]["active"] == active {
            return Ok(view);
        }
        ensure!(Instant::now() < deadline, "远端监控状态超时：{view}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
pub async fn run(work: &Workspace, stage: &Path, args: &Options) -> Result<()> {
    let mut master = Harness::new(work, stage, "fleet-master")?;
    let mut slave = Harness::new(work, stage, "fleet-slave")?;
    let (_, cert, _) = master.tls("localhost").await?;
    // The private fixture CA is compiled only into this disposable copy, never into a release.
    let project = work.root.join("fleet-project");
    fs::create_dir(&project)?;
    let source = repository().join("panel");
    for name in ["Cargo.toml", "Cargo.lock", "build.rs"] {
        fs::copy(source.join(name), project.join(name))?;
    }
    for name in ["src", "reality-candidates", "tests/fixtures"] {
        copy_tree(&source.join(name), &project.join(name))?;
    }
    let fleet = project.join("src/fleet_api.rs");
    let original = fs::read_to_string(&fleet)?;
    let marker = "let client = reqwest::Client::builder()\n        .no_proxy()";
    ensure!(
        original.contains(marker),
        "测试 CA 注入位置改变，请检查夹具"
    );
    fs::write(&fleet,original.replacen(marker,&format!("let client = reqwest::Client::builder()\n        .add_root_certificate(reqwest::Certificate::from_pem(include_bytes!({:?}))?)\n        .no_proxy()",cert.to_str().unwrap()),1))?;
    work.run(
        &project,
        &strings(&[
            "cargo",
            "build",
            "--locked",
            "--bin",
            "chain-panel",
            "--quiet",
        ]),
    )
    .await?;
    let bin = work.root.join("fleet-bin");
    fs::create_dir(&bin)?;
    fs::copy(
        Path::new(&work.env["CARGO_TARGET_DIR"]).join("debug/chain-panel"),
        bin.join("chain-panel"),
    )?;
    master.bin = bin.clone();
    slave.bin = bin;
    let base_env = map(&[
        ("CHAIN_SECURE_COOKIE", "1".into()),
        ("CHAIN_XRAY_BIN", "/nonexistent/test-xray".into()),
    ]);
    let mut master_pid = master.panel(base_env.clone()).await?;
    let mut slave_pid = slave.panel(base_env.clone()).await?;
    let port = master.port()?;
    fs::write(
        master.root.join("fleet-proxy.json"),
        serde_json::to_vec(&json!({"port":port,"base":slave.base}))?,
    )?;
    master.launch(
        "tls-forwarder",
        &std::env::current_exe()?,
        Default::default(),
        &strings(&[
            "integration",
            "--case",
            "fleet-tls-proxy",
            "--input",
            master.root.to_str().unwrap(),
        ]),
    )?;
    wait("舰队 HTTPS 入口", async || {
        Ok(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok())
    })
    .await?;
    let code = slave
        .request("/api/fleet/invite", "POST", Some(&json!({})), 200)
        .await?["code"]
        .clone();
    let remote = master
        .request(
            "/api/fleet/connect",
            "POST",
            Some(
                &json!({"name":"sim-slave","code":code,"url":format!("https://localhost:{port}")}),
            ),
            200,
        )
        .await?["id"]
        .as_str()
        .context("远端 ID 缺失")?
        .to_string();
    slave.anonymous("/api/federation/pair","POST",Some(&json!({"code":code,"name":"replay","controller_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"})),400).await?;
    forward(
        &master,
        &remote,
        "/security/token",
        "POST",
        Some(json!({})),
        403,
    )
    .await?;
    let external = forward(
        &master,
        &remote,
        "/externals",
        "POST",
        Some(json!({"name":"test","url":"socks5://127.0.0.1:10999"})),
        200,
    )
    .await?;
    forward(
        &master,
        &remote,
        &format!("/externals/{}", external["id"].as_str().unwrap()),
        "POST",
        Some(json!({"name":"updated","url":"socks5://127.0.0.1:10998"})),
        200,
    )
    .await?;
    ensure!(
        slave.state().await?["externals"][0]["name"] == "updated"
            && master.state().await?["externals"] == json!([]),
        "舰队外部节点修改或数据隔离失败"
    );
    let node = slave
        .request(
            "/api/nodes",
            "POST",
            Some(&json!({"name":"sim-agent","host":"127.0.0.1"})),
            200,
        )
        .await?;
    let input = json!({"name":"sim-socks","kind":"vless","hops":[{"node_id":node["id"],"port":slave.port()?}],"access":{"protocol":"socks"}});
    let l = forward(&master, &remote, "/lines", "POST", Some(input.clone()), 200).await?;
    let mut edit = input;
    edit["id"] = l["id"].clone();
    edit["name"] = json!("edited-socks");
    forward(&master, &remote, "/lines", "POST", Some(edit), 200).await?;
    ensure!(
        forward(&master, &remote, "/state", "GET", None, 200).await?["lines"][0]["name"]
            == "edited-socks",
        "舰队编辑未保存"
    );
    forward(
        &master,
        &remote,
        &format!("/lines/{}", l["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    forward(
        &master,
        &remote,
        &format!("/externals/{}", external["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    if args
        .values
        .get("--case")
        .is_some_and(|v| v == "fleet-sharing")
    {
        sharing(&master, &slave, &remote, &node).await?;
    }
    slave
        .request(
            &format!("/api/nodes/{}", node["id"].as_str().unwrap()),
            "DELETE",
            None,
            200,
        )
        .await?;
    let local = slave.request("/api/monitoring", "GET", None, 200).await?;
    let mut policy = local["local"].clone();
    policy["enabled"] = json!(true);
    policy["interval"] = json!(60);
    policy["offline"] = json!(30);
    policy["recoveries"] = json!(1);
    master
        .request("/api/fleet/policy", "POST", Some(&policy), 200)
        .await?;
    wait("远端策略覆盖与本地策略保留", async || {
        let view = slave.request("/api/monitoring", "GET", None, 200).await?;
        Ok(view["effective"] == policy && view["local"] == local["local"])
    })
    .await?;
    wait("定时舰队快照", async || {
        Ok(
            !master.request("/api/fleet", "GET", None, 200).await?["remotes"][0]["snapshot"]
                .is_null(),
        )
    })
    .await?;
    delegated(&master, &slave, &remote).await?;
    work.stop(slave_pid).await?;
    ensure!(
        monitor(&master, &remote, true).await?["notices"]
            .as_array()
            .unwrap()
            .len()
            == 1,
        "离线通知重复或丢失"
    );
    let mut slave_env = base_env.clone();
    slave_env.insert(
        "CHAIN_LISTEN".into(),
        slave.base.trim_start_matches("http://").into(),
    );
    slave_env.insert(
        "CHAIN_DATA_DIR".into(),
        slave.root.join("control").display().to_string(),
    );
    slave_pid = slave.launch(
        "slave-restart",
        &slave.bin.join("chain-panel"),
        slave_env,
        &[],
    )?;
    slave.ready().await?;
    slave.login(&slave.root.join("control")).await?;
    ensure!(
        slave.request("/api/monitoring", "GET", None, 200).await?["effective"] == policy,
        "重启丢失配对策略"
    );
    ensure!(
        monitor(&master, &remote, false).await?["notices"]
            .as_array()
            .unwrap()
            .len()
            == 2,
        "恢复通知重复或丢失"
    );
    work.stop(master_pid).await?;
    slave.state().await?;
    ensure!(
        slave.request("/api/monitoring", "GET", None, 200).await?["effective"] == policy,
        "主控离线影响远端策略"
    );
    let mut master_env = base_env;
    master_env.insert(
        "CHAIN_LISTEN".into(),
        master.base.trim_start_matches("http://").into(),
    );
    master_env.insert(
        "CHAIN_DATA_DIR".into(),
        master.root.join("control").display().to_string(),
    );
    master_pid = master.launch(
        "master-restart",
        &master.bin.join("chain-panel"),
        master_env,
        &[],
    )?;
    master.ready().await?;
    master.login(&master.root.join("control")).await?;
    ensure!(
        master.request("/api/fleet", "GET", None, 200).await?["remotes"][0]["id"] == remote,
        "主控重启丢失配对"
    );
    let cookie = delegated(&master, &slave, &remote).await?;
    master
        .request(&format!("/api/fleet/{remote}"), "DELETE", None, 200)
        .await?;
    ensure!(
        slave.request("/api/monitoring", "GET", None, 200).await?["effective"] == local["local"],
        "解绑未恢复本地策略"
    );
    let admin = slave.save_cookie();
    slave.restore_cookie(cookie);
    slave.request("/api/state", "GET", None, 401).await?;
    slave.restore_cookie(admin);
    slave.state().await?;
    ensure!(
        master.request("/api/fleet", "GET", None, 200).await?["remotes"] == json!([]),
        "解绑后仍保存远端"
    );
    let _ = (master_pid, slave_pid);
    slave.close().await?;
    master.close().await?;
    println!(
        "PASS 双面板 HTTPS 配对/重放拒绝、远端 CRUD、数据隔离、策略覆盖、真实停机通知、重启、解绑与委托会话撤销"
    );
    Ok(())
}

async fn sharing(
    master: &Harness<'_>,
    slave: &Harness<'_>,
    remote: &str,
    node: &Value,
) -> Result<()> {
    use futures_util::{StreamExt, TryStreamExt};
    let l=forward(master,remote,"/lines","POST",Some(json!({"name":"sharing-policy","kind":"vless","hops":[{"node_id":node["id"],"port":slave.port()?}],"access":{"protocol":"socks"}})),200).await?;
    slave
        .request_headers(
            &format!("/api/agent/{}/sync", node["id"].as_str().unwrap()),
            "POST",
            Some(&json!({"system":{"gateway_version":2,"sharing_version":0}})),
            200,
            &[
                ("Cookie", ""),
                (
                    "Authorization",
                    &format!("Bearer {}", node["token"].as_str().unwrap()),
                ),
            ],
        )
        .await?;
    for policy in [
        json!({"udp":"deny"}),
        json!({"udp":"ports","udp_ports":[53]}),
        json!({"denied_ports":[25]}),
    ] {
        forward(
            master,
            remote,
            &format!("/access/{}", l["id"].as_str().unwrap()),
            "POST",
            Some(policy),
            200,
        )
        .await?;
        let rejection = slave
            .request("/api/publish", "POST", Some(&json!({})), 400)
            .await?;
        ensure!(
            rejection["error"].as_str().unwrap().contains("升级 Agent")
                && slave.state().await?["jobs"] == json!([]),
            "旧 Agent 能发布限制策略"
        );
    }
    forward(
        master,
        remote,
        &format!("/access/{}", l["id"].as_str().unwrap()),
        "POST",
        Some(json!({})),
        200,
    )
    .await?;
    ensure!(
        forward(
            master,
            remote,
            &format!("/access/{}", l["id"].as_str().unwrap()),
            "GET",
            None,
            200
        )
        .await?["denied_ports"]
            == json!([]),
        "远端清除策略失败"
    );
    let links = forward(
        master,
        remote,
        "/externals/batch",
        "POST",
        Some(json!({"urls":["socks5://same.example:1080","socks5://same.example:1081"]})),
        200,
    )
    .await?;
    let ids = links
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].clone())
        .collect::<Vec<_>>();
    ensure!(
        forward(
            master,
            remote,
            "/resources/candidates",
            "POST",
            Some(json!({"ids":ids})),
            200
        )
        .await?
        .as_array()
        .unwrap()
        .len()
            == 1,
        "远端候选分组错误"
    );
    forward(
        master,
        remote,
        "/resources/groups",
        "POST",
        Some(json!({"ids":ids,"name":"remote-group","automatic":true})),
        200,
    )
    .await?;
    let mut resource = forward(master, remote, "/resources", "GET", None, 200).await?["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "remote-group")
        .context("远端资源组缺失")?
        .clone();
    resource.as_object_mut().unwrap().extend(json!({"monthly_gib":10,"mbps":100,"reset_day":21,"reset_hour":3,"timezone":"America/Los_Angeles"}).as_object().unwrap().clone());
    forward(
        master,
        remote,
        "/resources",
        "POST",
        Some(resource.clone()),
        200,
    )
    .await?;
    ensure!(
        slave.request("/api/resources", "GET", None, 200).await?["resources"][0]["mbps"] == 100
            && master.request("/api/resources", "GET", None, 200).await?["resources"] == json!([]),
        "资源组数据隔离失败"
    );
    ensure!(
        forward(
            master,
            remote,
            "/resources/candidates",
            "POST",
            Some(json!({"ids":ids})),
            200
        )
        .await?
        .as_array()
        .unwrap()
        .is_empty(),
        "已分组资源仍是候选"
    );
    forward(
        master,
        remote,
        "/resources/groups",
        "POST",
        Some(json!({"ids":ids,"automatic":false})),
        200,
    )
    .await?;
    forward(
        master,
        remote,
        &format!("/resources/{}", resource["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    let sources = forward(master, remote, "/rules", "GET", None, 200).await?;
    ensure!(
        sources.as_array().unwrap().len() == 55,
        "规则预设数量改变，请核对迁移覆盖"
    );
    futures_util::stream::iter(sources.as_array().unwrap().iter().map(|s| async move {
        forward(
            master,
            remote,
            &format!("/rules/{}/update", s["id"].as_str().unwrap()),
            "POST",
            Some(json!({})),
            200,
        )
        .await
        .map(|_| ())
    }))
    .buffer_unordered(4)
    .try_collect::<Vec<_>>()
    .await?;
    let sources = forward(master, remote, "/rules", "GET", None, 200).await?;
    for s in sources.as_array().unwrap() {
        ensure!(
            s["pending"]["count"].as_u64().unwrap_or(0) > 0,
            "预设规则下载失败：{} {}",
            s["name"],
            s["error"]
        );
        forward(
            master,
            remote,
            &format!("/rules/{}/apply", s["id"].as_str().unwrap()),
            "POST",
            Some(json!({})),
            200,
        )
        .await?;
    }
    ensure!(
        forward(master, remote, "/rules", "GET", None, 200)
            .await?
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["active"]["count"].as_u64().unwrap_or(0) > 0),
        "预设规则未全部生效"
    );
    let custom = forward(
        master,
        remote,
        "/rules",
        "POST",
        Some(json!({"name":"remote-custom","url":sources[0]["url"],"auto_apply":false})),
        200,
    )
    .await?;
    for action in ["update", "apply"] {
        forward(
            master,
            remote,
            &format!("/rules/{}/{action}", custom["id"].as_str().unwrap()),
            "POST",
            Some(json!({})),
            200,
        )
        .await?;
    }
    forward(master,remote,"/rules","POST",Some(json!({"id":custom["id"],"name":"edited-custom","url":sources[0]["url"],"auto_apply":true})),200).await?;
    forward(master,remote,&format!("/access/{}",l["id"].as_str().unwrap()),"POST",Some(json!({"restricted":true,"sources":["preset-danime","preset-googlesearch"],"udp":"deny"})),200).await?;
    ensure!(
        forward(
            master,
            remote,
            &format!("/access/{}", l["id"].as_str().unwrap()),
            "GET",
            None,
            200
        )
        .await?["restricted"]
            == true
            && master
                .request("/api/rules", "GET", None, 200)
                .await?
                .as_array()
                .unwrap()
                .len()
                == 55,
        "规则策略未保存或污染主控"
    );
    forward(
        master,
        remote,
        "/security/token",
        "POST",
        Some(json!({})),
        403,
    )
    .await?;
    forward(
        master,
        remote,
        &format!("/agent/{}/resources", node["id"].as_str().unwrap()),
        "POST",
        Some(json!({})),
        403,
    )
    .await?;
    if std::env::var_os("CHAIN_TEST_BROWSER").is_some() {
        master
            .work
            .run(
                &repository(),
                &strings(&[
                    "node",
                    repository()
                        .join("panel/tests/sharing-ui.cjs")
                        .to_str()
                        .unwrap(),
                    &master.base,
                    master.root.join("control/admin-token").to_str().unwrap(),
                    remote,
                ]),
            )
            .await?;
    }
    forward(
        master,
        remote,
        &format!("/lines/{}", l["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    for l in links.as_array().unwrap() {
        forward(
            master,
            remote,
            &format!("/externals/{}", l["id"].as_str().unwrap()),
            "DELETE",
            None,
            200,
        )
        .await?;
    }
    println!("PASS 舰队共享权限、远端资源分组、55 个真实 HTTPS 预设解析与数据隔离");
    Ok(())
}
