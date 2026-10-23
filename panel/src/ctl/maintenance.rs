use super::{
    package,
    upgrade::{Manager, OWNER, atomic, command, output},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    action: String,
    #[serde(default)]
    version: String,
    xray_channel: Option<String>,
}
impl Request {
    fn parse(value: Value) -> Result<Self> {
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
        Ok(request)
    }
    fn same(&self, job: &Value) -> bool {
        self.action == job["action"].as_str().unwrap_or("")
            && self.version == job["version"].as_str().unwrap_or("")
            && self.xray_channel.as_deref().unwrap_or("recommended")
                == job["xray_channel"].as_str().unwrap_or("recommended")
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
    let path = manager.path("/etc/systemd/system/chain-panel-https.service");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(info) => {
            ensure!(
                info.is_file()
                    && fs::read_to_string(path)?
                        .starts_with("# chain-panel HTTPS managed site v1\n"),
                "独立 HTTPS 服务归属不匹配"
            );
            Ok(Some("chain-panel-https.service"))
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
    if action == "upgrade" {
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
            command("systemctl", &["restart", service], 70).await?;
        }
        let after = versions(manager)?;
        for role in before.as_object().unwrap().keys() {
            ensure!(after[role] == version, "{role} 版本未达到目标");
        }
        manager.prune()?;
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
            command("systemctl", &["restart", service], 70).await?;
        }
    }
    if let Some(service) = https {
        ensure!(
            output("systemctl", &["is-active", "--quiet", service], 15)
                .await?
                .status
                .success(),
            "独立 HTTPS 服务未运行"
        );
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

pub async fn serve() -> Result<()> {
    use std::os::fd::FromRawFd;
    ensure!(unsafe { libc::geteuid() } == 0, "维护服务须由 systemd 启动");
    ensure!(
        std::env::var("LISTEN_PID").ok().as_deref() == Some(&std::process::id().to_string())
            && std::env::var("LISTEN_FDS").as_deref() == Ok("1"),
        "需要 systemd socket activation"
    );
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
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) };
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
                        Ok(()) => work["state"] = json!("succeeded"),
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
