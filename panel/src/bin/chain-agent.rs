use anyhow::{Context, Result, bail, ensure};
use chain_panel::{model::*, runtime::*};
use std::os::unix::fs::PermissionsExt;
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let dir =
        PathBuf::from(std::env::var("CHAIN_AGENT_DIR").unwrap_or_else(|_| "agent-data".into()));
    private_dir(&dir, "chain-agent")?;
    let _lock = lock_dir(&dir)?;
    let panel = std::env::var("CHAIN_PANEL_URL").context("缺少 CHAIN_PANEL_URL")?;
    let endpoint = url::Url::parse(&panel)?;
    ensure!(
        endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "控制台 URL 不得包含凭据、查询或片段"
    );
    let loopback = endpoint
        .host_str()
        .map(|h| {
            h == "localhost"
                || h.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    ensure!(
        endpoint.scheme() == "https"
            || (endpoint.scheme() == "http"
                && (loopback || std::env::var("CHAIN_ALLOW_HTTP").as_deref() == Ok("1"))),
        "非本机控制台需要 HTTPS；可信私网可显式设置 CHAIN_ALLOW_HTTP=1"
    );
    let node = std::env::var("CHAIN_NODE_ID").context("缺少 CHAIN_NODE_ID")?;
    uuid::Uuid::parse_str(&node)?;
    let token_path =
        std::env::var("CHAIN_NODE_TOKEN_FILE").context("缺少 CHAIN_NODE_TOKEN_FILE")?;
    ensure!(
        std::fs::metadata(&token_path)?.permissions().mode() & 0o077 == 0,
        "节点令牌文件必须只允许所属用户读取（例如 chmod 600）"
    );
    let token = std::fs::read_to_string(token_path)?;
    ensure!(token.trim().len() >= 32, "Agent token 无效");
    let binary = PathBuf::from(std::env::var("CHAIN_XRAY_BIN").unwrap_or_else(|_| "xray".into()));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut runtime = ManagedXray::open(binary, dir.clone()).await?;
    let mut report = Report {
        revision: runtime
            .active
            .as_ref()
            .map(|d| d.revision.clone())
            .unwrap_or_default(),
        applied: true,
        running: runtime.running(),
        error: String::new(),
        ..Default::default()
    };
    let url = format!("{}/api/agent/{}/sync", panel.trim_end_matches('/'), node);
    eprintln!(
        "Agent 已启动，数据目录 {}，仅管理自己启动的 Xray",
        dir.display()
    );
    let shutdown = async {
        let mut sig =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=sig.recv()=>{}}
    };
    tokio::pin!(shutdown);
    loop {
        if runtime.active.is_some() && !runtime.running() {
            report.running = false;
            report.applied = false;
            report.error =
                "代理内核或网关异常退出；Agent 将退出，由服务管理器恢复最后已应用配置".into();
            runtime.close().await;
            report.traffic = runtime.traffic.snapshot();
            // Keep this failure outside `step`: its errors are logged and retried in place.
            // The installed service restarts failures after 5s; active.json remains authoritative.
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                client
                    .post(&url)
                    .bearer_auth(token.trim())
                    .json(&report)
                    .send(),
            )
            .await;
            bail!("{}", report.error);
        }
        let step = async {
            report.running = runtime.running();
            report.traffic = runtime.traffic.snapshot();
            runtime.traffic.save()?;
            report.system = chain_panel::gateway::system_metrics();
            let ingress_status = std::env::var_os("CHAIN_INGRESS_STATUS")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/chain-ingress/status.json"));
            if let Ok(bytes) = std::fs::read(ingress_status) {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    report.system["ingress"] = value;
                }
            }
            if let Ok(bytes) = std::fs::read(dir.join("certificate-status.json")) {
                if let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    report.system["certificate"] = status;
                }
            }
            let response = client
                .post(&url)
                .bearer_auth(token.trim())
                .json(&report)
                .send()
                .await?
                .error_for_status()?;
            let desired = response.json::<Option<Desired>>().await?;
            if let Some(desired) = desired {
                if desired.revision != report.revision {
                    let traffic = runtime.traffic.clone();
                    let application = runtime.apply(&desired);
                    tokio::pin!(application);
                    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
                    let outcome = loop {
                        tokio::select! {
                            result=&mut application=>break result,
                            _=heartbeat.tick()=>{
                                report.traffic=traffic.snapshot();let _=traffic.save();
                                let _=client.post(&url).bearer_auth(token.trim()).json(&report).send().await;
                            }
                        }
                    };
                    drop(application);
                    let applied = outcome.is_ok();
                    report = Report {
                        revision: desired.revision.clone(),
                        applied: outcome.is_ok(),
                        error: outcome.err().map(|e| format!("{e:#}")).unwrap_or_default(),
                        running: applied,
                        traffic: traffic.snapshot(),
                        system: chain_panel::gateway::system_metrics(),
                    };
                    // Acknowledge immediately; do not report a saved config as running before application.
                    client
                        .post(&url)
                        .bearer_auth(token.trim())
                        .json(&report)
                        .send()
                        .await?
                        .error_for_status()?;
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        tokio::select! { _=&mut shutdown=>break, result=step=>{if let Err(e)=result {eprintln!("同步失败: {e}");}} }
        tokio::select! {_=&mut shutdown=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
    }
    runtime.close().await;
    eprintln!("Agent 已停止，所属 Xray 已退出");
    Ok(())
}
