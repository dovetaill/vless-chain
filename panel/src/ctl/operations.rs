use super::{
    package,
    upgrade::{Manager, atomic, ordinary},
};
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

pub fn confirm(message: &str, word: &str, non_interactive: bool) -> Result<()> {
    if non_interactive {
        return Ok(());
    }
    print!("{message} 输入 {word} 确认：");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    ensure!(input.trim() == word, "已取消操作");
    Ok(())
}

fn backup_files<W: Write>(
    bundle: &mut tar::Builder<W>,
    directory: &Path,
    current: &Path,
    prefix: &str,
) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        let relative = path.strip_prefix(directory)?;
        if kind.is_symlink() {
            let target = fs::read_link(&path)?;
            ensure!(
                prefix == "data"
                    && relative.starts_with("acme/config/live")
                    && target.is_relative()
                    && path
                        .canonicalize()?
                        .starts_with(directory.join("acme/config/archive").canonicalize()?)
                    && path.is_file(),
                "备份路径包含非 Certbot 内部证书链接：{}",
                path.display()
            );
            bundle.append_path_with_name(&path, Path::new(prefix).join(relative))?;
        } else if kind.is_dir() {
            backup_files(bundle, directory, &path, prefix)?;
        } else {
            ensure!(kind.is_file(), "备份目录含特殊文件：{}", path.display());
            if !matches!(
                entry.file_name().to_str(),
                Some("xray.log" | "service.log" | "process.lock" | "candidate.json")
            ) {
                bundle.append_path_with_name(&path, Path::new(prefix).join(relative))?;
            }
        }
    }
    Ok(())
}

pub fn backup(manager: &Manager, role: &str) -> Result<std::path::PathBuf> {
    ensure!(matches!(role, "panel" | "agent"), "组件无效");
    let directory = manager.path("/var/backups/chain-panel");
    ordinary(&directory)?;
    if !directory.exists() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
    }
    use std::os::unix::fs::MetadataExt;
    let info = fs::metadata(&directory)?;
    ensure!(
        info.uid() == unsafe { libc::geteuid() } && info.mode() & 0o077 == 0,
        "备份目录属主或权限不符合私有备份要求，未修改已有权限"
    );
    let name = format!(
        "{role}-{}-{}.tar.gz",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    let encoder =
        flate2::write::GzEncoder::new(temporary.as_file_mut(), flate2::Compression::default());
    let mut bundle = tar::Builder::new(encoder);
    bundle.follow_symlinks(false);
    for (prefix, path) in [
        ("config", manager.path(&format!("/etc/chain-{role}"))),
        ("data", manager.path(&format!("/var/lib/chain-{role}"))),
    ] {
        ordinary(&path)?;
        if path.exists() {
            backup_files(&mut bundle, &path, &path, prefix)?;
        }
    }
    bundle.into_inner()?.finish()?.sync_all()?;
    let path = directory.join(name);
    temporary.persist_noclobber(&path)?;
    File::open(&directory)?.sync_all()?;
    println!(
        "私有备份：{}（权限 600，含连接凭据，请私下保存）",
        path.display()
    );
    Ok(path)
}

pub async fn backup_stopped(manager: &Manager, role: &str) -> Result<()> {
    let _guard = manager.lock()?;
    ensure!(
        manager.state()?["roles"][role].is_object(),
        "尚未安装指定组件"
    );
    let active = manager.active(role).await?;
    manager.service("stop", role).await?;
    let result = backup(manager, role);
    if active {
        manager
            .service("start", role)
            .await
            .context("备份结束后恢复服务失败，请检查服务状态")?;
    }
    result?;
    Ok(())
}

pub async fn reset_auth(manager: &Manager, non_interactive: bool) -> Result<()> {
    confirm(
        "此操作清除 TOTP、Passkey 和全部会话，保留管理员密钥与业务线路。",
        "RESET",
        non_interactive,
    )?;
    let _guard = manager.lock()?;
    ensure!(
        manager.state()?["roles"]["panel"].is_object(),
        "尚未安装面板"
    );
    let path = manager.path("/var/lib/chain-panel/panel.db");
    ordinary(&path)?;
    ensure!(path.is_file(), "控制台数据库不存在");
    let active = manager.active("panel").await?;
    manager.service("stop", "panel").await?;
    let result: Result<()> = async {
        backup(manager, "panel")?;
        if manager.live() {
            use std::os::unix::{fs::MetadataExt, process::CommandExt};
            let user = unsafe { libc::getpwnam(c"chain-panel".as_ptr()) };
            ensure!(!user.is_null(), "缺少面板服务账户");
            let (uid, gid) = unsafe { ((*user).pw_uid, (*user).pw_gid) };
            let metadata = fs::metadata(&path)?;
            ensure!(
                uid != 0 && (metadata.uid(), metadata.gid()) == (uid, gid),
                "面板数据库归属与服务身份不一致"
            );
            let mut command = tokio::process::Command::new(std::env::current_exe()?);
            command
                .arg("internal-reset-db")
                .arg(&path)
                .uid(uid)
                .gid(gid)
                .kill_on_drop(true);
            unsafe {
                command.as_std_mut().pre_exec(|| {
                    if libc::setgroups(0, std::ptr::null()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let status = tokio::time::timeout(std::time::Duration::from_secs(30), command.status())
                .await??;
            ensure!(status.success(), "面板数据库安全设置更新失败");
        } else {
            reset_database(&path)?;
        }
        Ok(())
    }
    .await;
    if active {
        manager
            .service("start", "panel")
            .await
            .context("恢复登录结束后重新启动面板失败")?;
    }
    result?;
    println!("已重置登录二步验证和 Passkey，管理员密钥与业务线路保留");
    Ok(())
}

pub fn reset_database(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    ordinary(path)?;
    ensure!(
        path.file_name().is_some_and(|s| s == "panel.db")
            && fs::metadata(path)?.uid() == unsafe { libc::geteuid() },
        "数据库路径或执行身份不匹配"
    );
    let mut connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let transaction = connection.transaction()?;
    let body: String =
        transaction.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
    let mut document: serde_json::Value = serde_json::from_str(&body)?;
    document["security"] = serde_json::json!({});
    ensure!(
        transaction.execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(&document)?]
        )? == 1,
        "数据库状态记录不唯一"
    );
    transaction.commit()?;
    Ok(())
}

pub async fn rollback(manager: &Manager, role: &str) -> Result<()> {
    let _guard = manager.lock()?;
    let mut state = manager.state()?;
    let current = state["roles"][role]["current"]
        .as_str()
        .context("尚未安装指定组件")?
        .to_owned();
    let previous = state["roles"][role]["previous"]
        .as_str()
        .context("没有保留可回滚的历史版本；成功更新默认清理历史版本")?
        .to_owned();
    super::upgrade::release_name(&previous)?;
    let source = manager.base.join("releases").join(&previous);
    let manifest = package::verify(&source, false)?;
    package::compatible(&manifest)?;
    let active = manager.active(role).await?;
    manager.service("stop", role).await?;
    let result: Result<()> = async {
        backup(manager, role)?;
        manager.link(role, &previous)?;
        manager.service("start", role).await?;
        manager.healthy(role.into()).await?;
        state["roles"][role]["current"] = serde_json::json!(previous);
        state["roles"][role]["previous"] = serde_json::json!(current);
        state["roles"][role]["version"] = serde_json::json!(manifest.version);
        let path = manager.base.join(".chain-install.json");
        use std::os::unix::fs::PermissionsExt;
        atomic(
            &path,
            &serde_json::to_vec_pretty(&state)?,
            fs::metadata(&path)?.permissions().mode() & 0o777,
        )?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        manager.service("stop", role).await?;
        manager.link(role, &current)?;
        if active {
            manager.service("start", role).await?;
        }
        return Err(error.context("程序回滚未完成，已恢复原程序链接；业务数据保留"));
    }
    println!("程序版本已回滚；没有回退业务数据库");
    Ok(())
}

pub async fn logs(manager: &Manager, role: &str, source: &str, follow: bool) -> Result<()> {
    ensure!(manager.live(), "离线安装树没有服务日志");
    ensure!(
        manager.state()?["roles"][role].is_object(),
        "尚未安装指定组件"
    );
    let mut command;
    if source == "service" && manager.kind() == "systemd" {
        command = tokio::process::Command::new("journalctl");
        command.args([
            "-u",
            &format!("chain-{role}.service"),
            "--no-pager",
            "-n",
            "100",
        ]);
        if follow {
            command.arg("-f");
        }
    } else {
        let data = manager.path(&format!("/var/lib/chain-{role}"));
        let path = match source {
            "application" => data.join("diagnostics.log"),
            "xray" => data.join("xray.log"),
            "mihomo" => data.join("mihomo/mihomo.log"),
            "service" if manager.kind() == "openrc" => data.join("service.log"),
            "service" => manager.path(&format!("/var/log/chain-{role}.log")),
            other => {
                let id = other.strip_prefix("legacy-").context("日志来源无效")?;
                ensure!(
                    uuid::Uuid::parse_str(id)?.to_string() == id,
                    "迁移配置 ID 无效"
                );
                data.join(format!("legacy-{id}/xray.log"))
            }
        };
        ordinary(&path)?;
        ensure!(
            path.is_file() || follow,
            "该日志尚未产生；可添加 --follow 等待"
        );
        command = tokio::process::Command::new("tail");
        command.args(["-n", "100"]);
        if follow {
            command.arg("-F");
        }
        command.arg(path);
    }
    command.kill_on_drop(true).process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id().context("未取得日志进程 PID")?;
    let result = tokio::select! { result = child.wait() => Some(result?), _ = tokio::signal::ctrl_c() => None };
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    if result.is_none() {
        if tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .is_err()
        {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            child.wait().await?;
        }
    } else {
        ensure!(result.unwrap().success(), "读取日志失败");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn auth_reset_preserves_business_data_and_database_ownership() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("panel.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            .unwrap();
        conn.execute(
            "INSERT INTO state VALUES(1, ?)",
            [json!({"security":{"totp":"secret"},"lines":[{"id":"preserve"}]}).to_string()],
        )
        .unwrap();
        drop(conn);
        let before = fs::metadata(&path).unwrap();
        reset_database(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let body: String = conn
            .query_row("SELECT body FROM state", [], |r| r.get(0))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["security"], json!({}));
        assert_eq!(value["lines"][0]["id"], "preserve");
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            (before.uid(), before.gid(), before.mode()),
            (after.uid(), after.gid(), after.mode())
        );
    }
    #[test]
    fn backup_preserves_wal_and_rejects_external_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("opt/chain");
        fs::create_dir_all(&base).unwrap();
        fs::write(
            base.join(".chain-install.json"),
            serde_json::to_vec(&json!({"owner":super::super::upgrade::OWNER,"roles":{}})).unwrap(),
        )
        .unwrap();
        let data = root.path().join("var/lib/chain-agent");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("traffic.db"), "database").unwrap();
        fs::write(data.join("traffic.db-wal"), "committed").unwrap();
        let manager = Manager::open(root.path()).unwrap();
        let archive = backup(&manager, "agent").unwrap();
        let decoder = flate2::read::GzDecoder::new(File::open(&archive).unwrap());
        let mut tar = tar::Archive::new(decoder);
        let names = tar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().into_owned())
            .collect::<Vec<_>>();
        assert!(names.contains(&Path::new("data/traffic.db-wal").to_path_buf()));
        std::os::unix::fs::symlink("/etc/passwd", data.join("foreign")).unwrap();
        assert!(backup(&manager, "agent").is_err());
        assert_eq!(fs::read_dir(archive.parent().unwrap()).unwrap().count(), 1);
    }
}
