use super::{
    Options, repository, strings,
    workspace::{Workspace, copy_tree, files},
};
use crate::ctl::{cores, package, upgrade::atomic};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const SHIPPED: &[&str] = &[
    "panel",
    "scripts",
    "README.md",
    ".gitignore",
    ".github",
    "xray-vless-native-v3.sh",
    "xray-vless-chain-v2.sh",
];
pub const MANIFESTS: &[&str] = &[
    "panel/Cargo.toml",
    "panel/Cargo.lock",
    "panel/frontend/package.json",
    "panel/frontend/package-lock.json",
];
pub(super) const LEGACY_ENTRIES: &[&str] = &[
    "chain_common.py",
    "chain_install.py",
    "chain_network.py",
    "chain_platform.py",
    "chain_ingress.py",
    "chain_maintenance.py",
];
pub fn system() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    }
}
pub fn target() -> String {
    format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(target_os = "macos") {
            "apple-darwin"
        } else {
            "unknown-linux-musl"
        }
    )
}
pub fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub fn write(path: &Path, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic(path, &bytes, 0o644)
}

pub fn version(root: &Path) -> Result<String> {
    let text = fs::read_to_string(root.join("panel/Cargo.toml"))?;
    let value = text
        .lines()
        .find_map(|s| {
            s.strip_prefix("version = \"")
                .and_then(|s| s.strip_suffix('"'))
        })
        .context("Cargo 版本缺失")?;
    package::version(value)?;
    let npm = read(&root.join("panel/frontend/package.json"))?;
    let lock = read(&root.join("panel/frontend/package-lock.json"))?;
    ensure!(
        npm["version"] == value
            && lock["version"] == value
            && lock["packages"][""]["version"] == value
            && fs::read_to_string(root.join("panel/Cargo.lock"))?
                .contains(&format!("name = \"chain-panel\"\nversion = \"{value}\"\n")),
        "Cargo/npm 清单与锁文件版本不一致"
    );
    Ok(value.into())
}

pub fn set_version(root: &Path, value: &str) -> Result<()> {
    package::version(value)?;
    let old = version(root)?;
    let cargo = root.join(MANIFESTS[0]);
    let text = fs::read_to_string(&cargo)?.replacen(
        &format!("version = \"{old}\""),
        &format!("version = \"{value}\""),
        1,
    );
    let lock = root.join(MANIFESTS[1]);
    let from = format!("name = \"chain-panel\"\nversion = \"{old}\"");
    let source = fs::read_to_string(&lock)?;
    ensure!(
        source.matches(&from).count() == 1,
        "Cargo.lock 主包版本不唯一"
    );
    let lock_text = source.replacen(
        &from,
        &format!("name = \"chain-panel\"\nversion = \"{value}\""),
        1,
    );
    let mut npm = read(&root.join(MANIFESTS[2]))?;
    npm["version"] = json!(value);
    let mut npm_lock = read(&root.join(MANIFESTS[3]))?;
    npm_lock["version"] = json!(value);
    npm_lock["packages"][""]["version"] = json!(value);
    atomic(&cargo, text.as_bytes(), 0o644)?;
    atomic(&lock, lock_text.as_bytes(), 0o644)?;
    write(&root.join(MANIFESTS[2]), &npm)?;
    write(&root.join(MANIFESTS[3]), &npm_lock)?;
    ensure!(version(root)? == value, "版本写入校验失败");
    println!("版本已设为 {value}，请检查并提交版本变更。");
    Ok(())
}

pub async fn git(work: &Workspace, root: &Path, args: &[&str]) -> Result<String> {
    let mut command = strings(&["git"]);
    command.extend(strings(args));
    work.output(root, &command).await
}
pub fn default_output() -> Result<PathBuf> {
    Ok(repository().join("release-output").join(format!(
        "v{}-{}-{}",
        version(&repository())?,
        system(),
        std::env::consts::ARCH
    )))
}

pub async fn status(args: &Options) -> Result<()> {
    println!(
        "版本：{}\n架构：{}\n默认发行目录：{}",
        version(&repository())?,
        std::env::consts::ARCH,
        default_output()?.display()
    );
    let work = Workspace::new(&args.parent())?;
    println!(
        "{}",
        git(&work, &repository(), &["status", "--short"]).await?
    );
    Ok(())
}

pub fn archive(stage: &Path, destination: &Path) -> Result<()> {
    let manifest = package::verify(stage, true)?;
    let stream = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        stream,
        flate2::Compression::default(),
    ));
    for name in manifest
        .files
        .keys()
        .map(String::as_str)
        .chain(["release.json"])
    {
        let mut header = tar::Header::new_gnu();
        header.set_size(fs::metadata(stage.join(name))?.len());
        header.set_mode(manifest.files.get(name).map_or(0o644, |v| v.mode));
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_username("root")?;
        header.set_groupname("root")?;
        header.set_cksum();
        tar.append_data(&mut header, name, fs::File::open(stage.join(name))?)?;
    }
    tar.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

pub async fn core(work: &Workspace, name: &str, stage: &Path) -> Result<()> {
    let versions: Value = serde_json::from_str(include_str!("../core-versions.json"))?;
    let directory = work.root.join(format!("download-{name}"));
    fs::create_dir(&directory)?;
    let meta = cores::download(
        name,
        versions[name].as_str().context("内核版本缺失")?,
        &directory,
        false,
    )
    .await?;
    fs::create_dir_all(stage.join("bin"))?;
    fs::create_dir_all(stage.join("share"))?;
    fs::copy(
        directory.join(format!("bin/{name}")),
        stage.join(format!("bin/{name}")),
    )?;
    for path in files(&directory.join("share"))? {
        fs::copy(&path, stage.join("share").join(path.file_name().unwrap()))?;
    }
    write(&work.root.join(format!("{name}-provenance.json")), &meta)?;
    Ok(())
}

pub async fn build(args: &Options) -> Result<()> {
    let root = repository();
    let ver = version(&root)?;
    let destination = args
        .values
        .get("--output")
        .map(PathBuf::from)
        .unwrap_or(default_output()?);
    ensure!(
        !destination.exists(),
        "输出目录已存在，拒绝覆盖：{}",
        destination.display()
    );
    let mut work = Workspace::new(&args.parent())?;
    let node = work.output(&root, &strings(&["node", "--version"])).await?;
    let node = package::version(node.trim_start_matches('v'))?;
    ensure!(
        (node[0] == 20 && node >= [20, 19, 0]) || node >= [22, 12, 0],
        "Vite 需要 Node.js 20.19+ 或 22.12+"
    );
    ensure!(
        Path::new(
            &work
                .output(
                    &root,
                    &strings(&["rustc", "--print", "target-libdir", "--target", &target()])
                )
                .await?
        )
        .is_dir(),
        "缺少 Rust 目标 {}",
        target()
    );
    let mut status = strings(&[
        "git",
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
    ]);
    status.extend(strings(SHIPPED));
    let dirty = !work.output(&root, &status).await?.is_empty();
    let commit = git(&work, &root, &["rev-parse", "HEAD"]).await?;
    let project = work.root.join("project");
    fs::create_dir(&project)?;
    for name in ["Cargo.toml", "Cargo.lock", "build.rs"] {
        fs::copy(root.join("panel").join(name), project.join(name))?;
    }
    for name in [
        "src",
        "empty-web",
        "frontend",
        "tests/fixtures",
        "reality-candidates",
    ] {
        copy_tree(&root.join("panel").join(name), &project.join(name))?;
    }
    work.env.insert(
        "CHAIN_WEB_ASSETS".into(),
        project.join("frontend/dist").display().to_string(),
    );
    if system() == "linux" {
        work.env.insert(
            format!("CC_{}", target().replace('-', "_")),
            "musl-gcc".into(),
        );
    } else {
        work.env
            .insert("MACOSX_DEPLOYMENT_TARGET".into(), "14.0".into());
    }
    work.run(
        &project.join("frontend"),
        &strings(&["npm", "ci", "--no-audit", "--no-fund"]),
    )
    .await?;
    work.run(
        &project.join("frontend"),
        &strings(&["npm", "run", "build"]),
    )
    .await?;
    for target_args in [
        vec!["--lib"],
        vec!["--bin", "chain-panel"],
        vec!["--bin", "chainctl"],
        vec!["--bin", "xtask"],
    ] {
        let mut cmd = strings(&["cargo", "test", "--locked", "--quiet"]);
        cmd.extend(strings(&target_args));
        work.run(&project, &cmd).await?;
    }
    let mut cmd = strings(&[
        "cargo",
        "build",
        "--locked",
        "--release",
        "--target",
        &target(),
    ]);
    for name in ["chain-panel", "chain-agent", "chainctl", "xtask"] {
        cmd.extend(strings(&["--bin", name]));
    }
    work.run(&project, &cmd).await?;
    let stage = work.root.join("stage");
    fs::create_dir_all(stage.join("bin"))?;
    for name in ["chain-panel", "chain-agent", "chainctl"] {
        let source = Path::new(&work.env["CARGO_TARGET_DIR"])
            .join(target())
            .join("release")
            .join(name);
        fs::copy(source, stage.join("bin").join(name))?;
        if system() == "linux" {
            let headers = work
                .output(
                    &root,
                    &strings(&[
                        "readelf",
                        "-l",
                        "-d",
                        stage.join("bin").join(name).to_str().unwrap(),
                    ]),
                )
                .await?;
            ensure!(
                !headers.contains("INTERP") && !headers.contains("(NEEDED)"),
                "{name} 不是静态 musl 发行程序"
            );
        }
    }
    for name in ["xray", "mihomo"] {
        core(&work, name, &stage).await?;
        work.run(
            &root,
            &strings(&[
                stage.join("bin").join(name).to_str().unwrap(),
                if name == "xray" { "version" } else { "-v" },
            ]),
        )
        .await?;
    }
    super::integration::ssh_checks(&work, &stage.join("bin")).await?;
    work.env
        .insert("CHAIN_TEST_ROOT".into(), work.root.display().to_string());
    work.env.insert(
        "CHAIN_TEST_XRAY".into(),
        stage.join("bin/xray").display().to_string(),
    );
    work.run(&root, &strings(&["node", "panel/tests/xray-reality.cjs"]))
        .await?;
    fs::create_dir(stage.join("scripts"))?;
    fs::copy(
        root.join("scripts/install-server.sh"),
        stage.join("scripts/install-server.sh"),
    )?;
    for name in LEGACY_ENTRIES {
        fs::copy(root.join("scripts/legacy-native.py"), stage.join("scripts").join(name))?;
    }
    fs::copy(
        root.join("panel/src/core-versions.json"),
        stage.join("scripts/core-versions.json"),
    )?;
    fs::copy(
        root.join("panel/reality-candidates/university-domains-LICENSE.txt"),
        stage.join("share/university-domains-LICENSE.txt"),
    )?;
    fs::copy(root.join("panel/README.md"), stage.join("README.md"))?;
    copy_tree(&root.join("panel/docs"), &stage.join("docs"))?;
    fs::create_dir(stage.join("frontend"))?;
    for name in ["THEME.md", "frontend/THEME.md"] {
        fs::copy(root.join("panel/frontend/THEME.md"), stage.join(name))?;
    }
    let mut entries = serde_json::Map::new();
    for path in files(&stage)? {
        let name = path
            .strip_prefix(&stage)?
            .to_str()
            .context("路径须为 UTF-8")?;
        let mode = if name.starts_with("bin/") || name.ends_with(".sh") {
            0o755
        } else {
            0o644
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        entries.insert(
            name.into(),
            json!({"sha256":package::digest(&path)?,"mode":mode}),
        );
    }
    let versions: Value = serde_json::from_str(include_str!("../core-versions.json"))?;
    let mut manifest = json!({"format":1,"version":ver,"arch":std::env::consts::ARCH,"os":system(),"target":target(),"commit":commit,"dirty":dirty,
        "xray":versions["xray"],"xray_channel":"recommended","mihomo":versions["mihomo"],"files":entries,"created_at":chrono::Utc::now().to_rfc3339()});
    if system() == "linux" {
        manifest["libc"] = json!("musl");
    } else {
        manifest["macos_min"] = json!("14.0");
    }
    write(&stage.join("release.json"), &manifest)?;
    let name = format!(
        "chain-panel-{ver}-{}-{}.tar.gz",
        system(),
        std::env::consts::ARCH
    );
    let bundle = work.root.join(&name);
    archive(&stage, &bundle)?;
    fs::create_dir_all(&destination)?;
    fs::copy(
        Path::new(&work.env["CARGO_TARGET_DIR"])
            .join(target())
            .join("release/xtask"),
        destination.join("verify-xtask"),
    )?;
    fs::copy(&bundle, destination.join(&name))?;
    fs::copy(stage.join("release.json"), destination.join("release.json"))?;
    atomic(
        &destination.join("SHA256SUMS"),
        format!("{}  {name}\n", package::digest(&bundle)?).as_bytes(),
        0o644,
    )?;
    package::checksum(&destination.join(&name))?;
    work.close()?;
    println!("发行包：{}", destination.join(name).display());
    if dirty {
        println!("这是包含未提交修改的本地构建；GitHub 发布前需提交并重新构建。");
    }
    Ok(())
}

pub async fn build_cores(args: &Options) -> Result<()> {
    let output = Path::new(args.value("--output")?);
    ensure!(!output.exists(), "内核输出目录已存在，拒绝覆盖");
    let mut work = Workspace::new(&args.parent())?;
    let stage = work.root.join("cores");
    fs::create_dir(&stage)?;
    for name in ["xray", "mihomo"] {
        core(&work, name, &stage).await?;
    }
    copy_tree(&stage, output)?;
    for name in ["xray", "mihomo"] {
        fs::copy(
            work.root.join(format!("{name}-provenance.json")),
            output.join(format!("{name}-provenance.json")),
        )?;
    }
    work.close()
}

fn prompt(label: &str, default: &str) -> Result<String> {
    ensure!(io::stdin().is_terminal(), "发布操作需要交互终端");
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut value = String::new();
    ensure!(io::stdin().read_line(&mut value)? > 0, "输入已结束");
    let value = value.trim();
    Ok(if value.is_empty() {
        default.into()
    } else {
        value.into()
    })
}

async fn push(work: &Workspace, root: &Path, tag: &str) -> Result<()> {
    let answer = prompt("现在原子推送 main 和标签？Y/n", "Y")?.to_lowercase();
    ensure!(
        ["y", "yes", "n", "no"].contains(&answer.as_str()),
        "未识别的选项，未推送"
    );
    if ["n", "no"].contains(&answer.as_str()) {
        println!("{tag} 已保留在本地，尚未推送。");
        return Ok(());
    }
    git(
        work,
        root,
        &[
            "push",
            "--atomic",
            "origin",
            "main",
            &format!("refs/tags/{tag}"),
        ],
    )
    .await?;
    Ok(())
}

pub async fn wizard(push_only: bool) -> Result<()> {
    ensure!(io::stdin().is_terminal(), "发布向导需要交互终端");
    let root = repository();
    let work = Workspace::new(root.parent().unwrap())?;
    ensure!(
        git(&work, &root, &["branch", "--show-current"]).await? == "main",
        "请切换到 main 分支"
    );
    ensure!(
        git(&work, &root, &["diff", "--cached", "--name-only"])
            .await?
            .is_empty(),
        "暂存区已有修改，向导不会改动"
    );
    ensure!(
        git(&work, &root, &["ls-files", "-u"]).await?.is_empty(),
        "请先解决合并冲突"
    );
    let mut command = strings(&[
        "git",
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--",
    ]);
    command.extend(strings(SHIPPED));
    ensure!(
        work.output(&root, &command).await?.is_empty(),
        "请先提交发行源码"
    );
    let remote = git(&work, &root, &["remote", "get-url", "origin"]).await?;
    ensure!(
        remote == git(&work, &root, &["remote", "get-url", "--push", "origin"]).await?,
        "origin 拉取和推送地址不同"
    );
    println!("发布目标：{remote}；分支：main");
    git(&work, &root, &["fetch", "origin", "main"]).await?;
    git(
        &work,
        &root,
        &["merge-base", "--is-ancestor", "FETCH_HEAD", "HEAD"],
    )
    .await?;
    let old = version(&root)?;
    let old_tag = format!("v{old}");
    let local = git(&work, &root, &["tag", "--list"]).await?;
    let remote_tags = git(&work, &root, &["ls-remote", "--tags", "origin"]).await?;
    let remote_has = |tag: &str| {
        remote_tags
            .lines()
            .filter_map(|s| s.split_once('\t'))
            .any(|(_, r)| r == format!("refs/tags/{tag}"))
    };
    if push_only || (local.lines().any(|v| v == old_tag) && !remote_has(&old_tag)) {
        ensure!(local.lines().any(|v| v == old_tag), "本地没有 {old_tag}");
        ensure!(
            git(
                &work,
                &root,
                &["rev-parse", &format!("{old_tag}^{{commit}}")]
            )
            .await?
                == git(&work, &root, &["rev-parse", "HEAD"]).await?,
            "标签未指向当前提交，拒绝移动标签"
        );
        return push(&work, &root, &old_tag).await;
    }
    let [major, minor, patch] = package::version(&old)?;
    let next = prompt(
        "下一版本",
        &format!(
            "{major}.{minor}.{}",
            patch.checked_add(1).context("版本溢出")?
        ),
    )?;
    ensure!(
        package::version(&next)? > [major, minor, patch],
        "新版本必须大于当前版本"
    );
    let tag = format!("v{next}");
    ensure!(
        !local.lines().any(|v| v == tag) && !remote_has(&tag),
        "标签已经存在"
    );
    let message = format!(
        "chore: 发布 {tag}\n\n背景与目的：为已提交的业务代码创建正式版本。\n\n具体修改：同步四份 Cargo/npm 清单和锁文件，从 {old} 升级到 {next}；不修改业务代码。\n\n验证：版本一致性、暂存区、远端历史及标签冲突检查通过。仅修改版本元数据；业务及发行验证以对应提交的验证记录和 Actions 为准。\n"
    );
    println!("仅同步版本文件：{}\n{message}", MANIFESTS.join("\n"));
    println!(
        "一并推送的已有提交：{}",
        git(
            &work,
            &root,
            &["--no-pager", "log", "--oneline", "FETCH_HEAD..HEAD"]
        )
        .await?
    );
    if prompt(&format!("确认创建版本提交和标签 {tag}？输入 y"), "n")?.to_lowercase() != "y"
    {
        println!("已取消。");
        return Ok(());
    }
    set_version(&root, &next)?;
    let mut add = strings(&["git", "add", "--"]);
    add.extend(strings(MANIFESTS));
    work.run(&root, &add).await?;
    git(&work, &root, &["diff", "--cached", "--check"]).await?;
    let file = work.root.join("commit.txt");
    fs::write(&file, message)?;
    git(&work, &root, &["commit", "-F", file.to_str().unwrap()]).await?;
    git(
        &work,
        &root,
        &[
            "tag",
            "-a",
            &tag,
            "-m",
            &format!("发布 {tag}：同步版本文件并发布已提交的业务代码"),
        ],
    )
    .await?;
    push(&work, &root, &tag).await
}

pub async fn github(args: &Options) -> Result<()> {
    let root = repository();
    let work = Workspace::new(&args.parent())?;
    let directory = args
        .values
        .get("--directory")
        .map(PathBuf::from)
        .unwrap_or(default_output()?);
    let manifest = read(&directory.join("release.json"))?;
    let mut status = strings(&["git", "status", "--porcelain", "--"]);
    status.extend(strings(SHIPPED));
    ensure!(
        work.output(&root, &status).await?.is_empty()
            && manifest["dirty"] == false
            && manifest["version"] == version(&root)?
            && manifest["commit"] == git(&work, &root, &["rev-parse", "HEAD"]).await?,
        "发行包必须来自当前干净提交"
    );
    super::delivery::validate_repo(args.value("--repo")?)?;
    super::delivery::publish_single(args, &work, &directory, &manifest).await
}

pub async fn menu() -> Result<()> {
    loop {
        println!(
            "\nChain v{} · Rust 构建发布\n1) 构建发行包\n2) 检查工具与发行流程\n3) 完整验证\n4) 版本与 Git 状态\n5) 设置版本\n6) 创建 Release 草稿\n7) 发布草稿\n8) 本机安装运维\n9) 发布已提交代码\n10) 推送已有版本\n0) 退出",
            version(&repository())?
        );
        let choice = prompt("选择", "9")?;
        let args = match choice.as_str() {
            "0" => return Ok(()),
            "1" => strings(&["build"]),
            "2" => strings(&["check"]),
            "3" => strings(&["check", "--full"]),
            "4" => strings(&["status"]),
            "5" => strings(&["version", "--set", &prompt("新版本 X.Y.Z", "")?]),
            "6" | "7" => strings(&[
                if choice == "6" { "draft" } else { "publish" },
                "--repo",
                &prompt("GitHub OWNER/REPO", "")?,
            ]),
            "8" => strings(&["ops"]),
            "9" => strings(&["release"]),
            "10" => strings(&["push"]),
            _ => continue,
        };
        if let Err(e) = Box::pin(super::main(args)).await {
            eprintln!("操作未完成：{e:#}");
        }
    }
}
