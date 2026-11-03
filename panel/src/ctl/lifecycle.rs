use super::{
    install::{self, Options, owned_dir, owned_write},
    package, platform,
    upgrade::{Manager, OWNER, atomic, command, ordinary},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::{MetadataExt, symlink},
    path::{Path, PathBuf},
    time::Duration,
};

// Service definitions contain literal arguments, never shell substitutions.
fn words(value: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            } else {
                word.push(c);
            }
        } else if c == '\\' {
            word.push(chars.next().context("启动命令转义不完整")?);
            started = true;
        } else if quote == Some('"') {
            if c == '"' {
                quote = None;
            } else {
                ensure!(!matches!(c, '$' | '`'), "启动命令不是字面值");
                word.push(c);
            }
        } else if matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
        } else if c.is_ascii_whitespace() {
            if started {
                result.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            ensure!(
                !matches!(c, '$' | '`' | ';' | '|' | '&' | '<' | '>'),
                "启动命令不是字面值"
            );
            word.push(c);
            started = true;
        }
    }
    ensure!(quote.is_none(), "启动命令引号不完整");
    if started {
        result.push(word);
    }
    Ok(result)
}

fn field<'a>(text: &'a str, name: &str) -> Result<&'a str> {
    let values = text
        .lines()
        .filter_map(|line| line.strip_prefix(name))
        .collect::<Vec<_>>();
    ensure!(values.len() == 1, "服务字段不唯一：{name}");
    Ok(values[0])
}
fn replace_field(text: &str, name: &str, value: &str) -> Result<String> {
    let old = field(text, name)?;
    if old == value {
        return Ok(text.into());
    }
    Ok(text
        .split_inclusive('\n')
        .map(|line| {
            if line.strip_suffix('\n').unwrap_or(line).strip_prefix(name) == Some(old) {
                format!(
                    "{name}{value}{}",
                    if line.ends_with('\n') { "\n" } else { "" }
                )
            } else {
                line.into()
            }
        })
        .collect())
}
fn native_command(manager: &Manager, text: &str, role: &str) -> Result<String> {
    let component = if role == "ingress" { "agent" } else { "panel" };
    let binary = manager
        .base
        .join(format!("current-{component}/bin/chainctl"))
        .display()
        .to_string();
    let action = if role == "maintenance" {
        "maintenance"
    } else if manager.kind() == "systemd" {
        "internal-ingress-apply"
    } else {
        "internal-ingress-watch"
    };
    let args = if manager.kind() == "systemd" {
        words(field(text, "ExecStart=")?)?
            .into_iter()
            .map(|v| v.replace("%%", "%"))
            .collect()
    } else {
        ensure!(
            text.starts_with("#!/sbin/openrc-run\n")
                && field(text, "supervisor=")? == "supervise-daemon",
            "OpenRC 服务定义不匹配"
        );
        let command = words(field(text, "command=")?)?;
        let argument = words(field(text, "command_args=")?)?;
        ensure!(
            command.len() == 1 && argument.len() == 1,
            "OpenRC 启动命令不是字面值"
        );
        let mut args = command;
        args.extend(words(&argument[0])?);
        args
    };
    let expected = vec![binary.clone(), action.into()];
    let mut legacy = vec![
        "-B".into(),
        manager
            .base
            .join(format!("current-{component}/scripts/chain_{role}.py"))
            .display()
            .to_string(),
    ];
    if role == "ingress" {
        legacy.push(
            if manager.kind() == "systemd" {
                "apply"
            } else {
                "watch"
            }
            .into(),
        );
    }
    let python = args.first().is_some_and(|s| {
        ["/usr/bin/python3", "/usr/local/bin/python3"]
            .iter()
            .any(|prefix| {
                s == prefix
                    || s.strip_prefix(&format!("{prefix}."))
                        .is_some_and(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
            })
    });
    let repair = manager
        .base
        .join("repairs/chainctl-0.2.66-nginx-log-linux-x86_64-d60fd0307d6c")
        .display()
        .to_string();
    ensure!(
        args == expected
            || (python && args[1..] == legacy)
            || (role == "ingress" && args == [repair, action.into()]),
        "拒绝覆盖自定义启动命令：chain-{role}"
    );
    if args == expected {
        return Ok(text.into());
    }
    if manager.kind() == "systemd" {
        let quoted = binary
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        replace_field(text, "ExecStart=", &format!("\"{quoted}\" {action}"))
    } else {
        let text = replace_field(text, "command=", &platform::shell(&binary))?;
        replace_field(
            &text,
            "command_args=",
            &platform::shell(&platform::shell(action)),
        )
    }
}

pub(super) async fn service_process(manager: &Manager, role: &str) -> Result<Option<u32>> {
    let unit = manager.unit_path(role);
    ordinary(&unit)?;
    match fs::symlink_metadata(&unit) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => {
            result?;
        }
    }
    let pid = if manager.kind() == "systemd" {
        let out = super::upgrade::output(
            "systemctl",
            &[
                "show",
                &format!("chain-{role}.service"),
                "-p",
                "MainPID",
                "--value",
            ],
            15,
        )
        .await?;
        ensure!(out.status.success(), "无法读取 chain-{role} 主进程");
        String::from_utf8(out.stdout)?.trim().parse::<u32>()?
    } else {
        let name = format!("chain-{role}");
        let path = manager.path(&format!("/run/supervise-{name}.pid"));
        ordinary(&path)?;
        if !path.exists() {
            return Ok(None);
        }
        let meta = fs::metadata(&path)?;
        ensure!(
            meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0,
            "supervisor PID 文件归属无效"
        );
        let pid = fs::read_to_string(path)?.trim().parse::<u32>()?;
        ensure!(pid > 1, "supervisor PID 无效");
        let process = Path::new("/proc").join(pid.to_string());
        if !process.exists() {
            return Ok(None);
        }
        let exe = platform::executable("supervise-daemon")
            .context("缺少 supervisor")?
            .canonicalize()?;
        let args = fs::read(process.join("cmdline"))?;
        let args = args.split(|c| *c == 0).collect::<Vec<_>>();
        ensure!(
            fs::metadata(&process)?.uid() == 0
                && fs::read_link(process.join("exe"))? == exe
                && args.get(1).copied() == Some(name.as_bytes())
                && args.get(2).copied() == Some(b"--start"),
            "supervisor 身份不匹配"
        );
        let children = fs::read_to_string(process.join(format!("task/{pid}/children")))?;
        let children = children.split_whitespace().collect::<Vec<_>>();
        ensure!(children.len() <= 1, "supervisor 子进程不明确");
        children
            .first()
            .map(|v| v.parse::<u32>())
            .transpose()?
            .unwrap_or(0)
    };
    Ok((pid > 1).then_some(pid))
}
pub(super) fn process_ready(pid: u32, binary: &Path, confirmed: Option<u32>) -> Result<bool> {
    if !process_matches(pid, binary)? {
        ensure!(confirmed.is_none(), "验收期间程序发生变化");
        return Ok(false);
    }
    ensure!(confirmed.is_none_or(|prior| prior == pid), "验收期间进程重启");
    Ok(true)
}

pub(super) fn process_matches(pid: u32, binary: &Path) -> Result<bool> {
    let process = Path::new("/proc").join(pid.to_string());
    if !process.exists() {
        return Ok(false);
    }
    Ok(fs::read_link(process.join("exe"))? == binary.canonicalize()?)
}
fn process_start(pid: u32) -> Result<Option<String>> {
    let path = PathBuf::from(format!("/proc/{pid}/stat"));
    if !path.exists() {
        return Ok(None);
    }
    let stat = fs::read_to_string(path)?;
    Ok(Some(
        stat.rsplit_once(')')
            .context("进程状态无效")?
            .1
            .split_whitespace()
            .nth(19)
            .context("进程启动时间缺失")?
            .into(),
    ))
}
fn ancestor(pid: u32) -> Result<bool> {
    let mut current = std::process::id();
    while current > 1 {
        if current == pid {
            return Ok(true);
        }
        let stat = fs::read_to_string(format!("/proc/{current}/stat"))?;
        current = stat
            .rsplit_once(')')
            .context("进程状态无效")?
            .1
            .split_whitespace()
            .nth(1)
            .context("父进程缺失")?
            .parse()?;
    }
    Ok(false)
}
fn idle_maintenance(manager: &Manager) -> Result<()> {
    for entry in fs::read_dir(manager.path("/var/lib/chain-maintenance"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|s| s == "json") {
            ordinary(&path)?;
            let job: Value = serde_json::from_slice(&fs::read(path)?)?;
            ensure!(
                !matches!(job["state"].as_str(), Some("queued" | "running")),
                "维护任务尚未结束，未中断执行器"
            );
        }
    }
    Ok(())
}
fn legacy_maintenance(manager: &Manager, pid: u32) -> Result<bool> {
    let process = PathBuf::from(format!("/proc/{pid}"));
    ensure!(fs::metadata(&process)?.uid() == 0, "维护进程不属于 root");
    let bytes = fs::read(process.join("cmdline"))?;
    let args = bytes
        .split(|c| *c == 0)
        .filter(|v| !v.is_empty())
        .map(|v| String::from_utf8(v.to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let exe = fs::read_link(process.join("exe"))?;
    let script = manager
        .base
        .join("current-panel/scripts/chain_maintenance.py")
        .display()
        .to_string();
    let python = exe
        .file_name()
        .is_some_and(|s| s.to_string_lossy().starts_with("python3"));
    if python && args.len() == 3 && args[1..] == ["-B", &script] {
        return Ok(true);
    }
    ensure!(
        args.len() == 2
            && args[1] == "maintenance"
            && exe.starts_with(manager.base.join("releases"))
            && exe.file_name().is_some_and(|s| s == "chainctl"),
        "维护进程命令不匹配，拒绝接管"
    );
    Ok(false)
}
async fn maintenance_running(manager: &Manager, binary: &Path) -> Result<()> {
    for _ in 0..100 {
        if let Some(pid) = service_process(manager, "maintenance").await? {
            if process_matches(pid, binary)?
                && manager.path("/run/chain-maintenance/control.sock").exists()
            {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("原生维护进程未完成接管")
}

pub async fn maintenance_handoff(value: &str) -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "维护接管需要 root");
    let identity: Value = serde_json::from_str(value)?;
    let pid = identity["pid"]
        .as_u64()
        .filter(|v| *v > 1 && *v <= u32::MAX as u64)
        .context("接管 PID 无效")? as u32;
    let start = identity["start"].as_str().context("接管启动时间缺失")?;
    let exe = identity["exe"].as_str().context("接管程序缺失")?;
    let manager = Manager::open(Path::new("/"))?;
    ensure!(manager.kind() == "openrc", "延后接管仅用于 OpenRC");
    let log = PathBuf::from(identity["log"].as_str().context("接管日志缺失")?);
    ordinary(&log)?;
    let log_info = fs::metadata(&log)?;
    ensure!(
        log.parent() == Some(manager.path("/var/lib/chain-maintenance").as_path())
            && log
                .file_name()
                .is_some_and(|s| s.to_string_lossy().starts_with(".handoff-"))
            && log_info.is_file()
            && log_info.uid() == 0
            && log_info.mode() & 0o777 == 0o600
            && log_info.nlink() == 1,
        "接管日志归属不符"
    );
    println!("等待旧维护进程 {pid}（启动时间 {start}）完成回执；超时 300 秒");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        if process_start(pid)?.as_deref() != Some(start) {
            break;
        }
        let current = fs::read_link(format!("/proc/{pid}/exe"))?;
        if current != Path::new(exe) {
            ensure!(
                process_matches(pid, &manager.base.join("current-panel/bin/chainctl"))?,
                "待接管维护进程身份已变化"
            );
            break;
        }
        ensure!(
            legacy_maintenance(&manager, pid)?,
            "待接管维护进程命令已变化"
        );
        ensure!(
            tokio::time::Instant::now() < deadline,
            "等待旧维护任务退出超时，未中断任务"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let _lock = manager.lock()?;
    idle_maintenance(&manager)?;
    let unit = manager.unit_path("maintenance");
    ordinary(&unit)?;
    let text = fs::read_to_string(&unit)?;
    ensure!(
        text.contains(OWNER) && native_command(&manager, &text, "maintenance")? == text,
        "维护服务已被修改，未重启"
    );
    if let Some(current) = service_process(&manager, "maintenance").await? {
        legacy_maintenance(&manager, current)?;
    }
    platform::related(&manager, "restart", "chain-maintenance").await?;
    maintenance_running(&manager, &manager.base.join("current-panel/bin/chainctl")).await?;
    let after = fs::symlink_metadata(&log)?;
    ensure!(
        (
            log_info.dev(),
            log_info.ino(),
            log_info.uid(),
            log_info.mode()
        ) == (after.dev(), after.ino(), after.uid(), after.mode()),
        "接管日志身份已变化，未删除"
    );
    fs::remove_file(log)?;
    Ok(())
}
fn defer_maintenance(manager: &Manager, pid: u32) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let log = tempfile::Builder::new()
        .prefix(".handoff-")
        .suffix(".log")
        .tempfile_in(manager.path("/var/lib/chain-maintenance"))?;
    let value = json!({"pid":pid,"start":process_start(pid)?.context("旧维护进程已退出")?,"exe":fs::read_link(format!("/proc/{pid}/exe"))?,"log":log.path()});
    let mut command = std::process::Command::new(manager.base.join("current-panel/bin/chainctl"));
    command
        .arg("internal-maintenance-handoff")
        .arg(serde_json::to_string(&value)?)
        .stdin(std::process::Stdio::null())
        .stdout(log.as_file().try_clone()?)
        .stderr(log.as_file().try_clone()?);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let (_, path) = log.keep()?;
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            fs::remove_file(path)?;
            return Err(error.into());
        }
    };
    println!(
        "维护接管助手 PID {} 等待旧任务完成后重启 OpenRC（最长 300 秒）；失败日志 {}，成功后自动清理",
        child.id(),
        path.display()
    );
    Ok(())
}

pub(super) struct ServiceMigration {
    files: Vec<(String, PathBuf, String, String)>,
    ingress: bool,
}
impl ServiceMigration {
    pub async fn needed(&self, manager: &Manager, roles: &[String]) -> Result<bool> {
        if self
            .files
            .iter()
            .any(|(_, _, before, after)| before != after)
        {
            return Ok(true);
        }
        if manager.live() && manager.kind() != "launchd" {
            for role in roles {
                let Some(pid) = service_process(manager, role).await? else {
                    return Ok(true);
                };
                if !process_matches(
                    pid,
                    &manager
                        .base
                        .join(format!("current-{role}/bin/chain-{role}")),
                )? {
                    return Ok(true);
                }
            }
            if self.ingress && manager.kind() == "openrc" {
                let Some(pid) = service_process(manager, "ingress").await? else {
                    return Ok(true);
                };
                if !process_matches(pid, &manager.base.join("current-agent/bin/chainctl"))? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    pub fn prepare(manager: &Manager, roles: &[String]) -> Result<Self> {
        let ingress = roles.iter().any(|r| r == "agent")
            && manager.path("/var/lib/chain-ingress/state.json").exists();
        let mut result = Self {
            files: Vec::new(),
            ingress,
        };
        for role in roles {
            if manager.kind() == "systemd" {
                let path = manager.unit_path(role);
                let before = fs::read_to_string(&path)?;
                let value = field(&before, "EnvironmentFile=")?;
                let env = manager
                    .path(&format!("/etc/chain-{role}/{role}.env"))
                    .display()
                    .to_string();
                let literal = env.replace('%', "%%");
                ensure!(
                    value == literal || value == format!("\"{literal}\""),
                    "拒绝覆盖自定义环境文件：chain-{role}"
                );
                let after = replace_field(&before, "EnvironmentFile=", &literal)?;
                result.push(manager, role, path, before, after)?;
            }
        }
        if ingress && manager.kind() != "launchd" {
            let path = manager.unit_path("ingress");
            ordinary(&path)?;
            let before = fs::read_to_string(&path).context("统一入口已启用但服务定义缺失")?;
            ensure!(
                before.contains(OWNER) || before.contains("# chain-ingress managed v1"),
                "统一入口服务归属不符"
            );
            let after = native_command(manager, &before, "ingress")?;
            result.push(manager, "agent", path, before, after)?;
            if manager.kind() == "systemd" {
                let path = manager.path("/etc/systemd/system/chain-ingress.path");
                ordinary(&path)?;
                let text = fs::read_to_string(&path).context("统一入口已启用但 path 定义缺失")?;
                ensure!(
                    text.contains(OWNER) || text.contains("# chain-ingress managed v1"),
                    "统一入口 path 归属不符"
                );
                result.push(manager, "agent", path, text.clone(), text)?;
            }
        }
        Ok(result)
    }
    fn push(
        &mut self,
        manager: &Manager,
        role: &str,
        path: PathBuf,
        before: String,
        after: String,
    ) -> Result<()> {
        ordinary(&path)?;
        let info = fs::symlink_metadata(&path)?;
        ensure!(
            info.is_file()
                && info.nlink() == 1
                && (!manager.live() || (info.uid() == 0 && info.mode() & 0o022 == 0)),
            "服务文件类型或归属不符"
        );
        if manager.live() && manager.kind() == "systemd" {
            let name = path.file_name().unwrap().to_string_lossy();
            for base in [
                "/etc/systemd/system",
                "/run/systemd/system",
                "/usr/lib/systemd/system",
                "/lib/systemd/system",
            ] {
                let dropins = manager.path(base).join(format!("{name}.d"));
                ensure!(
                    !dropins.exists() || fs::read_dir(dropins)?.next().is_none(),
                    "服务包含自定义 drop-in，未覆盖：{name}"
                );
            }
        }
        self.files.push((role.into(), path, before, after));
        Ok(())
    }
    pub fn journal(&self) -> Value {
        json!(self.files.iter().map(|(role, path, before, after)| json!({"role":role,"path":path,"before":before,"after":after})).collect::<Vec<_>>())
    }
    pub async fn write(&self, manager: &Manager, role: &str, forward: bool) -> Result<()> {
        for (_, path, before, after) in self.files.iter().filter(|(r, _, _, _)| r == role) {
            if before == after {
                continue;
            }
            let (expected, value) = if forward {
                (before, after)
            } else {
                (after, before)
            };
            if !forward && fs::read_to_string(path)? == *value {
                continue;
            }
            super::ingress::write_existing(path, expected, value)?;
        }
        if manager.live() && manager.kind() == "systemd" {
            command("systemctl", &["daemon-reload"], 30).await?;
        }
        Ok(())
    }
    pub async fn ingress(&self, manager: &Manager, action: &str) -> Result<()> {
        if !self.ingress || !manager.live() {
            return Ok(());
        }
        if manager.kind() == "systemd" {
            command("systemctl", &[action, "chain-ingress.path"], 30).await?;
            if action == "stop" {
                platform::related(manager, "stop", "chain-ingress").await?;
            }
        } else {
            platform::related(manager, action, "chain-ingress").await?;
        }
        Ok(())
    }
    pub fn request(&self, manager: &Manager) -> Result<Option<Value>> {
        if !self.ingress
            || !manager.live()
            || !manager.path("/var/lib/chain-agent/active.json").exists()
        {
            return Ok(None);
        }
        let path = manager.path("/var/lib/chain-agent/ingress-request.json");
        Ok(Some(if path.exists() {
            serde_json::from_slice::<Value>(&fs::read(path)?)?["id"].clone()
        } else {
            Value::Null
        }))
    }
    pub fn applied(&self, manager: &Manager, old: Option<Value>) -> Result<()> {
        if let Some(old) = old {
            let request: Value = serde_json::from_slice(&fs::read(
                manager.path("/var/lib/chain-agent/ingress-request.json"),
            )?)?;
            let status: Value = serde_json::from_slice(&fs::read(
                manager.path("/var/lib/chain-ingress/status.json"),
            )?)?;
            ensure!(
                request["id"].is_string()
                    && request["id"] != old
                    && status["id"] == request["id"]
                    && status["applied"] == true,
                "升级后统一入口新请求未应用：{}",
                status["error"].as_str().unwrap_or("状态未更新")
            );
        }
        Ok(())
    }
}

pub fn open(root: &Path) -> Result<Manager> {
    let root = root.canonicalize()?;
    ensure!(
        root != Path::new("/") || unsafe { libc::geteuid() } == 0,
        "安装需要 root 或 sudo"
    );
    let base = root.join("opt/chain");
    ordinary(&base)?;
    if !base.join(".chain-install.json").exists() {
        ensure!(
            !base.exists() || fs::read_dir(&base)?.next().is_none(),
            "安装目录已有未识别内容，拒绝接管"
        );
        fs::create_dir_all(base.join("releases"))?;
        atomic(
            &base.join(".chain-install.json"),
            serde_json::to_string(&json!({"owner":OWNER,"roles":{}}))?.as_bytes(),
            0o600,
        )?;
    }
    Manager::open(&root)
}
pub fn access(manager: &Manager) -> Result<()> {
    let env = manager.environment("panel")?;
    println!(
        "控制台：{}",
        env.get("CHAIN_PUBLIC_URL")
            .or(env.get("CHAIN_LISTEN"))
            .context("未配置访问地址")?
    );
    println!(
        "登录令牌：{}",
        fs::read_to_string(manager.path("/var/lib/chain-panel/admin-token"))?.trim()
    );
    Ok(())
}
pub fn prompt(label: &str, default: &str) -> Result<String> {
    ensure!(
        io::stdin().is_terminal(),
        "需要交互终端或显式提供 --non-interactive 及参数"
    );
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut v = String::new();
    ensure!(io::stdin().read_line(&mut v)? > 0, "输入已结束");
    let v = v.trim();
    Ok(if v.is_empty() {
        default.into()
    } else {
        v.into()
    })
}
pub fn secret(label: &str) -> Result<String> {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicPtr, Ordering};
    static TERMINAL: AtomicPtr<libc::termios> = AtomicPtr::new(std::ptr::null_mut());
    extern "C" fn interrupted(signal: libc::c_int) {
        let previous = TERMINAL.load(Ordering::SeqCst);
        if !previous.is_null() {
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, previous) };
        }
        unsafe { libc::_exit(128 + signal) };
    }
    ensure!(io::stdin().is_terminal(), "需要交互终端");
    let fd = io::stdin().as_raw_fd();
    let mut previous = std::mem::MaybeUninit::<libc::termios>::uninit();
    ensure!(
        unsafe { libc::tcgetattr(fd, previous.as_mut_ptr()) } == 0,
        "无法读取终端状态"
    );
    let previous = unsafe { previous.assume_init() };
    let mut hidden = previous;
    hidden.c_lflag &= !libc::ECHO;
    struct Restore(i32, libc::termios, libc::sighandler_t, libc::sighandler_t);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe { libc::tcsetattr(self.0, libc::TCSANOW, &self.1) };
            unsafe {
                libc::signal(libc::SIGINT, self.2);
                libc::signal(libc::SIGTERM, self.3);
            }
            TERMINAL.store(std::ptr::null_mut(), Ordering::SeqCst);
            println!();
        }
    }
    let mut restore = Restore(
        fd,
        previous,
        unsafe { libc::signal(libc::SIGINT, interrupted as *const () as libc::sighandler_t) },
        unsafe {
            libc::signal(
                libc::SIGTERM,
                interrupted as *const () as libc::sighandler_t,
            )
        },
    );
    TERMINAL.store(&mut restore.1, Ordering::SeqCst);
    ensure!(
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } == 0,
        "无法关闭输入回显"
    );
    prompt(label, "")
}
pub fn handoff(manager: &Manager) -> Result<()> {
    let state = manager.state()?;
    let role = ["panel", "agent"]
        .into_iter()
        .find(|r| {
            state["roles"][*r].is_object()
                && manager
                    .base
                    .join(format!("current-{r}/bin/chainctl"))
                    .is_file()
        })
        .context("已安装组件未提供 Rust chainctl")?;
    let target = manager.base.join(format!("current-{role}/bin/chainctl"));
    let manifest = package::verify(
        &manager.base.join("releases").join(
            state["roles"][role]["current"]
                .as_str()
                .context("缺少已安装版本")?,
        ),
        true,
    )?;
    ensure!(
        manifest.files.contains_key("bin/chainctl"),
        "发行包未提供 Rust chainctl"
    );
    let wrapper = manager.path("/usr/local/bin/chainctl");
    ordinary(wrapper.parent().unwrap())?;
    if let Ok(info) = fs::symlink_metadata(&wrapper) {
        if info.file_type().is_symlink() {
            ensure!(
                ["panel", "agent"]
                    .iter()
                    .any(|r| fs::read_link(&wrapper).ok().as_ref()
                        == Some(&manager.base.join(format!("current-{r}/bin/chainctl")))),
                "chainctl 链接归属不匹配"
            );
        } else {
            ensure!(
                info.is_file() && fs::read_to_string(&wrapper)?.contains(OWNER),
                "现有 chainctl 不属于本项目，拒绝覆盖"
            );
        }
    }
    fs::create_dir_all(wrapper.parent().unwrap())?;
    let tmp = tempfile::Builder::new()
        .prefix(".chainctl-")
        .tempdir_in(wrapper.parent().unwrap())?;
    symlink(&target, tmp.path().join("chainctl"))?;
    fs::rename(tmp.path().join("chainctl"), &wrapper)?;
    Ok(())
}
pub async fn maintenance(manager: &Manager) -> Result<()> {
    if manager.kind() == "launchd" || !manager.state()?["roles"]["panel"].is_object() {
        return Ok(());
    }
    let dir = manager.path("/var/lib/chain-maintenance");
    ordinary(&dir)?;
    if dir.exists() {
        ensure!(
            fs::read_to_string(dir.join(".chain-owner"))? == OWNER,
            "维护目录归属不匹配"
        );
    }
    owned_dir(
        &dir,
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
        0o700,
    )?;
    if !dir.join(".chain-owner").exists() {
        atomic(&dir.join(".chain-owner"), OWNER.as_bytes(), 0o600)?;
    }
    let binary = manager
        .base
        .join("current-panel/bin/chainctl")
        .to_string_lossy()
        .into_owned();
    let previous = if manager.live() {
        service_process(manager, "maintenance").await?
    } else {
        None
    };
    let mut from_job = false;
    let mut legacy = false;
    let needs_restart = if let Some(pid) = previous {
        legacy = legacy_maintenance(manager, pid)?;
        from_job = ancestor(pid)?;
        let stale = !process_matches(pid, Path::new(&binary))?;
        if stale && !from_job {
            idle_maintenance(manager)?;
        }
        stale
    } else {
        false
    };
    let definitions = if manager.kind() == "systemd" {
        vec![
            (
                manager.path("/etc/systemd/system/chain-maintenance.socket"),
                format!(
                    "# {OWNER}\n[Unit]\nDescription=Chain maintenance socket\n[Socket]\nListenStream=/run/chain-maintenance/control.sock\nSocketUser=root\nSocketGroup=chain-panel\nSocketMode=0660\nDirectoryMode=0755\nRemoveOnStop=true\n[Install]\nWantedBy=sockets.target\n"
                ),
                0o644,
            ),
            (
                manager.path("/etc/systemd/system/chain-maintenance.service"),
                format!(
                    "# {OWNER}\n[Unit]\nDescription=Chain native maintenance\n[Service]\nUser=root\nGroup=root\nUMask=0077\nStateDirectory=chain-maintenance\nStateDirectoryMode=0700\nExecStart=\"{binary}\" maintenance\nPrivateTmp=true\nProtectHome=true\nKillMode=control-group\nTimeoutStopSec=45\n"
                ),
                0o644,
            ),
        ]
    } else {
        vec![(
            manager.path("/etc/init.d/chain-maintenance"),
            platform::definition(
                manager,
                "chain-maintenance",
                &[binary.clone(), "maintenance".into()],
                None,
                &Default::default(),
            )?,
            0o755,
        )]
    };
    for (path, _, _) in &definitions {
        ordinary(path)?;
        if path.exists() {
            ensure!(
                fs::read_to_string(path)?.contains(OWNER),
                "维护服务归属不匹配"
            );
        }
    }
    for (path, text, mode) in definitions {
        fs::create_dir_all(path.parent().unwrap())?;
        if path.exists() {
            let before = fs::read_to_string(&path)?;
            let after = if path == manager.unit_path("maintenance") {
                native_command(manager, &before, "maintenance")?
            } else {
                before.clone()
            };
            let mut checked = ServiceMigration {
                files: Vec::new(),
                ingress: false,
            };
            checked.push(
                manager,
                "maintenance",
                path.clone(),
                before.clone(),
                after.clone(),
            )?;
            if before != after {
                super::ingress::write_existing(&path, &before, &after)?;
            }
        } else {
            atomic(&path, text.as_bytes(), mode)?;
        }
    }
    if manager.live() {
        if manager.kind() == "systemd" {
            command("systemctl", &["daemon-reload"], 30).await?;
            command(
                "systemctl",
                &["enable", "--now", "chain-maintenance.socket"],
                30,
            )
            .await?;
        } else {
            platform::enable(manager, "chain-maintenance").await?;
            if !super::upgrade::output("rc-service", &["chain-maintenance", "status"], 15)
                .await?
                .status
                .success()
            {
                platform::related(manager, "start", "chain-maintenance").await?;
            }
        }
        if needs_restart {
            if from_job {
                if manager.kind() == "openrc" && legacy {
                    defer_maintenance(manager, previous.unwrap())?;
                }
            } else {
                idle_maintenance(manager)?;
                platform::related(manager, "restart", "chain-maintenance").await?;
                maintenance_running(manager, Path::new(&binary)).await?;
            }
        } else if manager.kind() == "openrc" && previous.is_none() {
            maintenance_running(manager, Path::new(&binary)).await?;
        }
    }
    Ok(())
}
fn mark_local(manager: &Manager, id: &str, token: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    uuid::Uuid::parse_str(id)?;
    super::install::token(token)?;
    let data = manager.path("/var/lib/chain-panel");
    let meta = fs::metadata(&data)?;
    owned_write(
        &data.join("local-system-node"),
        format!("{id}\n").as_bytes(),
        0o600,
        meta.uid(),
        meta.gid(),
    )?;
    owned_write(
        &data.join("local-system-identity.json"),
        serde_json::to_string(
            &json!({"id":id,"token_hash":hex::encode(Sha256::digest(token.as_bytes()))}),
        )?
        .as_bytes(),
        0o600,
        meta.uid(),
        meta.gid(),
    )
}
pub async fn local_agent(manager: &Manager, source: &Path, host: &str, target: &str) -> Result<()> {
    let state = manager.state()?;
    let mut addr: std::net::SocketAddr = manager.environment("panel")?["CHAIN_LISTEN"].parse()?;
    if addr.ip().is_unspecified() {
        addr.set_ip(if addr.is_ipv6() {
            "::1".parse()?
        } else {
            "127.0.0.1".parse()?
        });
    }
    ensure!(addr.ip().is_loopback(), "本机自动接入只能连接回环控制台");
    let base = format!("http://{addr}");
    if state["roles"]["agent"].is_object() || state["retained_roles"]["agent"].is_object() {
        let env = manager.environment("agent")?;
        let endpoint = super::install::url(env.get("CHAIN_PANEL_URL").context("Agent 地址缺失")?)?;
        let local = endpoint.scheme() == "http"
            && endpoint.port_or_known_default() == Some(addr.port())
            && endpoint.host_str().is_some_and(|h| {
                h == "localhost"
                    || h.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
        if local {
            if !state["roles"]["agent"].is_object() {
                install::component(manager, source, "agent", &Options::default()).await?;
            }
            mark_local(
                manager,
                env.get("CHAIN_NODE_ID").context("节点 ID 缺失")?,
                fs::read_to_string(manager.path("/etc/chain-agent/token"))?.trim(),
            )?;
        }
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()?;
    let response=client.post(format!("{base}/api/login")).header("X-Chain-Request","1").json(&json!({"token":fs::read_to_string(manager.path("/var/lib/chain-panel/admin-token"))?.trim()})).send().await?.error_for_status()?;
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .context("缺少登录会话")?
        .to_str()?
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let request = |path: &str| {
        client
            .post(format!("{base}/api/{path}"))
            .header("X-Chain-Request", "1")
            .header(reqwest::header::COOKIE, &cookie)
    };
    let server_name = if target.is_empty() {
        String::new()
    } else {
        crate::compiler::target(target)?.0
    };
    let node: Value = request("nodes")
        .json(&json!({"name":"本机节点","host":host,"target":target,"server_name":server_name}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let result: Result<()> = async {
        let id = node["id"].as_str().context("接入缺少节点 ID")?;
        install::component(
            manager,
            source,
            "agent",
            &Options {
                panel_url: Some(base.clone()),
                node_id: Some(id.into()),
                node_token: Some(node["token"].as_str().context("接入缺少令牌")?.into()),
                ..Default::default()
            },
        )
        .await?;
        mark_local(manager, id, node["token"].as_str().context("令牌缺失")?)?;
        Ok(())
    }
    .await;
    if result.is_err() && !manager.state()?["roles"]["agent"].is_object() {
        let _ = client
            .delete(format!(
                "{base}/api/nodes/{}",
                node["id"].as_str().unwrap_or_default()
            ))
            .header("X-Chain-Request", "1")
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await;
    }
    let _ = request("logout").json(&json!({})).send().await;
    result
}
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_waits_for_exec_but_rejects_changes_after_confirmation() {
        use std::io::Write;
        use std::process::{Child, Command, Stdio};
        use std::time::Instant;
        struct Reap(Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("agent");
        fs::copy("/bin/sleep", &binary).unwrap();
        let mut child = Reap(Command::new("/bin/sh")
            .args(["-c", "read ready; exec \"$1\" 30", "startup"])
            .arg(&binary).stdin(Stdio::piped()).spawn().unwrap());
        let pid = child.0.id();
        // A live pre-exec process must neither pass acceptance nor fail immediately.
        for _ in 0..3 {
            assert!(!process_ready(pid, &binary, None).unwrap());
        }
        writeln!(child.0.stdin.take().unwrap(), "go").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !process_ready(pid, &binary, None).unwrap() {
            assert!(Instant::now() < deadline, "child did not exec");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(process_ready(pid, &binary, Some(pid)).unwrap());
        assert!(process_ready(pid, &binary, Some(pid + 1)).is_err());
        assert!(process_ready(pid, Path::new("/bin/sh"), Some(pid)).is_err());
        assert!(!process_ready(pid, Path::new("/bin/sh"), None).unwrap());
    }

    #[tokio::test]
    async fn fresh_maintenance_does_not_query_a_missing_service() {
        let root = tempfile::tempdir().unwrap();
        let manager = open(root.path()).unwrap();
        assert_eq!(
            service_process(&manager, "maintenance").await.unwrap(),
            None
        );
        let unit = manager.unit_path("maintenance");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        symlink("missing", &unit).unwrap();
        assert!(service_process(&manager, "maintenance").await.is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn managed_commands_migrate_without_replacing_custom_settings() {
        let r = tempfile::tempdir().unwrap();
        let manager = open(r.path()).unwrap();
        let old = format!(
            "# {OWNER}\n[Service]\nExecStart=\"/usr/bin/python3.12\" -B \"{}/current-agent/scripts/chain_ingress.py\" apply\nPrivateTmp=true\n",
            manager.base.display()
        );
        let changed = native_command(&manager, &old, "ingress").unwrap();
        assert!(changed.ends_with("\nPrivateTmp=true\n"));
        assert!(changed.contains("/current-agent/bin/chainctl\" internal-ingress-apply"));
        assert_eq!(
            native_command(&manager, &changed, "ingress").unwrap(),
            changed
        );
        let patch = changed.replace(
            "current-agent/bin/chainctl",
            "repairs/chainctl-0.2.66-nginx-log-linux-x86_64-d60fd0307d6c",
        );
        assert_eq!(
            native_command(&manager, &patch, "ingress").unwrap(),
            changed
        );
        assert!(native_command(&manager, &old.replace(" apply", " custom"), "ingress").is_err());
        assert!(native_command(&manager, &format!("{old}ExecStart=/custom\n"), "ingress").is_err());
        assert!(words("$(touch /tmp/foreign)").is_err());

        fs::create_dir_all(manager.path("/run/openrc")).unwrap();
        for role in ["ingress", "maintenance"] {
            let component = if role == "ingress" { "agent" } else { "panel" };
            let mut argv = vec![
                "/usr/bin/python3".into(),
                "-B".into(),
                manager
                    .base
                    .join(format!("current-{component}/scripts/chain_{role}.py"))
                    .display()
                    .to_string(),
            ];
            if role == "ingress" {
                argv.push("watch".into());
            }
            let old = platform::definition(
                &manager,
                &format!("chain-{role}"),
                &argv,
                None,
                &[("PYTHONDONTWRITEBYTECODE".into(), "1".into())].into(),
            )
            .unwrap();
            let new = native_command(&manager, &old, role).unwrap();
            assert!(new.contains("export PYTHONDONTWRITEBYTECODE='1'"));
            assert!(!new.contains("scripts/chain_"));
            assert_eq!(native_command(&manager, &new, role).unwrap(), new);
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn maintenance_unit_migration_retains_inode_and_existing_settings() {
        let r = tempfile::tempdir().unwrap();
        let manager = open(r.path()).unwrap();
        let mut state = manager.state().unwrap();
        state["roles"]["panel"] = json!({"current":"old","version":"0.2.61"});
        fs::write(
            manager.base.join(".chain-install.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        let unit = manager.unit_path("maintenance");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, format!("# {OWNER}\n[Service]\nExecStart=\"/usr/bin/python3\" -B \"{}/current-panel/scripts/chain_maintenance.py\"\nPrivateTmp=true\nTimeoutStopSec=91\n", manager.base.display())).unwrap();
        let before = fs::metadata(&unit).unwrap();
        maintenance(&manager).await.unwrap();
        let after = fs::metadata(&unit).unwrap();
        assert_eq!(
            (before.ino(), before.uid(), before.gid(), before.mode()),
            (after.ino(), after.uid(), after.gid(), after.mode())
        );
        assert!(
            fs::read_to_string(&unit)
                .unwrap()
                .ends_with("PrivateTmp=true\nTimeoutStopSec=91\n")
        );
        maintenance(&manager).await.unwrap();
        let repeated = fs::metadata(unit).unwrap();
        assert_eq!(
            (after.ctime(), after.ctime_nsec()),
            (repeated.ctime(), repeated.ctime_nsec())
        );
    }
    #[test]
    fn fresh_state_and_foreign_directory() {
        let r = tempfile::tempdir().unwrap();
        let m = open(r.path()).unwrap();
        assert_eq!(m.state().unwrap()["owner"], OWNER);
        assert!(open(r.path()).is_ok());
        let other = tempfile::tempdir().unwrap();
        fs::create_dir_all(other.path().join("opt/chain")).unwrap();
        fs::write(other.path().join("opt/chain/foreign"), "keep").unwrap();
        assert!(open(other.path()).is_err());
    }
}

pub async fn uninstall(manager: &Manager, purge: bool) -> Result<()> {
    ensure!(
        !manager.base.join(".update-pending.json").exists()
            && !manager.base.join(".core-pending.json").exists(),
        "存在未完成的程序或内核更新，请先处理后卸载"
    );
    let state = manager.state()?;
    let mut roles = state["retained_roles"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    roles.extend(state["roles"].as_object().context("安装状态无效")?.clone());
    ensure!(!roles.is_empty(), "没有可卸载组件");
    let wrapper = manager.path("/usr/local/bin/chainctl");
    if wrapper.is_symlink() {
        let target = fs::read_link(&wrapper)?;
        ensure!(
            ["panel", "agent"]
                .iter()
                .any(|r| target == manager.base.join(format!("current-{r}/bin/chainctl"))),
            "系统 chainctl 归属不匹配"
        );
    } else if wrapper.exists() {
        ensure!(
            fs::read_to_string(&wrapper)?.contains(OWNER),
            "系统 chainctl 归属不匹配"
        );
    }
    let mut units = vec![];
    for role in roles.keys() {
        ensure!(matches!(role.as_str(), "panel" | "agent"), "组件无效");
        let path = manager.unit_path(role);
        ordinary(&path)?;
        if path.exists() {
            ensure!(fs::read_to_string(&path)?.contains(OWNER), "服务归属不匹配");
            units.push((role.clone(), path));
        }
    }
    let dirs = roles
        .keys()
        .flat_map(|r| {
            [
                manager.path(&format!("/etc/chain-{r}")),
                manager.path(&format!("/var/lib/chain-{r}")),
            ]
        })
        .collect::<Vec<_>>();
    for path in &dirs {
        ordinary(path)?;
        if path.exists() {
            let meta = fs::metadata(path)?;
            ensure!(
                meta.is_dir() && meta.dev() == fs::metadata(path.parent().unwrap())?.dev(),
                "拒绝删除挂载目录"
            );
        }
    }
    let mut owned_files = vec![];
    let nginx_root = super::nginx::main_path(manager)
        .parent()
        .context("Nginx 路径无效")?
        .to_path_buf();
    fn sites(path: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        ordinary(path)?;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let meta = entry.file_type()?;
            if meta.is_dir() {
                sites(&entry.path(), out)?;
            } else if matches!(
                entry.file_name().to_str(),
                Some("chain-panel.conf" | "chain-proxy-acme.conf")
            ) {
                ordinary(&entry.path())?;
                out.push(entry.path());
            }
        }
        Ok(())
    }
    let mut site_files = vec![];
    sites(&nginx_root, &mut site_files)?;
    for path in &site_files {
        ensure!(
            fs::read_to_string(path)?.contains(super::https::OWNER),
            "站点归属不匹配"
        );
    }
    for (path, owner) in [
        (
            manager.path("/etc/letsencrypt/renewal-hooks/deploy/chain-panel-nginx"),
            super::https::OWNER,
        ),
        (
            manager.path("/etc/chain-panel-https/nginx.conf"),
            super::https::OWNER,
        ),
    ] {
        ordinary(&path)?;
        if path.exists() {
            ensure!(
                fs::read_to_string(&path)?.contains(owner),
                "HTTPS 文件归属不匹配"
            );
            owned_files.push(path);
        }
    }
    let mut auxiliary = vec![];
    for (name, owner) in [
        ("chain-panel-https", super::https::OWNER),
        ("chain-panel-cert-renew", super::https::OWNER),
        ("chain-ingress", "# chain-ingress managed v1"),
    ] {
        let suffixes = if manager.kind() == "systemd" {
            if name == "chain-panel-cert-renew" {
                vec!["service", "timer"]
            } else if name == "chain-ingress" {
                vec!["service", "path"]
            } else {
                vec!["service"]
            }
        } else {
            vec![""]
        };
        for suffix in suffixes {
            let path = if manager.kind() == "systemd" {
                manager.path(&format!("/etc/systemd/system/{name}.{suffix}"))
            } else if manager.kind() == "openrc" {
                manager.path(&format!("/etc/init.d/{name}"))
            } else {
                manager.path(&format!(
                    "/Library/LaunchDaemons/org.vless-chain.{name}.plist"
                ))
            };
            ordinary(&path)?;
            if path.exists() {
                let text = fs::read_to_string(&path)?;
                ensure!(
                    text.contains(owner) || text.contains(OWNER),
                    "辅助服务归属不匹配"
                );
                auxiliary.push((name.to_owned(), suffix.to_owned(), path));
            }
        }
    }
    let backups = manager.path("/var/backups/chain-panel");
    if purge && backups.exists() {
        ordinary(&backups)?;
        for entry in fs::read_dir(&backups)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            ensure!(
                entry.file_type()?.is_file()
                    && (name.starts_with("panel-") || name.starts_with("agent-"))
                    && name.ends_with(".tar.gz"),
                "备份目录包含未知文件"
            );
        }
    }
    if manager.path("/var/lib/chain-ingress/state.json").exists() {
        super::ingress::operate(manager, "disable").await?;
    }
    let _lock = manager.lock()?;
    ensure!(manager.state()? == state, "安装状态已改变");
    for (role, path) in units {
        manager.service("stop", &role).await?;
        ensure!(!manager.active(&role).await?, "服务尚未停止");
        platform::related(manager, "disable", &format!("chain-{role}")).await?;
        fs::remove_file(path)?;
    }
    if manager.kind() == "systemd" {
        for suffix in ["socket", "service"] {
            let path = manager.path(&format!("/etc/systemd/system/chain-maintenance.{suffix}"));
            ordinary(&path)?;
            if path.exists() {
                ensure!(
                    fs::read_to_string(&path)?.contains(OWNER),
                    "维护服务归属不匹配"
                );
                if manager.live() {
                    command(
                        "systemctl",
                        &["disable", "--now", &format!("chain-maintenance.{suffix}")],
                        60,
                    )
                    .await?;
                }
                fs::remove_file(path)?;
            }
        }
    } else if manager.kind() == "openrc" {
        let path = manager.path("/etc/init.d/chain-maintenance");
        ordinary(&path)?;
        if path.exists() {
            ensure!(
                fs::read_to_string(&path)?.contains(OWNER),
                "维护服务归属不匹配"
            );
            platform::related(manager, "stop", "chain-maintenance").await?;
            platform::related(manager, "disable", "chain-maintenance").await?;
            fs::remove_file(path)?;
        }
    }
    for (name, suffix, path) in auxiliary {
        if manager.live() {
            if manager.kind() == "systemd" {
                command(
                    "systemctl",
                    &["disable", "--now", &format!("{name}.{suffix}")],
                    60,
                )
                .await?;
            } else {
                platform::related(manager, "stop", &name).await?;
                platform::related(manager, "disable", &name).await?;
            }
        }
        fs::remove_file(path)?;
    }
    let previous = site_files
        .iter()
        .map(|p| Ok((p.clone(), fs::read(p)?, fs::metadata(p)?.mode() & 0o777)))
        .collect::<Result<Vec<_>>>()?;
    for (path, _, _) in &previous {
        fs::remove_file(path)?;
    }
    if manager.live() && !previous.is_empty() {
        if let Err(error) = super::nginx::reload(manager).await {
            for (path, bytes, mode) in &previous {
                atomic(path, bytes, *mode)?;
            }
            return Err(error.context("移除面板站点失败，站点内容已恢复"));
        }
    }
    for path in owned_files {
        fs::remove_file(path)?;
    }
    if fs::symlink_metadata(&wrapper).is_ok() {
        fs::remove_file(wrapper)?;
    }
    for role in roles.keys() {
        let link = manager.base.join(format!("current-{role}"));
        if link.is_symlink() {
            fs::remove_file(link)?;
        }
    }
    if purge {
        if manager.live() {
            for role in roles.keys() {
                platform::remove_account(&format!("chain-{role}")).await?;
            }
        }
        for path in dirs.into_iter().chain([backups]) {
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
    }
    let retained = if purge {
        json!({})
    } else {
        Value::Object(roles)
    };
    atomic(
        &manager.base.join(".chain-install.json"),
        &serde_json::to_vec_pretty(&json!({"owner":OWNER,"roles":{},"retained_roles":retained}))?,
        0o600,
    )?;
    if manager.kind() == "systemd" && manager.live() {
        command("systemctl", &["daemon-reload"], 30).await?;
    }
    drop(_lock);
    manager.prune().await?;
    println!(
        "服务与原生 chainctl 已卸载；{}。系统 Nginx、证书和手工备份保留。",
        if purge {
            "应用配置及数据已删除"
        } else {
            "配置及数据保留，可重新安装恢复"
        }
    );
    Ok(())
}
