#[path = "../agent_sync.rs"]
mod agent_sync;
use anyhow::{Context, Result, ensure};
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
    // The installer changes this marker only while the Agent is stopped.
    let blocked = dir.join(".core-upgrade-blocked").symlink_metadata().map(|_| true)
        .unwrap_or_else(|e| e.kind() != std::io::ErrorKind::NotFound);
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
    let mut core_cache = CoreVersionCache::default();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    chain_panel::gateway::capacity::raise_nofile_limit();
    let diagnostic_binary = binary.clone();
    let mut runtime = ManagedXray::new(binary, dir.clone())?;
    *runtime.traffic.resources.write().unwrap() = Some(chain_panel::resources::Client::open(
        &dir, format!("{}/api/agent/{}/resources",panel.trim_end_matches('/'),node), token.trim().into())?);
    if let Err(e) = chain_panel::diagnostics::append(&dir.join("diagnostics.log"), "Agent 已启动") { eprintln!("诊断日志写入失败: {e}"); }
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
        let mut last_log = std::time::Instant::now() - Duration::from_secs(60);
        loop {
            match diagnostic_client.post(&diagnostic_url).bearer_auth(&diagnostic_token).json(&reply).send().await {
                Ok(response) if response.status().is_success() => {
                    match response.json::<serde_json::Value>().await {
                        Ok(value) => {
                            reply = serde_json::json!({});
                            if let Some(id) = value["id"].as_str() {
                                let task = &value["request"];
                                let result = match task["operation"].as_str() {
                                    Some("path" | "reality" | "reality-preflight") if blocked =>
                                        Err(anyhow::anyhow!("代理内核已隔离，请通过维护入口修复")),
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
                                    Some("reality-candidates") => match tokio::time::timeout(Duration::from_secs(45), chain_panel::reality_candidates::recommend()).await {
                                        Ok(result) => result,
                                        Err(_) => Err(anyhow::anyhow!("目标推荐超时；未填入默认域名，请稍后重试")),
                                    },
                                    Some("reality-preflight") => match serde_json::from_value::<RealityTarget>(task["request"].clone()) {
                                        Ok(input) => match tokio::time::timeout(Duration::from_secs(60), chain_panel::reality_candidates::preflight(&diagnostic_binary, &input)).await {
                                            Ok(result) => result,
                                            Err(_) => Err(anyhow::anyhow!("REALITY 检查超时；请稍后重试")),
                                        },
                                        Err(e) => Err(e.into()),
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
                        Err(e) => {
                            if last_log.elapsed() >= Duration::from_secs(30) {
                                eprintln!("诊断轮询响应解析失败: {e}");
                                last_log = std::time::Instant::now();
                            }
                        }
                    }
                }
                Ok(response) => {
                    let status = response.status();
                    if status.is_client_error() && reply.get("id").is_some() {
                        eprintln!("诊断回执被控制台拒绝（HTTP {status}），丢弃异常回执以恢复轮询");
                        reply = serde_json::json!({});
                    } else if last_log.elapsed() >= Duration::from_secs(30) {
                        eprintln!("诊断轮询返回 HTTP {status}");
                        last_log = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    if last_log.elapsed() >= Duration::from_secs(30) {
                        eprintln!("诊断轮询连接异常: {}", e.without_url());
                        last_log = std::time::Instant::now();
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    let mut report = Report::default();
    let sync = std::sync::Arc::new(agent_sync::Sync::new(client.clone(),
        format!("{}/api/agent/{}", panel.trim_end_matches('/'), node), token.trim().into(), runtime.traffic.clone()));
    let statistics = tokio::spawn(sync.clone().statistics());
    let mut restore_at = Some(tokio::time::Instant::now());
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
            report.error = "代理内核或网关异常退出，Agent 将重试最近应用的配置".into();
            if let Err(e) = runtime.close().await {
                report.error.push_str(&format!("；流量账本结算失败: {e:#}"));
            }
            runtime.active = None;
            restore_at = Some(tokio::time::Instant::now() + Duration::from_secs(5));
            let _ = tokio::time::timeout(Duration::from_secs(2), sync.report(&mut report)).await;
        }
        let step = async {
            report.running = runtime.running();
            report.system = chain_panel::gateway::system_metrics();
            report.system["max_proxy_connections"] = serde_json::json!(chain_panel::rules::MAX_PROXY_CONNECTIONS);
            report.system["node_connection_budget"] = serde_json::json!(runtime.traffic.proxy_capacity.limit());
            report.system["capacity_drops"] = serde_json::json!(runtime.traffic.proxy_capacity.dropped());
            if now().saturating_sub(certificate_checked) >= 300 {
                if let Some(active) = &runtime.active {
                    if let Ok(specs) = chain_panel::gateway::specs(&active.config) {
                        certificate_expiry = chain_panel::website::certificate_expiry(&specs, &dir);
                    }
                }
                certificate_checked = now();
            }
            report.system["clock"] = clock.lock().unwrap().clone();
            report.system["cores"] = core_cache.refresh(&runtime.binary);
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
            if blocked {
                report.applied = false;
                report.running = false;
                report.error = "代理内核已隔离，Agent 在线，请通过维护入口安装安全内核".into();
                sync.report(&mut report).await?;
                return Ok(());
            }
            let saved = if restore_at.is_some_and(|at| at <= tokio::time::Instant::now()) {
                restore_at = None;
                match runtime.saved_config() {
                    Ok(saved) => {
                        if saved.is_none() && runtime.active.is_none() {
                            report.applied = true;
                            report.error.clear();
                        }
                        saved
                    }
                    Err(e) => {
                        report.applied = false;
                        report.error = format!("读取已保存配置失败: {e:#}");
                        restore_at = Some(tokio::time::Instant::now() + Duration::from_secs(60));
                        None
                    }
                }
            } else { None };
            let restoring = saved.is_some();
            let desired = match saved {
                Some(saved) => Some(saved),
                None => sync.report(&mut report).await?.filter(|d| d.revision != report.revision),
            };
            if let Some(desired) = desired {
                let outcome = tokio::select! {
                    result = runtime.apply(&desired) => result,
                    _ = async {
                        loop {
                            let _ = sync.report(&mut report).await;
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    } => unreachable!(),
                };
                report.applied = outcome.is_ok();
                report.running = runtime.running();
                report.revision = desired.revision;
                report.error = outcome.err().map(|e| format!("{e:#}")).unwrap_or_default();
                if report.applied {
                    restore_at = None;
                    certificate_checked = 0;
                } else if restoring {
                    restore_at = Some(tokio::time::Instant::now() + Duration::from_secs(60));
                }
                let message = if report.applied { format!("配置 {} 应用成功", report.revision) }
                    else { format!("配置 {} 应用失败: {}", report.revision, report.error) };
                let _ = chain_panel::diagnostics::append(&dir.join("diagnostics.log"), &message);
                sync.report(&mut report).await?;
            }
            Ok::<(), anyhow::Error>(())
        };
        tokio::select! { _=&mut shutdown=>break, result=step=>{if let Err(e)=result {eprintln!("同步失败: {e}"); let _=chain_panel::diagnostics::append(&dir.join("diagnostics.log"), &format!("同步失败: {e}"));}} }
        tokio::select! {_=&mut shutdown=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
    }
    statistics.abort();
    let _ = statistics.await;
    clock_task.abort();
    let _ = clock_task.await;
    diagnostics.abort();
    let _ = diagnostics.await;
    runtime.close().await.context("Agent 已停止，但流量账本结算失败")?;
    let _ = chain_panel::diagnostics::append(&dir.join("diagnostics.log"), "Agent 已停止");
    eprintln!("Agent 已停止，所属 Xray 已退出");
    Ok(())
}

#[derive(Default)]
struct CoreVersionCache {
    fingerprint: Option<(Option<PathBuf>, Option<std::time::SystemTime>, Option<PathBuf>, Option<std::time::SystemTime>)>,
    versions: serde_json::Value,
}

impl CoreVersionCache {
    fn refresh(&mut self, xray: &std::path::Path) -> serde_json::Value {
        let mihomo = std::env::var_os("CHAIN_MIHOMO_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| xray.with_file_name("mihomo"));
        let stat = |p: &std::path::Path| {
            (p.canonicalize().ok(), std::fs::metadata(p).and_then(|m| m.modified()).ok())
        };
        let (xp, xm) = stat(xray);
        let (mp, mm) = stat(&mihomo);
        let fp = (xp, xm, mp, mm);
        if self.fingerprint.as_ref() != Some(&fp) {
            self.versions = detect_core_versions(xray, &mihomo);
            self.fingerprint = Some(fp);
        }
        self.versions.clone()
    }
}

fn detect_core_versions(xray: &std::path::Path, mihomo: &std::path::Path) -> serde_json::Value {
    let mut result = serde_json::json!({});
    for (core, path, flag) in [("xray", xray, "version"), ("mihomo", mihomo, "-v")] {
        let manifest = path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent()?.parent().map(std::path::Path::to_path_buf))
            .and_then(|root| std::fs::read(root.join("release.json")).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        if let Some(version) = manifest.as_ref().and_then(|m| m[core].as_str()).filter(|v| !v.is_empty())
            && path.is_file()
        {
            result[core] = serde_json::json!(version);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        match std::process::Command::new(path).arg(flag).output() {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(version) = parse_core_version(core, &text) {
                    result[core] = serde_json::json!(version);
                }
            }
            Ok(output) => {
                eprintln!("读取 {core} 版本返回非零状态: {}", output.status);
            }
            Err(e) => {
                eprintln!("读取 {core} 版本失败 ({}): {e}", path.display());
            }
        }
    }
    result
}

fn parse_core_version(core: &str, text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    let tokens: Vec<_> = line.split_whitespace().collect();
    let raw = if core == "xray" {
        // "Xray 26.2.6 (Xray, Penetrates Everything.) ..."
        tokens.get(1).copied()
    } else {
        // "Mihomo Meta v1.19.33 linux amd64 ..."
        tokens.iter().copied().find(|t| t.strip_prefix('v').unwrap_or(t).chars().next().is_some_and(|c| c.is_ascii_digit()))
    }?;
    Some(raw.trim_start_matches('v').to_string())
}
