use anyhow::{Context, Result, bail, ensure};
use chain_panel::{model::*, runtime::*};
use std::os::unix::fs::PermissionsExt;
use std::{path::PathBuf, time::Duration};

fn main() -> Result<()> {
    #[cfg(target_os = "macos")]
    watch_parent()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    let dir =
        PathBuf::from(std::env::var("CHAIN_AGENT_DIR").unwrap_or_else(|_| "agent-data".into()));
    ensure!(!dir.join(".core-upgrade-blocked").exists(), "安全更新失败后代理已停用，请通过维护入口安装安全内核；未启动代理");
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
    let core_versions = reported_cores(&binary);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let diagnostic_binary = binary.clone();
    let mut runtime = ManagedXray::open(binary, dir.clone()).await?;
    *runtime.traffic.resources.write().unwrap() = Some(chain_panel::resources::Client::open(
        &dir, format!("{}/api/agent/{}/resources",panel.trim_end_matches('/'),node), token.trim().into())?);
    chain_panel::diagnostics::append(&dir.join("diagnostics.log"), "Agent 已启动")?;
    let clock = std::sync::Arc::new(std::sync::Mutex::new(serde_json::json!({"verified":false,"detail":"正在检查系统时间"})));
    let clock_value = clock.clone();
    let cleanup_dir = dir.clone();
    let clock_task = tokio::spawn(async move {
        loop {
            let value = chain_panel::diagnostics::clock_status().await;
            *clock_value.lock().unwrap() = value;
            let _ = chain_panel::diagnostics::maintain(&cleanup_dir);
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
    let diagnostic_client = client.clone();
    let diagnostic_token = token.trim().to_string();
    let diagnostic_dir = dir.clone();
    let diagnostic_clock = clock.clone();
    let diagnostic_url = format!("{}/api/agent/{}/diagnostics", panel.trim_end_matches('/'), node);
    let diagnostics = tokio::spawn(async move {
        let mut reply = serde_json::json!({});
        loop {
            if let Ok(response) = diagnostic_client.post(&diagnostic_url).bearer_auth(&diagnostic_token).json(&reply).send().await {
                if response.status().is_success() {
                    if let Ok(value) = response.json::<serde_json::Value>().await {
                        reply = serde_json::json!({});
                        if let Some(id) = value["id"].as_str() {
                            let task = &value["request"];
                            let result = match task["operation"].as_str() {
                                Some("logs") => {
                                    match serde_json::from_value::<chain_panel::diagnostics::LogRequest>(task["request"].clone()) {
                                        Ok(request) if request.source != "panel" => {
                                            let path = diagnostic_dir.clone();
                                            match tokio::task::spawn_blocking(move || chain_panel::diagnostics::read(&path, &request)).await {
                                                Ok(result) => result,
                                                Err(e) => Err(anyhow::anyhow!("日志读取失败: {e}")),
                                            }
                                        },
                                        _ => Err(anyhow::anyhow!("日志来源无效")),
                                    }
                                },
                                Some("path") => {
                                    match tokio::time::timeout(Duration::from_secs(35), chain_panel::runtime::diagnose_path(&diagnostic_binary, &diagnostic_dir, task["request"]["line_id"].as_str().unwrap_or(""), task["request"]["url"].as_str().unwrap_or(""))).await {
                                        Ok(result) => result,
                                        Err(_) => Err(anyhow::anyhow!("出口诊断超时；隔离进程已停止")),
                                    }
                                },
                                Some("reality") => match serde_json::from_value::<RealityTarget>(task["request"].clone()) {
                                    Ok(input) => match tokio::time::timeout(Duration::from_secs(45), chain_panel::diagnostics::reality(&diagnostic_binary, &input)).await {
                                        Ok(result) => result,
                                        Err(_) => Ok(serde_json::json!({"ok":false,"status":"unverified","detail":"隔离 REALITY 检测超时，临时进程已终止；没有修改运行中的代理"})),
                                    },
                                    Err(e) => Err(e.into()),
                                },
                                _ => Err(anyhow::anyhow!("诊断操作无效")),
                            };
                            let mut result = result.unwrap_or_else(|e| serde_json::json!({"error":chain_panel::diagnostics::redact(&format!("{e:#}"))}));
                            result["clock"] = diagnostic_clock.lock().unwrap().clone();
                            reply = serde_json::json!({"id":id,"result":result});
                            continue;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
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
    let mut certificate_expiry = serde_json::json!([]);
    let mut certificate_checked = 0;
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
            if now().saturating_sub(certificate_checked) >= 300 {
                if let Some(active) = &runtime.active {
                    if let Ok(specs) = chain_panel::gateway::specs(&active.config) {
                        certificate_expiry = chain_panel::website::certificate_expiry(&specs, &dir);
                    }
                }
                certificate_checked = now();
            }
            report.system["clock"] = clock.lock().unwrap().clone();
            report.system["diagnostics_version"] = serde_json::json!(1);
            report.system["cores"] = core_versions.clone();
            report.system["certificate_expiry"] = certificate_expiry.clone();
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
                    let _ = chain_panel::diagnostics::append(&dir.join("diagnostics.log"), &match &outcome {Ok(_)=>format!("配置 {} 应用成功",desired.revision),Err(e)=>format!("配置 {} 应用失败: {e:#}",desired.revision)});
                    report = Report {
                        revision: desired.revision.clone(),
                        applied: outcome.is_ok(),
                        error: outcome.err().map(|e| format!("{e:#}")).unwrap_or_default(),
                        running: applied,
                        traffic: traffic.snapshot(),
                        system: chain_panel::gateway::system_metrics(),
                    };
                    report.system["diagnostics_version"] = serde_json::json!(1);
                    report.system["cores"] = core_versions.clone();
                    report.system["clock"] = clock.lock().unwrap().clone();
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
        tokio::select! { _=&mut shutdown=>break, result=step=>{if let Err(e)=result {eprintln!("同步失败: {e}"); let _=chain_panel::diagnostics::append(&dir.join("diagnostics.log"), &format!("同步失败: {e}"));}} }
        tokio::select! {_=&mut shutdown=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
    }
    clock_task.abort();
    let _ = clock_task.await;
    diagnostics.abort();
    let _ = diagnostics.await;
    runtime.close().await;
    let _ = chain_panel::diagnostics::append(&dir.join("diagnostics.log"), "Agent 已停止");
    eprintln!("Agent 已停止，所属 Xray 已退出");
    Ok(())
}

fn reported_cores(xray: &std::path::Path) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut result = serde_json::json!({});
    let Some(root) = xray.canonicalize().ok().and_then(|p| p.parent()?.parent().map(std::path::Path::to_path_buf)) else { return result; };
    let Some(manifest) = std::fs::read(root.join("release.json")).ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else { return result; };
    for core in ["xray", "mihomo"] {
        let path = if core == "mihomo" { std::env::var_os("CHAIN_MIHOMO_BIN").map(PathBuf::from).unwrap_or_else(|| xray.with_file_name("mihomo")) } else { xray.to_path_buf() };
        let checked = (|| -> Result<String> {
            let mut file = std::fs::File::open(path)?;
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop { let n = file.read(&mut buffer)?; if n == 0 { break; } hash.update(&buffer[..n]); }
            ensure!(hex::encode(hash.finalize()) == manifest["files"][format!("bin/{core}")]["sha256"].as_str().unwrap_or(""), "内核摘要与发行记录不一致");
            Ok(manifest[core].as_str().context("缺少内核版本")?.into())
        })();
        if let Ok(version) = checked { result[core] = serde_json::json!(version); }
    }
    result
}
