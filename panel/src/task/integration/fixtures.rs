//! Small native subprocesses used by isolated acceptance tests.
use super::super::{Options, workspace::Workspace};
#[cfg(target_os = "macos")]
use super::{super::strings, harness::Harness, scenarios::wait};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::Path,
    time::{Duration, Instant},
};

pub fn command_fixture() -> Result<Option<i32>> {
    let Some(case) = std::env::var_os("TIME_CASE") else {
        return Ok(None);
    };
    let name = std::env::args().next().context("命令名缺失")?;
    let name = Path::new(&name)
        .file_name()
        .context("命令名无效")?
        .to_string_lossy()
        .to_string();
    ensure!(
        ["timedatectl", "systemctl", "chronyc", "apt-get"].contains(&name.as_str()),
        "时间夹具命令无效"
    );
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut row = vec![name.clone()];
    row.extend(args.clone());
    let path = std::env::var_os("TIME_CALLS").context("缺少夹具记录路径")?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    writeln!(file, "{}", serde_json::to_string(&row)?)?;
    let case: Value = serde_json::from_str(case.to_str().context("时间夹具无效")?)?;
    let code = if name == "timedatectl" {
        println!("Timezone=America/New_York\nNTP=yes\nNTPSynchronized=yes");
        0
    } else if name == "systemctl" && args.first().is_some_and(|s| s == "list-unit-files") {
        print!("{}", case["units"].as_str().unwrap());
        if case["inventory_error"] == true {
            eprintln!("inventory denied");
        }
        case["inventory_code"].as_i64().unwrap_or(0) as i32
    } else if name == "systemctl" && args.first().is_some_and(|s| s == "is-active") {
        if case["active"].as_array().is_some_and(|a| {
            a.iter()
                .any(|v| v.as_str() == args.last().map(String::as_str))
        }) {
            0
        } else {
            3
        }
    } else if name == "chronyc"
        && args.first().is_some_and(|s| s == "waitsync")
        && case["no_source"] == true
    {
        1
    } else {
        0
    };
    Ok(Some(code))
}

pub fn dispatch(args: &Options) -> Result<bool> {
    if args.action != "integration" {
        return Ok(false);
    }
    match args.values.get("--case").map(String::as_str) {
        Some("cleanup-worker") => {
            let root = Path::new(args.value("--input")?).canonicalize()?;
            let port: u16 = args.trailing.first().context("缺少清理夹具端口")?.parse()?;
            let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
            fs::write(root.join("ready"), listener.local_addr()?.to_string())?;
            println!("cleanup-worker-ready");
            std::thread::sleep(Duration::from_secs(45));
        }
        Some("fleet-tls-proxy") => super::fleet::proxy(args)?,
        Some("ingress-watcher") => {
            let root = Path::new(args.value("--input")?).canonicalize()?;
            let nginx: i32 = args.trailing.first().context("缺少 Nginx PID")?.parse()?;
            let request = root.join("agent-0/ingress-request.json");
            let routes = root.join("system/etc/chain-ingress/routes.conf");
            let status = root.join("system/var/lib/chain-ingress/status.json");
            let mut seen = String::new();
            loop {
                if let Ok(bytes) = fs::read(&request) {
                    let value: Value = serde_json::from_slice(&bytes)?;
                    if value["id"].as_str() != Some(&seen) {
                        seen = value["id"].as_str().context("路由请求缺少 ID")?.into();
                        let before = fs::read(&routes)?;
                        let result = (|| -> Result<()> {
                            let text = crate::ctl::ingress::routes(
                                &value["routes"],
                                &json!(["existing.test"]),
                            )?;
                            fs::write(&routes, text)?;
                            let checked = std::process::Command::new("nginx")
                                .args([
                                    "-t",
                                    "-c",
                                    root.join("nginx.conf").to_str().unwrap(),
                                    "-p",
                                    root.to_str().unwrap(),
                                ])
                                .output()?;
                            ensure!(
                                checked.status.success(),
                                "{}",
                                String::from_utf8_lossy(&checked.stderr)
                            );
                            ensure!(
                                unsafe { libc::kill(nginx, libc::SIGHUP) } == 0,
                                "Nginx 重载失败"
                            );
                            std::thread::sleep(Duration::from_millis(150));
                            Ok(())
                        })();
                        if result.is_err() {
                            fs::write(&routes, &before)?;
                            unsafe {
                                libc::kill(nginx, libc::SIGHUP);
                            }
                        }
                        fs::write(
                            status.with_extension("tmp"),
                            serde_json::to_vec(
                                &json!({"ready":true,"applied":result.is_ok(),"id":seen,"error":result.err().map(|e|e.to_string()).unwrap_or_default()}),
                            )?,
                        )?;
                        fs::rename(status.with_extension("tmp"), &status)?;
                    }
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        Some("sandbox-worker") => {
            let protected = Path::new(args.value("--input")?);
            ensure!(
                fs::write(protected, "bad")
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied),
                "沙箱允许写入外部文件"
            );
            ensure!(
                fs::set_permissions(protected, fs::Permissions::from_mode(0o777))
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied),
                "沙箱允许修改权限"
            );
            ensure!(
                std::net::TcpStream::connect("127.0.0.1:9")
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied),
                "沙箱允许联网"
            );
            fs::write("allowed", "ok")?;
        }
        Some("nginx-worker") => {
            let socket = std::net::TcpListener::bind("127.0.0.1:0")?;
            ensure!(socket.local_addr()?.port() == 0, "Nginx 检查实际监听了端口");
            let mut accept: libc::c_int = -1;
            let mut length = std::mem::size_of_val(&accept) as libc::socklen_t;
            ensure!(
                unsafe {
                    libc::getsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_ACCEPTCONN,
                        (&mut accept as *mut libc::c_int).cast(),
                        &mut length,
                    )
                } == 0
                    && accept == 0,
                "Nginx 检查建立了监听"
            );
            let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
            ensure!(
                socket
                    .send_to(b"blocked", "127.0.0.1:9")
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied),
                "Nginx 检查允许发送数据"
            );
        }
        Some("watcher-owner") => {
            let mut child = std::process::Command::new("sleep").arg("60").spawn()?;
            let mut watcher = std::process::Command::new(&args.trailing[0])
                .args([
                    "--chain-watch",
                    &std::process::id().to_string(),
                    &child.id().to_string(),
                ])
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let mut byte = [0];
            watcher.stdout.as_mut().unwrap().read_exact(&mut byte)?;
            ensure!(byte == [b'R'], "watcher 未注册");
            fs::write(
                args.value("--input")?,
                serde_json::to_vec(&json!([child.id(), watcher.id()]))?,
            )?;
            child.wait()?;
            watcher.wait()?;
        }
        Some("maintenance-request") => {
            use std::os::unix::net::UnixStream;
            let mut socket = UnixStream::connect(args.value("--input")?)?;
            socket.set_read_timeout(Some(Duration::from_secs(10)))?;
            socket.write_all(args.trailing[0].as_bytes())?;
            socket.write_all(b"\n")?;
            let mut result = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(socket), &mut result)?;
            print!("{result}");
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub async fn terminal_checks(work: &Workspace, ctl: &Path, root: &Path) -> Result<()> {
    let (code, body) = terminal(work, ctl, root, &["menu"], "选择", b"4\n0\n").await?;
    ensure!(
        code == 0
            && body.contains("chain-panel")
            && body.contains("chain-agent")
            && body.matches("Chain 原生管理").count() >= 2,
        "终端菜单未返回或缺少角色：{body}"
    );
    let fresh = root.parent().context("缺少父目录")?.join("fresh-terminal");
    fs::create_dir(&fresh)?;
    let (code, body) = terminal(
        work,
        ctl,
        &fresh,
        &["install", "--component", "agent"],
        "粘贴",
        b"PRIVATE_SENTINEL\x03",
    )
    .await?;
    ensure!(
        code == 130 && !body.contains("PRIVATE_SENTINEL"),
        "中断没有恢复终端或泄漏连接码：{body}"
    );
    Ok(())
}
async fn terminal(
    work: &Workspace,
    ctl: &Path,
    root: &Path,
    args: &[&str],
    marker: &str,
    keys: &[u8],
) -> Result<(i32, String)> {
    let mut command = vec![ctl.display().to_string()];
    command.extend(args.iter().map(|s| s.to_string()));
    command.extend(["--root".into(), root.display().to_string()]);
    terminal_command(work, &command, root, &Default::default(), &[(marker, keys)]).await
}
pub async fn terminal_command(
    work: &Workspace,
    argv: &[String],
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    steps: &[(&str, &[u8])],
) -> Result<(i32, String)> {
    let (mut master, mut slave) = (-1, -1);
    ensure!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        } == 0,
        "无法创建 PTY"
    );
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .envs(&work.env)
        .envs(env)
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave.try_clone()?);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let pid = work.register(command.spawn()?)?;
    drop(slave);
    ensure!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == 0,
        "PTY 无法设为非阻塞"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut body = vec![];
    let mut step = 0;
    let mut searched = 0;
    loop {
        let mut chunk = [0u8; 65536];
        match master.read(&mut chunk) {
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(libc::EIO) =>
            {
                ()
            }
            Err(e) => return Err(e.into()),
        }
        if let Some((marker, keys)) = steps.get(step) {
            if String::from_utf8_lossy(&body[searched..]).contains(marker) {
                master.write_all(keys)?;
                step += 1;
                searched = body.len();
            }
        }
        if let Some(status) = work.poll(pid)? {
            let mut attr = std::mem::MaybeUninit::<libc::termios>::uninit();
            ensure!(
                unsafe { libc::tcgetattr(master.as_raw_fd(), attr.as_mut_ptr()) } == 0,
                "无法读取终端状态"
            );
            ensure!(
                unsafe { attr.assume_init() }.c_lflag & libc::ECHO != 0,
                "终端回显未恢复"
            );
            return Ok((
                status.code().unwrap_or(-1),
                String::from_utf8_lossy(&body).into(),
            ));
        }
        ensure!(
            Instant::now() < deadline,
            "PTY 超时：{}",
            String::from_utf8_lossy(&body)
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[cfg(target_os = "macos")]
pub async fn watcher(h: &Harness<'_>) -> Result<()> {
    let marker = h.root.join("watcher.json");
    let exe = std::env::current_exe()?;
    let owner = h.launch(
        "parent-watch",
        &exe,
        Default::default(),
        &strings(&[
            "integration",
            "--case",
            "watcher-owner",
            "--input",
            marker.to_str().unwrap(),
            "--",
            h.bin.join("chain-agent").to_str().unwrap(),
        ]),
    )?;
    wait("watcher 注册", async || Ok(marker.exists())).await?;
    let watched: Vec<i32> = serde_json::from_slice(&fs::read(marker)?)?;
    unsafe {
        libc::kill(owner as i32, libc::SIGKILL);
    }
    wait("父进程死亡后回收核心", async || {
        Ok(watched
            .iter()
            .all(|pid| unsafe { libc::kill(*pid, 0) } != 0))
    })
    .await?;
    Ok(())
}
