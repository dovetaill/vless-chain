#[path = "../ctl/mod.rs"]
mod ctl;

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
                "--latest" => (),
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
                    | "internal-nginx-reload"
                    | "internal-cert-renew"
                    | "internal-adopt"
                    | "internal-render-site"
                    | "internal-render-acme"
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
                    | "rollback"
                    | "reset-auth"
            ),
            "不支持的命令，请运行 chainctl help"
        );
        ensure!(
            !args.latest_xray || args.action == "update",
            "--xray-channel latest 仅用于主动 update"
        );
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
    if std::env::args().nth(1).as_deref() == Some("internal-render-routes") {
        use std::io::Read;
        let mut body = String::new();
        std::io::stdin().take(65537).read_to_string(&mut body)?;
        ensure!(body.len() <= 65536, "路由输入过大");
        let value: serde_json::Value = serde_json::from_str(&body)?;
        print!(
            "{}",
            ctl::ingress::routes(&value["routes"], &value["blocked"])?
        );
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
    let mut args = Args::parse(std::env::args().skip(1))?;
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
16 回滚程序版本
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
            "16" => "rollback",
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

async fn dispatch(mut args: Args, menu: bool) -> Result<()> {
    if args.action == "maintenance" {
        return ctl::maintenance::serve().await;
    }
    if args.action == "sync-time" {
        ensure!(
            args.root == std::path::Path::new("/") && unsafe { libc::geteuid() } == 0,
            "时间同步仅用于实际系统的管理员操作"
        );
        println!("{}", ctl::time::synchronize().await?);
        return Ok(());
    }
    if args.action == "help" {
        println!(
            "chainctl — Rust 原生安装与运维

install / setup / update / status / start / stop / restart / access / token
logs / backup / rollback / reset-auth / uninstall
--component panel|agent  --package 文件.tar.gz  --root 离线安装树
--connection-code 接入码  --listen IP:端口  --panel-only
--domain 域名  --non-interactive

update 默认更新本机全部组件；--component agent 仅更新 Agent。"
        );
        return Ok(());
    }
    if matches!(
        args.action.as_str(),
        "internal-render-site" | "internal-render-acme"
    ) {
        let root = args.root.canonicalize()?;
        let manager = ctl::upgrade::Manager {
            base: root.join("opt/chain"),
            root,
        };
        if args.action == "internal-render-acme" {
            print!("{}", ctl::https::proxy_acme_config(&manager));
        } else {
            let domain = ctl::nginx::domain(args.domain.as_deref().context("缺少域名")?)?;
            let listen: std::net::SocketAddr = args
                .install
                .listen
                .as_deref()
                .context("缺少监听地址")?
                .parse()?;
            print!(
                "{}",
                ctl::https::site(&manager, &domain, listen.port(), true)
            );
        }
        return Ok(());
    }
    if matches!(args.action.as_str(), "install" | "setup") {
        let role = args.component.as_deref().unwrap_or("panel");
        let manager = ctl::lifecycle::open(&args.root)?;
        let state = manager.state()?;
        if args.action == "setup"
            && role == "panel"
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
            let stage = temporary.path().join("stage");
            ctl::package::extract(&archive, &stage)?;
            stage
        };
        if !args.non_interactive && role == "panel" && args.domain.is_none() && existing.is_empty()
        {
            args.domain = Some(ctl::lifecycle::prompt("面板域名（留空使用 IP）", "")?);
        }
        let local_host = match args.local_host {
            Some(host) => host,
            None if role == "panel" && manager.live() => ctl::https::public_ipv4().await?,
            None => "127.0.0.1".into(),
        };
        let mut https_previous = None;
        let mut manual_https = state["roles"]["panel"]["manual_https"].clone();
        if manual_https.is_null() {
            manual_https = state["retained_roles"]["panel"]["manual_https"].clone();
        }
        if let Some(domain) = args.domain.as_deref().filter(|s| !s.is_empty()) {
            let domain = ctl::nginx::domain(domain)?;
            let listen: std::net::SocketAddr = args
                .install
                .listen
                .as_deref()
                .context("缺少监听")?
                .parse()?;
            if manual_https.is_null()
                && !args.non_interactive
                && manager.live()
                && ([80, 443]
                    .into_iter()
                    .any(|p| std::net::TcpListener::bind(("0.0.0.0", p)).is_err()))
            {
                if ctl::lifecycle::prompt(
                    "端口已有服务：输入 manual 使用已有证书及独立端口，留空尝试系统 Nginx",
                    "",
                )? == "manual"
                {
                    let socket = std::net::TcpListener::bind(("0.0.0.0", 0))?;
                    let port: u16 = ctl::lifecycle::prompt(
                        "独立 HTTPS 端口",
                        &socket.local_addr()?.port().to_string(),
                    )?
                    .parse()?;
                    ensure!(port != 0 && port != listen.port(), "HTTPS 端口无效");
                    manual_https = serde_json::json!({"port":port,
                        "cert":ctl::lifecycle::prompt("证书完整链绝对路径", "")?,
                        "key":ctl::lifecycle::prompt("证书私钥绝对路径", "")?});
                }
            }
            if manual_https.is_null() {
                let previous =
                    ctl::https::activate(&manager, &domain, listen.port(), &local_host).await?;
                let path = ctl::nginx::site_directory(&ctl::nginx::main_path(&manager))?
                    .join("chain-panel.conf");
                https_previous = Some((path, previous));
            }
            args.install.listen = Some(format!("127.0.0.1:{}", listen.port()));
            args.install.public_url = Some(if manual_https.is_null() {
                format!("https://{domain}")
            } else {
                format!("https://{domain}:{}", manual_https["port"])
            });
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
            state["roles"][role]["manual_https"] = manual_https.clone();
            ctl::upgrade::atomic(
                &manager.base.join(".chain-install.json"),
                &serde_json::to_vec_pretty(&state)?,
                0o600,
            )?;
        }
        if role == "panel" && !manual_https.is_null() {
            let env = manager.environment("panel")?;
            let url = ctl::install::url(&env["CHAIN_PUBLIC_URL"])?;
            let listen: std::net::SocketAddr = env["CHAIN_LISTEN"].parse()?;
            ctl::https::manual(
                &manager,
                url.host_str().context("缺少面板域名")?,
                listen.port(),
                &manual_https,
            )
            .await?;
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
        ctl::lifecycle::handoff(&manager)?;
        ctl::lifecycle::maintenance(&manager).await?;
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
            state["roles"][role]["manual_https"] = manual_https;
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
    if args.action == "internal-nginx-reload" {
        return ctl::nginx::reload(&manager).await;
    }
    if args.action == "internal-cert-renew" {
        ensure!(manager.live(), "离线树不会续期");
        let domain = ctl::nginx::domain(args.domain.as_deref().context("缺少续期域名")?)?;
        loop {
            ctl::upgrade::command(
                "certbot",
                &[
                    "renew",
                    "--cert-name",
                    &format!("chain-panel-{domain}"),
                    "--quiet",
                ],
                300,
            )
            .await?;
            tokio::time::sleep(std::time::Duration::from_secs(43200)).await;
        }
    }
    if args.action == "uninstall" {
        if !args.non_interactive {
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
        let source = temporary.path().join("stage");
        let manifest = ctl::package::extract(&archive, &source)?;
        ensure!(
            requested.is_none_or(|v| v == manifest.version),
            "下载包版本与发行信息不一致"
        );
        if args.latest_xray {
            ctl::package::latest_xray(&source, temporary.path()).await?;
        }
        manager.update(&source, args.component.as_deref()).await?;
        ctl::lifecycle::handoff(&manager)?;
        ctl::lifecycle::maintenance(&manager).await?;
        manager.prune().await?;
        return Ok(());
    }
    if matches!(args.action.as_str(), "access" | "token") {
        ensure!(
            state["roles"]["panel"].is_object() && args.component.as_deref() != Some("agent"),
            "登录信息仅用于控制台"
        );
        if args.action == "access" {
            let env = manager.environment("panel")?;
            println!(
                "控制台：{}",
                env.get("CHAIN_PUBLIC_URL")
                    .or(env.get("CHAIN_LISTEN"))
                    .context("未配置访问地址")?
            );
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
        "rollback" => return ctl::operations::rollback(&manager, selected).await,
        "reset-auth" => {
            ensure!(selected == "panel", "登录恢复仅用于控制台");
            return ctl::operations::reset_auth(&manager, args.non_interactive).await;
        }
        _ => (),
    }
    let _lock = manager.lock()?;
    let mut failures = vec![];
    for role in roles.into_iter().filter(|role| menu || role == selected) {
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
        } else {
            if let Err(error) = manager.service(&args.action, &role).await {
                failures.push(format!("chain-{role}: {error:#}"));
            }
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("；"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
