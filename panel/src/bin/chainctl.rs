use chain_panel::ctl;

use anyhow::{Context, Result, bail, ensure};
use std::path::PathBuf;

#[derive(Clone, Debug)]
struct Args {
    action: String,
    root: PathBuf,
    component: Option<String>,
    package: Option<PathBuf>,
    work_parent: Option<PathBuf>,
    latest_xray: bool,
    follow: bool,
    log_source: String,
    non_interactive: bool,
    install: ctl::install::Options,
    panel_only: bool,
    local_host: Option<String>,
    local_target: String,
    domain: Option<String>,
    purge: bool,
}

impl Args {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = Self {
            action: "menu".into(),
            root: "/".into(),
            component: None,
            package: None,
            work_parent: None,
            latest_xray: false,
            follow: false,
            log_source: "service".into(),
            non_interactive: false,
            install: Default::default(),
            panel_only: false,
            local_host: None,
            local_target: String::new(),
            domain: None,
            purge: false,
        };
        let mut words = arguments.into_iter();
        let mut action = false;
        let mut seen = std::collections::BTreeSet::new();
        while let Some(word) = words.next() {
            if !word.starts_with('-') {
                ensure!(!action, "只能指定一个操作");
                args.action = word;
                action = true;
                continue;
            }
            ensure!(seen.insert(word.clone()), "重复选项：{word}");
            match word.as_str() {
                "--help" | "-h" => args.action = "help".into(),
                "--non-interactive" => args.non_interactive = true,
                "--panel-only" => args.panel_only = true,
                "--allow-http" => args.install.allow_http = true,
                "--purge" => args.purge = true,
                "--listen" => {
                    args.install.listen = Some(words.next().context("--listen 缺少地址")?)
                }
                "--public-url" => {
                    args.install.public_url = Some(words.next().context("--public-url 缺少地址")?)
                }
                "--secure-cookie" => {
                    args.install.secure_cookie =
                        Some(words.next().context("--secure-cookie 缺少值")?)
                }
                "--panel-url" => {
                    args.install.panel_url = Some(words.next().context("--panel-url 缺少地址")?)
                }
                "--node-id" => {
                    args.install.node_id = Some(words.next().context("--node-id 缺少 UUID")?)
                }
                "--token-file" => {
                    args.install.token_file =
                        Some(words.next().context("--token-file 缺少路径")?.into())
                }
                "--connection-code" => {
                    let c = ctl::install::connection(&words.next().context("缺少接入码")?)?;
                    args.install.panel_url = Some(c.panel_url);
                    args.install.node_id = Some(c.node_id);
                    args.install.node_token = Some(c.token);
                }
                "--local-host" => {
                    args.local_host = Some(words.next().context("--local-host 缺少地址")?)
                }
                "--local-target" => {
                    args.local_target = words.next().context("--local-target 缺少地址")?
                }
                "--domain" => args.domain = Some(words.next().context("--domain 缺少域名")?),
                "--follow" | "-f" => args.follow = true,
                "--log-source" => {
                    args.log_source = words.next().context("--log-source 缺少日志来源")?
                }
                "--root" => args.root = words.next().context("--root 缺少路径")?.into(),
                "--package" => {
                    args.package = Some(words.next().context("--package 缺少路径")?.into())
                }
                "--work-parent" => {
                    args.work_parent = Some(words.next().context("--work-parent 缺少路径")?.into())
                }
                "--component" => {
                    let role = words.next().context("--component 缺少组件")?;
                    ensure!(
                        matches!(role.as_str(), "panel" | "agent"),
                        "组件只能是 panel 或 agent"
                    );
                    args.component = Some(role);
                }
                "--xray-channel" => {
                    args.latest_xray = match words.next().as_deref() {
                        Some("latest") => true,
                        Some("recommended") => false,
                        _ => bail!("Xray 来源无效"),
                    }
                }
                _ => bail!("不支持选项 {word}；本次未执行操作"),
            }
        }
        ensure!(
            matches!(
                args.action.as_str(),
                "help"
                    | "menu"
                    | "setup"
                    | "install"
                    | "uninstall"
                    | "ingress-enable"
                    | "ingress-disable"
                    | "internal-ingress-apply"
                    | "internal-ingress-watch"
                    | "internal-cert-renew"
                    | "internal-adopt"
                    | "internal-prune"
                    | "update"
                    | "status"
                    | "start"
                    | "stop"
                    | "restart"
                    | "access"
                    | "token"
                    | "maintenance"
                    | "sync-time"
                    | "logs"
                    | "backup"
                    | "reset-auth"
            ),
            "不支持的命令，请运行 chainctl help"
        );
        ensure!(
            !args.latest_xray || args.action == "update",
            "--xray-channel latest 仅用于主动 update"
        );
        if let Some(domain) = args.domain.as_deref() {
            args.domain = Some(ctl::nginx::domain(domain)?);
        }
        if let Some(public_url) = args.install.public_url.as_deref() {
            if !public_url.is_empty() {
                let parsed = ctl::install::url(public_url)?;
                if let Some(domain) = args.domain.as_deref() {
                    let host = parsed.host_str().context("缺少面板域名")?;
                    ensure!(
                        ctl::nginx::domain(host).ok().as_deref() == Some(domain),
                        "--domain 与 --public-url 域名不一致"
                    );
                }
            }
        }
        Ok(args)
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("操作未完成：{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    if std::env::args().nth(1).as_deref()==Some("internal-cert-obtain") {
        let words=std::env::args().skip(2).collect::<Vec<_>>();
        ensure!(words.len()==5,"证书内部参数无效");
        let config=std::path::Path::new(&words[1]);
        let changed=match words[0].as_str(){
            "http"=>chain_panel::acme::obtain(config,&words[2],&words[3],std::path::Path::new(&words[4])).await?,
            "cloudflare"=>chain_panel::acme::obtain_dns(config,&words[2],&words[3],std::path::Path::new(&words[4])).await?,
            _=>bail!("证书验证方式无效"),
        };
        println!("{}",if changed{"renewed"}else{"reused"});
        println!("{}",chain_panel::acme::paths(config,&words[2]).display());
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("internal-core-worker") {
        let words = std::env::args().collect::<Vec<_>>();
        ensure!(words.len() == 3, "内部预检参数无效");
        return ctl::cores::worker(std::path::Path::new(&words[2]));
    }
    if std::env::args().nth(1).as_deref() == Some("internal-reset-db") {
        let words = std::env::args().collect::<Vec<_>>();
        ensure!(words.len() == 3, "内部数据库操作参数无效");
        return ctl::operations::reset_database(std::path::Path::new(&words[2]));
    }
    if std::env::args().nth(1).as_deref() == Some("internal-maintenance-handoff") {
        let args = std::env::args().skip(2).collect::<Vec<_>>();
        ensure!(args.len() == 1, "维护接管参数无效");
        return ctl::lifecycle::maintenance_handoff(&args[0]).await;
    }
    let mut args = Args::parse(std::env::args().skip(1))?;
    if args.action == "internal-prune" {
        ctl::upgrade::Manager::open(&args.root)?.prune().await?;
        if args.non_interactive {
            return Ok(());
        }
        args.action = "menu".into();
    }
    use std::io::IsTerminal;
    if args.action == "menu" && !std::io::stdin().is_terminal() {
        args.action = "help".into();
    }
    if args.action != "menu" {
        return dispatch(args, false).await;
    }
    loop {
        let mut args = args.clone();
        println!(
            "Chain 原生管理
1 安装 / 修复面板与本机节点
2 接入 Agent
3 更新全部组件
4 状态
5 登录信息
6 日志
7 重启
8 备份
9 卸载
10 启用统一 443
11 关闭统一 443
12 恢复登录二步验证
13 同步系统时间
14 停止服务
15 启动服务
0 退出"
        );
        args.action = match ctl::lifecycle::prompt("选择", "4")?.as_str() {
            "0" => return Ok(()),
            "1" => "install",
            "2" => {
                args.component = Some("agent".into());
                "install"
            }
            "3" => "update",
            "4" => "status",
            "5" => "access",
            "6" => "logs",
            "7" => "restart",
            "8" => "backup",
            "9" => "uninstall",
            "10" => "ingress-enable",
            "11" => "ingress-disable",
            "12" => "reset-auth",
            "13" => "sync-time",
            "14" => "stop",
            "15" => "start",
            _ => {
                eprintln!("无效选择");
                continue;
            }
        }
        .into();
        if let Err(error) = dispatch(args, true).await {
            eprintln!("操作未完成：{error:#}");
        }
    }
}

async fn dispatch(args: Args, menu: bool) -> Result<()> {
    let changing_release = matches!(args.action.as_str(), "setup" | "install" | "update");
    let root = args.root.clone();
    dispatch_action(args, menu).await?;
    if !changing_release {
        return Ok(());
    }
    let releases = root.canonicalize()?.join("opt/chain/releases");
    let running = std::env::current_exe()?;
    if !running.starts_with(&releases) {
        return Ok(());
    }
    let state = ctl::upgrade::Manager::open(&root)?.state()?;
    let roles = state["roles"].as_object().context("安装状态缺少组件")?;
    let mut successor = None;
    for role in roles.values() {
        let binary = releases.join(role["current"].as_str().unwrap()).join("bin/chainctl");
        if binary == running {
            return Ok(());
        }
        if ctl::package::version(role["version"].as_str().unwrap())? >= [0, 2, 78] {
            successor = Some(binary);
        }
    }
    let Some(binary) = successor else { return Ok(()) };
    // Release the old CLI executable after all temporary directories have dropped.
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(binary);
    command.arg("internal-prune").arg("--root").arg(root);
    if !menu {
        command.arg("--non-interactive");
    }
    Err(command.exec().into())
}

async fn dispatch_action(mut args: Args, menu: bool) -> Result<()> {
    if args.action == "maintenance" {
        return ctl::maintenance::serve().await;
    }
    if args.action == "sync-time" {
        ensure!(
            args.root.canonicalize()? == std::path::Path::new("/") && unsafe { libc::geteuid() } == 0,
            "时间同步仅用于实际系统的管理员操作"
        );
        println!("{}", ctl::time::synchronize().await?);
        return Ok(());
    }
    if args.action == "help" {
        println!(
            "chainctl — Rust 原生安装与运维

install / setup / update / status / start / stop / restart / access / token
logs / backup / reset-auth / uninstall
--component panel|agent  --package 文件.tar.gz  --root 离线安装树
--connection-code 接入码  --listen IP:端口  --public-url 访问地址  --panel-only
--domain 域名  --allow-http  --non-interactive

指定 --domain 时自动签发证书并配置 Nginx HTTPS 443；修复安装默认复用既有访问配置。
uninstall 交互选择只删除程序或删除全部数据；非交互删除全部数据使用 --purge。
update 默认更新本机全部组件；--component panel|agent 仅更新指定组件。"
        );
        return Ok(());
    }
    if matches!(args.action.as_str(), "install" | "setup") {
        let role = args.component.as_deref().unwrap_or("panel");
        let manager = ctl::lifecycle::open(&args.root)?;
        let state = manager.state()?;
        if args.action == "setup"
            && role == "panel"
            && !manager.live()
            && state["roles"]["panel"]["setup_complete"] == true
            && (state["roles"]["panel"]["panel_only"] == true
                || state["roles"]["agent"].is_object())
        {
            ctl::lifecycle::handoff(&manager)?;
            ctl::lifecycle::maintenance(&manager).await?;
            return ctl::lifecycle::access(&manager);
        }
        let existing =
            if state["roles"][role].is_object() || state["retained_roles"][role].is_object() {
                manager.environment(role)?
            } else {
                Default::default()
            };
        if !args.non_interactive
            && role == "agent"
            && args.install.node_id.is_none()
            && existing.is_empty()
        {
            let c = ctl::install::connection(&ctl::lifecycle::secret("粘贴 Agent 接入码")?)?;
            args.install.panel_url = Some(c.panel_url);
            args.install.node_id = Some(c.node_id);
            args.install.node_token = Some(c.token);
        }
        if args.action == "setup" {
            args.panel_only |= state["roles"]["panel"]["panel_only"] == true
                || state["retained_roles"]["panel"]["panel_only"] == true;
        }
        if role == "panel" {
            args.install.listen = args
                .install
                .listen
                .or_else(|| existing.get("CHAIN_LISTEN").cloned());
            args.install.public_url = args
                .install
                .public_url
                .or_else(|| existing.get("CHAIN_PUBLIC_URL").cloned());
            args.install.secure_cookie = args
                .install
                .secure_cookie
                .or_else(|| existing.get("CHAIN_SECURE_COOKIE").cloned());
            if manager.live()
                && args.domain.is_none()
                && args.install.public_url.is_none()
                && !args.install.allow_http
            {
                ensure!(
                    !args.non_interactive,
                    "面板安装请指定 --domain 域名（自动配置 HTTPS 443），或显式指定 --public-url / --allow-http"
                );
                loop {
                    let domain = ctl::lifecycle::prompt("面板域名（使用 HTTPS 443）", "")?;
                    match ctl::nginx::domain(&domain) {
                        Ok(domain) => {
                            args.domain = Some(domain);
                            break;
                        }
                        Err(error) => eprintln!("{error}；请输入有效域名。"),
                    }
                }
            }
        }
        if role == "panel" && args.install.listen.is_none() {
            let socket = std::net::TcpListener::bind("127.0.0.1:0")?;
            let port = socket.local_addr()?.port();
            args.install.listen = Some(format!("0.0.0.0:{port}"));
        }
        ctl::time::prepare(&manager).await;
        let parent = args.work_parent.unwrap_or_else(|| std::env::temp_dir());
        let temporary = tempfile::Builder::new()
            .prefix("chain-native-install-")
            .tempdir_in(parent)?;
        let executable = std::env::current_exe()?;
        let bundled = executable.parent().and_then(|p| p.parent());
        let installed_archive;
        let stage = if args.package.is_none()
            && bundled.is_some_and(|p| p.join("release.json").is_file())
        {
            let source = bundled.unwrap();
            ctl::package::verify(source, true)?;
            source.to_path_buf()
        } else {
            let archive = match args.package {
                Some(p) => p.canonicalize()?,
                None => ctl::package::download(temporary.path(), None).await?.0,
            };
            installed_archive = manager.install_archive(&archive)?;
            installed_archive.0.clone()
        };
        let local_host = match args.local_host {
            Some(host) => host,
            None if role == "panel" && manager.live() => ctl::https::public_ipv4()
                .await
                .unwrap_or_else(|_| args.domain.clone().unwrap_or_else(|| "127.0.0.1".into())),
            None => "127.0.0.1".into(),
        };
        let mut https_previous = None;
        if let Some(domain) = args.domain.as_deref() {
            let listen: std::net::SocketAddr = args
                .install
                .listen
                .as_deref()
                .context("缺少监听")?
                .parse()?;
            if manager.live() {
                let previous = ctl::https::activate(&manager, domain, listen.port()).await?;
                let path = ctl::nginx::site_directory(&ctl::nginx::main_path(&manager))?
                    .join("chain-panel.conf");
                https_previous = Some((path, previous));
            }
            args.install.listen = Some(format!("127.0.0.1:{}", listen.port()));
            args.install.public_url = Some(format!("https://{domain}"));
            args.install.secure_cookie = Some("1".into());
        } else if role == "panel" && args.install.public_url.is_none() {
            let listen: std::net::SocketAddr = args.install.listen.as_deref().unwrap().parse()?;
            args.install.public_url = Some(format!("http://{}:{}", local_host, listen.port()));
        }
        if let Err(error) = ctl::install::component(&manager, &stage, role, &args.install).await {
            if let Some((path, previous)) = &https_previous {
                if let Some(text) = previous {
                    ctl::nginx::replace_owned(path, None, text, ctl::https::OWNER)?;
                } else {
                    std::fs::remove_file(path)?;
                }
                ctl::nginx::reload(&manager)
                    .await
                    .context("安装失败，HTTPS 站点已恢复但重载失败")?;
            }
            return Err(error);
        }
        ctl::lifecycle::handoff(&manager)?;
        if role == "panel" {
            let _lock = manager.lock()?;
            let mut state = manager.state()?;
            state["roles"][role]["setup_complete"] = false.into();
            ctl::upgrade::atomic(
                &manager.base.join(".chain-install.json"),
                &serde_json::to_vec_pretty(&state)?,
                0o600,
            )?;
        }
        if https_previous.is_some() {
            ctl::https::renewal(&manager, args.domain.as_deref().unwrap()).await?;
        }
        if role == "panel" && !args.panel_only {
            ctl::lifecycle::local_agent(&manager, &stage, &local_host, &args.local_target).await?;
        }
        if role == "panel" && !args.panel_only && !args.non_interactive && manager.live() {
            let owners = ctl::migration::discover(std::path::Path::new("/proc")).await?;
            ctl::migration::handover(&manager, owners).await?;
        }
        ctl::lifecycle::finish_update(&manager).await?;
        if manager.live() {
            if let Err(e) = ctl::https::proxy_acme(&manager).await {
                eprintln!("代理 HTTP-01 路由未启用：{e:#}");
            }
        }
        if role == "panel" {
            let _lock = manager.lock()?;
            let mut state = manager.state()?;
            state["roles"][role]["setup_complete"] = true.into();
            state["roles"][role]["panel_only"] = args.panel_only.into();
            ctl::upgrade::atomic(
                &manager.base.join(".chain-install.json"),
                &serde_json::to_vec_pretty(&state)?,
                0o600,
            )?;
            if manager.live() {
                ctl::lifecycle::access(&manager)?;
            }
        }
        println!("Rust chainctl 已安装并接管系统入口。");
        return Ok(());
    }
    let manager = ctl::upgrade::Manager::open(&args.root)?;
    if args.action == "internal-adopt" {
        manager.adopt().await?;
        let _lock = manager.lock()?;
        ctl::lifecycle::handoff(&manager)?;
        return ctl::lifecycle::maintenance(&manager).await;
    }
    if matches!(
        args.action.as_str(),
        "ingress-enable" | "ingress-disable" | "internal-ingress-apply"
    ) {
        return ctl::ingress::operate(
            &manager,
            if args.action == "ingress-enable" {
                "enable"
            } else if args.action == "ingress-disable" {
                "disable"
            } else {
                "apply"
            },
        )
        .await;
    }
    if args.action == "internal-ingress-watch" {
        return ctl::ingress::watch(&manager).await;
    }
    if args.action == "internal-cert-renew" {
        ensure!(manager.live(), "离线树不会续期");
        let domain = ctl::nginx::domain(args.domain.as_deref().context("缺少续期域名")?)?;
        loop {
            ctl::https::renew_once(&manager, &domain).await?;
            if manager.kind() == "systemd" { break; }
            tokio::time::sleep(std::time::Duration::from_secs(43200)).await;
        }
        return Ok(());
    }
    if args.action == "uninstall" {
        if !args.non_interactive {
            if !args.purge {
                args.purge = match ctl::lifecycle::prompt(
                    "卸载范围：1 只删除程序，保留 VLESS 等数据；2 删除程序及全部数据",
                    "1",
                )?.as_str() {
                    "1" => false,
                    "2" => true,
                    _ => bail!("卸载选项无效，未执行卸载"),
                };
            }
            let word = if args.purge { "PURGE" } else { "UNINSTALL" };
            ensure!(
                ctl::lifecycle::prompt(&format!("输入 {word} 确认卸载"), "")? == word,
                "已取消"
            );
        }
        return ctl::lifecycle::uninstall(&manager, args.purge).await;
    }

    let state = manager.state()?;
    let roles = manager.roles(&state, args.component.as_deref())?;
    if args.action == "update" {
        manager.prune_before_update().await?;
        ctl::time::prepare(&manager).await;
        let parent = args.work_parent.unwrap_or_else(|| manager.path("/var/tmp"));
        // Only a private task directory receives downloads and extracted files.
        let temporary = tempfile::Builder::new()
            .prefix("chain-install-")
            .tempdir_in(&parent)?;
        let (archive, requested) = if let Some(package) = args.package {
            (package.canonicalize()?, None)
        } else {
            let (package, version) = ctl::package::download(temporary.path(), None).await?;
            (package, Some(version))
        };
        manager.update_archive(&archive, requested.as_deref(),
            args.latest_xray.then_some(temporary.path()), args.component.as_deref()).await?;
        ctl::lifecycle::finish_update(&manager).await?;
        return Ok(());
    }
    if matches!(args.action.as_str(), "access" | "token") {
        ensure!(
            state["roles"]["panel"].is_object() && args.component.as_deref() != Some("agent"),
            "登录信息仅用于控制台"
        );
        if args.action == "access" {
            return ctl::lifecycle::access(&manager);
        }
        println!(
            "{}",
            std::fs::read_to_string(manager.path("/var/lib/chain-panel/admin-token"))?.trim()
        );
        return Ok(());
    }
    let selected = args.component.as_deref().unwrap_or("panel");
    ensure!(
        menu && matches!(
            args.action.as_str(),
            "status" | "start" | "stop" | "restart"
        ) || state["roles"][selected].is_object(),
        "尚未安装指定组件"
    );
    match args.action.as_str() {
        "logs" => {
            return ctl::operations::logs(&manager, selected, &args.log_source, args.follow).await;
        }
        "backup" => return ctl::operations::backup_stopped(&manager, selected).await,
        "reset-auth" => {
            ensure!(selected == "panel", "登录恢复仅用于控制台");
            return ctl::operations::reset_auth(&manager, args.non_interactive).await;
        }
        _ => (),
    }
    let _lock = manager.lock()?;
    let mut failures = vec![];
    for role in roles.into_iter().filter(|role| menu || args.component.is_some() || role == selected) {
        if args.action == "status" {
            println!(
                "chain-{role} · {} · {}",
                state["roles"][&role]["version"].as_str().unwrap_or("未知"),
                if !manager.live() {
                    "离线安装树，未检测运行状态"
                } else if manager.active(&role).await? {
                    "服务运行中（不代表代理整链通过）"
                } else {
                    "服务未运行"
                }
            );
        } else if let Err(error) = manager.service(&args.action, &role).await {
            failures.push(format!("chain-{role}: {error:#}"));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("；"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn install_validates_domains_and_public_urls_without_locking_topology() {
        for words in [
            vec!["install", "--domain", ""],
            vec!["install", "--domain", "204.138.145.91"],
            vec!["install", "--domain", "panel.test", "--public-url", "https://other.test"],
            vec!["install", "--public-url", "https://u:p@panel.test"],
        ] {
            assert!(Args::parse(words.into_iter().map(str::to_owned)).is_err());
        }
        let args = Args::parse(["install", "--domain", "Panel.Test", "--non-interactive"].map(str::to_owned)).unwrap();
        assert_eq!(args.domain.as_deref(), Some("panel.test"));
        for words in [
            vec!["install", "--public-url", "https://panel.test:8443", "--non-interactive"],
            vec!["install", "--public-url", "https://204.138.145.91", "--non-interactive"],
            vec!["install", "--allow-http", "--non-interactive"],
            vec!["update", "--non-interactive"],
            vec!["install", "--component", "agent", "--non-interactive"],
            vec!["uninstall", "--non-interactive", "--purge"],
        ] {
            assert!(Args::parse(words.into_iter().map(str::to_owned)).is_ok());
        }
    }
    #[test]
    fn refuses_unknown_options_before_any_mutation() {
        for args in [
            vec!["update", "--component", "other"],
            vec!["update", "--package"],
            vec!["update", "--xray-channel", "unknown"],
            vec!["update", "--root", "/", "--root", "/tmp"],
            vec!["update", "install"],
        ] {
            assert!(Args::parse(args.into_iter().map(str::to_owned)).is_err());
        }
        let args =
            Args::parse(["update", "--component", "agent", "--non-interactive"].map(str::to_owned))
                .unwrap();
        assert_eq!(args.component.as_deref(), Some("agent"));
    }
}
