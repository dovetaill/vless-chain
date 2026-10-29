use super::package;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    time::Duration,
};

pub const OWNER: &str = "chain-panel-installer-v1";

pub fn atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().context("缺少父目录")?)?;
    temporary.write_all(bytes)?;
    match fs::symlink_metadata(path) {
        Ok(old) => {
            ensure!(old.is_file(), "拒绝替换链接或特殊文件：{}", path.display());
            let created = temporary.as_file().metadata()?;
            if (old.uid(), old.gid()) != (created.uid(), created.gid()) {
                ensure!(
                    unsafe { libc::fchown(temporary.as_file().as_raw_fd(), old.uid(), old.gid()) }
                        == 0,
                    "无法保留文件属主和属组"
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

pub(super) fn ordinary(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(info) => ensure!(
                !info.file_type().is_symlink(),
                "路径包含符号链接：{}",
                ancestor.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub fn release_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
        "发行目录名称无效"
    );
    Ok(())
}

pub struct Manager {
    pub root: PathBuf,
    pub base: PathBuf,
}

impl Manager {
    pub fn open(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        ensure!(
            root != Path::new("/") || unsafe { libc::geteuid() } == 0,
            "运维需要 root 或 sudo"
        );
        let base = root.join("opt/chain");
        ordinary(&base)?;
        let manager = Self { root, base };
        manager.state()?;
        Ok(manager)
    }
    pub fn path(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
    pub fn live(&self) -> bool {
        self.root == Path::new("/")
    }
    pub fn state(&self) -> Result<Value> {
        let path = self.base.join(".chain-install.json");
        ordinary(&path)?;
        ensure!(fs::metadata(&path)?.len() <= 1024 * 1024, "安装状态过大");
        let state: Value = serde_json::from_reader(File::open(path)?)?;
        ensure!(state["owner"] == OWNER, "安装目录归属不匹配");
        let roles = state["roles"].as_object().context("安装状态缺少组件")?;
        for (role, item) in roles {
            ensure!(
                matches!(role.as_str(), "panel" | "agent"),
                "安装状态包含未知组件"
            );
            release_name(item["current"].as_str().context("当前版本缺失")?)?;
            package::version(item["version"].as_str().context("组件版本缺失")?)?;
        }
        Ok(state)
    }
    pub fn lock(&self) -> Result<File> {
        let path = self.base.join(".install.lock");
        ordinary(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        ensure!(file.metadata()?.is_file(), "安装锁不是普通文件");
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "已有安装或运维任务运行"
        );
        Ok(file)
    }
    pub(super) fn link(&self, role: &str, release: &str) -> Result<()> {
        release_name(release)?;
        let temporary = tempfile::Builder::new()
            .prefix(".link-")
            .tempdir_in(&self.base)?;
        let link = temporary.path().join("current");
        symlink(format!("releases/{release}"), &link)?;
        fs::rename(link, self.base.join(format!("current-{role}")))?;
        File::open(&self.base)?.sync_all()?;
        Ok(())
    }
    pub fn roles(&self, state: &Value, component: Option<&str>) -> Result<Vec<String>> {
        let roles = state["roles"].as_object().context("安装状态无效")?;
        ensure!(!roles.is_empty(), "尚未安装组件");
        if let Some(role) = component {
            ensure!(
                matches!(role, "panel" | "agent") && roles.contains_key(role),
                "尚未安装指定组件"
            );
        }
        Ok(["panel", "agent"]
            .into_iter()
            .filter(|role| {
                roles.contains_key(*role) && (component != Some("agent") || *role == "agent")
            })
            .map(str::to_owned)
            .collect())
    }
    pub async fn update(&self, source: &Path, component: Option<&str>) -> Result<()> {
        self.update_checked(source, component, |role| self.healthy(role))
            .await
    }
    async fn update_checked<F, Fut>(
        &self,
        source: &Path,
        component: Option<&str>,
        check: F,
    ) -> Result<()>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let manifest = package::verify(source, true)?;
        package::compatible(&manifest)?;
        let _lock = self.lock()?;
        let journal = self.base.join(".update-pending.json");
        ensure!(
            fs::symlink_metadata(&journal).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "上次升级中断，请先核对 .update-pending.json 中的原版本与当前服务；未继续覆盖"
        );
        let old = self.state()?;
        ensure!(
            !self.base.join(".core-pending.json").exists() && old["core_blocked"].is_null(),
            "请先完成内核升级验证或恢复安全版本"
        );
        let roles = self.roles(&old, component)?;
        let mut running = Vec::new();
        for role in &roles {
            ensure!(
                package::version(&manifest.version)?
                    >= package::version(old["roles"][role]["version"].as_str().unwrap())?,
                "拒绝自动降级组件或数据库"
            );
            let old_release = old["roles"][role]["current"].as_str().unwrap();
            let link = self.base.join(format!("current-{role}"));
            ensure!(
                fs::symlink_metadata(&link)?.file_type().is_symlink()
                    && link.canonicalize()?
                        == self
                            .base
                            .join("releases")
                            .join(old_release)
                            .canonicalize()?,
                "当前程序链接与安装状态不一致"
            );
            let unit = self.unit_path(role);
            ordinary(&unit)?;
            ensure!(
                fs::read_to_string(&unit)?.contains(OWNER),
                "服务文件归属不匹配：{}",
                unit.display()
            );
            // Configuration and directory metadata are not rewritten during a binary update.
            self.environment(role)?;
            running.push(self.active(role).await?);
        }
        let temporary = tempfile::Builder::new()
            .prefix(".update-core-")
            .tempdir_in(&self.base)?;
        let stage = temporary.path().join("stage");
        package::copy(source, &stage, &manifest)?;
        super::cores::preserve(self, &stage)?;
        let source = stage.as_path();
        let manifest = package::verify(source, true)?;
        let id = format!(
            "{}-{}-{}-{}",
            manifest.version,
            manifest.os,
            manifest.arch,
            &package::digest(&source.join("release.json"))?[..12]
        );
        let releases = self.base.join("releases");
        ordinary(&releases)?;
        let destination = releases.join(&id);
        ordinary(&destination)?;
        if destination.exists() {
            package::verify(&destination, true)?;
            ensure!(
                fs::read(destination.join("release.json"))?
                    == fs::read(source.join("release.json"))?,
                "同名发行目录内容冲突"
            );
        } else {
            let temporary = tempfile::Builder::new()
                .prefix(".install-")
                .tempdir_in(&releases)?;
            let staging = temporary.path().join("release");
            package::copy(source, &staging, &manifest)?;
            fs::rename(staging, &destination)?;
            File::open(&releases)?.sync_all()?;
        }
        let state_path = self.base.join(".chain-install.json");
        let old_bytes = fs::read(&state_path)?;
        let metadata = fs::metadata(&state_path)?;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "安装状态属主与执行身份不匹配，拒绝替换"
        );
        atomic(
            &journal,
            &serde_json::to_vec(
                &json!({"before":old,"target":id,"roles":roles,"running":running}),
            )?,
            0o600,
        )?;
        let mut attempted = 0;
        let result: Result<()> = async {
            for role in &roles {
                attempted += 1;
                self.service("stop", role).await?;
                self.link(role, &id)?;
                self.service("start", role).await?;
                check(role.clone())
                    .await
                    .with_context(|| format!("{role} 启动检查失败"))?;
            }
            let mut state = old.clone();
            for role in &roles {
                state["roles"][role]["current"] = json!(id);
                state["roles"][role]["previous"] = Value::Null;
                state["roles"][role]["version"] = json!(manifest.version);
            }
            atomic(
                &state_path,
                &serde_json::to_vec_pretty(&state)?,
                metadata.mode() & 0o777,
            )?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            let mut failures = Vec::new();
            for (role, was_running) in roles.iter().zip(&running).take(attempted).rev() {
                let restore: Result<()> = async {
                    self.service("stop", role).await?;
                    self.link(role, old["roles"][role]["current"].as_str().unwrap())?;
                    if *was_running {
                        self.service("start", role).await?;
                    }
                    Ok(())
                }
                .await;
                if let Err(e) = restore {
                    failures.push(format!("{role}: {e:#}"));
                }
            }
            if failures.is_empty() {
                atomic(&state_path, &old_bytes, metadata.mode() & 0o777)?;
                fs::remove_file(&journal)?;
                bail!("升级失败，已恢复原程序链接和服务运行状态，业务数据未回滚：{error:#}");
            }
            bail!(
                "升级失败：{error:#}；恢复未完成（{}），保留 {} 供核对",
                failures.join("；"),
                journal.display()
            );
        }
        fs::remove_file(journal)?;
        println!(
            "已升级至 {}；配置、节点身份和数据保持原样",
            manifest.version
        );
        Ok(())
    }
    pub fn kind(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "launchd"
        } else if self.path("/run/openrc").exists() && !self.path("/run/systemd/system").exists() {
            "openrc"
        } else {
            "systemd"
        }
    }
    pub fn unit_path(&self, role: &str) -> PathBuf {
        self.path(&match self.kind() {
            "launchd" => format!("/Library/LaunchDaemons/org.vless-chain.chain-{role}.plist"),
            "openrc" => format!("/etc/init.d/chain-{role}"),
            _ => format!("/etc/systemd/system/chain-{role}.service"),
        })
    }
    pub async fn service(&self, action: &str, role: &str) -> Result<()> {
        ensure!(
            matches!(action, "start" | "stop" | "restart") && matches!(role, "panel" | "agent"),
            "服务操作无效"
        );
        if !self.live() {
            return Ok(());
        }
        let name = format!("chain-{role}");
        match self.kind() {
            "systemd" => {
                command("systemctl", &[action, &format!("{name}.service")], 70).await?;
            }
            "openrc" => {
                if matches!(action, "stop" | "restart") {
                    command("rc-service", &[&name, "stop"], 70).await?;
                    super::platform::stop_openrc_residue(self, &name).await?;
                }
                if action != "stop" {
                    command("rc-service", &[&name, "start"], 70).await?;
                }
            }
            _ => {
                let target = format!("system/org.vless-chain.{name}");
                if matches!(action, "stop" | "restart") && self.active(role).await? {
                    command("launchctl", &["bootout", &target], 70).await?;
                }
                if matches!(action, "start" | "restart") {
                    command(
                        "launchctl",
                        &[
                            "bootstrap",
                            "system",
                            self.unit_path(role).to_str().context("服务路径无效")?,
                        ],
                        70,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
    pub async fn active(&self, role: &str) -> Result<bool> {
        if !self.live() {
            return Ok(false);
        }
        let name = format!("chain-{role}");
        let (program, args) = match self.kind() {
            "openrc" => ("rc-service", vec![name, "status".into()]),
            "launchd" => (
                "launchctl",
                vec!["print".into(), format!("system/org.vless-chain.{name}")],
            ),
            _ => (
                "systemctl",
                vec![
                    "is-active".into(),
                    "--quiet".into(),
                    format!("{name}.service"),
                ],
            ),
        };
        let output = output(
            program,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            15,
        )
        .await?;
        Ok(output.status.success()
            && (self.kind() != "launchd"
                || String::from_utf8_lossy(&output.stdout).contains("pid = ")))
    }
    pub fn environment(&self, role: &str) -> Result<std::collections::BTreeMap<String, String>> {
        let path = self.path(&format!("/etc/chain-{role}/{role}.env"));
        ordinary(&path)?;
        let mut result = std::collections::BTreeMap::new();
        for line in fs::read_to_string(path)?
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let (name, value) = line.split_once('=').context("环境文件格式无效")?;
            ensure!(
                name.starts_with("CHAIN_")
                    && name.bytes().all(|c| c.is_ascii_uppercase() || c == b'_'),
                "环境变量名无效"
            );
            let value: String = serde_json::from_str(value)?;
            ensure!(
                !value.contains(['\r', '\n', '\0']) && result.insert(name.into(), value).is_none(),
                "环境变量重复或包含控制字符"
            );
        }
        Ok(result)
    }
    pub(super) async fn healthy(&self, role: String) -> Result<()> {
        if !self.live() {
            return Ok(());
        }
        let env = self.environment(&role)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()?;
        let url = if role == "panel" {
            let mut addr: std::net::SocketAddr = env
                .get("CHAIN_LISTEN")
                .context("缺少 CHAIN_LISTEN")?
                .parse()?;
            if addr.ip().is_unspecified() {
                addr.set_ip(if addr.is_ipv6() {
                    "::1".parse()?
                } else {
                    "127.0.0.1".parse()?
                });
            }
            format!("http://{addr}/")
        } else {
            String::new()
        };
        let mut active_count = 0;
        for _ in 0..20 {
            if self.active(&role).await? {
                active_count += 1;
                if role == "agent" && active_count >= 2 {
                    return Ok(());
                }
                if role == "panel" {
                    if let Ok(mut response) = client.get(&url).send().await {
                        if response.status() == 200 {
                            let mut prefix = Vec::new();
                            while prefix.len() < 100 {
                                match response.chunk().await {
                                    Ok(Some(chunk)) => prefix.extend_from_slice(
                                        &chunk[..chunk.len().min(100 - prefix.len())],
                                    ),
                                    _ => break,
                                }
                            }
                            if String::from_utf8_lossy(&prefix)
                                .to_ascii_lowercase()
                                .contains("<!doctype html>")
                            {
                                return Ok(());
                            }
                        }
                    }
                }
            } else {
                active_count = 0;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!(
            "chain-{role} 启动检查未通过，请查看该组件服务日志；Agent 存活检查不代表代理整链已通过"
        )
    }
    pub async fn prune(&self) -> Result<()> {
        let _lock = self.lock()?;
        ensure!(
            !self.base.join(".update-pending.json").exists(),
            "升级尚未完成，未清理历史版本"
        );
        let state = self.state()?;
        let mut keep = BTreeSet::new();
        for item in state["roles"].as_object().unwrap().values() {
            keep.insert(item["current"].as_str().unwrap().to_string());
        }
        if let Some(history) = state["core_history"].as_object() {
            for item in history.values() {
                let release = item["release"].as_str().context("内核回退记录无效")?;
                release_name(release)?;
                keep.insert(release.into());
            }
        }
        ensure!(
            !self.base.join(".core-pending.json").exists(),
            "内核升级尚未确认，未清理历史版本"
        );
        for role in ["panel", "agent"] {
            let link = self.base.join(format!("current-{role}"));
            if link.is_symlink() {
                keep.insert(
                    link.canonicalize()?
                        .file_name()
                        .context("当前发行目录无效")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        let releases = self.base.join("releases");
        ordinary(&releases)?;
        for entry in fs::read_dir(&releases)? {
            let entry = entry?;
            if keep.contains(&entry.file_name().to_string_lossy().into_owned()) {
                continue;
            }
            let directory = entry.path();
            let result: Result<()> = async {
                ordinary(&directory)?;
                let info = fs::symlink_metadata(&directory)?;
                ensure!(
                    info.is_dir() && info.uid() == fs::metadata(&self.base)?.uid(),
                    "目录属主或类型不匹配"
                );
                let manifest = package::verify(&directory, false)?;
                let expected = format!(
                    "{}-{}-{}-{}",
                    manifest.version,
                    manifest.os,
                    manifest.arch,
                    &package::digest(&directory.join("release.json"))?[..12]
                );
                ensure!(
                    entry.file_name() == std::ffi::OsStr::new(&expected),
                    "发行目录名称与清单不匹配"
                );
                ensure!(
                    !in_use(&directory).await?,
                    "目录仍被进程使用，或不能确认占用情况"
                );
                println!(
                    "清理前：{} dev/inode={}/{} uid/gid={}/{} mode={:o} ctime={}.{}",
                    directory.display(),
                    info.dev(),
                    info.ino(),
                    info.uid(),
                    info.gid(),
                    info.mode() & 0o7777,
                    info.ctime(),
                    info.ctime_nsec()
                );
                fs::remove_dir_all(&directory)?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                eprintln!("保留 {}：{error:#}", directory.display());
            }
        }
        Ok(())
    }
}

// External service tools run in a task-owned process group, including timeout cleanup.
pub async fn output(program: &str, args: &[&str], seconds: u64) -> Result<std::process::Output> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let child = command.spawn()?;
    struct Group(u32, bool);
    impl Drop for Group {
        fn drop(&mut self) {
            if self.1 {
                unsafe {
                    libc::kill(-(self.0 as i32), libc::SIGKILL);
                }
            }
        }
    }
    let mut group = Group(child.id().context("未取得子进程 PID")?, true);
    let result = tokio::time::timeout(Duration::from_secs(seconds), child.wait_with_output())
        .await
        .context("系统命令超时，已终止任务进程组")?
        .map_err(anyhow::Error::from)?;
    // Service managers may leave an intentional supervisor in the invocation group.
    if matches!(program, "rc-service" | "launchctl") && result.status.success() {
        group.1 = false;
    }
    Ok(result)
}
pub async fn command(program: &str, args: &[&str], seconds: u64) -> Result<()> {
    let result = output(program, args, seconds).await?;
    ensure!(
        result.status.success(),
        "{program} 执行失败（{}）：{}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );
    Ok(())
}

async fn in_use(directory: &Path) -> Result<bool> {
    let directory = directory.canonicalize()?;
    if cfg!(target_os = "macos") {
        let result = output(
            "lsof",
            &[
                "-nP",
                "-t",
                "+D",
                directory.to_str().context("发行路径无效")?,
            ],
            30,
        )
        .await?;
        ensure!(
            result.status.success() || result.status.code() == Some(1) && result.stderr.is_empty(),
            "无法检查发行目录占用"
        );
        return Ok(!result.stdout.is_empty());
    }
    for process in fs::read_dir("/proc")? {
        let process = process?;
        if !process
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let check = || -> Result<bool> {
            let process = process.path();
            let mut links = vec![process.join("exe"), process.join("cwd")];
            for fd in fs::read_dir(process.join("fd"))? {
                links.push(fd?.path());
            }
            for link in links {
                match fs::read_link(&link) {
                    Ok(path) if path.starts_with(&directory) => return Ok(true),
                    Ok(_) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.into()),
                }
            }
            for line in fs::read_to_string(process.join("maps"))?.lines() {
                if let Some(start) = line.find('/') {
                    if Path::new(line[start..].trim_end_matches(" (deleted)"))
                        .starts_with(&directory)
                    {
                        return Ok(true);
                    }
                }
            }
            for arg in fs::read(process.join("cmdline"))?.split(|c| *c == 0) {
                use std::os::unix::ffi::OsStrExt;
                let path = Path::new(std::ffi::OsStr::from_bytes(arg));
                if path.is_absolute()
                    && (path.starts_with(&directory)
                        || path.canonicalize().is_ok_and(|p| p.starts_with(&directory)))
                {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        match check() {
            Ok(true) => return Ok(true),
            Ok(false) => (),
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                ()
            }
            Err(_) => return Ok(true),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctl::package::tests::fixture;

    fn installed(root: &Path) -> Manager {
        let base = root.join("opt/chain");
        fs::create_dir_all(base.join("releases/old")).unwrap();
        fixture(&base.join("releases/old"));
        let mut roles = serde_json::Map::new();
        for role in ["panel", "agent"] {
            symlink("releases/old", base.join(format!("current-{role}"))).unwrap();
            fs::create_dir_all(root.join(format!("etc/chain-{role}"))).unwrap();
            fs::write(
                root.join(format!("etc/chain-{role}/{role}.env")),
                "CHAIN_LISTEN=\"127.0.0.1:8088\"\n",
            )
            .unwrap();
            fs::create_dir_all(root.join("etc/systemd/system")).unwrap();
            fs::write(
                root.join(format!("etc/systemd/system/chain-{role}.service")),
                OWNER,
            )
            .unwrap();
            roles.insert(role.into(), json!({"current":"old","version":"0.2.48","previous":null,"setup_complete":true,"panel_only":false,"custom":"preserve"}));
        }
        fs::write(
            base.join(".chain-install.json"),
            serde_json::to_vec(&json!({"owner":OWNER,"roles":roles})).unwrap(),
        )
        .unwrap();
        Manager::open(root).unwrap()
    }

    #[tokio::test]
    async fn update_preserves_configuration_and_role_metadata_without_backups() {
        let root = tempfile::tempdir().unwrap();
        let manager = installed(root.path());
        let source = root.path().join("source");
        fixture(&source);
        let env = root.path().join("etc/chain-agent/agent.env");
        let before = fs::metadata(&env).unwrap();
        manager.update(&source, None).await.unwrap();
        let state = manager.state().unwrap();
        for role in ["panel", "agent"] {
            assert_eq!(state["roles"][role]["version"], "0.3.0");
            assert_eq!(state["roles"][role]["custom"], "preserve");
            assert!(state["roles"][role]["previous"].is_null());
        }
        let after = fs::metadata(env).unwrap();
        assert_eq!(
            (
                before.ino(),
                before.uid(),
                before.gid(),
                before.mode(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.ino(),
                after.uid(),
                after.gid(),
                after.mode(),
                after.ctime(),
                after.ctime_nsec()
            )
        );
        assert!(!root.path().join("var/backups").exists());
        assert!(!manager.base.join(".update-pending.json").exists());
    }

    #[tokio::test]
    async fn second_component_failure_restores_both_links_and_exact_state() {
        let root = tempfile::tempdir().unwrap();
        let manager = installed(root.path());
        let source = root.path().join("source");
        fixture(&source);
        let before = fs::read(manager.base.join(".chain-install.json")).unwrap();
        let result = manager
            .update_checked(&source, None, |role| async move {
                ensure!(role != "agent", "injected agent failure");
                Ok(())
            })
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("injected agent failure")
        );
        assert_eq!(
            fs::read(manager.base.join(".chain-install.json")).unwrap(),
            before
        );
        for role in ["panel", "agent"] {
            assert_eq!(
                fs::read_link(manager.base.join(format!("current-{role}"))).unwrap(),
                Path::new("releases/old")
            );
        }
        assert!(!manager.base.join(".update-pending.json").exists());
    }

    #[tokio::test]
    async fn refuses_foreign_unit_busy_lock_and_interrupted_update() {
        let root = tempfile::tempdir().unwrap();
        let manager = installed(root.path());
        let source = root.path().join("source");
        fixture(&source);
        let guard = manager.lock().unwrap();
        assert!(manager.update(&source, None).await.is_err());
        drop(guard);
        fs::write(manager.base.join(".update-pending.json"), "{}").unwrap();
        assert!(
            manager
                .update(&source, None)
                .await
                .unwrap_err()
                .to_string()
                .contains("上次升级中断")
        );
        fs::remove_file(manager.base.join(".update-pending.json")).unwrap();
        fs::write(manager.unit_path("panel"), "foreign").unwrap();
        assert!(
            manager
                .update(&source, None)
                .await
                .unwrap_err()
                .to_string()
                .contains("归属")
        );
        assert_eq!(
            fs::read_link(manager.base.join("current-panel")).unwrap(),
            Path::new("releases/old")
        );
    }
}
