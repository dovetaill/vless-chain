use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path},
    time::Duration,
};

pub const REPO: &str = "dovetaill/vless-chain";
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub sha256: String,
    pub mode: u32,
}
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub version: String,
    pub arch: String,
    #[serde(default = "linux")]
    pub os: String,
    pub files: BTreeMap<String, Entry>,
}
fn linux() -> String {
    "linux".into()
}

pub fn version(value: &str) -> Result<[u64; 3]> {
    let parts = value
        .split('.')
        .map(|s| {
            ensure!(
                !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
                "发行版本无效"
            );
            Ok(s.parse::<u64>()?)
        })
        .collect::<Result<Vec<_>>>()?;
    parts
        .try_into()
        .map_err(|_| anyhow::anyhow!("发行版本无效"))
}

pub fn relative(name: &str) -> Result<&Path> {
    let path = Path::new(name);
    ensure!(
        !name.is_empty()
            && !name.contains(['\\', '\0'])
            && path.components().all(|c| matches!(c, Component::Normal(_)))
            && path
                .components()
                .map(|s| s.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
                == name,
        "发行路径不安全：{name}"
    );
    Ok(path)
}

pub fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn inventory(root: &Path, directory: &Path, result: &mut BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(kind.is_file() || kind.is_dir(), "发行目录含链接或特殊文件");
        if kind.is_dir() {
            inventory(root, &entry.path(), result)?;
        } else {
            result.insert(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .context("发行路径须为 UTF-8")?
                    .into(),
            );
        }
        ensure!(result.len() <= 300, "发行包文件数量超限");
    }
    Ok(())
}

pub fn verify(root: &Path, native: bool) -> Result<Manifest> {
    ensure!(fs::symlink_metadata(root)?.is_dir(), "发行目录不能是链接");
    let mut actual = BTreeSet::new();
    inventory(root, root, &mut actual)?;
    let path = root.join("release.json");
    ensure!(fs::metadata(&path)?.len() <= 1024 * 1024, "发行清单过大");
    let manifest: Manifest = serde_json::from_reader(File::open(path)?)?;
    ensure!(manifest.format == 1, "发行包格式无效");
    version(&manifest.version)?;
    for name in ["bin/chain-panel", "bin/chain-agent", "bin/xray"] {
        ensure!(manifest.files.contains_key(name), "发行包缺少 {name}");
    }
    if native {
        ensure!(
            manifest.files.contains_key("bin/chainctl"),
            "目标发行包没有原生 chainctl，拒绝降回 Python 管理器"
        );
    }
    let expected = manifest
        .files
        .keys()
        .cloned()
        .chain(["release.json".into()])
        .collect();
    ensure!(actual == expected, "发行包文件清单与实际内容不一致");
    for (name, info) in &manifest.files {
        relative(name)?;
        ensure!(
            name != "release.json" && matches!(info.mode, 0o644 | 0o755),
            "发行文件权限或路径无效"
        );
        ensure!(
            info.sha256.len() == 64
                && info
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "清单摘要无效"
        );
        ensure!(
            digest(&root.join(name))? == info.sha256,
            "发行文件校验失败：{name}"
        );
        if name.starts_with("bin/") {
            ensure!(info.mode == 0o755, "发行程序必须可执行：{name}");
        }
    }
    Ok(manifest)
}

pub fn compatible(manifest: &Manifest) -> Result<()> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    ensure!(
        manifest.os == os && manifest.arch == std::env::consts::ARCH,
        "发行包与本机操作系统或 CPU 架构不匹配"
    );
    Ok(())
}

pub fn checksum(archive: &Path) -> Result<()> {
    ensure!(
        fs::metadata(archive)?.len() <= MAX_ARCHIVE,
        "发行附件大小超限"
    );
    let sums = archive.with_file_name("SHA256SUMS");
    ensure!(fs::metadata(&sums)?.len() <= 65536, "校验清单过大");
    let sums = fs::read_to_string(sums).context("请将 SHA256SUMS 与发行包放在同一目录")?;
    let name = archive
        .file_name()
        .and_then(|s| s.to_str())
        .context("安装包文件名无效")?;
    let matches = sums
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() == 2 && fields[1] == name).then(|| fields[0])
        })
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1 && matches[0] == digest(archive)?,
        "安装包 SHA256 不匹配"
    );
    Ok(())
}

pub fn extract(archive: &Path, destination: &Path) -> Result<Manifest> {
    checksum(archive)?;
    fs::create_dir(destination)?;
    let result = (|| {
        let decoder = flate2::read::GzDecoder::new(File::open(archive)?);
        let mut bundle = tar::Archive::new(decoder);
        let mut seen = BTreeSet::new();
        let mut total = 0u64;
        for entry in bundle.entries()? {
            let mut entry = entry?;
            ensure!(
                entry.header().entry_type().is_file(),
                "发行包只能包含普通文件"
            );
            let name = String::from_utf8(entry.path_bytes().to_vec())?;
            relative(&name)?;
            ensure!(
                seen.insert(name.clone()) && seen.len() <= 300,
                "发行包文件重复或数量超限"
            );
            total = total
                .checked_add(entry.size())
                .context("发行包展开体积溢出")?;
            ensure!(total <= MAX_EXPANDED, "发行包展开体积超过 1 GiB");
            let path = destination.join(&name);
            fs::create_dir_all(path.parent().unwrap())?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            let written = std::io::copy(&mut entry, &mut file)?;
            ensure!(written == entry.size(), "发行文件截断");
            file.sync_all()?;
        }
        verify(destination, true)
    })();
    if result.is_err() {
        fs::remove_dir_all(destination).context("清理未通过校验的临时发行目录失败")?;
    }
    result
}

pub fn copy(source: &Path, destination: &Path, manifest: &Manifest) -> Result<()> {
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    for name in manifest
        .files
        .keys()
        .map(String::as_str)
        .chain(["release.json"])
    {
        let path = destination.join(relative(name)?);
        fs::create_dir_all(path.parent().unwrap())?;
        for directory in path
            .parent()
            .unwrap()
            .ancestors()
            .take_while(|p| p.starts_with(destination))
        {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
        }
        fs::copy(source.join(name), &path)?;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(manifest.files.get(name).map_or(0o644, |v| v.mode)),
        )?;
    }
    verify(destination, true)?;
    Ok(())
}

async fn fetch(client: &reqwest::Client, url: &str, path: &Path, limit: u64) -> Result<()> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(response.url().scheme() == "https", "下载必须使用 HTTPS");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut total = 0u64;
    while let Some(chunk) = response.chunk().await? {
        total += chunk.len() as u64;
        ensure!(total <= limit, "发行附件大小超限");
        file.write_all(&chunk)?;
    }
    file.sync_all()?;
    Ok(())
}

pub async fn download(
    directory: &Path,
    requested: Option<&str>,
) -> Result<(std::path::PathBuf, String)> {
    if let Some(v) = requested {
        version(v)?;
    }
    let client = reqwest::Client::builder()
        .https_only(true)
        .user_agent("chainctl")
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()?;
    let endpoint = requested.map_or("latest".into(), |v| format!("tags/v{v}"));
    let metadata = directory.join("github-release.json");
    fetch(
        &client,
        &format!("https://api.github.com/repos/{REPO}/releases/{endpoint}"),
        &metadata,
        1024 * 1024,
    )
    .await?;
    let release: serde_json::Value = serde_json::from_reader(File::open(&metadata)?)?;
    let tag = release["tag_name"].as_str().context("发行版本缺失")?;
    let version_string = tag.strip_prefix('v').context("发行版本无效")?;
    version(version_string)?;
    ensure!(
        requested.is_none_or(|v| v == version_string)
            && release["draft"] == false
            && release["prerelease"] == false,
        "正式版本信息无效"
    );
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    let name = format!(
        "chain-panel-{version_string}-{os}-{}.tar.gz",
        std::env::consts::ARCH
    );
    for (filename, limit) in [("SHA256SUMS", 65536), (name.as_str(), MAX_ARCHIVE)] {
        let assets = release["assets"].as_array().context("发行附件缺失")?;
        let matches = assets
            .iter()
            .filter(|a| a["name"] == filename)
            .collect::<Vec<_>>();
        let url = format!("https://github.com/{REPO}/releases/download/{tag}/{filename}");
        ensure!(
            matches.len() == 1 && matches[0]["browser_download_url"] == url,
            "发行附件来源不匹配或附件缺失"
        );
        fetch(&client, &url, &directory.join(filename), limit).await?;
    }
    let archive = directory.join(name);
    checksum(&archive)?;
    Ok((archive, version_string.into()))
}

fn xray_asset(releases: &serde_json::Value) -> Result<(String, String, String, bool)> {
    let releases = releases.as_array().context("Xray 发行信息无效")?;
    let release = releases
        .iter()
        .filter(|r| r["draft"] == false)
        .filter_map(|r| {
            r["tag_name"]
                .as_str()
                .and_then(|s| s.strip_prefix('v'))
                .and_then(|v| version(v).ok())
                .map(|v| (v, r))
        })
        .max_by_key(|(v, _)| *v)
        .context("未找到官方 Xray 发行版本")?
        .1;
    let tag = release["tag_name"].as_str().unwrap();
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "64",
        "aarch64" => "arm64-v8a",
        _ => anyhow::bail!("Xray 不支持本机架构"),
    };
    let name = format!("Xray-{os}-{arch}.zip");
    let matches = release["assets"]
        .as_array()
        .context("Xray 附件缺失")?
        .iter()
        .filter(|a| a["name"] == name)
        .collect::<Vec<_>>();
    ensure!(matches.len() == 1, "Xray 本机架构附件缺失或重复");
    let url = format!("https://github.com/XTLS/Xray-core/releases/download/{tag}/{name}");
    ensure!(
        matches[0]["browser_download_url"] == url,
        "Xray 附件来源不匹配"
    );
    let digest = matches[0]["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .context("Xray 附件缺少 SHA256")?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Xray 附件 SHA256 无效"
    );
    Ok((
        tag[1..].into(),
        url,
        digest.into(),
        release["prerelease"] == true,
    ))
}

fn xray_files(archive: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut bundle = zip::ZipArchive::new(File::open(archive)?)?;
    ensure!(bundle.len() <= 100, "Xray 附件文件数量超限");
    let mut result = Vec::new();
    for name in ["xray", "LICENSE"] {
        let matches = (0..bundle.len())
            .filter(|i| bundle.by_index(*i).is_ok_and(|e| e.name() == name))
            .collect::<Vec<_>>();
        ensure!(matches.len() == 1, "Xray 附件缺少或重复 {name}");
        let mut entry = bundle.by_index(matches[0])?;
        ensure!(
            entry.is_file() && !entry.is_symlink() && entry.size() <= 128 * 1024 * 1024,
            "Xray 文件类型或展开大小无效"
        );
        let mut bytes = Vec::new();
        (&mut entry)
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == entry.size() && bytes.len() <= 128 * 1024 * 1024,
            "Xray 文件展开大小无效"
        );
        result.push(bytes);
    }
    Ok((result.remove(0), result.remove(0)))
}

pub async fn latest_xray(stage: &Path, directory: &Path) -> Result<()> {
    verify(stage, true)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .user_agent("chainctl")
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()?;
    let metadata = directory.join("xray-releases.json");
    fetch(
        &client,
        "https://api.github.com/repos/XTLS/Xray-core/releases?per_page=30",
        &metadata,
        2 * 1024 * 1024,
    )
    .await?;
    let (version, url, expected, prerelease) =
        xray_asset(&serde_json::from_reader(File::open(metadata)?)?)?;
    let archive = directory.join("xray.zip");
    fetch(&client, &url, &archive, 128 * 1024 * 1024).await?;
    ensure!(
        digest(&archive)? == expected,
        "Xray 下载校验失败，未替换内核"
    );
    let (binary, license) = xray_files(&archive)?;
    let mut manifest: serde_json::Value =
        serde_json::from_reader(File::open(stage.join("release.json"))?)?;
    ensure!(
        manifest["files"]["share/Xray-LICENSE"].is_object(),
        "原发行包缺少 Xray 许可证"
    );
    for (name, bytes) in [("bin/xray", binary), ("share/Xray-LICENSE", license)] {
        fs::write(stage.join(name), bytes)?;
        manifest["files"][name]["sha256"] = serde_json::json!(digest(&stage.join(name))?);
    }
    let provenance = stage.join("share/Xray-PATCHES.txt");
    if provenance.exists() {
        fs::remove_file(provenance)?;
    }
    manifest["files"]
        .as_object_mut()
        .unwrap()
        .remove("share/Xray-PATCHES.txt");
    manifest["xray"] = serde_json::json!(version);
    manifest["xray_channel"] = serde_json::json!("latest");
    manifest["xray_prerelease"] = serde_json::json!(prerelease);
    manifest["xray_source"] = serde_json::json!(url);
    manifest["xray_archive_sha256"] = serde_json::json!(expected);
    fs::write(
        stage.join("release.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    verify(stage, true)?;
    println!(
        "主动选择官方最新版 Xray {version}{}；可能要求更新客户端",
        if prerelease { "（预发布）" } else { "" }
    );
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub fn fixture(root: &Path) {
        fs::create_dir_all(root.join("bin")).unwrap();
        let mut files = serde_json::Map::new();
        for name in ["chainctl", "chain-panel", "chain-agent", "xray"] {
            let path = root.join("bin").join(name);
            fs::write(&path, name).unwrap();
            files.insert(
                format!("bin/{name}"),
                json!({"sha256":digest(&path).unwrap(),"mode":493}),
            );
        }
        fs::write(root.join("release.json"), serde_json::to_vec(&json!({"format":1,"version":"0.3.0","arch":std::env::consts::ARCH,"os":if cfg!(target_os="macos") {"darwin"}else{"linux"},"files":files})).unwrap()).unwrap();
    }

    #[test]
    fn strict_paths_versions_and_manifest() {
        for path in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "a/",
            "a\\b",
        ] {
            assert!(relative(path).is_err(), "{path}");
        }
        for v in ["v1.2.3", "1.2", "1.2.3.4", "1.2.-3", "1.2.3\n", "1.2."] {
            assert!(version(v).is_err());
        }
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        assert!(verify(root.path(), true).is_ok());
        fs::write(root.path().join("bin/xray"), "tampered").unwrap();
        assert!(verify(root.path(), true).is_err());
        fs::remove_file(root.path().join("bin/xray")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.path().join("bin/xray")).unwrap();
        assert!(verify(root.path(), true).is_err());
    }

    #[test]
    fn checked_extraction_rejects_links_and_duplicates() {
        for variant in ["valid", "symlink", "duplicate", "checksum"] {
            let tmp = tempfile::tempdir().unwrap();
            let source = tmp.path().join("source");
            fixture(&source);
            let archive = tmp.path().join("release.tar.gz");
            let encoder = flate2::write::GzEncoder::new(
                File::create(&archive).unwrap(),
                flate2::Compression::default(),
            );
            let mut tar = tar::Builder::new(encoder);
            for name in [
                "release.json",
                "bin/chainctl",
                "bin/chain-panel",
                "bin/chain-agent",
                "bin/xray",
            ] {
                tar.append_path_with_name(source.join(name), name).unwrap();
            }
            if variant == "symlink" {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o644);
                tar.append_link(&mut header, "bad", "/etc/passwd").unwrap();
            }
            if variant == "duplicate" {
                tar.append_path_with_name(source.join("release.json"), "release.json")
                    .unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
            let sum = if variant == "checksum" {
                "0".repeat(64)
            } else {
                digest(&archive).unwrap()
            };
            fs::write(
                tmp.path().join("SHA256SUMS"),
                format!("{sum}  release.tar.gz\n"),
            )
            .unwrap();
            let target = tmp.path().join("stage");
            let result = extract(&archive, &target);
            assert_eq!(result.is_ok(), variant == "valid", "{variant}: {result:?}");
            assert_eq!(target.exists(), variant == "valid");
        }
    }

    #[test]
    fn latest_xray_requires_official_url_and_checksum() {
        let os = if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64-v8a"
        } else {
            "64"
        };
        let name = format!("Xray-{os}-{arch}.zip");
        let mut releases = json!([{"tag_name":"v26.9.9","draft":false,"prerelease":true,"assets":[{"name":name,"digest":format!("sha256:{}","a".repeat(64)),"browser_download_url":format!("https://github.com/XTLS/Xray-core/releases/download/v26.9.9/{name}")}]}]);
        assert_eq!(xray_asset(&releases).unwrap().0, "26.9.9");
        assert!(xray_asset(&releases).unwrap().3);
        releases[0]["assets"][0]["browser_download_url"] = json!("https://other.example/xray.zip");
        assert!(xray_asset(&releases).is_err());
    }
}
