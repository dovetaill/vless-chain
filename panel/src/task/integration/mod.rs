mod business;
mod delivery;
mod features;
mod fixtures;
mod fleet;
mod fronting;
mod harness;
mod load;
mod matrix;
mod native;
mod openrc;
mod plans;
mod protocols;
mod proxy_flow;
mod rentals;
mod resilience;
mod scenarios;
mod sharing;
mod ssh;

use super::{Options, repository, strings, workspace::Workspace};
use crate::ctl::package;
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub async fn smoke(args: &Options) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "发行冒烟测试必须以非特权身份运行"
    );
    let input = Path::new(args.value("--input")?).canonicalize()?;
    let archives = fs::read_dir(&input)?
        .filter_map(|p| p.ok().map(|p| p.path()))
        .filter(|p| p.to_string_lossy().ends_with(".tar.gz"))
        .collect::<Vec<_>>();
    ensure!(archives.len() == 1, "需要唯一的本机架构发行包");
    let mut work = Workspace::new(input.parent().unwrap())?;
    let stage = work.root.join("stage");
    let manifest = package::extract(&archives[0], &stage)?;
    let restored = work.root.join("restored");
    package::copy(&stage, &restored, &manifest)?;
    native::exercise(&work, &restored).await?;
    scenarios::local_install(&work, &restored).await?;
    if crate::ctl::platform::executable("nginx").is_some()
        && crate::ctl::platform::executable("openssl").is_some()
    {
        scenarios::https(&work, &restored).await?;
    }
    work.close()
}

pub async fn run(args: &Options) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "集成测试必须以非特权身份运行"
    );
    let bin = std::env::var_os("CHAIN_TEST_BIN_DIR")
        .map(PathBuf::from)
        .context("缺少 CHAIN_TEST_BIN_DIR")?;
    let stage = args
        .values
        .get("--input")
        .map(PathBuf::from)
        .unwrap_or_else(|| bin.parent().unwrap_or(&bin).to_path_buf());
    let parent = std::env::var_os("CHAIN_TEST_ROOT")
        .map(PathBuf::from)
        .context("缺少 CHAIN_TEST_ROOT")?;
    let mut work = Workspace::new(&parent)?;
    match args
        .values
        .get("--case")
        .map(String::as_str)
        .unwrap_or("smoke")
    {
        "native-upgrade" => native::exercise(&work, &stage).await?,
        "local-install" => scenarios::local_install(&work, &stage).await?,
        "legacy-migration" => scenarios::legacy(&work, &stage).await?,
        "customer-plans" => scenarios::customer_plans(&work, &stage).await?,
        "https-install" => scenarios::https(&work, &stage).await?,
        case => scenarios::run(&work, &stage, case, args).await?,
    }
    work.close()
}

pub async fn verify(args: &Options) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "完整验证必须以非特权身份运行"
    );
    let root = repository();
    let mut work = Workspace::new(&args.parent())?;
    let project = root.join("panel");
    super::workspace::copy_tree(&project.join("frontend"), &work.root.join("frontend"))?;
    work.env.insert(
        "CHAIN_WEB_ASSETS".into(),
        work.root.join("frontend/dist").display().to_string(),
    );
    work.env
        .insert("CHAIN_TEST_ROOT".into(), work.root.display().to_string());
    work.env.insert(
        "CHAIN_TEST_BIN_DIR".into(),
        Path::new(&work.env["CARGO_TARGET_DIR"])
            .join("debug")
            .display()
            .to_string(),
    );
    work.env.insert(
        "CHAIN_TEST_XRAY".into(),
        Path::new(args.value("--xray")?)
            .canonicalize()?
            .display()
            .to_string(),
    );
    work.run(
        &work.root.join("frontend"),
        &strings(&["npm", "ci", "--no-audit", "--no-fund"]),
    )
    .await?;
    work.run(
        &work.root.join("frontend"),
        &strings(&["npm", "run", "build"]),
    )
    .await?;
    work.run(
        &project,
        &strings(&["cargo", "test", "--locked", "--quiet"]),
    )
    .await?;
    work.run(
        &project,
        &strings(&["cargo", "build", "--locked", "--bins", "--quiet"]),
    )
    .await?;
    ssh_checks(&work, Path::new(&work.env["CHAIN_TEST_BIN_DIR"])).await?;
    work.run(&root, &strings(&["node", "panel/tests/xray-reality.cjs"]))
        .await?;
    if args.flags.contains("--browser") {
        let tools = work.root.join("browser-tools");
        fs::create_dir(&tools)?;
        fs::write(
            tools.join("package.json"),
            r#"{"private":true,"dependencies":{"playwright":"1.64.0"}}"#,
        )?;
        work.env.insert(
            "PLAYWRIGHT_BROWSERS_PATH".into(),
            work.root.join("browsers").display().to_string(),
        );
        work.run(
            &tools,
            &strings(&["npm", "install", "--no-audit", "--no-fund"]),
        )
        .await?;
        work.run(
            &tools,
            &strings(&[
                "node",
                "node_modules/playwright/cli.js",
                "install",
                "chromium",
            ]),
        )
        .await?;
        work.env.insert(
            "CHAIN_TEST_BROWSER".into(),
            tools.join("node_modules/playwright").display().to_string(),
        );
        for script in [
            "resource-flow",
            "management-ui",
            "monitoring-ui",
            "diagnostics-flow-ui",
        ] {
            work.run(
                &project,
                &strings(&[
                    "node",
                    &format!("tests/{script}.cjs"),
                    work.root.join("frontend/dist").to_str().unwrap(),
                ]),
            )
            .await?;
        }
    }
    let binary = std::env::current_exe()?;
    let mut cases = vec!["smoke", "local-web", "proxy-flow"];
    if args.flags.contains("--browser") {
        cases.push("diagnostics-browser");
    }
    for case in cases {
        work.run(
            &project,
            &strings(&[binary.to_str().unwrap(), "integration", "--case", case]),
        )
        .await?;
    }
    if std::env::var_os("CHAIN_TEST_STREAM_MODULE").is_some()
        && std::env::var_os("CHAIN_TEST_MIHOMO").is_some()
    {
        work.run(
            &project,
            &strings(&[
                binary.to_str().unwrap(),
                "integration",
                "--case",
                "fronting",
            ]),
        )
        .await?;
    }
    work.close()
}

pub fn fixture_xray(args: &Options) -> Result<()> {
    harness::fixture_xray(args)
}
pub fn fixture(args: &Options) -> Result<bool> {
    fixtures::dispatch(args)
}
pub fn command_fixture() -> Result<Option<i32>> {
    if let Some(code) = ssh::command_fixture()? {
        return Ok(Some(code));
    }
    fixtures::command_fixture()
}
pub async fn openrc(args: &Options) -> Result<()> {
    openrc::run(args).await
}

pub fn executable(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}
pub async fn delivery_checks(work: &Workspace) -> Result<()> {
    delivery::run(work).await
}
pub async fn ssh_checks(work: &Workspace, bin: &Path) -> Result<()> {
    ssh::run(work, bin).await
}
