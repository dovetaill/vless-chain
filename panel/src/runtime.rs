use crate::{compiler, model::*};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    process::{Child, Command},
};

pub fn private_dir(path: &Path, owner: &str) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let marker = path.join(".chain-owner");
    if marker.exists() {
        ensure!(
            std::fs::read_to_string(&marker)? == owner,
            "数据目录属于其他组件"
        );
    } else {
        ensure!(
            std::fs::read_dir(path)?.next().is_none(),
            "拒绝使用非空且不属于本程序的数据目录: {}",
            path.display()
        );
        atomic_write(&marker, owner.as_bytes())?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("缺少父目录")?)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}
pub fn lock_dir(path: &Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path.join("process.lock"))?;
    // The lock is tied to this file descriptor and released on process exit.
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "该数据目录已有程序运行"
    );
    Ok(file)
}
pub async fn validate(binary: &Path, config: &Path) -> Result<()> {
    let mut cmd = Command::new(binary);
    cmd.args(["run", "-test", "-config"])
        .arg(config)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .context("Xray 配置校验超时")??;
    ensure!(
        output.status.success(),
        "Xray 配置校验失败: {}",
        String::from_utf8_lossy(if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        })
        .chars()
        .take(1200)
        .collect::<String>()
    );
    Ok(())
}
pub async fn check_reality(input: &RealityTarget) -> Result<Value> {
    compiler::target(&input.target)?;
    ensure!(compiler::host_valid(&input.server_name), "SNI 域名无效");
    let start = std::time::Instant::now();
    let mut command = Command::new("openssl");
    command
        .args([
            "s_client",
            "-connect",
            &input.target,
            "-servername",
            &input.server_name,
            "-verify_hostname",
            &input.server_name,
            "-verify_return_error",
            "-tls1_3",
            "-alpn",
            "h2",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(12), command.output())
        .await
        .context("REALITY 目标检测超时")??;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let valid = output.status.success() && stdout.contains("Verify return code: 0 (ok)");
    let h2 = stdout.contains("ALPN protocol: h2");
    Ok(
        json!({"ok":valid && h2,"tls13":valid,"certificate_valid":valid,"h2":h2,
        "latency_ms":start.elapsed().as_millis(),"target":input.target,"server_name":input.server_name,
        "detail":if !valid {"TLS 1.3 握手或证书/SNI 验证失败"} else if !h2 {"证书与 SNI 匹配，但目标未协商 H2"} else {"TLS 1.3、证书/SNI 与 H2 检查通过"}}),
    )
}
pub fn spawn_xray(binary: &Path, config: &Path, log: &Path) -> Result<Child> {
    let mut cmd = Command::new(binary);
    cmd.args(["run", "-config"]).arg(config);
    spawn_core(cmd, log)
}
fn spawn_core(mut cmd: Command, log: &Path) -> Result<Child> {
    let output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(log)?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .kill_on_drop(true);
    let parent = unsafe { libc::getpid() };
    // Managed cores must exit even when their owning Agent is killed.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::other("Agent 已退出"));
            }
            Ok(())
        });
    }
    Ok(cmd.spawn().context("无法启动代理内核")?)
}
fn mihomo_command(xray: &Path, config: &Path, dir: &Path) -> Command {
    let binary = std::env::var_os("CHAIN_MIHOMO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| xray.with_file_name("mihomo"));
    let mut cmd = Command::new(binary);
    cmd.arg("-f")
        .arg(config)
        .arg("-d")
        .arg(dir)
        .env(
            "SAFE_PATHS",
            dir.parent().unwrap_or(dir).join("acme/config"),
        )
        .kill_on_drop(true);
    cmd
}
async fn validate_mihomo(xray: &Path, config: &Path, dir: &Path) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        mihomo_command(xray, config, dir).arg("-t").output(),
    )
    .await
    .context("Mihomo 校验超时")??;
    ensure!(
        output.status.success(),
        "Mihomo 配置校验失败：{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
pub async fn stop(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        if let Some(pid) = c.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(3), c.wait())
            .await
            .is_err()
        {
            let _ = c.kill().await;
            let _ = c.wait().await;
        }
    }
}

pub struct ManagedXray {
    pub binary: PathBuf,
    pub dir: PathBuf,
    pub child: Option<Child>,
    mihomo: Option<Child>,
    pub active: Option<Desired>,
    pub traffic: std::sync::Arc<crate::gateway::Book>,
    gateway: Option<crate::gateway::Gateway>,
}
impl ManagedXray {
    async fn apply_ingress(&self, config: &Value) -> Result<()> {
        let routes = config.get("_ingress").cloned().unwrap_or_else(|| json!([]));
        let status_path = std::env::var_os("CHAIN_INGRESS_STATUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/chain-ingress/status.json"));
        let status: Value = std::fs::read(&status_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        if status["ready"] != true && routes.as_array().is_some_and(Vec::is_empty) {
            return Ok(());
        }
        ensure!(
            status["ready"] == true,
            "统一 443 尚未准备，请在本机运行 chainctl ingress-enable"
        );
        let request_id = id();
        atomic_write(
            &self.dir.join("ingress-request.json"),
            &serde_json::to_vec(&json!({"id":request_id,"routes":routes}))?,
        )?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(bytes) = std::fs::read(&status_path) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value["id"] == request_id {
                        ensure!(
                            value["applied"] == true,
                            "443 分流应用失败：{}",
                            value["error"].as_str().unwrap_or("未知错误")
                        );
                        return Ok(());
                    }
                }
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "等待 443 分流服务超时，请检查 chain-ingress.service"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub async fn open(binary: PathBuf, dir: PathBuf) -> Result<Self> {
        let active = if dir.join("active.json").exists() {
            Some(serde_json::from_slice::<Desired>(&std::fs::read(
                dir.join("active.json"),
            )?)?)
        } else {
            None
        };
        let traffic = crate::gateway::Book::open(&dir)?;
        let mut x = Self {
            binary,
            dir,
            child: None,
            mihomo: None,
            active: None,
            traffic,
            gateway: None,
        };
        if let Some(config) = active {
            x.apply(&config).await?;
        }
        Ok(x)
    }
    pub fn running(&mut self) -> bool {
        self.gateway.as_ref().is_none_or(|g| g.running())
            && self
                .mihomo
                .as_mut()
                .is_none_or(|c| matches!(c.try_wait(), Ok(None)))
            && self
                .child
                .as_mut()
                .map(|c| matches!(c.try_wait(), Ok(None)))
                .unwrap_or(false)
    }
    fn xray_config(&self, config: &Value) -> Result<Value> {
        let mut value = crate::gateway::xray_config(config);
        for spec in crate::gateway::specs(config)? {
            if spec.kind == Kind::Vless
                && matches!(spec.protocol, Protocol::Vless | Protocol::Hysteria2)
                && spec.tls.mode == "auto"
            {
                let live = self.dir.join("acme/config/live").join(&spec.tls.domain);
                let inbound = value["inbounds"]
                    .as_array_mut()
                    .context("缺少入站配置")?
                    .iter_mut()
                    .find(|inbound| inbound["tag"] == format!("line-{}-in", spec.id))
                    .context("TLS 入口配置不存在")?;
                if spec.protocol == Protocol::Hysteria2 && inbound["protocol"] == "socks" {
                    continue;
                }
                inbound["streamSettings"]["tlsSettings"]["certificates"] = json!([{
                    "certificateFile":live.join("fullchain.pem"),
                    "keyFile":live.join("privkey.pem")
                }]);
            }
        }
        Ok(value)
    }
    fn mihomo_config(&self, config: &Value) -> Result<Option<Value>> {
        let Some(mut value) = config.get("_mihomo").cloned() else {
            return Ok(None);
        };
        for spec in crate::gateway::specs(config)?.iter().filter(|s| {
            matches!(s.protocol, Protocol::Tuic | Protocol::Hysteria2) && s.tls.mode == "auto"
        }) {
            // An upgrade may first restore a saved Xray Hysteria listener before the next publish.
            if spec.protocol == Protocol::Hysteria2
                && config["inbounds"].as_array().is_some_and(|inbounds| {
                    inbounds.iter().any(|inbound| {
                        inbound["tag"] == format!("line-{}-in", spec.id)
                            && inbound["protocol"] == "hysteria"
                    })
                })
            {
                continue;
            }
            let live = self.dir.join("acme/config/live").join(&spec.tls.domain);
            let listener = value["listeners"]
                .as_array_mut()
                .context("缺少 QUIC 入口")?
                .iter_mut()
                .find(|l| l["name"] == format!("line-{}-in", spec.id))
                .context("缺少 QUIC 入口")?;
            listener["certificate"] = json!(live.join("fullchain.pem"));
            listener["private-key"] = json!(live.join("privkey.pem"));
        }
        Ok(Some(value))
    }
    async fn start_config(&mut self, config: &Value) -> Result<()> {
        let path = self.dir.join("xray.json");
        atomic_write(
            &path,
            &serde_json::to_vec_pretty(&self.xray_config(config)?)?,
        )?;
        self.child = Some(spawn_xray(&self.binary, &path, &self.dir.join("xray.log"))?);
        if let Some(value) = self.mihomo_config(config)? {
            let dir = self.dir.join("mihomo");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("config.json");
            atomic_write(&path, &serde_json::to_vec_pretty(&value)?)?;
            self.mihomo = Some(spawn_core(
                mihomo_command(&self.binary, &path, &dir),
                &dir.join("mihomo.log"),
            )?);
        }
        // shortcut: Xray 26.3.27 samples REALITY targets for 5s; replace with a core readiness signal when available.
        let reality = config["inbounds"].as_array().is_some_and(|inbounds| {
            inbounds
                .iter()
                .any(|inbound| inbound["streamSettings"]["security"] == "reality")
        });
        tokio::time::sleep(if reality {
            Duration::from_secs(6)
        } else {
            Duration::from_millis(450)
        })
        .await;
        if !self.running() {
            let logs: Vec<_> = ["xray.log", "mihomo/mihomo.log"]
                .iter()
                .filter_map(|name| {
                    let log = std::fs::read_to_string(self.dir.join(name)).ok()?;
                    let tail: String = log
                        .chars()
                        .rev()
                        .take(1800)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    Some(format!("{name}: {tail}"))
                })
                .collect();
            bail!("代理内核启动失败：{}", logs.join("\n"));
        }
        if let Some(child) = &self.mihomo {
            let pid = child.id().context("Mihomo 已退出")?;
            let sockets: std::collections::BTreeSet<_> =
                std::fs::read_dir(format!("/proc/{pid}/fd"))?
                    .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
                    .collect();
            let log = std::fs::read_to_string(self.dir.join("mihomo/mihomo.log"))?;
            ensure!(!log.contains("listen err:"), "Mihomo 监听启动失败：{log}");
            for listener in config["_mihomo"]["listeners"].as_array().unwrap() {
                let table = if matches!(listener["type"].as_str(), Some("tuic" | "hysteria2")) {
                    "udp"
                } else {
                    "tcp"
                };
                let address = format!("0100007F:{:04X}", listener["port"].as_u64().unwrap());
                let bound = std::fs::read_to_string(format!("/proc/{pid}/net/{table}"))?
                    .lines()
                    .skip(1)
                    .any(|line| {
                        let fields: Vec<_> = line.split_whitespace().collect();
                        fields.get(1) == Some(&address.as_str())
                            && fields.get(9).is_some_and(|inode| {
                                sockets.contains(&PathBuf::from(format!("socket:[{inode}]")))
                            })
                            && (table == "udp" || fields.get(3) == Some(&"0A"))
                    });
                ensure!(
                    bound,
                    "Mihomo 监听未就绪：{} {}",
                    listener["name"],
                    listener["port"]
                );
            }
        }
        self.gateway = Some(
            crate::gateway::Gateway::start(
                crate::gateway::specs(config)?,
                self.traffic.clone(),
                self.dir.clone(),
            )
            .await?,
        );
        self.apply_ingress(config).await?;
        Ok(())
    }
    pub async fn apply(&mut self, desired: &Desired) -> Result<()> {
        let specs = crate::gateway::specs(&desired.config)?;
        let existing_challenge = self
            .active
            .as_ref()
            .and_then(|d| crate::gateway::specs(&d.config).ok())
            .is_some_and(|s| {
                s.iter()
                    .any(|s| s.tls.mode == "auto" || s.kind == Kind::Website && s.port == 80)
            });
        // Native TLS/QUIC listeners need certificates before core validation.
        let native_tls = specs.iter().any(|s| {
            s.kind == Kind::Vless
                && matches!(
                    s.protocol,
                    Protocol::Vless | Protocol::Hysteria2 | Protocol::Tuic
                )
                && s.tls.mode == "auto"
        });
        if native_tls {
            crate::website::prepare(&specs, &self.dir, existing_challenge).await?;
        }
        let candidate = self.dir.join("candidate.json");
        atomic_write(
            &candidate,
            &serde_json::to_vec_pretty(&self.xray_config(&desired.config)?)?,
        )?;
        let result = validate(&self.binary, &candidate).await;
        let _ = std::fs::remove_file(&candidate);
        result?;
        if let Some(value) = self.mihomo_config(&desired.config)? {
            let dir = self.dir.join("mihomo");
            std::fs::create_dir_all(&dir)?;
            let candidate = dir.join("candidate.json");
            atomic_write(&candidate, &serde_json::to_vec(&value)?)?;
            let result = validate_mihomo(&self.binary, &candidate, &dir).await;
            let _ = std::fs::remove_file(&candidate);
            result?;
        }
        if self.active.as_ref().is_some_and(|old| {
            crate::gateway::xray_config(&old.config) == crate::gateway::xray_config(&desired.config)
                && old.config.get("_mihomo") == desired.config.get("_mihomo")
        }) && self.running()
        {
            if self.gateway.as_ref().is_some_and(|g| g.limits_only(&specs)) {
                atomic_write(&self.dir.join("active.json"), &serde_json::to_vec(desired)?)?;
                self.gateway.as_mut().unwrap().update_limits(&specs);
                self.active = Some(desired.clone());
                return Ok(());
            }
        }
        if !native_tls {
            crate::website::prepare(&specs, &self.dir, existing_challenge).await?;
        }
        let old = self.active.clone();
        self.stop_all().await;
        if let Err(error) = self.start_config(&desired.config).await {
            self.stop_all().await;
            if let Some(old) = old {
                if let Err(restore) = self.start_config(&old.config).await {
                    bail!("应用失败: {error}; 恢复失败: {restore}");
                }
            }
            return Err(error);
        }
        if let Err(error) =
            atomic_write(&self.dir.join("active.json"), &serde_json::to_vec(desired)?)
        {
            self.stop_all().await;
            if let Some(old) = old {
                self.start_config(&old.config)
                    .await
                    .context("写入版本失败，恢复旧配置也失败")?;
            }
            return Err(error);
        }
        self.active = Some(desired.clone());
        Ok(())
    }
    pub async fn close(&mut self) {
        self.stop_all().await;
        let _ = self.traffic.save();
    }
    async fn stop_all(&mut self) {
        if let Some(mut gateway) = self.gateway.take() {
            gateway.close().await;
        }
        stop(&mut self.child).await;
        stop(&mut self.mihomo).await;
    }
}

#[derive(serde::Deserialize)]
pub struct ProbeRequest {
    pub mode: String,
    #[serde(default)]
    pub url: String,
}
pub async fn probe(
    binary: &Path,
    line: &Line,
    doc: &Document,
    request: &ProbeRequest,
) -> Result<Value> {
    let start = std::time::Instant::now();
    ensure!(line.enabled, "线路已停用");
    let entry = line.hops.first().context("无入口")?;
    let node = doc
        .nodes
        .iter()
        .find(|n| n.id == entry.node_id)
        .context("入口节点不存在")?;
    let endpoint = compiler::address(&node.host, entry.port);
    let marker = format!("chain-probe-{}", id()).into_bytes();
    let detail = match request.mode.as_str() {
        "udp_echo" => {
            ensure!(
                line.kind == Kind::Forward && line.network.contains("udp"),
                "此线路没有 UDP 转发入口"
            );
            let addr = tokio::net::lookup_host(&endpoint)
                .await?
                .next()
                .context("无法解析入口")?;
            let socket = UdpSocket::bind(if addr.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            })
            .await?;
            socket.connect(addr).await?;
            socket.send(&marker).await?;
            let mut buf = vec![0; 2048];
            let n = socket.recv(&mut buf).await?;
            ensure!(
                buf[..n] == marker,
                "目标未返回原始探测内容（此模式要求目标为 UDP 回显服务）"
            );
            "UDP 回显数据已经过实际入口和完整转发路径".to_string()
        }
        "tcp_echo" => {
            ensure!(
                line.kind == Kind::Forward && line.network.contains("tcp"),
                "此线路没有 TCP 转发入口"
            );
            let mut s = TcpStream::connect(&endpoint).await?;
            s.write_all(&marker).await?;
            let mut buf = vec![0; marker.len()];
            s.read_exact(&mut buf).await?;
            ensure!(
                buf == marker,
                "目标未返回原始探测内容（此模式要求目标为 TCP 回显服务）"
            );
            "TCP 回显数据已经过实际入口和完整转发路径".to_string()
        }
        "tcp" => {
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&endpoint)).await??;
            "入口 TCP 可以建立连接；未验证代理认证或业务响应".into()
        }
        "http" if line.kind == Kind::Website => {
            let scheme = if matches!(line.access.tls.mode.as_str(), "auto" | "manual") {
                "https"
            } else {
                "http"
            };
            let url = format!(
                "{scheme}://{}/",
                compiler::address(&line.access.tls.domain, entry.port)
            );
            let response = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(12))
                .build()?
                .get(url)
                .send()
                .await?;
            ensure!(
                response.status().is_success(),
                "网站返回 HTTP {}",
                response.status()
            );
            format!("网站入口返回 HTTP {}", response.status())
        }
        "http" => {
            ensure!(
                line.kind == Kind::Vless || line.network.contains("tcp"),
                "HTTP 探测需要 TCP"
            );
            let url = url::Url::parse(&request.url).context("请输入 HTTP/HTTPS 探测地址")?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.username().is_empty()
                    && url.password().is_none(),
                "探测地址必须为 HTTP/HTTPS 且不含凭据"
            );
            let temp = tempfile::tempdir()?;
            // Reserve a random local port, then start the short-lived SOCKS probe process.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            drop(listener);
            let outbound = if line.kind == Kind::Vless {
                compiler::external_outbound(&compiler::share(line, doc)?, "probe")?
            } else {
                json!({"tag":"probe","protocol":"freedom","settings":{"redirect":endpoint}})
            };
            let sidecar = compiler::needs_mihomo(&outbound);
            let config = if sidecar {
                let proxy = crate::external::mihomo(&compiler::share(line, doc)?, "probe")?;
                json!({"listeners":[{"name":"probe-in","type":"socks","listen":"127.0.0.1","port":port,"proxy":"probe"}],"proxies":[proxy],"mode":"rule","rules":["MATCH,REJECT"],"log-level":"warning"})
            } else {
                json!({"log":{"loglevel":log_level()},"inbounds":[{"listen":"127.0.0.1","port":port,"protocol":"socks","settings":{"auth":"noauth","udp":false}}],"outbounds":[outbound]})
            };
            let file = temp.path().join("probe.json");
            atomic_write(&file, &serde_json::to_vec(&config)?)?;
            let mut child = Some(if sidecar {
                validate_mihomo(binary, &file, temp.path()).await?;
                spawn_core(
                    mihomo_command(binary, &file, temp.path()),
                    &temp.path().join("probe.log"),
                )?
            } else {
                validate(binary, &file).await?;
                spawn_xray(binary, &file, &temp.path().join("probe.log"))?
            });
            let result = async {
                for _ in 0..40 {
                    if child.as_mut().unwrap().try_wait()?.is_some() {
                        bail!("探测 Xray 提前退出");
                    }
                    if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))?)
                    .timeout(Duration::from_secs(12))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?;
                let response = client.get(url).send().await?;
                ensure!(
                    response.status().is_success(),
                    "已收到响应，但 HTTP 状态为 {}",
                    response.status()
                );
                Ok::<_, anyhow::Error>(format!(
                    "实际入口 → 完整线路 → 探测目标，HTTP {}（不代表 UDP 已验证）",
                    response.status().as_u16()
                ))
            }
            .await;
            stop(&mut child).await;
            result.map_err(|e| {
                let log =
                    std::fs::read_to_string(temp.path().join("probe.log")).unwrap_or_default();
                anyhow::anyhow!(
                    "{e:#}; Xray: {}",
                    log.chars()
                        .rev()
                        .take(1800)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                )
            })?
        }
        _ => bail!("未知探测模式"),
    };
    Ok(
        json!({"ok":true,"latency_ms":start.elapsed().as_millis(),"detail":detail,"checked_at":now()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_hysteria_certificates_follow_saved_or_new_core_config() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = ManagedXray {
            binary: "xray".into(),
            dir: dir.path().into(),
            child: None,
            mihomo: None,
            active: None,
            traffic: crate::gateway::Book::open(dir.path()).unwrap(),
            gateway: None,
        };
        let mut config = json!({
            "inbounds":[{"tag":"line-test-in","protocol":"hysteria","streamSettings":{"security":"tls"}}],
            "_chain":[{"id":"test","port":20000,"backend":50000,"kind":"vless","protocol":"hysteria2","network":"udp","target":"","limits":{},"tls":{"mode":"auto","domain":"cert.example.com"}}],
            "_mihomo":{"listeners":[],"proxies":[]}
        });
        let cert = dir
            .path()
            .join("acme/config/live/cert.example.com/fullchain.pem");
        let legacy = runtime.xray_config(&config).unwrap();
        assert_eq!(
            legacy["inbounds"][0]["streamSettings"]["tlsSettings"]["certificates"][0]["certificateFile"],
            json!(cert)
        );
        assert!(
            runtime.mihomo_config(&config).unwrap().unwrap()["listeners"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        config["inbounds"][0] = json!({"tag":"line-test-in","protocol":"socks"});
        config["_mihomo"]["listeners"] = json!([{"name":"line-test-in","type":"hysteria2"}]);
        let current = runtime.xray_config(&config).unwrap();
        assert!(current["inbounds"][0].get("streamSettings").is_none());
        assert_eq!(
            runtime.mihomo_config(&config).unwrap().unwrap()["listeners"][0]["certificate"],
            json!(cert)
        );
    }
}
