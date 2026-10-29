use super::{
    package,
    upgrade::{Manager, OWNER, atomic, output},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    action: String,
    #[serde(default)]
    version: String,
    xray_channel: Option<String>,
    congestion_control: Option<String>,
    qdisc: Option<String>,
    core: Option<String>,
    security_update: Option<bool>,
    rollback_safe: Option<bool>,
    prerelease: Option<bool>,
    task_id: Option<String>,
}
impl Request {
    fn parse(value: Value) -> Result<Self> {
        if value["action"]
            .as_str()
            .is_some_and(|a| a.starts_with("core-") && a != "core-status")
        {
            super::cores::validate(&value)?;
        } else {
            ensure!(
                ![
                    "core",
                    "security_update",
                    "rollback_safe",
                    "prerelease",
                    "task_id"
                ]
                .iter()
                .any(|k| value.get(*k).is_some()),
                "只有内核操作可指定内核参数"
            );
        }
        let request: Self = serde_json::from_value(value)?;
        ensure!(
            uuid::Uuid::parse_str(&request.id)?.to_string() == request.id,
            "任务 ID 无效"
        );
        ensure!(
            matches!(
                request.action.as_str(),
                "status"
                    | "upgrade"
                    | "restart-all"
                    | "restart-panel"
                    | "restart-agent"
                    | "sync-time"
                    | "network-status"
                    | "network-tune"
                    | "core-status"
                    | "core-upgrade"
                    | "core-rollback"
                    | "core-confirm"
                    | "core-reject"
            ),
            "不支持的维护操作"
        );
        if request.action == "upgrade" {
            package::version(&request.version)?;
            ensure!(
                request
                    .xray_channel
                    .as_deref()
                    .is_none_or(|v| matches!(v, "recommended" | "latest")),
                "Xray 来源无效"
            );
        } else {
            ensure!(request.xray_channel.is_none(), "只有升级操作可选择内核来源");
        }
        if request.action == "network-tune" {
            let cc = request.congestion_control.as_deref().unwrap_or("");
            ensure!(
                !cc.is_empty()
                    && cc.len() <= 32
                    && cc.starts_with(|c: char| c.is_ascii_lowercase())
                    && cc
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
                "拥塞算法无效"
            );
            ensure!(
                matches!(
                    request.qdisc.as_deref(),
                    Some("fq" | "fq_codel" | "fq_pie" | "cake" | "pfifo_fast")
                ),
                "队列无效"
            );
        } else {
            ensure!(
                request.congestion_control.is_none() && request.qdisc.is_none(),
                "只有网络设置可选择算法或队列"
            );
        }
        Ok(request)
    }
    fn same(&self, job: &Value) -> bool {
        self.action == job["action"].as_str().unwrap_or("")
            && self.version == job["version"].as_str().unwrap_or("")
            && self.xray_channel.as_deref().unwrap_or("recommended")
                == job["xray_channel"].as_str().unwrap_or("recommended")
            && self.congestion_control.as_deref() == job["congestion_control"].as_str()
            && self.qdisc.as_deref() == job["qdisc"].as_str()
            && self.core.as_deref() == job["core"].as_str()
            && self.security_update == job["security_update"].as_bool()
            && self.rollback_safe == job["rollback_safe"].as_bool()
            && self.prerelease == job["prerelease"].as_bool()
            && self.task_id.as_deref() == job["task_id"].as_str()
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn write(path: &Path, value: &Value) -> Result<()> {
    atomic(path, &serde_json::to_vec(value)?, 0o600)
}

fn jobs(directory: &Path) -> Result<Vec<(PathBuf, Value)>> {
    let mut jobs = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("任务文件名无效")?;
        ensure!(
            uuid::Uuid::parse_str(id)?.to_string() == id,
            "任务文件名无效"
        );
        let info = fs::symlink_metadata(&path)?;
        ensure!(
            info.is_file() && info.uid() == unsafe { libc::geteuid() } && info.len() <= 65536,
            "任务文件归属、类型或大小无效"
        );
        let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
        ensure!(value["id"] == id, "任务文件与 ID 不匹配");
        jobs.push((path, value));
    }
    Ok(jobs)
}

fn receipt(directory: &Path, request: &Request) -> Result<(Value, bool)> {
    let mut existing = jobs(directory)?;
    if let Some((_, job)) = existing.iter().find(|(_, job)| job["id"] == request.id) {
        ensure!(
            request.action == "status" || request.same(job),
            "任务 ID 已用于其他操作"
        );
        return Ok((job.clone(), false));
    }
    if request.action == "status" {
        return Ok((json!({"id":request.id,"state":"not_found"}), false));
    }
    ensure!(
        !existing
            .iter()
            .any(|(_, job)| matches!(job["state"].as_str(), Some("queued" | "running"))),
        "已有维护任务正在执行"
    );
    let mut job = json!({"id":request.id,"action":request.action,"version":request.version,"state":"queued","created":now()});
    if let Some(channel) = &request.xray_channel {
        job["xray_channel"] = json!(channel);
    }
    if request.action == "network-tune" {
        job["congestion_control"] = json!(request.congestion_control);
        job["qdisc"] = json!(request.qdisc);
    }
    let fields = serde_json::to_value(request)?;
    for key in [
        "core",
        "security_update",
        "rollback_safe",
        "prerelease",
        "task_id",
    ] {
        if !fields[key].is_null() {
            job[key] = fields[key].clone();
        }
    }
    if request.action.starts_with("core-") && request.version.is_empty() {
        job.as_object_mut().unwrap().remove("version");
    }
    write(&directory.join(format!("{}.json", request.id)), &job)?;
    existing.sort_by_key(|(_, job)| job["finished"].as_u64().unwrap_or(0));
    for (path, old) in existing.iter().take(existing.len().saturating_sub(200)) {
        if old["finished"]
            .as_u64()
            .is_some_and(|t| now().saturating_sub(t) > 30 * 86400)
        {
            fs::remove_file(path)?;
        }
    }
    Ok((job, true))
}

fn versions(manager: &Manager) -> Result<Value> {
    Ok(Value::Object(
        manager.state()?["roles"]
            .as_object()
            .context("安装状态无效")?
            .iter()
            .map(|(role, item)| (role.clone(), item["version"].clone()))
            .collect(),
    ))
}

fn https_service(manager: &Manager) -> Result<Option<&'static str>> {
    let path = manager.unit_path("panel-https");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(info) => {
            ensure!(
                info.is_file()
                    && fs::read_to_string(path)?.contains("# chain-panel HTTPS managed site v1\n"),
                "独立 HTTPS 服务归属不匹配"
            );
            Ok(Some("chain-panel-https"))
        }
    }
}

async fn execute(manager: &Manager, directory: &Path, job: &mut Value) -> Result<()> {
    let before = versions(manager)?;
    ensure!(
        before.as_object().is_some_and(|v| !v.is_empty()),
        "未安装组件"
    );
    job["state"] = json!("running");
    job["before"] = before.clone();
    job["started"] = json!(now());
    let path = directory.join(format!("{}.json", job["id"].as_str().unwrap()));
    write(&path, job)?;
    let action = job["action"].as_str().unwrap().to_owned();
    let https = if matches!(action.as_str(), "upgrade" | "restart-all") {
        https_service(manager)?
    } else {
        None
    };
    if action.starts_with("core-") {
        let result = super::cores::operate(manager, job, directory).await?;
        for (key, value) in result.as_object().context("内核结果无效")? {
            job[key] = value.clone();
        }
    } else if action == "upgrade" {
        let version = job["version"].as_str().context("缺少目标版本")?.to_string();
        ensure!(before["panel"].is_string(), "未安装面板");
        for old in before.as_object().unwrap().values() {
            ensure!(
                package::version(&version)?
                    >= package::version(old.as_str().context("组件版本无效")?)?,
                "不允许自动降级组件或数据库"
            );
        }
        let temporary = tempfile::Builder::new()
            .prefix(&format!("work-{}-", job["id"].as_str().unwrap()))
            .tempdir_in(directory)?;
        let (archive, _) = package::download(temporary.path(), Some(&version)).await?;
        let stage = temporary.path().join("stage");
        let manifest = package::extract(&archive, &stage)?;
        ensure!(manifest.version == version, "目标版本与安装包不一致");
        if job["xray_channel"] == "latest" {
            package::latest_xray(&stage, temporary.path()).await?;
        }
        manager.update(&stage, None).await?;
        if let Some(service) = https {
            super::platform::related(manager, "restart", service).await?;
        }
        let after = versions(manager)?;
        for role in before.as_object().unwrap().keys() {
            ensure!(after[role] == version, "{role} 版本未达到目标");
        }
        super::lifecycle::handoff(manager)?;
        super::lifecycle::maintenance(manager).await?;
        manager.prune().await?;
    } else if action == "network-tune" {
        job["network"] = network(
            manager,
            &[
                job["congestion_control"].as_str().context("缺少拥塞算法")?,
                job["qdisc"].as_str().context("缺少队列")?,
            ],
        )
        .await?;
        job["detail"] = job["network"]["detail"].clone();
    } else if action == "sync-time" {
        job["detail"] = json!(super::time::synchronize().await?);
    } else {
        let _guard = manager.lock()?;
        let roles = if action == "restart-all" {
            before
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        } else {
            vec![
                action
                    .strip_prefix("restart-")
                    .context("维护操作无效")?
                    .into(),
            ]
        };
        for role in &roles {
            ensure!(before[role].is_string(), "该组件未安装");
            manager.service("restart", role).await?;
            ensure!(manager.active(role).await?, "chain-{role} 服务未运行");
        }
        if let Some(service) = https {
            super::platform::related(manager, "restart", service).await?;
        }
    }
    if let Some(service) = https {
        let check = if manager.kind() == "openrc" {
            output("rc-service", &[service, "status"], 15).await?
        } else {
            output("systemctl", &["is-active", "--quiet", service], 15).await?
        };
        ensure!(check.status.success(), "独立 HTTPS 服务未运行");
    }
    job["after"] = versions(manager)?;
    if job["detail"].is_null() {
        job["detail"] = json!("组件检查通过；总控还需检查代理整链");
    }
    Ok(())
}

fn redacted(error: &str) -> String {
    error
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "://",
                "token",
                "password",
                "passwd",
                "secret",
                "authorization",
                "api_key",
                "api-key",
            ]
            .iter()
            .any(|v| lower.contains(v))
            {
                "[可能包含地址或凭据的错误行已隐藏]".into()
            } else {
                line.chars().filter(|c| !c.is_control()).collect::<String>()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(4096)
        .collect()
}

async fn network(manager: &Manager, args: &[&str]) -> Result<Value> {
    ensure!(manager.live(), "离线安装树不能修改宿主机网络");
    match args {
        ["status"] => super::tuning::status().await,
        [cc, qdisc] => super::tuning::apply(cc, qdisc).await,
        _ => anyhow::bail!("网络参数无效"),
    }
}

pub async fn serve() -> Result<()> {
    use std::os::fd::FromRawFd;
    ensure!(unsafe { libc::geteuid() } == 0, "维护服务需要管理员身份");
    let manager = Arc::new(Manager::open(Path::new("/"))?);
    let directory = manager.path("/var/lib/chain-maintenance");
    let info = fs::symlink_metadata(&directory)?;
    let marker = directory.join(".chain-owner");
    ensure!(
        info.is_dir()
            && info.uid() == 0
            && info.gid() == 0
            && info.mode() & 0o777 == 0o700
            && fs::symlink_metadata(&marker)?.is_file()
            && fs::read_to_string(marker)? == OWNER,
        "维护目录归属或权限不匹配"
    );
    for (path, mut job) in jobs(&directory)? {
        if matches!(job["state"].as_str(), Some("queued" | "running")) {
            job["state"] = json!("unknown");
            job["error"] = json!("执行组件中断，请检查当前版本后重新创建任务");
            write(&path, &job)?;
        }
        let prefix = format!("work-{}-", job["id"].as_str().unwrap());
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let info = fs::symlink_metadata(entry.path())?;
                ensure!(
                    info.is_dir() && info.uid() == 0,
                    "维护临时目录归属不匹配，未清理"
                );
                fs::remove_dir_all(entry.path())?;
            }
        }
    }
    let user = unsafe { libc::getpwnam(c"chain-panel".as_ptr()) };
    ensure!(!user.is_null(), "缺少 chain-panel 服务账户");
    let panel_uid = unsafe { (*user).pw_uid };
    ensure!(panel_uid != 0, "chain-panel 不能是 root 账户");
    let _listener_lock;
    let listener = if manager.kind() == "openrc" {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        };
        let runtime = manager.path("/run/chain-maintenance");
        super::upgrade::ordinary(&runtime)?;
        if !runtime.exists() {
            fs::create_dir(&runtime)?;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755))?;
        }
        let info = fs::symlink_metadata(&runtime)?;
        ensure!(
            info.is_dir() && info.uid() == 0 && info.mode() & 0o022 == 0,
            "维护运行目录归属无效"
        );
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(runtime.join("listener.lock"))?;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "已有维护监听器"
        );
        _listener_lock = Some(lock);
        let socket = runtime.join("control.sock");
        if let Ok(meta) = fs::symlink_metadata(&socket) {
            ensure!(
                meta.file_type().is_socket() && meta.uid() == 0,
                "已有 socket 不属于维护服务"
            );
            fs::remove_file(&socket)?;
        }
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        let path = std::ffi::CString::new(socket.to_str().context("socket 路径无效")?)?;
        ensure!(
            unsafe { libc::chown(path.as_ptr(), 0, (*user).pw_gid) } == 0,
            "无法设置新 socket 属组"
        );
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o660))?;
        listener
    } else {
        _listener_lock = None;
        ensure!(
            std::env::var("LISTEN_PID").ok().as_deref() == Some(&std::process::id().to_string())
                && std::env::var("LISTEN_FDS").as_deref() == Ok("1"),
            "需要 systemd socket activation"
        );
        unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) }
    };
    listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(listener)?;
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let (finished, mut completion) = tokio::sync::mpsc::channel::<()>(1);
    loop {
        let stream = tokio::select! {
            result = listener.accept() => result?.0,
            _ = completion.recv() => break,
        };
        let uid = stream.peer_cred()?.uid();
        let (read, mut writer) = stream.into_split();
        let response: Result<Value> = async {
            ensure!(uid == 0 || uid == panel_uid, "未授权的本机进程");
            let mut line = Vec::new();
            tokio::time::timeout(
                Duration::from_secs(3),
                tokio::io::BufReader::new(read)
                    .take(8193)
                    .read_until(b'\n', &mut line),
            )
            .await??;
            ensure!(
                line.len() <= 8192 && line.last() == Some(&b'\n'),
                "请求过大或不完整"
            );
            let request = Request::parse(serde_json::from_slice(&line)?)?;
            if request.action == "core-status" {
                return super::cores::status(&manager);
            }
            if request.action == "network-status" {
                return network(&manager, &["status"]).await;
            }
            let _guard = lock.lock().await;
            let (job, start) = receipt(&directory, &request)?;
            if start {
                let (manager, directory, finished) =
                    (manager.clone(), directory.clone(), finished.clone());
                let mut work = job.clone();
                tokio::spawn(async move {
                    // Do not cancel a transaction halfway through rollback; each network/command step has a deadline.
                    let result = execute(&manager, &directory, &mut work).await;
                    match result {
                        Ok(()) => {
                            if work["state"] != "awaiting_verification" {
                                work["state"] = json!("succeeded");
                            }
                        }
                        Err(error) => {
                            work["state"] = json!("failed");
                            work["error"] = json!(redacted(&format!("{error:#}")));
                        }
                    }
                    if let Ok(after) = versions(&manager) {
                        work["after"] = after;
                    }
                    work["finished"] = json!(now());
                    if let Err(error) =
                        write(&directory.join(format!("{}.json", request.id)), &work)
                    {
                        eprintln!("维护结果写入失败：{error:#}");
                    }
                    if request.action == "upgrade" {
                        let _ = finished.send(()).await;
                    }
                });
            }
            Ok(job)
        }
        .await;
        let value = response.unwrap_or_else(|error| json!({"error":redacted(&error.to_string())}));
        let bytes = format!("{value}\n");
        let _ =
            tokio::time::timeout(Duration::from_secs(3), writer.write_all(bytes.as_bytes())).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn network_tuning_validates_choices_and_receipt_identity() {
        let directory = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let payload =
            json!({"id":id,"action":"network-tune","congestion_control":"bbr","qdisc":"fq"});
        let request = Request::parse(payload.clone()).unwrap();
        assert!(receipt(directory.path(), &request).unwrap().1);
        assert!(!receipt(directory.path(), &request).unwrap().1);
        let mut other = payload.clone();
        other["qdisc"] = json!("cake");
        assert!(receipt(directory.path(), &Request::parse(other).unwrap()).is_err());
        for (key, value) in [
            ("congestion_control", "bbr;id"),
            ("qdisc", "../fq"),
            ("action", "restart-all"),
        ] {
            let mut invalid = payload.clone();
            invalid[key] = json!(value);
            assert!(Request::parse(invalid).is_err());
        }
    }
    #[test]
    fn strict_request_boundary() {
        let id = uuid::Uuid::new_v4().to_string();
        assert!(
            Request::parse(
                json!({"id":id,"action":"upgrade","version":"0.3.0","xray_channel":"latest"})
            )
            .is_ok()
        );
        for value in [
            json!({"id":"../bad","action":"status"}),
            json!({"id":id,"action":"shell"}),
            json!({"id":id,"action":"restart-all","command":"id"}),
            json!({"id":id,"action":"restart-all","xray_channel":"latest"}),
            json!({"id":id,"action":"upgrade","version":"latest"}),
        ] {
            assert!(Request::parse(value).is_err());
        }
    }
    #[test]
    fn receipts_are_idempotent_and_busy_work_is_not_replayed() {
        let directory = tempfile::tempdir().unwrap();
        let request =
            Request::parse(json!({"id":uuid::Uuid::new_v4(),"action":"upgrade","version":"0.3.0"}))
                .unwrap();
        assert!(receipt(directory.path(), &request).unwrap().1);
        assert!(!receipt(directory.path(), &request).unwrap().1);
        let mut other = request.clone();
        other.version = "0.3.1".into();
        assert!(receipt(directory.path(), &other).is_err());
        other.id = uuid::Uuid::new_v4().to_string();
        assert!(receipt(directory.path(), &other).is_err());
        other.action = "status".into();
        assert_eq!(
            receipt(directory.path(), &other).unwrap().0["state"],
            "not_found"
        );
        fs::remove_file(directory.path().join(format!("{}.json", request.id))).unwrap();
        std::os::unix::fs::symlink(
            "/etc/passwd",
            directory.path().join(format!("{}.json", request.id)),
        )
        .unwrap();
        assert!(receipt(directory.path(), &request).is_err());
    }
    #[test]
    fn error_receipts_do_not_expose_credentials() {
        let text = redacted(
            "connection failed\nhttps://user:secret@example.test/?token=secret\nAuthorization: Bearer secret\npassword=secret\nsecret-token: value",
        );
        assert!(text.contains("connection failed"));
        for secret in ["secret", "example.test", "Bearer", "value"] {
            assert!(!text.contains(secret));
        }
    }
}
