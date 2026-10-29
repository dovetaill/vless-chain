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
    path::Path,
    time::Duration,
};

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
                &[binary, "maintenance".into()],
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
        atomic(&path, text.as_bytes(), mode)?;
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
        chain_panel::compiler::target(target)?.0
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
