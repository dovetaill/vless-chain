use super::{
    super::{Options, release, strings, workspace::Workspace},
    harness::map,
    scenarios::wait,
};
use crate::ctl::{package, upgrade::OWNER};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::{
        fs::{MetadataExt, PermissionsExt, symlink},
        process::CommandExt,
    },
    path::Path,
    time::Duration,
};

async fn cli(work: &Workspace, ctl: &Path, args: &[&str]) -> Result<String> {
    let mut command = vec![ctl.display().to_string()];
    command.extend(strings(args));
    work.output(&work.root, &command).await
}
async fn api(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    value: Option<&Value>,
    cookie: &mut String,
) -> Result<Value> {
    let mut request = client
        .request(
            if value.is_some() {
                reqwest::Method::POST
            } else {
                reqwest::Method::GET
            },
            format!("{base}/api/{path}"),
        )
        .header("X-Chain-Request", "1");
    if !cookie.is_empty() {
        request = request.header("Cookie", cookie.as_str());
    }
    if let Some(value) = value {
        request = request.json(value);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        anyhow::bail!("API {}: {}", response.status(), response.text().await?);
    }
    if let Some(header) = response.headers().get("set-cookie") {
        *cookie = header.to_str()?.split(';').next().unwrap().into();
    }
    Ok(response.json().await?)
}
async fn ready(
    client: &reqwest::Client,
    base: &str,
    admin: &[u8],
    cookie: &mut String,
) -> Result<()> {
    let last_error = std::cell::RefCell::new(String::new());
    wait("OpenRC 面板登录", async || {
        match api(
            client,
            base,
            "login",
            Some(&json!({"token":String::from_utf8_lossy(admin).trim()})),
            cookie,
        )
        .await
        {
            Ok(_) => Ok(true),
            Err(error) => {
                *last_error.borrow_mut() = format!("{error:#}");
                Ok(false)
            }
        }
    })
    .await
    .with_context(|| format!("OpenRC 登录请求：{}", last_error.borrow()))?;
    wait("OpenRC Agent 心跳", async || {
        Ok(api(client, base, "state", None, cookie).await?["nodes"][0]["online"] == true)
    })
    .await
}
async fn maintenance(work: &Workspace, uid: u32, gid: u32, value: &Value) -> Result<Value> {
    let log = work
        .root
        .join(format!("maintenance-{}.json", uuid::Uuid::new_v4()));
    let output = fs::File::create(&log)?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "integration",
            "--case",
            "maintenance-request",
            "--input",
            "/run/chain-maintenance/control.sock",
            "--",
            &serde_json::to_string(value)?,
        ])
        .envs(&work.env)
        .current_dir("/")
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0);
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let pid = work.register(command.spawn()?)?;
    wait("维护接口响应", async || Ok(work.poll(pid)?.is_some())).await?;
    ensure!(
        work.poll(pid)?.context("维护进程未退出")?.success(),
        "维护接口夹具失败：{}",
        fs::read_to_string(&log)?
    );
    Ok(serde_json::from_slice(&fs::read(log)?)?)
}
fn account(name: &str) -> Result<(u32, u32)> {
    let name = std::ffi::CString::new(name)?;
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    ensure!(!entry.is_null(), "测试账户不存在");
    Ok(unsafe { ((*entry).pw_uid, (*entry).pw_gid) })
}

pub async fn run(args: &Options) -> Result<()> {
    ensure!(
        std::env::var("CHAIN_TEST_OPENRC").as_deref() == Ok("1")
            && Path::new("/.dockerenv").is_file()
            && unsafe { libc::geteuid() } == 0
            && !Path::new("/opt/chain").exists(),
        "仅允许在全新独占 OpenRC 容器运行"
    );
    let source = Path::new(args.value("--input")?).canonicalize()?;
    package::verify(&source, true)?;
    let recovery = args.values.get("--case").is_some_and(|v| v == "recovery");
    let mut work = Workspace::new(Path::new("/var/tmp"))?;
    fs::create_dir_all("/run/openrc")?;
    fs::write("/run/openrc/softlevel", "default\n")?;
    fs::write(
        "/etc/init.d/net",
        "#!/sbin/openrc-run\nstart() { return 0; }\nstop() { return 0; }\n",
    )?;
    super::executable(Path::new("/etc/init.d/net"))?;
    work.run(&work.root, &strings(&["rc-service", "net", "start"]))
        .await?;
    let previous = if recovery {
        work.run(&work.root, &strings(&["addgroup", "-S", "chain-panel"]))
            .await?;
        work.run(
            &work.root,
            &strings(&[
                "adduser",
                "-S",
                "-D",
                "-H",
                "-h",
                "/var/lib/chain-panel",
                "-s",
                "/sbin/nologin",
                "-G",
                "chain-panel",
                "chain-panel",
            ]),
        )
        .await?;
        let (uid, gid) = account("chain-panel")?;
        fs::create_dir("/var/lib/chain-panel")?;
        fs::set_permissions("/var/lib/chain-panel", fs::Permissions::from_mode(0o700))?;
        ensure!(
            unsafe { libc::chown(c"/var/lib/chain-panel".as_ptr(), uid, gid) } == 0,
            "测试目录归属准备失败"
        );
        let log = Path::new("/var/lib/chain-panel/service.log");
        fs::write(log, "previous failed install\n")?;
        fs::set_permissions(log, fs::Permissions::from_mode(0o600))?;
        ensure!(
            unsafe { libc::chown(c"/var/lib/chain-panel/service.log".as_ptr(), uid, gid) } == 0,
            "测试日志归属准备失败"
        );
        fs::create_dir_all("/opt/chain/releases")?;
        crate::ctl::upgrade::atomic(
            Path::new("/opt/chain/.chain-install.json"),
            &serde_json::to_vec(&json!({"owner":OWNER,"roles":{}}))?,
            0o600,
        )?;
        let info = fs::metadata(log)?;
        Some((fs::read(log)?, info.uid(), info.gid(), info.mode()))
    } else {
        None
    };
    let program_upgrade = args.values.get("--case").is_some_and(|v| v == "program-upgrade");
    let core_repair = program_upgrade || args.values.get("--case").is_some_and(|v| v == "core-repair");
    let install_source = if core_repair { Path::new("/old") } else { source.as_path() };
    let archive = work.root.join("native.tar.gz");
    release::archive(install_source, &archive)?;
    fs::write(
        work.root.join("SHA256SUMS"),
        format!("{}  native.tar.gz\n", package::digest(&archive)?),
    )?;
    let ctl = install_source.join("bin/chainctl");
    let mut install = strings(&[
        ctl.to_str().unwrap(),
        "install",
        "--package",
        archive.to_str().unwrap(),
        "--non-interactive",
        "--listen",
        "127.0.0.1:37243",
        "--local-host",
        "127.0.0.1",
    ]);
    work.run(&work.root, &install).await?;
    let wrapper = Path::new("/usr/local/bin/chainctl");
    ensure!(
        wrapper.is_symlink() && wrapper.canonicalize()?.file_name().unwrap() == "chainctl",
        "原生管理入口未安装"
    );
    for role in ["panel", "agent"] {
        work.run(
            &work.root,
            &strings(&["rc-service", &format!("chain-{role}"), "status"]),
        )
        .await?;
        let unit = fs::read_to_string(format!("/etc/init.d/chain-{role}"))?;
        ensure!(
            !unit.contains("python")
                && unit.contains(":\"$PATH\"")
                && fs::read_to_string(format!("/var/lib/chain-{role}/.chain-owner"))?
                    == format!("chain-{role}"),
            "OpenRC 服务定义错误"
        );
    }
    if let Some((bytes, uid, gid, mode)) = previous {
        let log = Path::new("/var/lib/chain-panel/service.log");
        let info = fs::metadata(log)?;
        ensure!(
            fs::read(log)?.starts_with(&bytes)
                && (info.uid(), info.gid(), info.mode()) == (uid, gid, mode),
            "恢复安装覆盖已有日志或权限"
        );
    }
    let token = fs::read("/etc/chain-agent/token")?;
    let admin = fs::read("/var/lib/chain-panel/admin-token")?;
    let node = fs::read("/var/lib/chain-panel/local-system-node")?;
    let mut cookie = String::new();
    let mut base = "http://127.0.0.1:37243".to_string();
    let mut client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    ready(&client, &base, &admin, &mut cookie).await?;
    if recovery {
        let unit = Path::new("/etc/init.d/chain-panel");
        let definition = fs::read_to_string(unit)?;
        let pidfile = Path::new("/run/supervise-chain-panel.pid");
        cli(&work, &ctl, &["stop", "--component", "panel"]).await?;
        fs::write(
            unit,
            format!("{definition}\nstart_post() {{ return 1; }}\n"),
        )?;
        work.failure(
            &work.root,
            &strings(&["rc-service", "chain-panel", "start"]),
        )
        .await?;
        let orphan = fs::read_to_string(pidfile)?.trim().parse::<u32>()?;
        cli(&work, &ctl, &["stop", "--component", "panel"]).await?;
        ensure!(
            !Path::new(&format!("/proc/{orphan}")).exists() && !pidfile.exists(),
            "恢复安装遗留 supervisor"
        );
        fs::write(unit, definition)?;
        let foreign = work.spawn(
            &work.root,
            &strings(&["sleep", "300"]),
            &Default::default(),
            &work.root.join("foreign.log"),
        )?;
        fs::write(pidfile, foreign.to_string())?;
        fs::set_permissions(pidfile, fs::Permissions::from_mode(0o600))?;
        let result = work
            .failure(
                &work.root,
                &strings(&[ctl.to_str().unwrap(), "restart", "--component", "panel"]),
            )
            .await;
        let alive = work.poll(foreign)?.is_none();
        work.stop(foreign).await?;
        if pidfile.exists() {
            fs::remove_file(pidfile)?;
        }
        result?;
        ensure!(alive, "错误 PID 文件导致外来进程被停止");
        cli(&work, &ctl, &["start", "--component", "panel"]).await?;
        ready(&client, &base, &admin, &mut cookie).await?;
    }
    fs::create_dir("/etc/native-test-cert")?;
    let cert = Path::new("/etc/native-test-cert/cert.pem");
    let key = Path::new("/etc/native-test-cert/key.pem");
    work.run(
        &work.root,
        &strings(&[
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
            "-days",
            "90",
            "-subj",
            "/CN=panel.test",
            "-addext",
            "subjectAltName=DNS:panel.test",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-out",
            cert.to_str().unwrap(),
            "-keyout",
            key.to_str().unwrap(),
        ]),
    )
    .await?;
    if core_repair {
        fs::copy("/controller/cert.pem", &cert)?;
        fs::copy("/controller/key.pem", &key)?;
    }
    writeln!(
        fs::OpenOptions::new().append(true).open("/etc/hosts")?,
        "\n127.0.0.1 panel.test"
    )?;
    let state_path = Path::new("/opt/chain/.chain-install.json");
    let mut saved = release::read(state_path)?;
    saved["roles"]["panel"]["manual_https"] = json!({"port":37443,"cert":cert,"key":key});
    fs::write(state_path, serde_json::to_vec(&saved)?)?;
    install.extend(strings(&["--domain", "panel.test"]));
    work.run(&work.root, &install).await?;
    client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&fs::read(cert)?)?)
        .timeout(Duration::from_secs(10))
        .build()?;
    base = "https://panel.test:37443".into();
    ready(&client, &base, &admin, &mut cookie).await?;
    ensure!(
        client
            .get(&base)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?
            .to_lowercase()
            .contains("<!doctype html>"),
        "真实 HTTPS 页面未完整传输"
    );
    ensure!(
        Path::new("/etc/chain-panel-https/nginx.conf").is_file()
            && api(&client, &base, "state", None, &mut cookie).await?["nodes"]
                .as_array()
                .unwrap()
                .len()
                == 1,
        "HTTPS 安装重复创建节点"
    );
    ensure!(
        fs::read("/etc/chain-agent/token")? == token
            && fs::read("/var/lib/chain-panel/admin-token")? == admin
            && fs::read("/var/lib/chain-panel/local-system-node")? == node,
        "HTTPS 安装改变身份"
    );
    if core_repair {
        if program_upgrade {
            let job = core_task(&client, &base, &mut cookie, json!({"action":"upgrade","version":"0.2.69"})).await?;
            ensure!(job["state"] == "succeeded", "旧版 Web 程序升级失败：{job}");
            ready(&client, &base, &admin, &mut cookie).await?;
        } else {
        let failed = core_task(&client, &base, &mut cookie,
            json!({"action":"core-upgrade","core":"xray","version":"26.10.31","prerelease":false,"security_update":true,"rollback_safe":false})).await?;
        ensure!(failed["state"] == "failed" && failed["error"].as_str().unwrap_or("").contains("Operation not permitted"), "旧版未复现预检 EPERM：{failed}");
        }
        let blocked = fs::read("/var/lib/chain-agent/.core-upgrade-blocked").ok();
        let state_path = Path::new("/opt/chain/.chain-install.json");
        let before: Value = serde_json::from_slice(&fs::read(state_path)?)?;
        let repair = work.root.join("repair.tar.gz");
        release::archive(&source, &repair)?;
        fs::write(work.root.join("SHA256SUMS"), format!("{}  repair.tar.gz\n", package::digest(&repair)?))?;
        cli(&work, &source.join("bin/chainctl"), &["update", "--package", repair.to_str().unwrap(), "--non-interactive"]).await?;
        ready(&client, &base, &admin, &mut cookie).await?;
        ensure!(fs::read("/var/lib/chain-agent/.core-upgrade-blocked").ok() == blocked, "程序更新清除了安全停用标记");
        let after: Value = serde_json::from_slice(&fs::read(state_path)?)?;
        ensure!(after["core_blocked"] == before["core_blocked"], "程序更新清除了安全状态");
        if blocked.is_some() {
            ensure!(!std::process::Command::new("rc-service").args(["chain-agent", "status"]).status()?.success(), "安全停用期间启动了 Agent");
        }
        for (core, version) in [("xray", "26.10.31"), ("mihomo", "1.19.33")] {
            let job = core_task(&client, &base, &mut cookie,
                json!({"action":"core-upgrade","core":core,"version":version,"prerelease":false,"security_update":true,"rollback_safe":false})).await?;
            ensure!(job["state"] == "awaiting_verification", "真实 Web 内核更新失败：{job}");
            wait("新内核心跳", async || {
                let state = api(&client, &base, "state", None, &mut cookie).await?;
                Ok(state["agent_version"] == after["roles"]["agent"]["version"]
                    && state["core_maintenance"]["versions"][core] == version
                    && state["agent_seen"].as_u64().unwrap_or(0) >= job["started"].as_u64().unwrap_or(u64::MAX))
            }).await?;
            let confirmed = core_task(&client, &base, &mut cookie,
                json!({"action":"core-confirm","core":core,"task_id":job["id"]})).await?;
            ensure!(confirmed["state"] == "succeeded", "内核确认失败：{confirmed}");
        }
        let controller_data = work.root.join("controller-data");
        let controller = work.spawn(&work.root, &strings(&["/controller/chain-panel"]), &map(&[
            ("CHAIN_DATA_DIR", controller_data.display().to_string()),
            ("CHAIN_LISTEN", "127.0.0.1:37244".into()),
            ("CHAIN_SECURE_COOKIE", "0".into()),
        ]), &work.root.join("controller.log"))?;
        wait("总控令牌", async || Ok(controller_data.join("admin-token").exists())).await?;
        let mut controller_cookie = String::new();
        let controller_base = "http://127.0.0.1:37244";
        let controller_token = fs::read_to_string(controller_data.join("admin-token"))?;
        wait("独立总控登录", async || {
            Ok(api(&client, controller_base, "login", Some(&json!({"token":controller_token.trim()})), &mut controller_cookie).await.is_ok())
        }).await?;
        let invite = api(&client, &base, "fleet/invite", Some(&json!({})), &mut cookie).await?;
        let remote = api(&client, controller_base, "fleet/connect", Some(&json!({"name":"old-vps","code":invite["code"],"url":base})), &mut controller_cookie).await?["id"].clone();
        for (core, version) in [("xray", "26.10.31"), ("mihomo", "1.19.33")] {
            let id = uuid::Uuid::new_v4().to_string();
            tokio::time::sleep(Duration::from_secs(12)).await;
            api(&client, controller_base, "fleet/batch", Some(&json!({"id":id,"action":"core-upgrade","targets":[remote],"core":core,"version":version,"security_update":true,"rollback_safe":false})), &mut controller_cookie).await?;
            let deadline = std::time::Instant::now() + Duration::from_secs(180);
            let job = loop {
                let task = api(&client, controller_base, "fleet", None, &mut controller_cookie).await?["tasks"][&id].clone();
                let target = &task["targets"][0];
                if target["state"] == "awaiting_verification" { break target.clone(); }
                ensure!(!matches!(target["state"].as_str(), Some("failed" | "unknown")), "总控内核升级失败：{task}");
                ensure!(std::time::Instant::now() < deadline, "总控内核升级超时：{task}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            };
            tokio::time::sleep(Duration::from_secs(12)).await;
            api(&client, controller_base, &format!("fleet/{}/forward", remote.as_str().unwrap()), Some(&json!({"method":"POST","path":"/federation/task","body":{"id":uuid::Uuid::new_v4().to_string(),"action":"core-confirm","core":core,"task_id":job["task_id"]}})), &mut controller_cookie).await?;
            wait("总控内核确认", async || {
                let task = api(&client, controller_base, "fleet", None, &mut controller_cookie).await?["tasks"][&id].clone();
                Ok(task["targets"][0]["state"] == "succeeded")
            }).await?;
        }
        work.stop(controller).await?;
        ensure!(fs::read("/etc/chain-agent/token")? == token && fs::read("/var/lib/chain-panel/local-system-node")? == node, "恢复改变了节点身份");
        cli(&work, &source.join("bin/chainctl"), &["uninstall", "--purge", "--non-interactive"]).await?;
        work.close()?;
        println!("PASS 已发布旧版真实 Web 内核升级 EPERM、安全停用后程序更新、本机与独立总控 Xray/Mihomo 更新与新心跳、确认及身份保留");
        return Ok(());
    }
    let socket = Path::new("/run/chain-maintenance/control.sock");
    wait("维护 socket", async || Ok(socket.exists())).await?;
    let (uid, gid) = account("chain-panel")?;
    let info = fs::metadata(socket)?;
    ensure!(
        (info.uid(), info.gid(), info.mode() & 0o777) == (0, gid, 0o660),
        "维护 socket 权限错误"
    );
    let pidfile = Path::new("/run/supervise-chain-maintenance.pid");
    let pid = fs::read(pidfile)?;
    cli(&work, &ctl, &["internal-adopt"]).await?;
    ensure!(
        fs::read(pidfile)? == pid && fs::metadata(socket)?.ino() == info.ino(),
        "重复接管重新启动维护服务"
    );
    let id = uuid::Uuid::new_v4().to_string();
    ensure!(
        maintenance(
            &work,
            account("nobody")?.0,
            gid,
            &json!({"id":id,"action":"restart-all"})
        )
        .await?
        .get("error")
        .is_some(),
        "未拒绝无权维护账户"
    );
    ensure!(
        maintenance(
            &work,
            uid,
            gid,
            &json!({"id":id,"action":"restart-all","command":"id"})
        )
        .await?
        .get("error")
        .is_some(),
        "未拒绝未知维护字段"
    );
    ensure!(
        maintenance(&work, uid, gid, &json!({"id":id,"action":"status"})).await?["state"]
            == "not_found",
        "未知维护任务状态错误"
    );
    let request = json!({"id":id,"action":"restart-all"});
    ensure!(
        maintenance(&work, uid, gid, &request).await?["state"] == "queued",
        "维护任务未排队"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let job = loop {
        let job = maintenance(&work, uid, gid, &json!({"id":id,"action":"status"})).await?;
        if !matches!(job["state"].as_str(), Some("queued" | "running")) {
            break job;
        }
        ensure!(std::time::Instant::now() < deadline, "维护任务超时");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    ensure!(
        job["state"] == "succeeded" && maintenance(&work, uid, gid, &request).await? == job,
        "维护任务失败或重复执行"
    );
    ensure!(
        maintenance(&work, uid, gid, &json!({"id":id,"action":"restart-panel"}))
            .await?
            .get("error")
            .is_some(),
        "维护幂等 ID 被复用"
    );
    work.run(
        &work.root,
        &strings(&["rc-service", "chain-maintenance", "restart"]),
    )
    .await?;
    ensure!(
        maintenance(&work, uid, gid, &json!({"id":id,"action":"status"})).await? == job,
        "维护回执未持久化"
    );
    ready(&client, &base, &admin, &mut cookie).await?;
    cli(
        &work,
        &ctl,
        &[
            "update",
            "--package",
            archive.to_str().unwrap(),
            "--non-interactive",
        ],
    )
    .await?;
    ready(&client, &base, &admin, &mut cookie).await?;
    ensure!(
        fs::read("/etc/chain-agent/token")? == token,
        "更新改变 Agent 令牌"
    );
    cli(&work, &ctl, &["uninstall", "--non-interactive"]).await?;
    ensure!(
        !wrapper.exists() && fs::read("/var/lib/chain-panel/admin-token")? == admin,
        "卸载丢失业务数据或管理链接残留"
    );
    work.run(&work.root, &install).await?;
    ready(&client, &base, &admin, &mut cookie).await?;
    ensure!(
        fs::read("/etc/chain-agent/token")? == token,
        "重装改变 Agent 身份"
    );
    cli(&work, &ctl, &["uninstall", "--purge", "--non-interactive"]).await?;
    ensure!(
        !Path::new("/var/lib/chain-agent").exists() && !Path::new("/var/lib/chain-panel").exists(),
        "清理未移除业务数据"
    );
    time_checks(&work, &ctl).await?;
    work.close()?;
    println!("PASS OpenRC 安装/恢复、身份、维护回执、HTTPS、升级、卸载及重装");
    Ok(())
}

async fn core_task(client: &reqwest::Client, base: &str, cookie: &mut String, mut value: Value) -> Result<Value> {
    let id = uuid::Uuid::new_v4().to_string();
    value["id"] = json!(id);
    api(client, base, "federation/task", Some(&value), cookie).await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        let job = match api(client, base, &format!("federation/task?id={id}"), None, cookie).await {
            Ok(job) => job,
            Err(_) if value["action"] == "upgrade" => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                cookie.clear();
                let _ = api(client, base, "login", Some(&json!({"token":fs::read_to_string("/var/lib/chain-panel/admin-token")?.trim()})), cookie).await;
                ensure!(std::time::Instant::now() < deadline, "程序升级后管理连接未恢复");
                continue;
            }
            Err(error) => return Err(error),
        };
        if !matches!(job["state"].as_str(), Some("queued" | "running")) { return Ok(job); }
        ensure!(std::time::Instant::now() < deadline, "内核维护任务超时：{job}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn time_checks(work: &Workspace, ctl: &Path) -> Result<()> {
    let fake = work.root.join("time-tools");
    fs::create_dir(&fake)?;
    let exe = std::env::current_exe()?;
    for name in ["timedatectl", "systemctl", "chronyc", "apt-get"] {
        symlink(&exe, fake.join(name))?;
    }
    symlink("/usr/bin/env", fake.join("env"))?;
    let cases = [
        json!({"units":"chrony.service enabled\nchronyd.service alias\n","active":["chrony.service"]}),
        json!({"units":"","inventory_code":1}),
        json!({"units":"","inventory_code":1,"inventory_error":true}),
        json!({"units":"chrony.service enabled\nsystemd-timesyncd.service enabled\n","active":["chrony.service","systemd-timesyncd.service"]}),
        json!({"units":"chrony.service enabled\n","no_source":true}),
        json!({"units":"chrony.service disabled\nsystemd-timesyncd.service enabled\n","active":["systemd-timesyncd.service"]}),
    ];
    for (i, case) in cases.iter().enumerate() {
        let calls = work.root.join(format!("time-calls-{i}"));
        let env = map(&[
            ("PATH", fake.display().to_string()),
            ("TIME_CASE", serde_json::to_string(case)?),
            ("TIME_CALLS", calls.display().to_string()),
        ]);
        let log = work.root.join(format!("time-{i}.log"));
        let pid = work.spawn(
            &work.root,
            &strings(&[ctl.to_str().unwrap(), "sync-time"]),
            &env,
            &log,
        )?;
        wait("时间同步夹具", async || Ok(work.poll(pid)?.is_some())).await?;
        let success = work.poll(pid)?.unwrap().success();
        ensure!(
            success == matches!(i, 0 | 1 | 5),
            "时间同步决策错误：{i}：{}",
            fs::read_to_string(&log)?
        );
        let rows = fs::read_to_string(calls)?
            .lines()
            .map(serde_json::from_str::<Vec<String>>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            !rows.iter().any(|r| r.iter().any(|v| v == "set-timezone")),
            "修改了时区"
        );
        if matches!(i, 2 | 3) {
            ensure!(
                !rows
                    .iter()
                    .any(|r| r[0] == "apt-get" || r.get(1).is_some_and(|v| v == "enable")),
                "不确定服务状态仍安装或启用 NTP"
            );
        }
        if i == 4 {
            ensure!(
                !rows.contains(&strings(&["chronyc", "makestep"])),
                "没有时间源仍强制步进"
            );
        }
        if i == 1 {
            ensure!(
                rows.contains(&strings(&["apt-get", "install", "-y", "chrony"])),
                "空服务清单未安装 Chrony"
            );
        }
        if i == 5 {
            ensure!(
                !rows.iter().any(|r| r[0] == "chronyc"),
                "timesyncd 活跃时调用 Chrony"
            );
        }
        if success {
            ensure!(
                fs::read_to_string(log)?.contains("America/New_York"),
                "未报告实际时区"
            );
        }
    }
    Ok(())
}
