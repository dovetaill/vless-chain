use super::super::{
    release::{self, read},
    strings,
    workspace::Workspace,
};
use crate::ctl::{
    package,
    upgrade::{Manager, OWNER},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
};

pub struct Case<'a> {
    work: &'a Workspace,
    pub root: PathBuf,
    pub base: PathBuf,
    pub ctl: PathBuf,
    pub archive: PathBuf,
    pub state: Value,
    protected: Vec<PathBuf>,
    before: BTreeMap<PathBuf, (Vec<u8>, u64, u32, u32, u32, i64, i64)>,
}
impl<'a> Case<'a> {
    pub fn new(work: &'a Workspace, stage: &Path, name: &str) -> Result<Self> {
        let directory = work.root.join(format!("native-{name}"));
        fs::create_dir(&directory)?;
        let root = directory.join("host");
        let base = root.join("opt/chain");
        fs::create_dir_all(base.join("releases"))?;
        fs::create_dir_all(root.join("var/tmp"))?;
        let manager = Manager {
            root: root.clone(),
            base: base.clone(),
        };
        let mut state = json!({"owner":OWNER,"roles":{}});
        let mut protected = vec![];
        for role in ["panel", "agent"] {
            let etc = root.join(format!("etc/chain-{role}"));
            let data = root.join(format!("var/lib/chain-{role}"));
            fs::create_dir_all(&etc)?;
            fs::create_dir_all(&data)?;
            for (path, bytes) in [
                (
                    etc.join(format!("{role}.env")),
                    b"CHAIN_LISTEN=\"127.0.0.1:8088\"\nCHAIN_NODE_ID=\"fixture-node\"\n".as_slice(),
                ),
                (etc.join("token"), b"fixture-private-identity"),
                (data.join("traffic.db"), b"fixture-business-data"),
            ] {
                fs::write(&path, bytes)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
                protected.push(path);
            }
            let unit = manager.unit_path(role);
            fs::create_dir_all(unit.parent().unwrap())?;
            fs::write(
                &unit,
                format!(
                    "# {OWNER}\n[Service]\nEnvironmentFile={}\nExecStart=\"{}/current-{role}/bin/chain-{role}\"\n",
                    etc.join(format!("{role}.env")).display(),
                    base.display()
                ),
            )?;
            protected.push(unit);
            symlink("releases/old", base.join(format!("current-{role}")))?;
            state["roles"][role] =
                json!({"current":"old","previous":null,"version":"0.2.48","custom":"preserve"});
        }
        fs::write(
            base.join(".chain-install.json"),
            serde_json::to_vec(&state)?,
        )?;
        fs::set_permissions(
            base.join(".chain-install.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        let source = directory.join("stage");
        fs::create_dir_all(source.join("bin"))?;
        let ctl = if stage.join("bin/chainctl").is_file() {
            stage.join("bin/chainctl")
        } else {
            PathBuf::from(
                std::env::var_os("CHAIN_TEST_BIN_DIR").context("缺少 chainctl 二进制目录")?,
            )
            .join("chainctl")
        }
        .canonicalize()?;
        fs::copy(&ctl, source.join("bin/chainctl"))?;
        for name in ["chain-panel", "chain-agent", "xray"] {
            fs::write(
                source.join(format!("bin/{name}")),
                b"offline fixture; must never execute",
            )?;
        }
        let mut entries = serde_json::Map::new();
        for name in ["chainctl", "chain-panel", "chain-agent", "xray"] {
            entries.insert(format!("bin/{name}"),json!({"sha256":package::digest(&source.join(format!("bin/{name}")))?,"mode":0o755}));
        }
        fs::write(
            source.join("release.json"),
            serde_json::to_vec(
                &json!({"format":1,"version":"0.3.0","os":release::system(),"arch":std::env::consts::ARCH,"files":entries}),
            )?,
        )?;
        package::copy(
            &source,
            &base.join("releases/old"),
            &package::verify(&source, true)?,
        )?;
        let archive = directory.join("native.tar.gz");
        release::archive(&source, &archive)?;
        fs::write(
            directory.join("SHA256SUMS"),
            format!("{}  native.tar.gz\n", package::digest(&archive)?),
        )?;
        let mut value = Self {
            work,
            root,
            base,
            ctl,
            archive,
            state,
            protected,
            before: BTreeMap::new(),
        };
        value.before = value.snapshot()?;
        Ok(value)
    }
    fn snapshot(&self) -> Result<BTreeMap<PathBuf, (Vec<u8>, u64, u32, u32, u32, i64, i64)>> {
        self.protected
            .iter()
            .map(|p| {
                let m = fs::metadata(p)?;
                Ok((
                    p.clone(),
                    (
                        fs::read(p)?,
                        m.ino(),
                        m.uid(),
                        m.gid(),
                        m.mode(),
                        m.ctime(),
                        m.ctime_nsec(),
                    ),
                ))
            })
            .collect()
    }
    pub fn unchanged(&self) -> Result<()> {
        ensure!(
            self.snapshot()? == self.before,
            "凭据、配置或业务数据的内容/元数据发生变化"
        );
        Ok(())
    }
    pub async fn cli(&self, args: &[&str], success: bool) -> Result<String> {
        let mut command = vec![self.ctl.display().to_string()];
        command.extend(strings(args));
        command.extend(strings(&["--root", self.root.to_str().unwrap()]));
        let out = if success {
            self.work.output(&self.root, &command).await?
        } else {
            self.work.failure(&self.root, &command).await?
        };
        ensure!(
            fs::read_dir(self.root.join("var/tmp"))?.next().is_none(),
            "临时解包目录泄漏"
        );
        Ok(out)
    }
    pub async fn update(&self, component: Option<&str>, success: bool) -> Result<String> {
        let mut args = vec![
            "update",
            "--package",
            self.archive.to_str().unwrap(),
            "--non-interactive",
        ];
        if let Some(component) = component {
            args.extend(["--component", component]);
        }
        self.cli(&args, success).await
    }
}

pub async fn exercise(work: &Workspace, stage: &Path) -> Result<()> {
    let case = Case::new(work, stage, "update-both")?;
    case.update(None, true).await?;
    let state = read(&case.base.join(".chain-install.json"))?;
    for role in ["panel", "agent"] {
        ensure!(
            state["roles"][role]["version"] == "0.3.0"
                && state["roles"][role]["custom"] == "preserve"
                && state["roles"][role]["previous"].is_null(),
            "升级丢失角色元数据"
        );
        ensure!(
            case.base
                .join(format!("current-{role}"))
                .canonicalize()?
                .file_name()
                .unwrap()
                .to_string_lossy()
                == state["roles"][role]["current"].as_str().unwrap(),
            "程序链接与状态不一致"
        );
    }
    case.unchanged()?;
    ensure!(
        !case.root.join("var/backups").exists() && !case.base.join(".update-pending.json").exists(),
        "升级遗留备份或事务"
    );
    ensure!(
        case.cli(&["status"], true).await?.contains("离线安装树"),
        "状态未识别离线安装"
    );

    let only = Case::new(work, stage, "agent-only")?;
    only.update(Some("agent"), true).await?;
    let state = read(&only.base.join(".chain-install.json"))?;
    ensure!(
        state["roles"]["agent"]["version"] == "0.3.0"
            && state["roles"]["panel"] == only.state["roles"]["panel"],
        "单独更新影响控制台"
    );
    only.unchanged()?;

    let legacy = Case::new(work, stage, "legacy-wrapper")?;
    let wrapper = legacy.root.join("usr/local/bin/chainctl");
    fs::create_dir_all(wrapper.parent().unwrap())?;
    fs::write(&wrapper, format!("#!/bin/sh\n# {OWNER}\nexit 91\n"))?;
    let old = legacy.base.join("releases/old");
    fs::remove_file(old.join("bin/chainctl"))?;
    let mut old_manifest = read(&old.join("release.json"))?;
    old_manifest["files"]
        .as_object_mut()
        .unwrap()
        .remove("bin/chainctl");
    fs::write(old.join("release.json"), serde_json::to_vec(&old_manifest)?)?;
    let bootstrap = legacy
        .archive
        .parent()
        .unwrap()
        .join("stage/scripts/install-server.sh");
    fs::create_dir_all(bootstrap.parent().unwrap())?;
    fs::copy(
        super::super::repository().join("scripts/install-server.sh"),
        &bootstrap,
    )?;
    work.run(
        &legacy.root,
        &strings(&[
            "sh",
            bootstrap.to_str().unwrap(),
            "update",
            "--package",
            legacy.archive.to_str().unwrap(),
            "--non-interactive",
            "--root",
            legacy.root.to_str().unwrap(),
        ]),
    )
    .await?;
    ensure!(
        wrapper.is_symlink() && wrapper.canonicalize()?.file_name().unwrap() == "chainctl",
        "新版引导器未接管旧管理入口"
    );
    legacy.unchanged()?;
    let installed = wrapper.canonicalize()?;
    fs::OpenOptions::new()
        .append(true)
        .open(&installed)?
        .write_all(b"tamper")?;
    let manager = Manager {
        root: legacy.root.clone(),
        base: legacy.base.clone(),
    };
    ensure!(
        crate::ctl::lifecycle::handoff(&manager)
            .unwrap_err()
            .to_string()
            .contains("校验"),
        "接管未拒绝被篡改的原生工具"
    );

    let install = Case::new(work, stage, "local-bundle")?;
    let fresh = work.root.join("fresh-native");
    fs::create_dir(&fresh)?;
    let command = strings(&[
        install
            .base
            .join("releases/old/bin/chainctl")
            .to_str()
            .unwrap(),
        "install",
        "--root",
        fresh.to_str().unwrap(),
        "--panel-only",
        "--non-interactive",
    ]);
    work.run(&work.root, &command).await?;
    ensure!(
        read(&fresh.join("opt/chain/.chain-install.json"))?["roles"]["panel"]["version"] == "0.3.0",
        "打包安装未使用本地发行树"
    );
    install.unchanged()?;

    let rejected = Case::new(work, stage, "checksum")?;
    fs::write(
        rejected.archive.with_file_name("SHA256SUMS"),
        format!("{}  native.tar.gz\n", "0".repeat(64)),
    )?;
    ensure!(
        rejected.update(None, false).await?.contains("SHA256"),
        "损坏校验和未被拒绝"
    );
    rejected.unchanged()?;
    ensure!(
        read(&rejected.base.join(".chain-install.json"))? == rejected.state,
        "拒绝损坏包后状态发生变化"
    );
    ensure!(
        fs::read_dir(rejected.base.join("releases"))?.count() == 1,
        "损坏包产生发行目录"
    );

    let foreign = Case::new(work, stage, "foreign")?;
    let journal = foreign.base.join(".update-pending.json");
    fs::write(&journal, r#"{"fixture":"interrupted"}"#)?;
    ensure!(
        foreign.update(None, false).await?.contains("上次升级中断"),
        "未拒绝中断升级"
    );
    ensure!(
        fs::read_to_string(&journal)? == r#"{"fixture":"interrupted"}"#,
        "中断日志被覆盖"
    );
    fs::remove_file(journal)?;
    let manager = Manager {
        root: foreign.root.clone(),
        base: foreign.base.clone(),
    };
    let unit = manager.unit_path("agent");
    fs::write(&unit, "foreign service")?;
    ensure!(
        foreign.update(None, false).await?.contains("归属"),
        "未拒绝外来服务"
    );
    ensure!(
        fs::read_to_string(unit)? == "foreign service"
            && read(&foreign.base.join(".chain-install.json"))? == foreign.state,
        "拒绝外来服务前发生变更"
    );
    ensure!(
        fs::read_link(foreign.base.join("current-panel"))? == Path::new("releases/old"),
        "外来服务检查前切换了程序"
    );

    let uninstall = Case::new(work, stage, "uninstall")?;
    uninstall.update(None, true).await?;
    let cert = uninstall
        .root
        .join("etc/letsencrypt/live/panel.example/fullchain.pem");
    let other = uninstall.root.join("etc/nginx/conf.d/other.conf");
    for (path, text) in [(&cert, "keep certificate"), (&other, "foreign site")] {
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, text)?;
    }
    uninstall
        .cli(&["uninstall", "--non-interactive"], true)
        .await?;
    let state = read(&uninstall.base.join(".chain-install.json"))?;
    ensure!(state["roles"] == json!({}), "卸载后角色未清空");
    ensure!(
        state["retained_roles"]
            .as_object()
            .context("保留角色缺失")?
            .len()
            == 2,
        "卸载遗失保留角色"
    );
    for path in uninstall
        .protected
        .iter()
        .filter(|p| p.extension().is_none_or(|v| v != "service" && v != "plist"))
    {
        ensure!(
            fs::read(path)? == uninstall.before[path].0,
            "卸载删除业务数据"
        );
    }
    ensure!(
        !uninstall.root.join("usr/local/bin/chainctl").exists(),
        "卸载遗留管理链接"
    );
    uninstall
        .cli(&["uninstall", "--purge", "--non-interactive"], true)
        .await?;
    for role in ["panel", "agent"] {
        ensure!(
            !uninstall
                .root
                .join(format!("var/lib/chain-{role}"))
                .exists()
                && !uninstall.root.join(format!("etc/chain-{role}")).exists(),
            "显式清理未移除应用数据"
        );
    }
    ensure!(
        fs::read_to_string(cert)? == "keep certificate"
            && fs::read_to_string(other)? == "foreign site",
        "清理影响其他站点或证书"
    );

    let guarded = Case::new(work, stage, "uninstall-foreign")?;
    guarded.update(None, true).await?;
    let site = guarded.root.join(if cfg!(target_os = "macos") {
        "opt/homebrew/etc/nginx/conf.d/chain-panel.conf"
    } else {
        "etc/nginx/conf.d/chain-panel.conf"
    });
    fs::create_dir_all(site.parent().unwrap())?;
    fs::write(&site, "foreign site")?;
    let before = fs::read(guarded.base.join(".chain-install.json"))?;
    ensure!(
        guarded
            .cli(&["uninstall", "--purge", "--non-interactive"], false)
            .await?
            .contains("归属"),
        "卸载未拒绝外来站点"
    );
    ensure!(
        fs::read(guarded.base.join(".chain-install.json"))? == before
            && fs::read_to_string(site)? == "foreign site",
        "卸载拒绝前发生变更"
    );
    guarded.unchanged()?;
    sandbox(work, &case.ctl).await?;
    #[cfg(target_os = "linux")]
    {
        let root = work.root.join("nginx-sandbox");
        fs::create_dir(&root)?;
        fs::write(
            root.join("request.json"),
            serde_json::to_vec(
                &json!({"nginx_check":true,"args":[std::env::current_exe()?,"integration","--case","nginx-worker"],"env":{"PATH":"/usr/bin:/bin"}}),
            )?,
        )?;
        work.run(
            &work.root,
            &strings(&[
                case.ctl.to_str().unwrap(),
                "internal-core-worker",
                root.to_str().unwrap(),
            ]),
        )
        .await?;
    }
    terminal(work, &case).await?;
    println!(
        "PASS 原生双组件/单组件升级、离线安装、元数据保留、校验失败、外来服务/站点拒绝、中断升级、卸载/清理、沙箱和终端恢复"
    );
    Ok(())
}

async fn sandbox(work: &Workspace, ctl: &Path) -> Result<()> {
    let root = work.root.join("sandbox");
    fs::create_dir(&root)?;
    let protected = work.root.join("sandbox-protected");
    fs::write(&protected, "keep")?;
    let before = fs::metadata(&protected)?;
    let request = json!({"args":[std::env::current_exe()?,"integration","--case","sandbox-worker","--input",protected],"env":{"PATH":"/usr/bin:/bin","HOME":root}});
    fs::write(root.join("request.json"), serde_json::to_vec(&request)?)?;
    work.run(
        &work.root,
        &strings(&[
            ctl.to_str().unwrap(),
            "internal-core-worker",
            root.to_str().unwrap(),
        ]),
    )
    .await?;
    ensure!(
        fs::read_to_string(root.join("allowed"))? == "ok",
        "沙箱目录不可写"
    );
    let after = fs::metadata(&protected)?;
    ensure!(
        fs::read_to_string(protected)? == "keep"
            && (
                before.uid(),
                before.gid(),
                before.mode(),
                before.ctime(),
                before.ctime_nsec()
            ) == (
                after.uid(),
                after.gid(),
                after.mode(),
                after.ctime(),
                after.ctime_nsec()
            ),
        "候选进程逃逸沙箱"
    );
    Ok(())
}

async fn terminal(_work: &Workspace, _case: &Case<'_>) -> Result<()> {
    // Implemented with a native PTY in the fixture runner; no language runtime is spawned.
    super::scenarios::terminal_checks(_work, &_case.ctl, &_case.root).await
}
