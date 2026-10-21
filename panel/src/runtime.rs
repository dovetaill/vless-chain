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
    validate_legacy(binary, config, "").await
}
pub async fn validate_legacy(binary: &Path, config: &Path, asset_dir: &str) -> Result<()> {
    let mut cmd = Command::new(binary);
    if !asset_dir.is_empty() {
        cmd.env("XRAY_LOCATION_ASSET", asset_dir);
    }
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
    let mismatch = String::from_utf8_lossy(&output.stderr).contains("hostname mismatch");
    Ok(
        json!({"ok":valid && h2,"sni_mismatch":mismatch,"tls13":valid,"certificate_valid":valid,"h2":h2,
        "latency_ms":start.elapsed().as_millis(),"target":input.target,"server_name":input.server_name,
        "detail":if !valid {"TLS 1.3 握手或证书/SNI 验证失败"} else if !h2 {"证书与 SNI 匹配，但目标未协商 H2"} else {"TLS 1.3、证书/SNI 与 H2 检查通过"}}),
    )
}
pub fn spawn_xray(binary: &Path, config: &Path, log: &Path) -> Result<Child> {
    let mut cmd = Command::new(binary);
    cmd.args(["run", "-config"]).arg(config);
    spawn_core(cmd, log)
}
static LOG_TASKS: std::sync::Mutex<std::collections::BTreeMap<u32, Vec<tokio::task::JoinHandle<()>>>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
fn spawn_core(mut cmd: Command, log: &Path) -> Result<Child> {
    let args: Vec<_> = cmd.as_std().get_args().collect();
    let secrets = args.windows(2).find(|args| args[0] == "-config" || args[0] == "-f")
        .and_then(|args| std::fs::read(args[1]).ok()).and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|value| crate::diagnostics::secrets(&value)).unwrap_or_default();
    crate::diagnostics::append(log, "代理组件启动；此前日志保留")?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = spawn_owned(cmd)?;
    let tasks = vec![crate::diagnostics::capture(child.stdout.take().unwrap(), log.to_path_buf(), secrets.clone()),
        crate::diagnostics::capture(child.stderr.take().unwrap(), log.to_path_buf(), secrets)];
    let mut logs = LOG_TASKS.lock().unwrap();
    logs.retain(|_,tasks| !tasks.iter().all(|task| task.is_finished()));
    logs.insert(child.id().unwrap(),tasks);
    Ok(child)
}

pub fn spawn_owned(mut cmd: Command) -> Result<Child> {
    #[cfg(target_os = "linux")]
    let parent = unsafe { libc::getpid() };
    // Managed cores must exit even when their owning Agent is killed.
    #[cfg(target_os = "linux")]
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
    let child = cmd.spawn().context("无法启动代理组件")?;
    #[cfg(target_os = "macos")]
    let mut child = child;
    #[cfg(target_os = "macos")]
    {
        let watcher = Command::new(std::env::current_exe()?)
            .args([
                "--chain-watch",
                &std::process::id().to_string(),
                &child.id().unwrap().to_string(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        match watcher {
            Ok(mut watcher) => {
                use std::os::fd::AsRawFd;
                let pipe = watcher.stdout.take().unwrap();
                let mut poll = libc::pollfd {
                    fd: pipe.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let mut ready = 0u8;
                let registered = unsafe {
                    libc::poll(&mut poll, 1, 5000) > 0
                        && libc::read(pipe.as_raw_fd(), (&mut ready as *mut u8).cast(), 1) == 1
                        && ready == b'R'
                };
                if !registered {
                    let _ = watcher.start_kill();
                    let _ = child.start_kill();
                    tokio::spawn(async move {
                        let _ = watcher.wait().await;
                    });
                    bail!("无法注册代理组件的退出监控");
                }
                tokio::spawn(async move {
                    let _ = watcher.wait().await;
                });
            }
            Err(error) => {
                let _ = child.start_kill();
                return Err(error.into());
            }
        }
    }
    Ok(child)
}

#[cfg(target_os = "macos")]
pub fn watch_parent() -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--chain-watch") {
        return Ok(());
    }
    ensure!(args.len() == 4, "无效的进程监控参数");
    let parent: i32 = args[2].parse()?;
    let child: i32 = args[3].parse()?;
    ensure!(parent > 1 && child > 1, "无效的进程 ID");
    // kqueue retains process identity, so PID reuse cannot turn an exit into a new owner.
    unsafe {
        let fd = libc::kqueue();
        ensure!(fd >= 0, "无法创建进程监控队列");
        let queue = OwnedFd::from_raw_fd(fd);
        for pid in [child, parent] {
            let event = libc::kevent {
                ident: pid as _,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            if libc::kevent(
                queue.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            ) < 0
            {
                if pid == child {
                    std::process::exit(0);
                }
                libc::kill(child, libc::SIGTERM);
                break;
            }
        }
        let mut stopping = libc::getppid() != parent;
        if stopping {
            libc::kill(child, libc::SIGTERM);
        }
        std::io::stdout().write_all(b"R")?;
        std::io::stdout().flush()?;
        loop {
            let mut event: libc::kevent = std::mem::zeroed();
            let timeout = libc::timespec {
                tv_sec: 3,
                tv_nsec: 0,
            };
            let count = libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                if stopping { &timeout } else { std::ptr::null() },
            );
            if count == 0 {
                libc::kill(child, libc::SIGKILL);
                break;
            }
            if count < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                libc::kill(child, libc::SIGTERM);
                break;
            }
            if event.ident == child as usize {
                break;
            }
            libc::kill(child, libc::SIGTERM);
            stopping = true;
        }
    }
    std::process::exit(0);
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
        let log_pid = c.id();
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
        let tasks = log_pid.and_then(|pid| LOG_TASKS.lock().unwrap().remove(&pid)).unwrap_or_default();
        for mut task in tasks {
            if tokio::time::timeout(Duration::from_secs(2), &mut task).await.is_err() { task.abort(); let _ = task.await; }
        }
    }
}

pub struct ManagedXray {
    pub binary: PathBuf,
    pub dir: PathBuf,
    pub child: Option<Child>,
    mihomo: Option<Child>,
    legacy: Vec<Option<Child>>,
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
            legacy: Vec::new(),
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
        for child in &mut self.legacy {
            if !matches!(child.as_mut().map(|c| c.try_wait()), Some(Ok(None))) {
                return false;
            }
        }
        if self.gateway.as_ref().is_some_and(|g| !g.running()) {
            eprintln!("Runtime stopped: Gateway listener or policy task finished");
            return false;
        }
        if let Some(child) = &mut self.mihomo {
            match child.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => {
                    eprintln!("Runtime stopped: Mihomo exited with {status}");
                    return false;
                }
                Err(error) => {
                    eprintln!(
                        "Runtime stopped: Mihomo status error={error} errno={:?}",
                        error.raw_os_error()
                    );
                    return false;
                }
            }
        }
        let Some(child) = &mut self.child else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                eprintln!("Runtime stopped: Xray exited with {status}");
                false
            }
            Err(error) => {
                eprintln!(
                    "Runtime stopped: Xray status error={error} errno={:?}",
                    error.raw_os_error()
                );
                false
            }
        }
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
        for profile in crate::legacy_config::profiles(config)? {
            let dir = self.dir.join(format!("legacy-{}", profile.id));
            private_dir(&dir, "chain-legacy-config")?;
            let path = dir.join("config.json");
            atomic_write(
                &path,
                &serde_json::to_vec_pretty(&crate::legacy_config::runtime_config(&profile))?,
            )?;
            let mut command = Command::new(&self.binary);
            command.args(["run", "-config"]).arg(&path);
            if !profile.asset_dir.is_empty() {
                command.env("XRAY_LOCATION_ASSET", &profile.asset_dir);
            }
            self.legacy
                .push(Some(spawn_core(command, &dir.join("xray.log"))?));
        }
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
        let reality = !self.legacy.is_empty()
            || config["inbounds"].as_array().is_some_and(|inbounds| {
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
            let mut names = vec!["xray.log".to_owned(), "mihomo/mihomo.log".to_owned()];
            names.extend(
                crate::legacy_config::profiles(config)?
                    .iter()
                    .map(|p| format!("legacy-{}/xray.log", p.id)),
            );
            let logs: Vec<_> = names
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
            #[cfg(target_os = "linux")]
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
                #[cfg(target_os = "linux")]
                let address = format!("0100007F:{:04X}", listener["port"].as_u64().unwrap());
                #[cfg(target_os = "linux")]
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
                #[cfg(target_os = "macos")]
                let bound = {
                    let mut command = Command::new("/usr/sbin/lsof");
                    command.args(["-nP", "-a", "-p", &pid.to_string(), "-i", "-FfpnPT"]).kill_on_drop(true);
                    let result = tokio::time::timeout(Duration::from_secs(5), command.output()).await??;
                    ensure!(result.status.success(), "无法检测 Mihomo 监听端口归属");
                    lsof_listener(&String::from_utf8_lossy(&result.stdout), table, listener["port"].as_u64().unwrap() as u16)
                };
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
        for profile in crate::legacy_config::profiles(&desired.config)? {
            let directory = tempfile::tempdir_in(&self.dir)?;
            let candidate = directory.path().join("config.json");
            atomic_write(
                &candidate,
                &serde_json::to_vec(&crate::legacy_config::runtime_config(&profile))?,
            )?;
            validate_legacy(&self.binary, &candidate, &profile.asset_dir)
                .await
                .with_context(|| format!("旧配置 {} 预检失败", profile.name))?;
        }
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
                && old.config.get("_legacy") == desired.config.get("_legacy")
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
        for child in &mut self.legacy {
            stop(child).await;
        }
        self.legacy.clear();
    }
}

#[cfg(any(target_os = "macos", test))]
fn lsof_listener(text: &str, protocol: &str, port: u16) -> bool {
    text.split("\nf").skip(1).any(|record| {
        let lines: Vec<_> = record.lines().collect();
        lines.contains(&format!("P{}", protocol.to_uppercase()).as_str())
            && lines.contains(&format!("n127.0.0.1:{port}").as_str())
            && (protocol == "udp" || lines.contains(&"TST=LISTEN"))
    })
}

#[derive(serde::Deserialize)]
pub struct ProbeRequest {
    pub mode: String,
    #[serde(default)]
    pub url: String,
}

fn probe_share(line: &Line, doc: &Document, local: bool) -> Result<String> {
    if !local {
        return compiler::share(line, doc);
    }
    // A NAT host may not be able to reach its own public address.
    let mut line = line.clone();
    line.access.host = "127.0.0.1".into();
    line.access.public_port = Some(line.hops.first().context("无入口")?.port);
    compiler::share(&line, doc)
}

pub async fn probe(
    binary: &Path,
    line: &Line,
    doc: &Document,
    request: &ProbeRequest,
    local_node: Option<&str>,
) -> Result<Value> {
    let label = probe_label(line, doc, request, local_node)?;
    let mut result = probe_inner(binary, line, doc, request, local_node)
        .await
        .map_err(|e| anyhow::anyhow!("{label}\n{}", crate::diagnostics::redact(&format!("{e:#}"))))?;
    result["detail"] = json!(format!("{label}\n{}", result["detail"].as_str().unwrap_or("")));
    Ok(result)
}

fn probe_label(line: &Line, doc: &Document, request: &ProbeRequest, local_node: Option<&str>) -> Result<String> {
    let entry = line.hops.first().context("无入口")?;
    let node = doc.nodes.iter().find(|n| n.id == entry.node_id).context("入口节点不存在")?;
    let local = local_node == Some(node.id.as_str());
    let (endpoint, sni) = if line.kind == Kind::Vless && request.mode == "http" {
        let proxy = crate::external::mihomo(&probe_share(line, doc, local)?, "probe")?;
        (compiler::address(proxy["server"].as_str().context("入口地址为空")?, proxy["port"].as_u64().context("入口端口为空")? as u16),
         proxy["servername"].as_str().or_else(|| proxy["sni"].as_str()).unwrap_or("").to_string())
    } else if line.kind == Kind::Website && request.mode == "http" {
        (compiler::address(&line.access.tls.domain, entry.port), line.access.tls.domain.clone())
    } else {
        (compiler::address(if local { "127.0.0.1" } else { &node.host }, entry.port), String::new())
    };
    let target = if request.mode == "http" && line.kind == Kind::Website {
        format!("{}://{}/", if matches!(line.access.tls.mode.as_str(), "auto" | "manual") { "https" } else { "http" }, compiler::address(&line.access.tls.domain, entry.port))
    } else { request.url.clone() };
    Ok(format!("{}：{} {endpoint}{}{}", line.name,
        if local && line.kind != Kind::Website { "本机入口" } else { "公网入口" },
        if sni.is_empty() { String::new() } else { format!(" · SNI {sni}") },
        if target.is_empty() { String::new() } else { format!(" → {target}") }))
}

async fn probe_inner(
    binary: &Path,
    line: &Line,
    doc: &Document,
    request: &ProbeRequest,
    local_node: Option<&str>,
) -> Result<Value> {
    let start = std::time::Instant::now();
    ensure!(line.enabled, "线路已停用");
    let entry = line.hops.first().context("无入口")?;
    let node = doc
        .nodes
        .iter()
        .find(|n| n.id == entry.node_id)
        .context("入口节点不存在")?;
    let local = local_node == Some(node.id.as_str());
    let endpoint = compiler::address(if local { "127.0.0.1" } else { &node.host }, entry.port);
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
            let share = if line.kind == Kind::Vless {
                probe_share(line, doc, local)?
            } else {
                String::new()
            };
            let outbound = if line.kind == Kind::Vless {
                compiler::external_outbound(&share, "probe")?
            } else {
                json!({"tag":"probe","protocol":"freedom","settings":{"redirect":endpoint}})
            };
            let sidecar = compiler::needs_mihomo(&outbound);
            let config = if sidecar {
                let proxy = crate::external::mihomo(&share, "probe")?;
                json!({"listeners":[{"name":"probe-in","type":"socks","listen":"127.0.0.1","port":port,"proxy":"probe"}],"proxies":[proxy],"mode":"rule","rules":["MATCH,REJECT"],"log-level":"warning"})
            } else {
                json!({"log":{"loglevel":"debug"},"inbounds":[{"listen":"127.0.0.1","port":port,"protocol":"socks","settings":{"auth":"noauth","udp":false}}],"outbounds":[outbound]})
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
                    .redirect(reqwest::redirect::Policy::limited(5))
                    .build()?;
                let response = client.get(url).send().await?;
                ensure!(
                    response.status().is_success(),
                    "已收到响应，但 HTTP 状态为 {}",
                    response.status()
                );
                Ok::<_, anyhow::Error>(format!(
                    "{} → 完整线路 → 探测目标，HTTP {}（不代表 UDP 已验证）",
                    if local {
                        "本机入口"
                    } else {
                        "实际入口"
                    },
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

    #[tokio::test]
    #[ignore = "requires CHAIN_TEST_XRAY and isolated loopback ports"]
    async fn http_probe_follows_redirects_but_rejects_loops_and_http_errors() -> Result<()> {
        let binary = PathBuf::from(std::env::var("CHAIN_TEST_XRAY")?);
        let dir = tempfile::tempdir()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0u8; 4096];
                let n = stream.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]);
                let response = if request.starts_with("GET /start ") {
                    "HTTP/1.1 301 Moved Permanently\r\nLocation: /finish\r\n"
                } else if request.starts_with("GET /loop ") {
                    "HTTP/1.1 302 Found\r\nLocation: /loop\r\n"
                } else if request.starts_with("GET /error ") {
                    "HTTP/1.1 503 Service Unavailable\r\n"
                } else { "HTTP/1.1 200 OK\r\n" };
                let _ = stream.write_all(format!("{response}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
            }
        });
        let node: Node = serde_json::from_value(json!({"id":"node","name":"fixture","host":"127.0.0.1",
            "token_hash":"test","reality":Reality::new("example.com:443".into(), "example.com".into())}))?;
        let line: Line = serde_json::from_value(json!({"id":id(),"name":"redirect fixture","kind":"forward","network":"tcp",
            "hops":[{"node_id":"node","port":address.port(),"uuid":id()}]}))?;
        let doc = Document { nodes:vec![node], lines:vec![line.clone()], ..Default::default() };
        let result = async {
            let request = |path| ProbeRequest { mode:"http".into(), url:format!("http://{address}/{path}") };
            let passed = probe(&binary, &line, &doc, &request("start"), None).await?;
            ensure!(passed["ok"] == true && passed["detail"].as_str().unwrap().contains("HTTP 200"), "redirect must reach its final response");
            let loop_error = probe(&binary, &line, &doc, &request("loop"), None).await.unwrap_err();
            ensure!(format!("{loop_error:#}").contains("redirect"), "redirect loops must remain failures");
            let http_error = probe(&binary, &line, &doc, &request("error"), None).await.unwrap_err();
            ensure!(format!("{http_error:#}").contains("503"), "HTTP errors must remain failures");
            atomic_write(&dir.path().join("xray.json"), &serde_json::to_vec(&json!({"outbounds":[{"tag":format!("line-{}-out",line.id),"protocol":"freedom"}]}))?)?;
            let outbound = diagnose_path(&binary, dir.path(), &line.id, &request("start").url).await?;
            ensure!(outbound["result"]["ok"] == true, "outbound diagnosis must follow redirects too");
            Ok::<_, anyhow::Error>(())
        }.await;
        server.abort();
        let _ = server.await;
        let _released = std::net::TcpListener::bind(address)?;
        result
    }

    #[tokio::test]
    #[ignore = "requires CHAIN_TEST_XRAY and isolated loopback ports"]
    async fn legacy_real_core_keeps_two_users_and_recovers_after_rejection() -> Result<()> {
        let binary = PathBuf::from(std::env::var("CHAIN_TEST_XRAY")?);
        let dir = tempfile::tempdir()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let destination = listener.local_addr()?;
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut bytes = [0u8; 4096];
                let _ = stream.read(&mut bytes).await;
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nlegacy-ok").await;
            }
        });
        let port = {
            let socket = std::net::TcpListener::bind("127.0.0.1:0")?;
            socket.local_addr()?.port()
        };
        eprintln!(
            "legacy test directory={} proxy_port={} destination={}",
            dir.path().display(),
            port,
            destination
        );
        let profile = LegacyConfig {
            id: id(),
            node_id: "test".into(),
            name: "3x-ui fixture".into(),
            enabled: true,
            asset_dir: String::new(),
            source_inbounds: Vec::new(),
            config: json!({"log":{"loglevel":"warning"},"inbounds":[{"listen":"127.0.0.1","port":port,"protocol":"http",
                "settings":{"accounts":[{"user":"first","pass":"first-secret"},{"user":"second","pass":"second-secret"}]},
                "sniffing":{"enabled":true,"destOverride":["http","tls"]}}],
                "outbounds":[{"tag":"direct","protocol":"freedom"}],
                "routing":{"rules":[{"type":"field","ip":["127.0.0.0/8"],"outboundTag":"direct"}]},
                "transport":null,"reverse":null,"fakeDns":null}),
        };
        let mut config = compiler::empty_config();
        config["_legacy"] = json!([profile]);
        let desired = Desired {
            revision: id(),
            config,
        };
        let mut runtime = ManagedXray::open(binary.clone(), dir.path().into()).await?;
        let result = async {
            runtime.apply(&desired).await?;
            eprintln!(
                "legacy test core_pids={:?}",
                runtime
                    .legacy
                    .iter()
                    .filter_map(|c| c.as_ref().and_then(|c| c.id()))
                    .collect::<Vec<_>>()
            );
            for user in ["first", "second"] {
                let proxy = reqwest::Proxy::http(format!("http://127.0.0.1:{port}"))?
                    .basic_auth(user, &format!("{user}-secret"));
                let client = reqwest::Client::builder()
                    .proxy(proxy)
                    .timeout(Duration::from_secs(5))
                    .build()?;
                ensure!(
                    client
                        .get(format!("http://{destination}/test"))
                        .send()
                        .await?
                        .text()
                        .await?
                        == "legacy-ok",
                    "用户凭据未保留"
                );
            }
            let old_pid = runtime.legacy[0].as_ref().unwrap().id();
            let mut bad = desired.clone();
            bad.revision = id();
            bad.config["_legacy"][0]["config"]["inbounds"][0]["protocol"] =
                json!("unsupported-fixture");
            ensure!(runtime.apply(&bad).await.is_err(), "无效内核配置必须拒绝");
            ensure!(
                runtime.running() && runtime.legacy[0].as_ref().unwrap().id() == old_pid,
                "预检失败不应停止运行中的代理"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        runtime.close().await;
        let reopened = if result.is_ok() {
            Some(ManagedXray::open(binary, dir.path().into()).await)
        } else {
            None
        };
        let restart_result = match reopened {
            Some(Ok(mut runtime)) => {
                let running = runtime.running();
                runtime.close().await;
                ensure!(running, "重启未恢复迁移配置");
                Ok(())
            }
            Some(Err(error)) => Err(error),
            None => Ok(()),
        };
        server.abort();
        let _ = server.await;
        let _released_proxy = std::net::TcpListener::bind(("127.0.0.1", port))?;
        let _released_destination = std::net::TcpListener::bind(destination)?;
        result?;
        restart_result
    }

    #[test]
    fn local_probe_preserves_credentials_and_sni_without_changing_public_links() {
        let node: Node = serde_json::from_value(json!({
            "id":"local", "name":"local", "host":"198.51.100.1", "token_hash":"",
            "reality":Reality::new("example.com:443".into(), "example.com".into())
        }))
        .unwrap();
        let doc = Document {
            nodes: vec![node],
            ..Default::default()
        };
        let mut line: Line = serde_json::from_value(json!({
            "id":"line", "name":"line", "kind":"vless",
            "hops":[{"node_id":"local", "port":443, "uuid":id()}],
            "access":{"host":"public.example.com", "public_host":"nat.example.com",
                      "public_port":24443, "password":"test-password"}
        }))
        .unwrap();
        for protocol in [Protocol::Vless, Protocol::Vmess, Protocol::Hysteria2] {
            line.access.protocol = protocol.clone();
            if protocol != Protocol::Vless {
                line.access.tls.mode = "manual".into();
                line.access.tls.domain = "example.com".into();
            }
            let public = compiler::share(&line, &doc).unwrap();
            assert_eq!(probe_share(&line, &doc, false).unwrap(), public);
            let mut expected = crate::external::mihomo(&public, "probe").unwrap();
            expected["server"] = json!("127.0.0.1");
            expected["port"] = json!(443);
            let local = probe_share(&line, &doc, true).unwrap();
            assert_eq!(crate::external::mihomo(&local, "probe").unwrap(), expected);
            assert_eq!(compiler::share(&line, &doc).unwrap(), public);
            let request = ProbeRequest { mode: "http".into(), url: "https://check.example/".into() };
            let label = probe_label(&line, &doc, &request, Some("local")).unwrap();
            assert!(label.contains("本机入口 127.0.0.1:443"), "{label}");
            assert!(label.contains("SNI example.com"), "{label}");
            assert!(label.contains("https://check.example/"));
            assert!(!label.contains(&line.hops[0].uuid));
            assert!(!label.contains("test-password"));
            assert!(!label.contains(&doc.nodes[0].reality.short_id));
            assert!(probe_label(&line, &doc, &request, None).unwrap().contains("公网入口 public.example.com:24443"));
        }
    }

    #[test]
    fn macos_listener_requires_matching_socket_and_state() {
        let sockets = "p42\nf5\nPTCP\nn127.0.0.1:54321\nTST=LISTEN\nf6\nPUDP\nn127.0.0.1:54322\n";
        assert!(lsof_listener(sockets, "tcp", 54321));
        assert!(lsof_listener(sockets, "udp", 54322));
        assert!(!lsof_listener(sockets, "tcp", 54322));
        assert!(!lsof_listener(&sockets.replace("LISTEN", "ESTABLISHED"), "tcp", 54321));
        assert!(!lsof_listener(&sockets.replace("127.0.0.1", "*"), "tcp", 54321));
    }

    #[test]
    fn automatic_hysteria_certificates_follow_saved_or_new_core_config() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = ManagedXray {
            binary: "xray".into(),
            dir: dir.path().into(),
            child: None,
            mihomo: None,
            legacy: Vec::new(),
            active: None,
            traffic: crate::gateway::Book::open(dir.path()).unwrap(),
            gateway: None,
        };
        let mut config = json!({
            "inbounds":[{"tag":"line-test-in","protocol":"hysteria","streamSettings":{"security":"tls"}}],
            "_chain":[{"id":"test","port":20000,"backend":50000,"meter":{"port":20001,"backend":50001,"password":"test-secret"},"kind":"vless","protocol":"hysteria2","network":"udp","target":"","limits":{},"tls":{"mode":"auto","domain":"cert.example.com"}}],
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

/// Probe only the applied outbound route, keeping the live listener and accounting bridge intact.
pub async fn diagnose_path(binary: &Path, dir: &Path, line: &str, target: &str) -> Result<Value> {
    uuid::Uuid::parse_str(line)?;
    let url = url::Url::parse(target)?;
    ensure!(matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none(), "请输入不含凭据的 HTTP/HTTPS 地址");
    let config: Value = serde_json::from_slice(&std::fs::read(dir.join("xray.json"))?)?;
    let tag = format!("line-{line}-out");
    ensure!(config["outbounds"].as_array().is_some_and(|o| o.iter().any(|o| o["tag"] == tag)), "该节点没有此代理的已应用出口");
    let temporary = tempfile::tempdir()?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let mut isolated = json!({"log":{"loglevel":"debug"},"inbounds":[{"tag":"diagnostic","listen":"127.0.0.1","port":port,"protocol":"socks","settings":{"auth":"noauth"}}],"outbounds":config["outbounds"],"routing":{"rules":[{"type":"field","inboundTag":["diagnostic"],"outboundTag":tag}]}});
    if config["dns"].is_object() { isolated["dns"] = config["dns"].clone(); }
    let path = temporary.path().join("probe.json"); atomic_write(&path, &serde_json::to_vec(&isolated)?)?;
    validate(binary, &path).await?;
    let log = temporary.path().join("probe.log");
    let mut child = Some(spawn_xray(binary, &path, &log)?);
    let result = async {
        for _ in 0..40 {
            if child.as_mut().unwrap().try_wait()?.is_some() { bail!("诊断内核提前退出"); }
            if TcpStream::connect(("127.0.0.1",port)).await.is_ok() { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let response = reqwest::Client::builder().no_proxy().proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))?).timeout(Duration::from_secs(12)).redirect(reqwest::redirect::Policy::limited(5)).build()?.get(url).send().await?;
        ensure!(response.status().is_success(), "目标返回 HTTP {}",response.status());
        Ok::<_,anyhow::Error>(format!("出口实际收到 HTTP {}；没有验证公网入口或本机计量转发桥",response.status()))
    }.await;
    stop(&mut child).await;
    tokio::task::yield_now().await;
    let detail = match result {Ok(v)=>json!({"ok":true,"detail":v}), Err(e)=>json!({"ok":false,"detail":crate::diagnostics::redact(&format!("{e:#}\n{}",std::fs::read_to_string(&log).unwrap_or_default()))})};
    Ok(json!({"stage":"本机出口 → 后续代理 → 检查网站","result":detail,"checked_at":now(),"note":"独立诊断进程使用 debug 日志，完成即退出；未重启运行中的代理。出口成功而整链失败时，继续检查入口与本机转发。"}))
}
