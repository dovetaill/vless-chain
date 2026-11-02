use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        let kind = item.file_type()?;
        ensure!(
            kind.is_file() || kind.is_dir(),
            "目录含链接或特殊文件：{}",
            item.path().display()
        );
        if kind.is_dir() {
            result.extend(files(&item.path())?);
        } else {
            result.push(item.path());
        }
    }
    result.sort();
    Ok(result)
}

pub fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for item in fs::read_dir(source)? {
        let item = item?;
        let name = item.file_name();
        if [
            "target",
            "node_modules",
            "dist",
            ".git",
            "__pycache__",
            ".env",
        ]
        .iter()
        .any(|v| name == *v)
            || name.to_string_lossy().starts_with(".env.")
        {
            continue;
        }
        let kind = item.file_type()?;
        ensure!(kind.is_dir() || kind.is_file(), "源码含链接或特殊文件");
        if kind.is_dir() {
            copy_tree(&item.path(), &destination.join(name))?;
        } else {
            fs::copy(item.path(), destination.join(name))?;
        }
    }
    Ok(())
}

pub fn free_space(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    ensure!(
        unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } == 0,
        "无法读取磁盘空间"
    );
    let info = unsafe { info.assume_init() };
    Ok(info.f_bavail as u64 * info.f_frsize as u64)
}

pub struct Workspace {
    directory: Option<tempfile::TempDir>,
    pub root: PathBuf,
    pub env: BTreeMap<String, String>,
    children: RefCell<Vec<Child>>,
    before: u64,
}
pub(crate) fn configure_cache(
    env: &mut BTreeMap<String, String>,
    github: &str,
    enabled: &str,
    runner: Option<&Path>,
) -> Result<()> {
    if github == "true" && enabled == "1" {
        let cache = runner
            .context("CI 缺少 RUNNER_TEMP")?
            .canonicalize()?
            .join("chain-build-cache");
        for (key, name) in [
            ("CARGO_TARGET_DIR", "target"),
            ("CARGO_HOME", "cargo"),
            ("npm_config_cache", "npm"),
        ] {
            env.insert(key.into(), cache.join(name).display().to_string());
        }
    }
    Ok(())
}

impl Workspace {
    pub fn new(parent: &Path) -> Result<Self> {
        let parent = parent.canonicalize()?;
        let before = free_space(&parent)?;
        ensure!(
            before > 2 * 1024 * 1024 * 1024,
            "工作目录至少需要 2 GiB 可用空间"
        );
        let directory = tempfile::Builder::new()
            .prefix(".chain-build-")
            .tempdir_in(parent)?;
        let root = directory.path().to_path_buf();
        fs::create_dir(root.join("tmp"))?;
        let mut env = BTreeMap::from([
            ("CHAIN_TASK_OWNER".into(), root.display().to_string()),
            (
                "CARGO_TARGET_DIR".into(),
                root.join("target").display().to_string(),
            ),
            ("CARGO_INCREMENTAL".into(), "0".into()),
            ("CARGO_PROFILE_DEV_DEBUG".into(), "0".into()),
            ("CARGO_PROFILE_TEST_DEBUG".into(), "0".into()),
            (
                "npm_config_cache".into(),
                root.join("npm-cache").display().to_string(),
            ),
            ("npm_config_progress".into(), "false".into()),
            ("TMPDIR".into(), root.join("tmp").display().to_string()),
            ("CI".into(), "1".into()),
        ]);
        configure_cache(
            &mut env,
            std::env::var("GITHUB_ACTIONS").as_deref().unwrap_or(""),
            std::env::var("CHAIN_BUILD_CACHE").as_deref().unwrap_or(""),
            std::env::var_os("RUNNER_TEMP").as_deref().map(Path::new),
        )?;
        let work = Self {
            directory: Some(directory),
            root,
            env,
            children: RefCell::new(vec![]),
            before,
        };
        work.record()?;
        println!(
            "临时目录：{}；拥有者 PID/PGID={}/{}；不启动构建监听服务。",
            work.root.display(),
            std::process::id(),
            unsafe { libc::getpgrp() }
        );
        Ok(work)
    }

    fn record(&self) -> Result<()> {
        fs::write(
            self.root.join("ownership.json"),
            serde_json::to_vec_pretty(&json!({
                "pid":std::process::id(), "pgid":unsafe {libc::getpgrp()},
                "cwd":std::env::current_dir()?, "root":self.root,
                "groups":self.children.borrow().iter().map(Child::id).collect::<Vec<_>>(), "ports":[]
            }))?,
        )?;
        Ok(())
    }

    pub async fn run(&self, cwd: &Path, args: &[String]) -> Result<()> {
        self.execute(cwd, args, false, true).await.map(|_| ())
    }

    pub async fn output(&self, cwd: &Path, args: &[String]) -> Result<String> {
        self.execute(cwd, args, true, true).await
    }

    pub async fn failure(&self, cwd: &Path, args: &[String]) -> Result<String> {
        self.execute(cwd, args, true, false).await
    }

    pub fn spawn(
        &self,
        cwd: &Path,
        args: &[String],
        env: &BTreeMap<String, String>,
        log: &Path,
    ) -> Result<u32> {
        ensure!(!args.is_empty(), "命令不能为空");
        let stream = fs::File::create(log)?;
        let child = Command::new(&args[0])
            .args(&args[1..])
            .envs(&self.env)
            .envs(env)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(stream.try_clone()?)
            .stderr(stream)
            .process_group(0)
            .spawn()?;
        let pid = child.id();
        self.children.borrow_mut().push(child);
        self.record()?;
        Ok(pid)
    }

    pub async fn stop(&self, pid: u32) -> Result<()> {
        {
            let mut children = self.children.borrow_mut();
            let child = children
                .iter_mut()
                .find(|c| c.id() == pid)
                .context("子进程不属于当前任务")?;
            if child.try_wait()?.is_some() {
                return Ok(());
            }
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        let until = Instant::now() + Duration::from_secs(6);
        loop {
            let mut children = self.children.borrow_mut();
            let child = children
                .iter_mut()
                .find(|c| c.id() == pid)
                .context("子进程不属于当前任务")?;
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= until {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                child.wait()?;
                return Ok(());
            }
            drop(children);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn register(&self, child: Child) -> Result<u32> {
        let pid = child.id();
        self.children.borrow_mut().push(child);
        self.record()?;
        Ok(pid)
    }
    pub fn poll(&self, pid: u32) -> Result<Option<std::process::ExitStatus>> {
        self.children
            .borrow_mut()
            .iter_mut()
            .find(|c| c.id() == pid)
            .context("子进程不属于当前任务")?
            .try_wait()
            .map_err(Into::into)
    }

    async fn execute(
        &self,
        cwd: &Path,
        args: &[String],
        capture: bool,
        success: bool,
    ) -> Result<String> {
        ensure!(!args.is_empty(), "命令不能为空");
        let log = self
            .root
            .join(format!("command-{}.log", self.children.borrow().len()));
        let mut command = Command::new(&args[0]);
        command
            .args(&args[1..])
            .current_dir(cwd)
            .envs(&self.env)
            .process_group(0);
        if capture {
            let stream = fs::File::create(&log)?;
            command.stdout(stream.try_clone()?).stderr(stream);
        }
        let child = command
            .spawn()
            .with_context(|| format!("无法执行 {}", args[0]))?;
        println!("执行：{}；PID/PGID={}", args.join(" "), child.id());
        let index = self.children.borrow().len();
        self.children.borrow_mut().push(child);
        self.record()?;
        let start = Instant::now();
        loop {
            if let Some(status) = self.children.borrow_mut()[index].try_wait()? {
                let text = if capture {
                    fs::read_to_string(&log)?
                } else {
                    String::new()
                };
                if status.success() != success {
                    let tail: String = text
                        .chars()
                        .rev()
                        .take(3000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    let message = format!("{} 失败（{status}）：{tail}", args.join(" "));
                    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
                        eprintln!(
                            "::error::{}",
                            message
                                .replace('%', "%25")
                                .replace('\r', "%0D")
                                .replace('\n', "%0A")
                        );
                    }
                    anyhow::bail!(message);
                }
                return Ok(text.trim().into());
            }
            ensure!(
                start.elapsed() < Duration::from_secs(2400),
                "命令超时：{}",
                args.join(" ")
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn close(&mut self) -> Result<()> {
        if self.directory.is_none() {
            return Ok(());
        }
        let mut groups = BTreeSet::new();
        for child in self.children.borrow_mut().iter_mut() {
            if child.try_wait()?.is_none() {
                groups.insert(child.id());
            }
        }
        groups.extend(owned_groups(&self.root)?);
        groups.remove(&(unsafe { libc::getpgrp() } as u32));
        for group in &groups {
            unsafe {
                libc::kill(-(*group as i32), libc::SIGTERM);
            }
        }
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            let alive = groups
                .iter()
                .any(|g| unsafe { libc::kill(-(*g as i32), 0) } == 0);
            if !alive {
                break;
            }
            for child in self.children.borrow_mut().iter_mut() {
                let _ = child.try_wait();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for group in &groups {
            unsafe {
                libc::kill(-(*group as i32), libc::SIGKILL);
            }
        }
        for child in self.children.borrow_mut().iter_mut() {
            let _ = child.wait();
        }
        ensure!(
            owned_groups(&self.root)?.is_empty(),
            "仍有本次进程，保留目录 {}",
            self.root.display()
        );
        #[cfg(target_os = "linux")]
        for process in fs::read_dir("/proc")? {
            let process = process?.path();
            if let Ok(links) = fs::read_dir(process.join("fd")) {
                for link in links
                    .flatten()
                    .map(|v| v.path())
                    .chain([process.join("exe"), process.join("cwd")])
                {
                    ensure!(
                        !fs::read_link(&link).is_ok_and(|v| v.starts_with(&self.root)),
                        "仍有进程持有临时文件：{}",
                        link.display()
                    );
                }
            }
            if let Ok(maps) = fs::read_to_string(process.join("maps")) {
                ensure!(
                    !maps.contains(self.root.to_str().context("工作目录须为 UTF-8")?),
                    "仍有进程映射临时文件：{}",
                    process.display()
                );
            }
        }
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("lsof")
                .args(["-nP", "+D"])
                .arg(&self.root)
                .output()?;
            ensure!(
                output.status.code() == Some(1) && output.stdout.is_empty(),
                "临时目录仍有打开的文件"
            );
        }
        fn size(root: &Path) -> Result<u64> {
            let mut total = 0;
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    total += size(&entry.path())?;
                } else if kind.is_file() {
                    total += entry.metadata()?.len();
                }
            }
            Ok(total)
        }
        let size = size(&self.root)?;
        self.directory.take().unwrap().close()?;
        ensure!(!self.root.exists(), "临时目录删除失败");
        println!(
            "清理完成：进程组 {groups:?} 已退出；临时目录 {size} → 0 字节；磁盘可用空间 {} → {} 字节。",
            self.before,
            free_space(self.root.parent().unwrap())?
        );
        Ok(())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            if let Some(directory) = self.directory.take() {
                let _ = directory.keep();
            }
            eprintln!("清理未完成：{e:#}；保留 {}", self.root.display());
        }
    }
}

fn owned_groups(root: &Path) -> Result<BTreeSet<u32>> {
    let mut groups = BTreeSet::new();
    let marker = format!("CHAIN_TASK_OWNER={}", root.display());
    #[cfg(target_os = "linux")]
    for p in fs::read_dir("/proc")? {
        let p = p?;
        if p.file_name().to_string_lossy().parse::<u32>().ok() == Some(std::process::id()) {
            continue;
        }
        if let Ok(env) = fs::read(p.path().join("environ")) {
            if env.split(|v| *v == 0).any(|v| v == marker.as_bytes()) {
                if let Ok(pid) = p.file_name().to_string_lossy().parse::<i32>() {
                    let group = unsafe { libc::getpgid(pid) };
                    if group > 0 {
                        groups.insert(group as u32);
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("ps")
            .args(["eww", "-axo", "pid=,command="])
            .output()?;
        ensure!(out.status.success(), "无法枚举任务进程");
        for line in String::from_utf8(out.stdout)?.lines() {
            if line.contains(&format!("{marker} ")) {
                if let Some(pid) = line
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<i32>().ok())
                {
                    let group = unsafe { libc::getpgid(pid) };
                    if group > 0 {
                        groups.insert(group as u32);
                    }
                }
            }
        }
    }
    Ok(groups)
}
