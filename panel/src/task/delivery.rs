use super::{
    Options,
    release::{self, read},
    repository, strings,
    workspace::{Workspace, files},
};
use crate::ctl::package;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::{collections::BTreeSet, fs, path::Path};

const PLATFORMS: &[(&str, &str)] = &[
    ("linux", "x86_64"),
    ("linux", "aarch64"),
    ("darwin", "aarch64"),
];
fn option_env(args: &Options, key: &str, env: &str) -> Result<String> {
    args.values
        .get(key)
        .cloned()
        .or_else(|| std::env::var(env).ok())
        .with_context(|| format!("缺少 {key} / {env}"))
}
pub fn validate(args: &Options) -> Result<()> {
    let tag = option_env(args, "--tag", "GITHUB_REF_NAME")?;
    validate_tag(&repository(), &tag)
}
pub fn validate_tag(root: &Path, tag: &str) -> Result<()> {
    let value = tag
        .strip_prefix('v')
        .context("只能发布 vX.Y.Z 正式版本标签")?;
    package::version(value)?;
    ensure!(
        release::version(root)? == value,
        "标签与 Cargo/npm 版本不一致"
    );
    Ok(())
}
pub fn validate_repo(repo: &str) -> Result<()> {
    let parts = repo.split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|s| !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))),
        "仓库必须为 OWNER/REPO"
    );
    Ok(())
}
fn name(version: &str, system: &str, arch: &str) -> String {
    format!("chain-panel-{version}-{system}-{arch}.tar.gz")
}
fn expected(tag: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::from(["SHA256SUMS".into()]);
    for (system, arch) in PLATFORMS {
        result.insert(name(&tag[1..], system, arch));
        result.insert(format!("release-{system}-{arch}.json"));
    }
    result
}

pub fn assemble(args: &Options) -> Result<()> {
    validate(args)?;
    let tag = option_env(args, "--tag", "GITHUB_REF_NAME")?;
    let commit = option_env(args, "--commit", "GITHUB_SHA")?;
    let source = Path::new(args.value("--input")?);
    let destination = Path::new(args.value("--output")?);
    ensure!(!destination.exists(), "聚合输出目录已存在，拒绝覆盖");
    let mut work = Workspace::new(source.parent().context("输入目录无父目录")?)?;
    let final_dir = work.root.join("final");
    fs::create_dir(&final_dir)?;
    let mut sums = String::new();
    for (system, arch) in PLATFORMS {
        let input = source.join(format!("release-{system}-{arch}"));
        let name = name(&tag[1..], system, arch);
        let unpack = work.root.join(format!("{system}-{arch}"));
        package::extract(&input.join(&name), &unpack)?;
        let manifest = read(&input.join("release.json"))?;
        ensure!(
            manifest == read(&unpack.join("release.json"))?,
            "包内外清单不一致"
        );
        validate_manifest(&manifest, &tag, &commit, system, arch)?;
        fs::copy(input.join(&name), final_dir.join(&name))?;
        fs::copy(
            input.join("release.json"),
            final_dir.join(format!("release-{system}-{arch}.json")),
        )?;
        sums.push_str(&format!(
            "{}  {name}\n",
            package::digest(&input.join(&name))?
        ));
    }
    fs::write(final_dir.join("SHA256SUMS"), sums)?;
    fs::create_dir_all(destination)?;
    for file in files(&final_dir)? {
        fs::copy(&file, destination.join(file.file_name().unwrap()))?;
    }
    work.close()
}

fn validate_manifest(
    manifest: &Value,
    tag: &str,
    commit: &str,
    system: &str,
    arch: &str,
) -> Result<()> {
    let target = format!(
        "{arch}-{}",
        if system == "linux" {
            "unknown-linux-musl"
        } else {
            "apple-darwin"
        }
    );
    ensure!(
        manifest["version"] == tag[1..]
            && manifest["arch"] == arch
            && manifest["os"] == system
            && manifest["target"] == target
            && manifest["commit"] == commit
            && manifest["dirty"] == false,
        "架构、版本或提交来源不一致，拒绝发布"
    );
    ensure!(
        if system == "linux" {
            manifest["libc"] == "musl"
        } else {
            manifest["macos_min"] == "14.0"
        },
        "最低系统要求不一致"
    );
    Ok(())
}

async fn gh(work: &Workspace, args: Vec<String>) -> Result<String> {
    let mut command = strings(&["gh"]);
    command.extend(args);
    work.output(&repository(), &command).await
}

pub async fn publish(args: &Options) -> Result<()> {
    validate(args)?;
    let tag = option_env(args, "--tag", "GITHUB_REF_NAME")?;
    let commit = option_env(args, "--commit", "GITHUB_SHA")?;
    let repo = option_env(args, "--repo", "GITHUB_REPOSITORY")?;
    validate_repo(&repo)?;
    let directory = Path::new(args.value("--input")?);
    let actual = fs::read_dir(directory)?
        .map(|p| {
            Ok(p?
                .file_name()
                .to_str()
                .context("文件名须为 UTF-8")?
                .to_string())
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(actual == expected(&tag), "发布附件不完整");
    for (system, arch) in PLATFORMS {
        package::checksum(&directory.join(name(&tag[1..], system, arch)))?;
        validate_manifest(
            &read(&directory.join(format!("release-{system}-{arch}.json")))?,
            &tag,
            &commit,
            system,
            arch,
        )?;
    }
    let mut work = Workspace::new(&args.parent())?;
    let releases: Value = serde_json::from_str(
        &gh(
            &work,
            strings(&["api", &format!("repos/{repo}/releases?per_page=100")]),
        )
        .await?,
    )?;
    let existing = releases
        .as_array()
        .context("Release 列表无效")?
        .iter()
        .find(|r| r["tag_name"] == tag);
    if let Some(existing) = existing {
        ensure!(existing["draft"] == true, "正式版本已发布，不覆盖附件");
        ensure!(
            existing["target_commitish"] == commit,
            "已有草稿来自其他提交，拒绝覆盖"
        );
    } else {
        let notes = repository()
            .join("panel/docs/releases")
            .join(format!("{tag}.md"));
        let notes = if notes.is_file() {
            notes
        } else {
            let fallback = work.root.join("release-notes.md");
            fs::write(
                &fallback,
                "使用原生 chainctl 安装控制台与 Agent，支持自动 HTTPS 与续期。下载对应平台发行包及 SHA256SUMS。\n",
            )?;
            fallback
        };
        gh(
            &work,
            strings(&[
                "release",
                "create",
                &tag,
                "--repo",
                &repo,
                "--verify-tag",
                "--draft",
                "--target",
                &commit,
                "--title",
                &format!("Chain Panel {tag}"),
                "--notes-file",
                notes.to_str().unwrap(),
            ]),
        )
        .await?;
    }
    let mut upload = strings(&["release", "upload", &tag, "--repo", &repo, "--clobber"]);
    upload.extend(
        actual
            .iter()
            .map(|name| directory.join(name).display().to_string()),
    );
    gh(&work, upload).await?;
    let downloaded = work.root.join("downloaded");
    fs::create_dir(&downloaded)?;
    gh(
        &work,
        strings(&[
            "release",
            "download",
            &tag,
            "--repo",
            &repo,
            "--dir",
            downloaded.to_str().unwrap(),
        ]),
    )
    .await?;
    let actual_downloads = files(&downloaded)?
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    ensure!(actual == actual_downloads, "远端附件数量不匹配");
    for name in actual {
        ensure!(
            package::digest(&downloaded.join(&name))? == package::digest(&directory.join(&name))?,
            "上传后校验失败：{name}"
        );
    }
    gh(
        &work,
        strings(&[
            "release",
            "edit",
            &tag,
            "--repo",
            &repo,
            "--draft=false",
            "--latest",
        ]),
    )
    .await?;
    work.close()
}

pub async fn publish_single(
    args: &Options,
    work: &Workspace,
    directory: &Path,
    manifest: &Value,
) -> Result<()> {
    let repo = args.value("--repo")?;
    let tag = format!("v{}", manifest["version"].as_str().context("版本缺失")?);
    let name = name(
        manifest["version"].as_str().unwrap(),
        manifest["os"].as_str().context("平台缺失")?,
        manifest["arch"].as_str().context("架构缺失")?,
    );
    package::checksum(&directory.join(&name))?;
    gh(
        work,
        strings(&[
            "api",
            &format!(
                "repos/{repo}/commits/{}",
                manifest["commit"].as_str().context("提交缺失")?
            ),
            "--silent",
        ]),
    )
    .await?;
    if args.action == "draft" {
        gh(
            work,
            strings(&[
                "release",
                "create",
                &tag,
                "--repo",
                repo,
                "--draft",
                "--target",
                manifest["commit"].as_str().unwrap(),
                "--title",
                &format!("Chain Panel {tag}"),
                "--notes",
                "Rust 控制台与 Agent；下载对应架构发行包及 SHA256SUMS。",
                directory.join(&name).to_str().unwrap(),
                directory.join("SHA256SUMS").to_str().unwrap(),
                directory.join("release.json").to_str().unwrap(),
            ]),
        )
        .await?;
    } else {
        let value: Value = serde_json::from_str(
            &gh(
                work,
                strings(&[
                    "release",
                    "view",
                    &tag,
                    "--repo",
                    repo,
                    "--json",
                    "isDraft,assets",
                ]),
            )
            .await?,
        )?;
        ensure!(value["isDraft"] == true, "只发布已有草稿，不修改已发布版本");
        let required = BTreeSet::from([name.clone(), "release.json".into(), "SHA256SUMS".into()]);
        let actual = value["assets"]
            .as_array()
            .context("附件缺失")?
            .iter()
            .filter_map(|v| v["name"].as_str().map(String::from))
            .collect::<BTreeSet<_>>();
        ensure!(required.is_subset(&actual), "草稿附件不完整");
        let downloaded = work.root.join("single-download");
        fs::create_dir(&downloaded)?;
        let mut command = strings(&[
            "release",
            "download",
            &tag,
            "--repo",
            repo,
            "--dir",
            downloaded.to_str().unwrap(),
        ]);
        for name in &required {
            command.extend(strings(&["--pattern", name]));
        }
        gh(work, command).await?;
        for name in &required {
            ensure!(
                package::digest(&downloaded.join(name))? == package::digest(&directory.join(name))?,
                "草稿附件与本地不一致：{name}"
            );
        }
        gh(
            work,
            strings(&[
                "release",
                "edit",
                &tag,
                "--repo",
                repo,
                "--draft=false",
                "--latest",
            ]),
        )
        .await?;
    }
    Ok(())
}

pub async fn check(args: &Options) -> Result<()> {
    let root = repository();
    let mut work = Workspace::new(&args.parent())?;
    work.run(
        &root,
        &strings(&[
            "bash",
            "-n",
            "scripts/version-release.sh",
            "scripts/install-server.sh",
        ]),
    )
    .await?;
    work.run(&root, &strings(&["sh", "-n", "scripts/repair-native-services.sh"]))
        .await?;
    work.run(&root, &strings(&["python3", "-B", "scripts/tests/repair-native-services.py"]))
        .await?;
    work.run(&root, &strings(&["python3", "-B", "scripts/tests/legacy-native.py"]))
        .await?;
    work.run(&root, &strings(&["python3", "-B", "scripts/tests/local-package.py"]))
        .await?;
    work.run(
        &root.join("panel"),
        &strings(&["cargo", "test", "--locked", "--lib", "task::", "--quiet"]),
    )
    .await?;
    super::integration::delivery_checks(&work).await?;
    if args.flags.contains("--full") {
        let stage = work.root.join("cores");
        fs::create_dir(&stage)?;
        for name in ["xray", "mihomo"] {
            release::core(&work, name, &stage).await?;
        }
        let mut values = args.values.clone();
        values.insert(
            "--xray".into(),
            stage.join("bin/xray").display().to_string(),
        );
        let options = Options {
            action: "verify".into(),
            values,
            flags: args.flags.clone(),
            trailing: vec![],
        };
        super::integration::verify(&options).await?;
    } else {
        ensure!(
            !args.flags.contains("--browser"),
            "--browser 需要配合 --full"
        );
    }
    work.close()?;
    println!("所选检查已完成。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(root: &Path) {
        for (name, text) in [
            (
                "panel/Cargo.toml",
                "[package]\nname = \"chain-panel\"\nversion = \"0.2.64\"\n",
            ),
            (
                "panel/Cargo.lock",
                "[[package]]\nname = \"chain-panel\"\nversion = \"0.2.64\"\n",
            ),
            ("panel/frontend/package.json", r#"{"version":"0.2.64"}"#),
            (
                "panel/frontend/package-lock.json",
                r#"{"version":"0.2.64","packages":{"":{"version":"0.2.64"}}}"#,
            ),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }
    #[test]
    fn version_and_tag_contract() {
        let dir = tempfile::tempdir().unwrap();
        source(dir.path());
        assert_eq!(release::version(dir.path()).unwrap(), "0.2.64");
        assert!(validate_tag(dir.path(), "main").is_err());
        assert!(validate_tag(dir.path(), "v0.2.63").is_err());
        release::set_version(dir.path(), "2.0.1").unwrap();
        assert_eq!(release::version(dir.path()).unwrap(), "2.0.1");
        assert!(validate_tag(dir.path(), "v2.0.1").is_ok());
        fs::write(
            dir.path().join("panel/frontend/package.json"),
            r#"{"version":"1.0.0"}"#,
        )
        .unwrap();
        assert!(release::version(dir.path()).is_err());
    }
    #[test]
    fn platform_provenance_and_complete_artifacts_required() {
        let value = json!({"version":"2.0.1","arch":"aarch64","os":"darwin","target":"aarch64-apple-darwin","commit":"abc","dirty":false,"macos_min":"14.0"});
        assert!(validate_manifest(&value, "v2.0.1", "abc", "darwin", "aarch64").is_ok());
        for (key, replacement) in [
            ("version", json!("2.0.2")),
            ("arch", json!("x86_64")),
            ("commit", json!("other")),
            ("dirty", json!(true)),
            ("macos_min", json!("15.0")),
        ] {
            let mut value = value.clone();
            value[key] = replacement;
            assert!(validate_manifest(&value, "v2.0.1", "abc", "darwin", "aarch64").is_err());
        }
        assert_eq!(expected("v2.0.1").len(), 7);
        for repo in ["", "a", "a/b/c", "a;id/b", "a//b"] {
            assert!(validate_repo(repo).is_err());
        }
        assert!(validate_repo("dovetaill/vless-chain").is_ok());
    }
}
