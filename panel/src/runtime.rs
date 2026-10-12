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
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(1200)
            .collect::<String>()
    );
    Ok(())
}
pub fn spawn_xray(binary: &Path, config: &Path, log: &Path) -> Result<Child> {
    let output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(log)?;
    let mut cmd = Command::new(binary);
    cmd.args(["run", "-config"])
        .arg(config)
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .kill_on_drop(true);
    Ok(cmd
        .spawn()
        .with_context(|| format!("无法启动 Xray: {}", binary.display()))?)
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
    pub active: Option<Desired>,
}
impl ManagedXray {
    pub async fn open(binary: PathBuf, dir: PathBuf) -> Result<Self> {
        let active = if dir.join("active.json").exists() {
            Some(serde_json::from_slice::<Desired>(&std::fs::read(
                dir.join("active.json"),
            )?)?)
        } else {
            None
        };
        let mut x = Self {
            binary,
            dir,
            child: None,
            active: None,
        };
        if let Some(config) = active {
            x.apply(&config).await?;
        }
        Ok(x)
    }
    pub fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| matches!(c.try_wait(), Ok(None)))
            .unwrap_or(false)
    }
    async fn start_config(&mut self, config: &Value) -> Result<()> {
        let path = self.dir.join("xray.json");
        atomic_write(&path, &serde_json::to_vec_pretty(config)?)?;
        self.child = Some(spawn_xray(&self.binary, &path, &self.dir.join("xray.log"))?);
        tokio::time::sleep(Duration::from_millis(450)).await;
        ensure!(
            self.running(),
            "Xray 启动失败；请查看 Agent 数据目录中的 xray.log"
        );
        Ok(())
    }
    pub async fn apply(&mut self, desired: &Desired) -> Result<()> {
        let candidate = self.dir.join("candidate.json");
        atomic_write(&candidate, &serde_json::to_vec_pretty(&desired.config)?)?;
        let result = validate(&self.binary, &candidate).await;
        let _ = std::fs::remove_file(&candidate);
        result?;
        let old = self.active.clone();
        stop(&mut self.child).await;
        if let Err(error) = self.start_config(&desired.config).await {
            stop(&mut self.child).await;
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
            stop(&mut self.child).await;
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
        stop(&mut self.child).await;
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
            let config = json!({"log":{"loglevel":log_level()},"inbounds":[{"listen":"127.0.0.1","port":port,"protocol":"socks","settings":{"auth":"noauth","udp":false}}],"outbounds":[outbound]});
            let file = temp.path().join("probe.json");
            atomic_write(&file, &serde_json::to_vec(&config)?)?;
            validate(binary, &file).await?;
            let mut child = Some(spawn_xray(binary, &file, &temp.path().join("probe.log"))?);
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
