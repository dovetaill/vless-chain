#[path = "../ctl/mod.rs"]
mod ctl;

use anyhow::{Context, Result, bail, ensure};
use std::path::PathBuf;

#[derive(Debug)]
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
}

impl Args {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = Self {
            action: "help".into(),
            root: "/".into(),
            component: None,
            package: None,
            work_parent: None,
            latest_xray: false,
            follow: false,
            log_source: "service".into(),
            non_interactive: false,
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
                _ => bail!("原生入口尚未支持选项 {word}；本次未执行操作"),
            }
        }
        ensure!(
            matches!(
                args.action.as_str(),
                "help"
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
            "此命令尚未迁入原生入口；请继续使用现有已安装的 chainctl"
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
    if std::env::args().nth(1).as_deref() == Some("internal-reset-db") {
        let words = std::env::args().collect::<Vec<_>>();
        ensure!(words.len() == 3, "内部数据库操作参数无效");
        return ctl::operations::reset_database(std::path::Path::new(&words[2]));
    }
    let args = Args::parse(std::env::args().skip(1))?;
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
            "chainctl 原生迁移开发入口（尚未替换已安装管理器）\n\nupdate / status / start / stop / restart / access / token\n--component panel|agent  --package 文件.tar.gz  --root 离线安装树\n\nupdate 默认更新本机所有已安装组件；--component agent 仅更新 Agent。\n发行交接、安装向导及 Web 维护迁完前，本程序不写入系统 chainctl 入口、不删除旧管理脚本。"
        );
        return Ok(());
    }
    let manager = ctl::upgrade::Manager::open(&args.root)?;
    let state = manager.state()?;
    let roles = manager.roles(&state, args.component.as_deref())?;
    if args.action == "update" {
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
        manager.prune()?;
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
    ensure!(state["roles"][selected].is_object(), "尚未安装指定组件");
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
    for role in roles.into_iter().filter(|role| role == selected) {
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
            manager.service(&args.action, &role).await?;
        }
    }
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
