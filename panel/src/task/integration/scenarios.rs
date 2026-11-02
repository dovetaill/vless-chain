use super::{
    super::{Options, strings, workspace::Workspace},
    harness::{Harness, map},
};
use crate::ctl::{package, upgrade::Manager};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

pub async fn wait(
    description: &str,
    mut condition: impl AsyncFnMut() -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if condition().await? {
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "等待 {description} 超时");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn local_install(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "local-install")?;
    let port = h.port()?;
    h.base = format!("http://127.0.0.1:{port}");
    let system = h.root.join("system");
    fs::create_dir(&system)?;
    let archive = h.root.join("native.tar.gz");
    super::super::release::archive(stage, &archive)?;
    fs::write(
        h.root.join("SHA256SUMS"),
        format!("{}  native.tar.gz\n", package::digest(&archive)?),
    )?;
    let mut command = strings(&[
        stage.join("bin/chainctl").to_str().unwrap(),
        "install",
        "--root",
        system.to_str().unwrap(),
        "--package",
        archive.to_str().unwrap(),
        "--work-parent",
        h.root.to_str().unwrap(),
        "--non-interactive",
        "--listen",
        &format!("127.0.0.1:{port}"),
        "--local-host",
        "127.0.0.1",
        "--local-target",
        "localhost:443",
    ]);
    command.push("--panel-only".into());
    work.run(&h.root, &command).await?;
    command.pop();
    let manager = Manager {
        root: system.clone(),
        base: system.join("opt/chain"),
    };
    let panel_env = manager.environment("panel")?;
    h.launch("local-panel", &h.bin.join("chain-panel"), panel_env, &[])?;
    h.ready().await?;
    work.run(&h.root, &command).await?;
    let agent_env = manager.environment("agent")?;
    h.launch("auto-agent", &h.bin.join("chain-agent"), agent_env, &[])?;
    h.login(&system.join("var/lib/chain-panel")).await?;
    h.wait_state("自动注册 Agent", 30, |s| {
        s["nodes"]
            .as_array()
            .is_some_and(|n| n.len() == 1 && n[0]["online"] == true)
    })
    .await?;
    let state = system.join("opt/chain/.chain-install.json");
    let previous = super::super::release::read(&state)?["roles"]["agent"].clone();
    let identity = fs::read(system.join("etc/chain-agent/token"))?;
    work.run(&h.root, &command).await?;
    ensure!(
        super::super::release::read(&state)?["roles"]["agent"] == previous
            && fs::read(system.join("etc/chain-agent/token"))? == identity,
        "重复安装改变 Agent 身份"
    );
    ensure!(
        system.join("usr/local/bin/chainctl").is_symlink()
            && h.state().await?["nodes"].as_array().unwrap().len() == 1,
        "重复安装未保持管理链接或节点唯一性"
    );
    #[cfg(target_os = "macos")]
    watcher(&h).await?;
    h.close().await?;
    println!("PASS 离线安装服务配置、真实本机注册、心跳、重复安装保留身份");
    Ok(())
}

pub fn public_key(private: &str) -> Result<String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let bytes: [u8; 32] = URL_SAFE_NO_PAD
        .decode(private)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("X25519 私钥长度无效"))?;
    Ok(URL_SAFE_NO_PAD
        .encode(x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(bytes)).as_bytes()))
}
pub async fn legacy(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "legacy-migration")?;
    let (tls, _, _) = h.tls("localhost").await?;
    let http = h.http(b"chain-integration-ok".to_vec()).await?;
    h.allow_loopback(&[http])?;
    h.panel(BTreeMap::new()).await?;
    let node=h.request("/api/nodes","POST",Some(&json!({"name":"migration","host":"127.0.0.1","target":format!("127.0.0.1:{tls}"),"server_name":"localhost"})),200).await?;
    h.preflight(tls, false).await?;
    h.agent(0, &node, BTreeMap::new()).await?;
    h.wait_state("迁移 Agent", 30, |s| s["nodes"][0]["online"] == true)
        .await?;
    let mut sources: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/native-v3.json"))?;
    let mut exits = vec![];
    for (index, source) in sources.as_array_mut().unwrap().iter_mut().enumerate() {
        let port = h.port()?;
        source["config"]["inbounds"][0]["port"] = json!(port);
        source["config"]["inbounds"][0]["streamSettings"]["realitySettings"]["target"] =
            json!(format!("127.0.0.1:{tls}"));
        source["config"]["inbounds"][0]["streamSettings"]["realitySettings"]["serverNames"] =
            json!(["localhost"]);
        if index > 0 {
            let mut inbound = source["config"]["inbounds"][0].clone();
            let exit = h.port()?;
            inbound["listen"] = json!("127.0.0.1");
            inbound["port"] = json!(exit);
            let upstream = &mut source["config"]["outbounds"][0];
            upstream["settings"]["address"] = json!("127.0.0.1");
            upstream["settings"]["port"] = json!(exit);
            inbound["settings"]["clients"][0]["id"] = upstream["settings"]["id"].clone();
            let reality = &inbound["streamSettings"]["realitySettings"];
            upstream["streamSettings"]["realitySettings"]["serverName"] = json!("localhost");
            upstream["streamSettings"]["realitySettings"]["publicKey"] =
                json!(public_key(reality["privateKey"].as_str().unwrap())?);
            upstream["streamSettings"]["realitySettings"]["shortId"] =
                reality["shortIds"][0].clone();
            let log = h.root.join(format!("exit-{index}-access.log"));
            h.launch_core(&format!("exit-{index}"),&json!({"log":{"access":log,"loglevel":"warning"},"inbounds":[inbound],"outbounds":[{"protocol":"freedom"}]}))?;
            exits.push(log);
        }
    }
    let mut request = json!({"node_id":node["id"],"sources":sources,"preview":true});
    let preview = h
        .request("/api/legacy-import", "POST", Some(&request), 200)
        .await?;
    ensure!(
        preview["imported"].as_array().unwrap().len() == 4 && preview["pending"] == json!([]),
        "原生实例预览错误：{preview}"
    );
    let state = h.state().await?;
    ensure!(
        state["lines"] == json!([]) && state["externals"] == json!([]),
        "预览改变状态"
    );
    request["preview"] = json!(false);
    ensure!(
        h.request("/api/legacy-import", "POST", Some(&request), 200)
            .await?
            == preview,
        "提交与预览不一致"
    );
    h.request("/api/legacy-import", "POST", Some(&request), 200)
        .await?;
    let state = h.state().await?;
    ensure!(
        state["lines"].as_array().unwrap().len() == 4
            && state["externals"].as_array().unwrap().len() == 3,
        "迁移重复或丢失实例"
    );
    let names = state["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(names.len() == 4, "迁移名称重复");
    let warning = h
        .request("/api/publish", "POST", Some(&json!({})), 400)
        .await?;
    ensure!(
        warning["error"]
            .as_str()
            .is_some_and(|s| s.contains("REALITY") && s.contains("草稿")),
        "未拒绝私网伪装目标：{warning}"
    );
    h.request(
        "/api/publish",
        "POST",
        Some(&json!({"allow_unverified_reality":true})),
        422,
    )
    .await?;
    ensure!(
        h.state().await?["jobs"] == state["jobs"]
            && h.state().await?["lines"].as_array().unwrap().len() == 4,
        "拒绝发布改变草稿或任务"
    );
    h.preflight
        .as_ref()
        .unwrap()
        .enabled
        .store(true, Ordering::SeqCst);
    h.publish("applied").await?;
    ensure!(
        h.preflight
            .as_ref()
            .unwrap()
            .intercepted
            .load(Ordering::SeqCst)
            > 0,
        "未执行真实预检请求"
    );
    for (index, (source, line)) in request["sources"]
        .as_array()
        .unwrap()
        .iter()
        .zip(state["lines"].as_array().unwrap())
        .enumerate()
    {
        let inbound = &source["config"]["inbounds"][0];
        let reality = &inbound["streamSettings"]["realitySettings"];
        ensure!(
            line["hops"][0]["uuid"] == inbound["settings"]["clients"][0]["id"]
                && line["hops"][0]["port"] == inbound["port"],
            "原始客户端凭据发生变化"
        );
        let port = h.port()?;
        let mut user = inbound["settings"]["clients"][0].clone();
        user["encryption"] = json!("none");
        let pid=h.launch_core(&format!("client-{index}"),&json!({"log":{"loglevel":"warning"},"inbounds":[{"listen":"127.0.0.1","port":port,"protocol":"socks","settings":{"auth":"noauth"}}],"outbounds":[{"protocol":"vless","settings":{"vnext":[{"address":"127.0.0.1","port":inbound["port"],"users":[user]}]},"streamSettings":{"network":"tcp","security":"reality","realitySettings":{"serverName":"localhost","publicKey":public_key(reality["privateKey"].as_str().unwrap())?,"shortId":reality["shortIds"][0],"fingerprint":"chrome"}}}]}))?;
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))?)
            .timeout(Duration::from_secs(3))
            .build()?;
        let traffic = wait("原始客户端真实流量", async || {
            Ok(
                match client.get(format!("http://127.0.0.1:{http}/")).send().await {
                    Ok(r) => r
                        .bytes()
                        .await
                        .is_ok_and(|v| v == b"chain-integration-ok".as_slice()),
                    Err(_) => false,
                },
            )
        })
        .await;
        if let Err(error) = traffic {
            let mut logs = String::new();
            for entry in fs::read_dir(&h.root)? {
                let path = entry?.path();
                if path.extension().is_some_and(|v| v == "log") {
                    let bytes = fs::read(&path)?;
                    logs.push_str(&format!("\n{}:\n{}", path.display(),
                        String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(8192)..])));
                }
            }
            return Err(error.context(format!("迁移线路 {index}{logs}")));
        }
        if index > 0 {
            wait("外部出口流量日志", async || {
                Ok(fs::read_to_string(&exits[index - 1])
                    .is_ok_and(|s| s.contains(&format!("127.0.0.1:{http}"))))
            })
            .await?;
        }
        work.stop(pid).await?;
    }
    let mut bad = request.clone();
    bad["preview"] = json!(true);
    bad["sources"] = json!([request["sources"][1]]);
    bad["sources"][0]["id"] = json!(format!(
        "{}:unsupported",
        bad["sources"][0]["id"].as_str().unwrap()
    ));
    bad["sources"][0]["config"]["outbounds"][0]["mux"] = json!({"enabled":true});
    let result = h
        .request("/api/legacy-import", "POST", Some(&bad), 200)
        .await?;
    ensure!(
        result["pending"].as_array().unwrap().len() == 1 && result["imported"] == json!([]),
        "不支持配置未保留待处理"
    );
    ensure!(
        h.state().await?["externals"].as_array().unwrap().len() == 3
            && h.state().await?["pending_imports"] == json!([]),
        "不支持配置预览改变状态"
    );
    h.close().await?;
    println!("PASS 四个原生实例、三个外部出口、原客户端真实流量及预览/重试幂等");
    Ok(())
}

pub async fn https(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "https-install")?;
    h.panel(map(&[("CHAIN_SECURE_COOKIE", "1".into())])).await?;
    let upstream = h
        .base
        .strip_prefix("http://127.0.0.1:")
        .unwrap()
        .parse::<u16>()?;
    let http = h.port()?;
    let https = h.port()?;
    let (_, cert, key) = h.tls("localhost").await?;
    let webroot = h.root.join("webroot");
    fs::create_dir_all(webroot.join(".well-known/acme-challenge"))?;
    fs::write(
        webroot.join(".well-known/acme-challenge/probe"),
        "owned-challenge",
    )?;
    let manager = Manager {
        root: h.root.clone(),
        base: h.root.join("opt/chain"),
    };
    let site = crate::ctl::https::site(&manager, "panel.test", upstream, true)
        .replace(
            h.root
                .join("etc/letsencrypt/live/chain-panel-panel.test/fullchain.pem")
                .to_str()
                .unwrap(),
            cert.to_str().unwrap(),
        )
        .replace(
            h.root
                .join("etc/letsencrypt/live/chain-panel-panel.test/privkey.pem")
                .to_str()
                .unwrap(),
            key.to_str().unwrap(),
        )
        .replace(
            h.root.join("var/lib/chain-acme").to_str().unwrap(),
            webroot.to_str().unwrap(),
        )
        .replace("panel.test", "localhost")
        .replace("listen 80;", &format!("listen 127.0.0.1:{http};"))
        .replace("listen 443 ssl;", &format!("listen 127.0.0.1:{https} ssl;"));
    let mut body = format!(
        "pid {}/nginx.pid;\nerror_log {}/error.log;\nlock_file {}/nginx.lock;\nevents {{}}\nhttp {{ access_log off;\n",
        h.root.display(),
        h.root.display(),
        h.root.display()
    );
    for name in ["client_body", "proxy", "fastcgi", "uwsgi", "scgi"] {
        body.push_str(&format!("{name}_temp_path {}/{name};\n", h.root.display()));
    }
    body.push_str(&site);
    body.push_str("}\n");
    let conf = h.root.join("nginx.conf");
    fs::write(&conf, body)?;
    h.launch(
        "nginx",
        Path::new("nginx"),
        BTreeMap::new(),
        &strings(&[
            "-p",
            h.root.to_str().unwrap(),
            "-c",
            conf.to_str().unwrap(),
            "-g",
            "daemon off; master_process off; error_log stderr;",
        ]),
    )?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&fs::read(cert)?)?)
        .timeout(Duration::from_secs(3))
        .build()?;
    let base = format!("https://localhost:{https}");
    wait("HTTPS 完整页面", async || {
        Ok(match client.get(&base).send().await {
            Ok(r) => r
                .text()
                .await
                .is_ok_and(|v| v.to_lowercase().contains("<!doctype html>")),
            Err(_) => false,
        })
    })
    .await?;
    ensure!(
        client
            .get(format!(
                "http://127.0.0.1:{http}/.well-known/acme-challenge/probe"
            ))
            .send()
            .await?
            .text()
            .await?
            == "owned-challenge",
        "HTTP-01 路由错误"
    );
    let response = client
        .post(format!("{base}/api/login"))
        .header("X-Chain-Request", "1")
        .header("Origin", &base)
        .json(&json!({"token":fs::read_to_string(h.root.join("control/admin-token"))?}))
        .send()
        .await?
        .error_for_status()?;
    let cookie = response
        .headers()
        .get("set-cookie")
        .context("缺少登录 cookie")?
        .to_str()?
        .to_string();
    ensure!(cookie.contains("Secure"), "登录 cookie 缺少 Secure");
    client
        .get(format!("{base}/api/state"))
        .header("X-Chain-Request", "1")
        .header("Cookie", cookie.split(';').next().unwrap())
        .send()
        .await?
        .error_for_status()?;
    let html = client.get(&base).send().await?.text().await?;
    for asset in html
        .split('"')
        .filter(|v| v.starts_with("/assets/") && (v.ends_with(".js") || v.ends_with(".css")))
    {
        let direct = reqwest::get(format!("{}{asset}", h.base))
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let proxied = client
            .get(format!("{base}{asset}"))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        ensure!(
            !direct.is_empty() && proxied == direct,
            "静态资源传输不完整：{asset}"
        );
    }
    ensure!(
        !fs::read_to_string(h.root.join("error.log"))?.contains("Permission denied"),
        "Nginx 出现权限错误"
    );
    h.close().await?;
    println!("PASS 真实 HTTPS、可信证书、安全 cookie、API、HTTP-01 和完整静态资源");
    Ok(())
}

pub async fn customer_plans(work: &Workspace, stage: &Path) -> Result<()> {
    super::plans::run(work, stage).await
}
pub async fn terminal_checks(work: &Workspace, ctl: &Path, root: &Path) -> Result<()> {
    super::fixtures::terminal_checks(work, ctl, root).await
}
pub async fn run(work: &Workspace, stage: &Path, case: &str, args: &Options) -> Result<()> {
    if case == "delivery-checks" {
        return super::delivery::run(work).await;
    }
    if case == "ssh-manager" {
        let bin = std::env::var_os("CHAIN_TEST_BIN_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| stage.join("bin"));
        return super::ssh::run(work, &bin).await;
    }
    if case == "load-protocols" {
        return super::load::run(work, stage, args).await;
    }
    super::business::run(work, stage, case, args).await
}
#[cfg(target_os = "macos")]
async fn watcher(h: &Harness<'_>) -> Result<()> {
    super::fixtures::watcher(h).await
}
