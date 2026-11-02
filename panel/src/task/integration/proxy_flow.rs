use super::{
    super::workspace::Workspace,
    business::line,
    harness::{Harness, map},
};
use crate::ctl::{https::proxy_acme_config, upgrade::Manager};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

pub async fn run(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "proxy-flow")?;
    let (_, cert, key) = h.tls("localhost").await?;
    let data = h.root.join("control");
    fs::create_dir(&data)?;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700))?;
    fs::write(data.join(".chain-owner"), "chain-panel")?;
    let id = uuid::Uuid::new_v4().to_string();
    let token = uuid::Uuid::new_v4().simple().to_string();
    fs::write(data.join("local-system-node"), &id)?;
    fs::write(
        data.join("local-system-identity.json"),
        serde_json::to_vec(
            &json!({"id":id,"token_hash":hex::encode(Sha256::digest(token.as_bytes()))}),
        )?,
    )?;
    let token_file = h.root.join("token");
    fs::write(&token_file, &token)?;
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600))?;
    let web = h.root.join("web");
    fs::create_dir(&web)?;
    let nginx_port = h.port()?;
    let manager = Manager {
        root: h.root.clone(),
        base: h.root.join("opt/chain"),
    };
    let site = proxy_acme_config(&manager)
        .replace("listen 80;", &format!("listen 127.0.0.1:{nginx_port};"))
        .replace(
            h.root.join("var/lib/chain-acme").to_str().unwrap(),
            web.to_str().unwrap(),
        );
    let mut conf = format!(
        "pid {}/nginx.pid;\nerror_log {}/nginx-error.log;\nlock_file {}/nginx.lock;\nevents {{}}\nhttp {{ access_log off;\n",
        h.root.display(),
        h.root.display(),
        h.root.display()
    );
    for name in ["client_body", "proxy", "fastcgi", "uwsgi", "scgi"] {
        conf.push_str(&format!("{name}_temp_path {}/{name};\n", h.root.display()));
    }
    conf.push_str(&site);
    conf.push_str("}\n");
    fs::write(h.root.join("nginx.conf"), conf)?;
    h.launch(
        "nginx",
        Path::new("nginx"),
        Default::default(),
        &super::super::strings(&[
            "-p",
            h.root.to_str().unwrap(),
            "-c",
            h.root.join("nginx.conf").to_str().unwrap(),
            "-e",
            h.root.join("nginx-error.log").to_str().unwrap(),
            "-g",
            "daemon off; master_process off;",
        ]),
    )?;
    let challenge = web.join("agent/.well-known/acme-challenge/owned-probe");
    fs::create_dir_all(challenge.parent().unwrap())?;
    fs::write(&challenge, "owned-challenge")?;
    super::scenarios::wait("共享 Nginx HTTP-01 路由", async || {
        Ok(
            match reqwest::get(format!(
                "http://127.0.0.1:{nginx_port}/.well-known/acme-challenge/owned-probe"
            ))
            .await
            {
                Ok(r) => r.text().await.is_ok_and(|v| v == "owned-challenge"),
                Err(_) => false,
            },
        )
    })
    .await?;
    fs::remove_file(challenge)?;
    let http = h.http(b"chain-integration-ok".to_vec()).await?;
    h.allow_loopback(&[http])?;
    h.panel(map(&[("SSL_CERT_FILE", cert.display().to_string())]))
        .await?;
    ensure!(
        h.state().await?["local_node_id"].is_null(),
        "损坏本地映射未待恢复"
    );
    ensure!(
        h.request(
            "/api/local-node",
            "POST",
            Some(&json!({"host":"127.0.0.1"})),
            200
        )
        .await?["id"]
            == id,
        "恢复本机身份改变 ID"
    );
    let live = h.root.join("agent/acme/config/live/localhost");
    fs::create_dir_all(&live)?;
    fs::write(h.root.join("agent/.chain-owner"), "chain-agent")?;
    fs::copy(&cert, live.join("fullchain.pem"))?;
    fs::copy(&key, live.join("privkey.pem"))?;
    h.launch(
        "agent",
        &h.bin.join("chain-agent"),
        map(&[
            (
                "CHAIN_AGENT_DIR",
                h.root.join("agent").display().to_string(),
            ),
            ("CHAIN_PANEL_URL", h.base.clone()),
            ("CHAIN_NODE_ID", id.clone()),
            ("CHAIN_NODE_TOKEN_FILE", token_file.display().to_string()),
            ("CHAIN_XRAY_BIN", h.xray.display().to_string()),
            (
                "CHAIN_ACME_WEBROOT",
                web.join("agent").display().to_string(),
            ),
            ("SSL_CERT_FILE", cert.display().to_string()),
        ]),
        &[],
    )?;
    h.wait_state("原身份 Agent 恢复", 30, |s| {
        s["nodes"][0]["online"] == true
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
            == id
            && h.state().await?["nodes"].as_array().unwrap().len() == 1,
        "恢复产生重复节点"
    );
    let nodes = vec![json!({"id":id})];
    let tls = json!({"mode":"auto","domain":"localhost"});
    let access = json!({"protocol":"vless","host":"127.0.0.1","tls":tls});
    let a = line(&h, &nodes, "Landing A", &[0], json!({"access":access}), 200).await?;
    let b = line(&h, &nodes, "Landing B", &[0], json!({"access":access}), 200).await?;
    h.publish("applied").await?;
    let url = format!("http://127.0.0.1:{http}/");
    h.probe(&a, &url).await?;
    let mut externals = vec![];
    for l in [&a, &b] {
        let exported = h
            .request(
                &format!("/api/lines/{}/export", l["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        let mut proxy: Value = serde_json::from_str(
            exported["node"]
                .as_str()
                .unwrap()
                .trim()
                .strip_prefix("- ")
                .unwrap(),
        )?;
        proxy["skip-cert-verify"] = json!(false);
        let external = h
            .request(
                "/api/externals",
                "POST",
                Some(&json!({"url":format!("- {}",serde_json::to_string(&proxy)?)})),
                200,
            )
            .await?;
        let both = h
            .request(
                &format!("/api/externals/{}/export", external["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?;
        ensure!(
            both["share"].as_str().unwrap().starts_with("vless://")
                && serde_json::from_str::<Value>(
                    both["node"]
                        .as_str()
                        .unwrap()
                        .trim()
                        .strip_prefix("- ")
                        .unwrap()
                )?["skip-cert-verify"]
                    == false,
            "导入导出丢失证书验证选项"
        );
        externals.push(external);
    }
    let chained=line(&h,&nodes,"Ordered A B",&[0],json!({"access":{"protocol":"http","host":"127.0.0.1"},"chain":externals.iter().map(|e|format!("external:{}",e["id"].as_str().unwrap())).collect::<Vec<_>>()}),200).await?;
    let local=line(&h,&nodes,"Local reference",&[0],json!({"access":{"protocol":"socks","host":"127.0.0.1"},"chain":[format!("line:{}",a["id"].as_str().unwrap())]}),200).await?;
    h.publish("applied").await?;
    let state = h.state().await?;
    let used = |l: &Value| {
        l["traffic"]["upload"].as_u64().unwrap_or(0)
            + l["traffic"]["download"].as_u64().unwrap_or(0)
    };
    let before = state["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["id"] == a["id"] || l["id"] == b["id"])
        .map(|l| (l["id"].clone(), used(l)))
        .collect::<Vec<_>>();
    h.probe(&chained, &url).await?;
    h.probe(&local, &url).await?;
    h.wait_state("有序两个落地流量", 30, |s| {
        before.iter().all(|(id, total)| {
            s["lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| &l["id"] == id && used(l) > *total)
        })
    })
    .await?;
    h.request(
        &format!("/api/externals/{}", externals[0]["id"].as_str().unwrap()),
        "DELETE",
        None,
        400,
    )
    .await?;
    h.request(
        &format!("/api/lines/{}", a["id"].as_str().unwrap()),
        "DELETE",
        None,
        400,
    )
    .await?;
    for (field, value) in [
        (
            "chain",
            json!([format!("line:{}", local["id"].as_str().unwrap())]),
        ),
        ("kind", json!("forward")),
        ("access", {
            let mut access = a["access"].clone();
            access["public_host"] = json!("relay.example");
            access
        }),
    ] {
        let mut invalid = a.clone();
        invalid[field] = value;
        h.request("/api/lines", "POST", Some(&invalid), 400).await?;
    }
    h.close().await?;
    println!("PASS 旧本机身份恢复、共享 HTTP-01、自动证书复用、有序出口真实流量与循环/删除保护");
    Ok(())
}
