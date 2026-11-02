use super::{
    super::{Options, strings, workspace::Workspace},
    harness::{Harness, map},
    protocols,
    scenarios::wait,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

fn client(cert: &Path, name: &str, port: u16, h2: bool) -> Result<reqwest::Client> {
    ensure!(!h2, "H2 使用 curl 的真实 ALPN 验收");
    Ok(reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&fs::read(cert)?)?)
        .resolve(name, ([127, 0, 0, 1], port).into())
        .timeout(Duration::from_secs(8))
        .build()?)
}
async fn page(cert: &Path, name: &str, port: u16, h2: bool) -> Result<(String, reqwest::Version)> {
    let r = client(cert, name, port, h2)?
        .get(format!("https://{name}:{port}/"))
        .send()
        .await?
        .error_for_status()?;
    let v = r.version();
    Ok((r.text().await?, v))
}
async fn h2_page(h: &Harness<'_>, cert: &Path, name: &str, port: u16) -> Result<String> {
    let body = h
        .work
        .output(
            &h.root,
            &strings(&[
                "curl",
                "--silent",
                "--show-error",
                "--fail",
                "--noproxy",
                "*",
                "--max-time",
                "10",
                "--http2",
                "--cacert",
                cert.to_str().unwrap(),
                "--resolve",
                &format!("{name}:{port}:127.0.0.1"),
                "--write-out",
                "\n%{http_version}",
                &format!("https://{name}:{port}/"),
            ]),
        )
        .await?;
    Ok(body
        .strip_suffix("\n2")
        .context("网页回落未协商 H2")?
        .into())
}
pub async fn run(work: &Workspace, stage: &Path, _: &Options) -> Result<()> {
    let mut h = Harness::new(work, stage, "fronting")?;
    let cert = h.root.join("cert.pem");
    let key = h.root.join("key.pem");
    work.run(
        &h.root,
        &strings(&[
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
            "-days",
            "90",
            "-subj",
            "/CN=reality.test",
            "-addext",
            "subjectAltName=DNS:reality.test,DNS:tls.test,DNS:existing.test",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-out",
            cert.to_str().unwrap(),
            "-keyout",
            key.to_str().unwrap(),
        ]),
    )
    .await?;
    let (tls, existing, public) = (h.port()?, h.port()?, h.port()?);
    let http = h.http(b"chain-integration-ok".to_vec()).await?;
    let udp = h.echo().await?;
    h.allow_loopback(&[http, udp])?;
    let config = h.root.join("system/etc/chain-ingress");
    let data = h.root.join("system/var/lib/chain-ingress");
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&data)?;
    fs::write(data.join("state.json"), br#"{"blocked":["existing.test"]}"#)?;
    fs::write(
        data.join("status.json"),
        br#"{"ready":true,"applied":true,"id":"","error":""}"#,
    )?;
    let routes = config.join("routes.conf");
    fs::write(&routes, "# chain-ingress managed v1\n")?;
    let module = std::env::var("CHAIN_TEST_STREAM_MODULE")
        .context("SNI 路由验收需要 CHAIN_TEST_STREAM_MODULE")?;
    ensure!(
        Path::new(&module).is_absolute() && Path::new(&module).is_file(),
        "stream 模块路径无效"
    );
    let root = h.root.display();
    let body = format!(
        r#"load_module {module};
pid {root}/nginx.pid;
lock_file {root}/nginx.lock;
worker_processes 1;
error_log {root}/nginx-error.log info;
events {{ worker_connections 1024; }}
http {{
 access_log off;
 client_body_temp_path {root}/client_body;
 proxy_temp_path {root}/proxy;
 fastcgi_temp_path {root}/fastcgi;
 uwsgi_temp_path {root}/uwsgi;
 scgi_temp_path {root}/scgi;
 ssl_certificate {cert}; ssl_certificate_key {key}; ssl_protocols TLSv1.2 TLSv1.3;
 server {{ listen 127.0.0.1:{tls} ssl; http2 on; server_name reality.test; location / {{ return 200 'target-ok'; }} }}
 server {{ listen 127.0.0.1:{existing} ssl proxy_protocol; server_name existing.test;
  real_ip_header proxy_protocol; set_real_ip_from 127.0.0.1;
  location / {{ return 200 'existing-site $remote_addr'; }} }}
}}
stream {{
 map $ssl_preread_server_name $destination {{ hostnames; default 127.0.0.1:{existing}; include {routes}; }}
 server {{ listen 127.0.0.1:{public}; ssl_preread on; proxy_pass $destination; proxy_protocol on; }}
}}
"#,
        cert = cert.display(),
        key = key.display(),
        routes = routes.display()
    );
    let nginx_conf = h.root.join("nginx.conf");
    fs::write(&nginx_conf, body)?;
    let nginx = h.launch(
        "nginx",
        Path::new("nginx"),
        Default::default(),
        &strings(&[
            "-c",
            nginx_conf.to_str().unwrap(),
            "-p",
            h.root.to_str().unwrap(),
            "-g",
            "daemon off;",
        ]),
    )?;
    wait("已有 HTTPS 与 PROXY 来源地址", async || {
        Ok(page(&cert, "existing.test", public, false)
            .await
            .is_ok_and(|v| v.0 == "existing-site 127.0.0.1"))
    })
    .await?;
    ensure!(
        h2_page(&h, &cert, "reality.test", tls).await? == "target-ok",
        "TLS 目标 H2 内容错误"
    );
    h.panel(map(&[("SSL_CERT_FILE", cert.display().to_string())]))
        .await?;
    for name in ["reality.test", "wrong.test"] {
        let check = h
            .request(
                "/api/reality/check",
                "POST",
                Some(&json!({"target":format!("127.0.0.1:{tls}"),"server_name":name})),
                200,
            )
            .await?;
        ensure!(check["ok"] == false, "生产检查不应接受私网目标：{check}");
    }
    let n=h.request("/api/nodes","POST",Some(&json!({"name":"fronting","host":"127.0.0.1","target":format!("127.0.0.1:{tls}"),"server_name":"reality.test"})),200).await?;
    h.preflight_named(tls, "reality.test", true).await?;
    crate::runtime::private_dir(&h.root.join("agent-0"), "chain-agent")?;
    h.launch(
        "ingress-watcher",
        &std::env::current_exe()?,
        Default::default(),
        &strings(&[
            "integration",
            "--case",
            "ingress-watcher",
            "--input",
            h.root.to_str().unwrap(),
            "--",
            &nginx.to_string(),
        ]),
    )?;
    h.agent(
        0,
        &n,
        map(&[
            ("SSL_CERT_FILE", cert.display().to_string()),
            (
                "CHAIN_INGRESS_STATUS",
                data.join("status.json").display().to_string(),
            ),
        ]),
    )
    .await?;
    h.wait_state("SNI 路由 Agent", 30, |s| s["nodes"][0]["online"] == true)
        .await?;
    let tls_settings = json!({"mode":"manual","domain":"tls.test","cert":fs::read_to_string(&cert)?,"key":fs::read_to_string(&key)?});
    let mut inputs = vec![];
    for (network, security) in [
        ("tcp", "reality"),
        ("tcp", "tls"),
        ("xhttp", "reality"),
        ("xhttp", "tls"),
    ] {
        inputs.push(json!({"name":format!("{network} {security}"),"kind":"vless","hops":[{"node_id":n["id"],"port":h.port()?}],"access":{"protocol":"vless","host":"127.0.0.1","transport":{"network":network,"path":"/channel","host":"","mode":"auto"},"reality":{"target":format!("127.0.0.1:{tls}"),"server_name":"reality.test"},"tls":if security=="tls"{tls_settings.clone()}else{json!({"mode":"off"})},"fallback":if network=="tcp"&&security=="tls"{"builtin"}else{""}}}));
    }
    let before = h.state().await?["lines"].clone();
    let mut bad = inputs[0].clone();
    bad["hops"][0]["node_id"] = json!("missing");
    h.request(
        "/api/lines/batch",
        "POST",
        Some(&json!([inputs[0], bad])),
        400,
    )
    .await?;
    ensure!(
        h.state().await?["lines"] == before,
        "批量失败仍保存部分线路"
    );
    let mut lines = h
        .request("/api/lines/batch", "POST", Some(&json!(inputs)), 200)
        .await?
        .as_array()
        .context("批量返回无效")?
        .clone();
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
        let node: Value =
            serde_json::from_str(export["node"].as_str().unwrap().strip_prefix("- ").unwrap())?;
        if l["access"]["transport"]["network"] == "xhttp" {
            ensure!(
                node.get("flow").is_none()
                    && !url::Url::parse(export["share"].as_str().unwrap())?
                        .query_pairs()
                        .any(|(k, _)| k == "flow")
                    && node["xhttp-opts"]["path"] == "/channel",
                "XHTTP 导出错误"
            );
        }
        for key in ["share", "node"] {
            h.request(
                "/api/externals",
                "POST",
                Some(&json!({"url":export[key]})),
                200,
            )
            .await?;
        }
    }
    protocols::mihomo(&h, &lines, http, Some(udp), &cert).await?;
    let port = lines[1]["hops"][0]["port"].as_u64().unwrap() as u16;
    ensure!(
        page(&cert, "tls.test", port, false)
            .await?
            .0
            .contains("欢迎访问"),
        "TLS 网页回落丢失"
    );
    ensure!(
        h2_page(&h, &cert, "tls.test", port)
            .await?
            .contains("欢迎访问"),
        "网页 H2 回落内容错误"
    );
    for index in [0, 1] {
        let mut input = inputs[index].clone();
        input["id"] = lines[index]["id"].clone();
        input["hops"][0]["port"] = json!(443);
        input["access"]["shared_port"] = json!(true);
        if index == 1 {
            input["access"]["fallback"] = json!(format!("http://127.0.0.1:{http}"));
        }
        lines[index] = h.request("/api/lines", "POST", Some(&input), 200).await?;
    }
    h.publish("applied").await?;
    ensure!(
        page(&cert, "existing.test", public, false).await?.0 == "existing-site 127.0.0.1",
        "共享端口影响已有站点"
    );
    ensure!(
        h2_page(&h, &cert, "tls.test", public).await? == "chain-integration-ok",
        "共享端口 H2 回落内容错误"
    );
    protocols::mihomo_routes(&h, &lines[..2], http, Some(udp), &cert, Some(public)).await?;
    let before = fs::read(&routes)?;
    let mut collision = inputs[1].clone();
    collision["name"] = json!("collision");
    collision["hops"][0]["port"] = json!(443);
    collision["access"]["shared_port"] = json!(true);
    collision["access"]["tls"]["domain"] = json!("existing.test");
    let rejected = h
        .request("/api/lines", "POST", Some(&collision), 200)
        .await?;
    h.publish("rolled_back").await?;
    ensure!(
        fs::read(&routes)? == before
            && page(&cert, "existing.test", public, false).await?.0 == "existing-site 127.0.0.1",
        "回滚改变既有站点或路由"
    );
    h.request(
        &format!("/api/lines/{}", rejected["id"].as_str().unwrap()),
        "DELETE",
        None,
        200,
    )
    .await?;
    for l in &lines[..2] {
        h.request(
            &format!("/api/lines/{}", l["id"].as_str().unwrap()),
            "DELETE",
            None,
            200,
        )
        .await?;
    }
    h.publish("applied").await?;
    ensure!(
        fs::read_to_string(routes)? == "# chain-ingress managed v1\n\n"
            && page(&cert, "existing.test", public, false).await?.0 == "existing-site 127.0.0.1",
        "删除后路由或既有站点异常"
    );
    ensure!(
        !fs::read_to_string(h.root.join("nginx-error.log"))?.contains("Permission denied"),
        "Nginx 夹具权限错误"
    );
    h.close().await?;
    println!(
        "PASS 四种原样协议、XHTTP、H2 回落、批量原子性、Nginx SNI/PROXY、既有站点保护及回滚删除"
    );
    Ok(())
}
