use super::{
    super::{release, repository, strings, workspace::Workspace},
    fixtures::terminal_command,
    scenarios::wait,
};
use crate::ctl::{package, platform::shell};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

struct Repo<'a> {
    work: &'a Workspace,
    root: PathBuf,
    remote: PathBuf,
}
impl<'a> Repo<'a> {
    async fn new(work: &'a Workspace, name: &str) -> Result<Self> {
        let root = work.root.join(format!("release-{name}"));
        fs::create_dir(&root)?;
        for name in release::MANIFESTS {
            let p = root.join(name);
            fs::create_dir_all(p.parent().unwrap())?;
            fs::copy(repository().join(name), p)?;
        }
        for name in [
            "README.md",
            ".gitignore",
            "scripts/fixture.sh",
            ".github/fixture.yml",
            "unrelated.txt",
        ] {
            let p = root.join(name);
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(p, "fixture\n")?;
        }
        let value = Self {
            work,
            remote: work.root.join(format!("origin-{name}.git")),
            root,
        };
        value.git(&["init", "-b", "main"]).await?;
        for (key, setting) in [
            ("user.name", "Release Test"),
            ("user.email", "release@example.invalid"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ] {
            value.git(&["config", key, setting]).await?;
        }
        value.git(&["add", "."]).await?;
        value.git(&["commit", "-m", "测试初始提交"]).await?;
        value
            .git(&["init", "--bare", value.remote.to_str().unwrap()])
            .await?;
        value
            .git(&["remote", "add", "origin", value.remote.to_str().unwrap()])
            .await?;
        value.git(&["push", "origin", "main"]).await?;
        Ok(value)
    }
    async fn git(&self, args: &[&str]) -> Result<String> {
        let mut command = strings(&["git"]);
        command.extend(strings(args));
        self.work.output(&self.root, &command).await
    }
    async fn wizard(&self, action: &str, steps: &[(&str, &[u8])], success: bool) -> Result<String> {
        let env = BTreeMap::from([("CHAIN_TASK_REPO".into(), self.root.display().to_string())]);
        let command = vec![
            std::env::current_exe()?.display().to_string(),
            action.into(),
        ];
        let (code, body) = terminal_command(self.work, &command, &self.root, &env, steps).await?;
        ensure!((code == 0) == success, "发版向导退出不符 {code}：{body}");
        Ok(body)
    }
}
pub async fn run(work: &Workspace) -> Result<()> {
    let repo = Repo::new(work, "wizard").await?;
    fs::write(repo.root.join("panel/new.txt"), "new\n")?;
    repo.git(&["add", "panel/new.txt"]).await?;
    repo.git(&["commit", "-m", "提交业务改动"]).await?;
    fs::write(repo.root.join("unrelated.txt"), "keep\n")?;
    fs::write(repo.root.join("theme.zip"), "keep")?;
    let before = repo.git(&["rev-parse", "HEAD"]).await?;
    let old = release::version(&repo.root)?;
    repo.wizard("release", &[("下一版本", b"\nn\n")], true)
        .await?;
    ensure!(
        release::version(&repo.root)? == old
            && repo.git(&["rev-parse", "HEAD"]).await? == before
            && repo
                .git(&["diff", "--cached", "--name-only"])
                .await?
                .is_empty(),
        "取消发版仍改变版本或提交"
    );
    let hook = repo.remote.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n")?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))?;
    repo.wizard("release", &[("下一版本", b"\ny\n\n")], false)
        .await?;
    let next = release::version(&repo.root)?;
    let tag = format!("v{next}");
    let commit = repo.git(&["rev-parse", "HEAD"]).await?;
    ensure!(
        next != old
            && repo
                .git(&["rev-parse", &format!("{tag}^{{commit}}")])
                .await?
                == commit
            && !repo
                .git(&["ls-remote", "--tags", "origin"])
                .await?
                .contains(&tag),
        "失败推送丢失本地版本或错误更新远端"
    );
    repo.wizard("push", &[("现在原子推送", b"n\n")], true)
        .await?;
    ensure!(
        repo.git(&["ls-remote", "--tags", "origin"])
            .await?
            .is_empty(),
        "取消推送仍写入远端"
    );
    fs::remove_file(hook)?;
    repo.wizard("release", &[("现在原子推送", b"y\n")], true)
        .await?;
    ensure!(
        repo.git(&["rev-parse", "HEAD"]).await? == commit
            && repo
                .git(&["ls-remote", "origin", "refs/heads/main"])
                .await?
                .contains(&commit)
            && repo
                .git(&["ls-remote", "--tags", "origin"])
                .await?
                .contains(&format!("{commit}\trefs/tags/{tag}^{{}}")),
        "重试未原子推送当前提交与原标签"
    );
    let names = repo
        .git(&["show", "--pretty=format:", "--name-only", "HEAD"])
        .await?;
    let actual = names.lines().collect::<std::collections::BTreeSet<_>>();
    ensure!(
        actual == release::MANIFESTS.iter().copied().collect()
            && repo.git(&["show", "HEAD:panel/new.txt"]).await? == "new"
            && repo
                .git(&["log", "-1", "--format=%B"])
                .await?
                .contains("背景与目的")
            && repo
                .git(&["status", "--porcelain"])
                .await?
                .contains("unrelated.txt"),
        "版本提交混入无关修改或遗漏已有业务提交"
    );
    repo.git(&["add", "unrelated.txt"]).await?;
    ensure!(
        repo.wizard("release", &[], false).await?.contains("暂存区"),
        "未拒绝已有暂存内容"
    );
    ensure!(
        repo.git(&["diff", "--cached", "--name-only"]).await? == "unrelated.txt",
        "改写已有暂存区"
    );
    repo.git(&["reset", "HEAD", "--", "unrelated.txt"]).await?;
    fs::write(repo.root.join("panel/pending.txt"), "not committed\n")?;
    ensure!(
        repo.wizard("release", &[], false)
            .await?
            .contains("请先提交发行源码"),
        "允许未提交源码发版"
    );
    fs::remove_file(repo.root.join("panel/pending.txt"))?;
    repo.git(&[
        "config",
        "remote.origin.pushurl",
        work.root.join("different.git").to_str().unwrap(),
    ])
    .await?;
    ensure!(
        repo.wizard("release", &[], false)
            .await?
            .contains("拉取和推送地址不同"),
        "允许 origin 读写目标不同"
    );
    repo.git(&["config", "--unset", "remote.origin.pushurl"])
        .await?;
    repo.git(&["tag", "v99.0.0"]).await?;
    ensure!(
        repo.wizard("release", &[("下一版本", b"99.0.0\n")], false)
            .await?
            .contains("标签已经存在"),
        "覆盖已有标签"
    );
    let custom = Repo::new(work, "custom").await?;
    ensure!(
        custom
            .wizard("push", &[], false)
            .await?
            .contains("本地没有"),
        "没有标签仍推送"
    );
    custom
        .wizard("release", &[("下一版本", b"98.2.0\ny\nn\n")], true)
        .await?;
    ensure!(
        release::version(&custom.root)? == "98.2.0"
            && custom
                .git(&["ls-remote", "--tags", "origin"])
                .await?
                .is_empty(),
        "自定义版本或延后推送失败"
    );
    custom
        .wizard("push", &[("现在原子推送", b"\n")], true)
        .await?;
    ensure!(
        custom
            .git(&["ls-remote", "--tags", "origin"])
            .await?
            .contains("v98.2.0"),
        "独立 push 丢失原版本"
    );
    aggregation(work, &custom).await?;
    bootstrap(work).await?;
    cleanup(work).await?;
    println!("PASS Rust 发版向导取消、重试、暂存保护、三平台聚合、安装分派与进程清理");
    Ok(())
}
async fn aggregation(work: &Workspace, repo: &Repo<'_>) -> Result<()> {
    let incoming = work.root.join("incoming");
    let commit = "a".repeat(40);
    let version = release::version(&repo.root)?;
    let tag = format!("v{version}");
    let source = work.root.join("aggregate-stage");
    fs::create_dir_all(source.join("bin"))?;
    let mut files = serde_json::Map::new();
    for name in ["chainctl", "chain-panel", "chain-agent", "xray"] {
        let p = source.join("bin").join(name);
        fs::write(&p, name)?;
        files.insert(
            format!("bin/{name}"),
            json!({"sha256":package::digest(&p)?,"mode":0o755}),
        );
    }
    for (os, arch) in [
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("darwin", "aarch64"),
    ] {
        let dir = incoming.join(format!("release-{os}-{arch}"));
        fs::create_dir_all(&dir)?;
        let manifest = json!({"format":1,"version":version,"os":os,"arch":arch,"target":format!("{arch}-{}",if os=="linux"{"unknown-linux-musl"}else{"apple-darwin"}),"libc":"musl","macos_min":"14.0","commit":commit,"dirty":false,"files":files});
        release::write(&source.join("release.json"), &manifest)?;
        let name = format!("chain-panel-{version}-{os}-{arch}.tar.gz");
        release::archive(&source, &dir.join(&name))?;
        fs::copy(source.join("release.json"), dir.join("release.json"))?;
        fs::write(
            dir.join("SHA256SUMS"),
            format!("{}  {name}\n", package::digest(&dir.join(&name))?),
        )?;
    }
    let output = work.root.join("aggregated");
    let command = |output: &Path| {
        format!(
            "exec env CHAIN_TASK_REPO={} {} assemble --input {} --output {} --tag {} --commit {}",
            shell(repo.root.to_str().unwrap()),
            shell(std::env::current_exe().unwrap().to_str().unwrap()),
            shell(incoming.to_str().unwrap()),
            shell(output.to_str().unwrap()),
            shell(&tag),
            shell(&commit)
        )
    };
    work.run(&work.root, &strings(&["sh", "-ec", &command(&output)]))
        .await?;
    ensure!(fs::read_dir(&output)?.count() == 7, "三平台附件不完整");
    for entry in fs::read_dir(&output)? {
        let p = entry?.path();
        if p.to_string_lossy().ends_with(".tar.gz") {
            package::checksum(&p)?;
        }
    }
    let dir = incoming.join("release-darwin-aarch64");
    let path = dir.join("release.json");
    let mut manifest: Value = release::read(&path)?;
    manifest["dirty"] = json!(true);
    release::write(&source.join("release.json"), &manifest)?;
    release::write(&path, &manifest)?;
    let name = format!("chain-panel-{version}-darwin-aarch64.tar.gz");
    fs::remove_file(dir.join(&name))?;
    release::archive(&source, &dir.join(&name))?;
    fs::write(
        dir.join("SHA256SUMS"),
        format!("{}  {name}\n", package::digest(&dir.join(&name))?),
    )?;
    let rejected = work.root.join("rejected");
    ensure!(
        work.failure(&work.root, &strings(&["sh", "-ec", &command(&rejected)]))
            .await?
            .contains("拒绝发布")
            && !rejected.exists(),
        "接受脏构建或拒绝前写入输出"
    );
    let publish = format!(
        "exec env CHAIN_TASK_REPO={} {} publish-ci --input {} --repo owner/repo --tag main --commit {}",
        shell(repo.root.to_str().unwrap()),
        shell(std::env::current_exe()?.to_str().unwrap()),
        shell(output.to_str().unwrap()),
        shell(&commit)
    );
    ensure!(
        work.failure(&work.root, &strings(&["sh", "-ec", &publish]))
            .await?
            .contains("只能发布"),
        "正式发布接受 main 分支名"
    );
    Ok(())
}
async fn bootstrap(work: &Workspace) -> Result<()> {
    let stage = work.root.join("bootstrap");
    fs::create_dir_all(stage.join("bin"))?;
    fs::create_dir(stage.join("scripts"))?;
    fs::copy(
        repository().join("scripts/install-server.sh"),
        stage.join("scripts/install-server.sh"),
    )?;
    let binary = stage.join("bin/chainctl");
    fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n")?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    for (args, expected) in [
        (vec![], "setup"),
        (vec!["--non-interactive"], "setup\n--non-interactive"),
        (
            vec!["update", "--component", "agent"],
            "update\n--component\nagent",
        ),
    ] {
        let mut command = strings(&[
            "sh",
            stage.join("scripts/install-server.sh").to_str().unwrap(),
        ]);
        command.extend(strings(&args));
        ensure!(
            work.output(&stage, &command).await? == expected,
            "本地安装未直接分派到 Rust"
        );
    }
    let output = work.root.join("bootstrapped-chainctl");
    work.run(
        &stage,
        &strings(&[
            "sh",
            stage.join("scripts/install-server.sh").to_str().unwrap(),
            "bootstrap-native",
            output.to_str().unwrap(),
        ]),
    )
    .await?;
    ensure!(
        fs::read(output)? == fs::read(binary)?,
        "原生辅助工具引导错误"
    );
    online_bootstrap(work, &stage).await?;
    let repo = Repo::new(work, "shell-bootstrap").await?;
    let script = repo.root.join("scripts/version-release.sh");
    fs::copy(repository().join("scripts/version-release.sh"), &script)?;
    let mocks = repo.root.join("mocks");
    fs::create_dir(&mocks)?;
    let cargo = mocks.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nset -eu\nmkdir -p \"$CARGO_TARGET_DIR/debug\"\nln -s {} \"$CARGO_TARGET_DIR/debug/xtask\"\n",
            shell(std::env::current_exe()?.to_str().unwrap())
        ),
    )?;
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700))?;
    let env = BTreeMap::from([
        (
            "PATH".into(),
            format!("{}:{}", mocks.display(), std::env::var("PATH")?),
        ),
        ("RUNNER_TEMP".into(), repo.root.display().to_string()),
        ("GITHUB_ACTIONS".into(), "false".into()),
        (
            "CARGO_HOME".into(),
            repo.root.join("empty-cargo").display().to_string(),
        ),
    ]);
    let (code, body) = terminal_command(
        work,
        &strings(&["bash", script.to_str().unwrap(), "version"]),
        &repo.root,
        &env,
        &[],
    )
    .await?;
    ensure!(
        code == 0 && body.contains(&release::version(&repo.root)?),
        "Shell 引导未透传终端及参数：{body}"
    );
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let (code, body) = terminal_command(
        work,
        &strings(&[
            "bash",
            script.to_str().unwrap(),
            "integration",
            "--case",
            "cleanup-worker",
            "--input",
            repo.root.to_str().unwrap(),
            "--",
            &port.to_string(),
        ]),
        &repo.root,
        &env,
        &[("cleanup-worker-ready", b"\x03")],
    )
    .await?;
    ensure!(
        code != 0 && body.contains("启动目录已清理"),
        "中断未回收 Shell 引导器：{body}"
    );
    ensure!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "中断后辅助进程仍占用端口"
    );
    ensure!(
        fs::read_dir(&repo.root)?.all(|p| p.is_ok_and(|p| !p
            .file_name()
            .to_string_lossy()
            .starts_with(".chain-bootstrap-"))),
        "Shell 引导器遗留编译目录"
    );
    Ok(())
}
async fn online_bootstrap(work: &Workspace, stage: &Path) -> Result<()> {
    let root = work.root.join("online-bootstrap");
    let scripts = root.join("scripts");
    let mocks = root.join("mocks");
    let assets = root.join("assets");
    for dir in [&scripts, &mocks, &assets] {
        fs::create_dir_all(dir)?;
    }
    let source = fs::read_to_string(repository().join("scripts/install-server.sh"))?;
    let source = source.replacen("PATH=/opt", &format!("PATH={}:/opt", mocks.display()), 1);
    let script = scripts.join("install-server.sh");
    fs::write(&script, source)?;
    let curl = mocks.join("curl");
    fs::write(
        &curl,
        r#"#!/bin/sh
set -eu
if [ "${TEST_RELEASE_MISSING:-}" = 1 ]; then echo 'HTTP 404' >&2; exit 22; fi
previous= output= url=
for arg in "$@"; do
    [ "$previous" != -o ] || output=$arg
    case "$arg" in https://*) url=$arg;; esac
    previous=$arg
done
case "$url" in
    */releases/latest) printf '%s' 'https://github.com/dovetaill/vless-chain/releases/tag/v0.1.0';;
    *) cp "$TEST_RELEASE_ASSETS/${url##*/}" "$output";;
esac
"#,
    )?;
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o700))?;
    let name = format!(
        "chain-panel-0.1.0-{}-{}.tar.gz",
        release::system(),
        std::env::consts::ARCH
    );
    let bundle = assets.join(&name);
    let package = root.join("package");
    fs::create_dir_all(package.join("bin"))?;
    fs::copy(stage.join("bin/chainctl"), package.join("bin/chainctl"))?;
    let mut entries = serde_json::Map::new();
    for binary in ["chainctl", "chain-panel", "chain-agent", "xray"] {
        let path = package.join(format!("bin/{binary}"));
        if binary != "chainctl" {
            fs::write(&path, "offline fixture")?;
        }
        entries.insert(
            format!("bin/{binary}"),
            json!({"sha256":package::digest(&path)?,"mode":0o755}),
        );
    }
    fs::write(package.join("release.json"), json!({"format":1,"version":"0.1.0","os":release::system(),"arch":std::env::consts::ARCH,"files":entries}).to_string())?;
    release::archive(&package, &bundle)?;
    let sums = assets.join("SHA256SUMS");
    fs::write(&sums, format!("{}  {name}\n", "0".repeat(64)))?;
    let output = root.join("chainctl");
    let command = |missing: bool| {
        strings(&[
            "env",
            &format!("TEST_RELEASE_MISSING={}", u8::from(missing)),
            &format!("TEST_RELEASE_ASSETS={}", assets.display()),
            "sh",
            script.to_str().unwrap(),
            "bootstrap-native",
            output.to_str().unwrap(),
        ])
    };
    ensure!(
        work.failure(&root, &command(true)).await?.contains("404") && !output.exists(),
        "缺少发行版仍继续安装"
    );
    ensure!(
        work.failure(&root, &command(false))
            .await?
            .contains("发行包校验失败")
            && !output.exists(),
        "损坏下载仍导出原生工具"
    );
    fs::write(&sums, format!("{}  {name}\n", package::digest(&bundle)?))?;
    work.run(&root, &command(false)).await?;
    ensure!(
        fs::read(&output)? == fs::read(stage.join("bin/chainctl"))?,
        "在线引导未提取已校验工具"
    );
    ensure!(
        fs::read_dir(&work.env["TMPDIR"])?.all(|p| p.is_ok_and(|p| !p
            .file_name()
            .to_string_lossy()
            .starts_with("chain-bootstrap-"))),
        "下载失败或成功后遗留临时目录"
    );
    Ok(())
}
async fn cleanup(work: &Workspace) -> Result<()> {
    let mut child = Workspace::new(&work.root)?;
    let directory = child.root.clone();
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?.port()
    };
    let pid = child.spawn(
        &directory,
        &strings(&[
            std::env::current_exe()?.to_str().unwrap(),
            "integration",
            "--case",
            "cleanup-worker",
            "--input",
            directory.to_str().unwrap(),
            "--",
            &port.to_string(),
        ]),
        &Default::default(),
        &directory.join("worker.log"),
    )?;
    wait("清理夹具监听", async || {
        Ok(directory.join("ready").exists())
    })
    .await?;
    child.close()?;
    ensure!(
        !directory.exists()
            && unsafe { libc::kill(-(pid as i32), 0) } < 0
            && std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "进程组、端口或产物未清理"
    );
    Ok(())
}
