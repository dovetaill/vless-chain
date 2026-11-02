use super::{
    super::workspace::Workspace,
    business::{line, node},
    harness::{Harness, map},
    plans::{echo, socks},
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::{path::Path, time::Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn run(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "sharing")?;
    let target = h.echo().await?;
    h.allow_loopback(&[target])?;
    h.panel(map(&[("CHAIN_APPLY_TIMEOUT", "9".into())])).await?;
    let node = node(&h, "sharing", None).await?;
    h.agent(
        0,
        &node,
        map(&[(
            "CHAIN_INGRESS_STATUS",
            h.root.join("no-ingress").display().to_string(),
        )]),
    )
    .await?;
    h.wait_state("共享策略 Agent", 30, |s| {
        s["nodes"][0]["online"] == true
    })
    .await?;
    let line = line(
        &h,
        &[node.clone()],
        "sharing",
        &[0],
        json!({"access":{"protocol":"socks"}}),
        200,
    )
    .await?;
    h.publish("applied").await?;
    h.synced(&[line.clone()]).await?;
    echo(&line, target, b"forwarded").await?;
    let path = format!("/api/access/{}", line["id"].as_str().unwrap());
    for policy in [
        json!({"denied_ports":[target]}),
        json!({"restricted":true,"domains":["example.com"]}),
    ] {
        h.request(&path, "POST", Some(&policy), 200).await?;
        h.synced(&[line.clone()]).await?;
        ensure!(
            echo(&line, target, b"must-be-denied").await.is_err(),
            "禁止目标仍转发"
        );
    }
    h.request(&path, "POST", Some(&json!({"max_connections":1})), 200)
        .await?;
    h.synced(&[line.clone()]).await?;
    let (mut held, _) = socks(&line, target, 1, None).await?;
    held.write_all(b"held").await?;
    let mut bytes = [0; 4];
    held.read_exact(&mut bytes).await?;
    ensure!(
        &bytes == b"held" && echo(&line, target, b"second").await.is_err(),
        "连接上限未执行"
    );
    drop(held);
    h.request(&path, "POST", Some(&json!({})), 200).await?;
    h.synced(&[line.clone()]).await?;
    echo(&line, target, b"recovered").await?;
    let links=h.request("/api/externals/batch","POST",Some(&json!({"urls":["socks5://u:p@same.example.com:1001#one","socks5://u:p@same.example.com:1002#two"]})),200).await?;
    let candidates = h
        .request("/api/resources/candidates", "POST", Some(&json!({})), 200)
        .await?;
    ensure!(
        candidates.as_array().unwrap().len() == 1
            && candidates[0]["ids"].as_array().unwrap().len() == 2,
        "批量导入未正确分组"
    );
    h.request(
        "/api/resources/groups",
        "POST",
        Some(&json!({"ids":[links[0]["id"]],"name":"shared","automatic":true})),
        200,
    )
    .await?;
    let candidates = h
        .request("/api/resources/candidates", "POST", Some(&json!({})), 200)
        .await?;
    ensure!(
        candidates[0]["ids"] == json!([links[1]["id"]])
            && candidates[0]["matching"].as_array().unwrap().len() == 1,
        "已有分组匹配丢失"
    );
    let group = candidates[0]["matching"][0]["id"].clone();
    h.request(
        "/api/resources/groups",
        "POST",
        Some(&json!({"ids":[links[0]["id"]],"group":group,"automatic":true})),
        400,
    )
    .await?;
    h.request(
        "/api/resources/groups",
        "POST",
        Some(&json!({"ids":[links[1]["id"]],"group":group,"automatic":true})),
        200,
    )
    .await?;
    ensure!(
        h.request("/api/resources/candidates", "POST", Some(&json!({})), 200)
            .await?
            == json!([]),
        "分组后仍出现候选"
    );
    h.request(
        "/api/resources/groups",
        "POST",
        Some(&json!({"ids":[links[0]["id"]],"automatic":false})),
        200,
    )
    .await?;
    ensure!(
        h.request("/api/resources/candidates", "POST", Some(&json!({})), 200)
            .await?[0]["ids"]
            == json!([links[0]["id"]]),
        "移出分组后候选未恢复"
    );
    let mut resource=h.request("/api/resources","POST",Some(&json!({"name":"tiny","members":[format!("node:{}",node["id"].as_str().unwrap())],"monthly_gib":128.0/1073741824.0,"mbps":0.008,"reset_day":21,"reset_hour":0,"timezone":"America/Los_Angeles"})),200).await?;
    h.synced(&[line.clone()]).await?;
    echo(&line, target, &[b'a'; 32]).await?;
    echo(&line, target, &[b'b'; 32]).await?;
    ensure!(
        echo(&line, target, b"over-quota").await.is_err(),
        "资源额度未限制真实流量"
    );
    resource["monthly_gib"] = json!(4096.0 / 1073741824.0);
    h.request("/api/resources", "POST", Some(&resource), 200)
        .await?;
    let start = Instant::now();
    echo(&line, target, &[b'c'; 512]).await?;
    ensure!(
        start.elapsed().as_secs_f64() >= 0.8,
        "共享带宽未计入双向流量"
    );
    h.close().await?;
    println!("PASS 访问策略、连接上限、资源分组、硬额度、共享带宽及恢复");
    Ok(())
}
