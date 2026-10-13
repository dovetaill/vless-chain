use crate::{compiler::host_valid, runtime::atomic_write};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

#[derive(Serialize)]
pub struct HostKey {
    pub fingerprint: String,
    pub algorithm: String,
}
#[derive(Deserialize)]
pub struct Install {
    pub port: u16,
    pub fingerprint: String,
    pub password: Option<String>,
    pub private_key: Option<String>,
    pub panel_url: String,
    pub token: String,
    #[serde(default)]
    pub allow_http: bool,
}

struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

async fn execute(command: &mut Command, directory: &Path, seconds: u64) -> Result<String> {
    let log = tempfile::NamedTempFile::new_in(directory)?;
    command.as_std_mut().process_group(0);
    let output = log.reopen()?;
    command
        .stdout(output.try_clone()?)
        .stderr(output)
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .context("无法运行 SSH 工具；请通过安装向导补齐 openssh-client")?;
    let group = ProcessGroup(child.id().context("缺少子进程 ID")?);
    let status = match tokio::time::timeout(Duration::from_secs(seconds), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            unsafe {
                libc::kill(-(group.0 as i32), libc::SIGKILL);
            }
            let _ = child.wait().await;
            anyhow::bail!("SSH 操作超时，请检查服务器网络及 SSH 端口");
        }
    };
    drop(group);
    let text = std::fs::read_to_string(log.path())?;
    ensure!(
        status.success(),
        "远程安装失败：{}",
        text.chars().take(4000).collect::<String>()
    );
    Ok(text)
}

async fn scan_lines(host: &str, port: u16, directory: &Path) -> Result<Vec<(HostKey, String)>> {
    ensure!(
        host_valid(host) && !host.starts_with('-') && port > 0,
        "SSH 地址或端口无效"
    );
    let mut command = Command::new("ssh-keyscan");
    command
        .args([
            "-T",
            "5",
            "-t",
            "ed25519,ecdsa,rsa",
            "-p",
            &port.to_string(),
            host,
        ])
        .stdin(Stdio::null());
    let text = execute(&mut command, directory, 20).await?;
    let mut keys = Vec::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue;
        }
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(parts[2]) {
            keys.push((
                HostKey {
                    fingerprint: format!(
                        "SHA256:{}",
                        STANDARD_NO_PAD.encode(Sha256::digest(bytes))
                    ),
                    algorithm: parts[1].into(),
                },
                line.into(),
            ));
        }
    }
    ensure!(!keys.is_empty(), "无法读取服务器 SSH 主机密钥");
    Ok(keys)
}

pub async fn scan(host: &str, port: u16) -> Result<Vec<HostKey>> {
    let work = tempfile::tempdir()?;
    Ok(scan_lines(host, port, work.path())
        .await?
        .into_iter()
        .map(|(key, _)| key)
        .collect())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub async fn install(host: &str, node: &str, request: &Install) -> Result<()> {
    uuid::Uuid::parse_str(node)?;
    ensure!(
        request.token.len() >= 32
            && request.token.len() <= 256
            && request
                .token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "节点凭据无效"
    );
    let url = url::Url::parse(&request.panel_url)?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "控制台地址必须是不含凭据的根地址"
    );
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && request.allow_http),
        "公网接入需要 HTTPS；可信私网需明确允许 HTTP"
    );
    let password = request.password.as_deref().unwrap_or("");
    let key = request.private_key.as_deref().unwrap_or("");
    ensure!(
        password.is_empty() != key.is_empty() && password.len() < 4096 && key.len() < 32768,
        "请选择一种 SSH 认证方式"
    );
    let release = std::env::var("CHAIN_RELEASE_DIR")
        .context("Web 安装需要通过安装脚本部署的控制台，请先用安装向导升级")?;
    let release = Path::new(&release).canonicalize()?;
    // PrivateTmp removes credentials on service shutdown as well; never back them up with data.
    let work = tempfile::Builder::new().prefix(".ssh-install-").tempdir()?;
    let found = scan_lines(host, request.port, work.path()).await?;
    let (_, known) = found
        .into_iter()
        .find(|(key, _)| key.fingerprint == request.fingerprint)
        .context("SSH 主机指纹已变化，请重新检测并核对服务器身份")?;
    atomic_write(&work.path().join("known_hosts"), known.as_bytes())?;
    let package = work.path().join("package");
    std::fs::create_dir(&package)?;
    let manifest_bytes = std::fs::read(release.join("release.json"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    let files = manifest["files"].as_object().context("发行包清单无效")?;
    for (name, info) in files {
        let path = Path::new(name);
        ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "发行文件路径无效"
        );
        let source = release.join(path).canonicalize()?;
        ensure!(source.starts_with(&release), "发行文件超出目录");
        let bytes = std::fs::read(source)?;
        ensure!(
            hex::encode(Sha256::digest(&bytes)) == info["sha256"].as_str().unwrap_or(""),
            "控制台发行文件校验失败"
        );
        let destination = package.join(path);
        std::fs::create_dir_all(destination.parent().unwrap())?;
        atomic_write(&destination, &bytes)?;
    }
    atomic_write(&package.join("release.json"), &manifest_bytes)?;
    atomic_write(&work.path().join("node-token"), request.token.as_bytes())?;
    let archive = work.path().join("upload.tar.gz");
    execute(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(work.path())
            .args(["package", "node-token"]),
        work.path(),
        60,
    )
    .await?;
    let mut ssh = Command::new("ssh");
    ssh.args([
        "-F",
        "/dev/null",
        "-T",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "GlobalKnownHostsFile=/dev/null",
        "-o",
        "ConnectTimeout=12",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "NumberOfPasswordPrompts=1",
    ])
    .arg("-o")
    .arg(format!(
        "UserKnownHostsFile={}",
        work.path().join("known_hosts").display()
    ))
    .args(["-p", &request.port.to_string()]);
    if !key.is_empty() {
        let path = work.path().join("identity");
        atomic_write(&path, format!("{}\n", key.trim_end()).as_bytes())?;
        ssh.args(["-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes"])
            .arg("-i")
            .arg(path);
    } else {
        let secret = work.path().join("ssh-password");
        atomic_write(&secret, password.as_bytes())?;
        let helper = work.path().join("askpass");
        atomic_write(&helper, b"#!/bin/sh\ncat -- \"$CHAIN_SSH_PASSWORD_FILE\"\n")?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        ssh.args([
            "-o",
            "PreferredAuthentications=password,keyboard-interactive",
            "-o",
            "PubkeyAuthentication=no",
        ])
        .env("SSH_ASKPASS", helper)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "chain:0")
        .env("CHAIN_SSH_PASSWORD_FILE", secret);
    }
    let script = format!(
        "set -eu; umask 077; t=$(mktemp -d /var/tmp/chain-ssh-XXXXXXXX); trap 'rm -rf -- \"$t\"' EXIT HUP INT TERM; tar -xzf - -C \"$t\"; bash \"$t/package/scripts/install-server.sh\" install --component agent --non-interactive --panel-url {} --node-id {} --token-file \"$t/node-token\" {}",
        quote(request.panel_url.trim_end_matches('/')),
        quote(node),
        if request.allow_http {
            "--allow-http"
        } else {
            ""
        }
    );
    ssh.arg(format!("root@{host}"))
        .arg(format!("bash -c {}", quote(&script)))
        .stdin(std::fs::File::open(archive)?);
    let result = execute(&mut ssh, work.path(), 240).await;
    // Avoid reflecting submitted credentials through command diagnostics.
    result.map(|_| ()).map_err(|error| {
        let mut text = format!("{error:#}").replace(&request.token, "[已隐藏]");
        for secret in [password, key] {
            if !secret.is_empty() {
                text = text.replace(secret, "[已隐藏]");
            }
        }
        anyhow::anyhow!(text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_quote_keeps_untrusted_text_literal() {
        assert_eq!(quote("x'; $(id)"), "'x'\\''; $(id)'");
    }
    #[tokio::test]
    #[ignore = "requires an isolated, task-owned sshd fixture"]
    async fn real_ssh_transfer_and_host_identity() -> Result<()> {
        let root = std::env::var("CHAIN_TEST_SSH_ROOT")?;
        let root = Path::new(&root);
        let port: u16 = std::fs::read_to_string(root.join("port"))?.parse()?;
        let keys = scan("127.0.0.1", port).await?;
        let mut request = Install {
            port,
            fingerprint: keys[0].fingerprint.clone(),
            password: None,
            private_key: Some(std::fs::read_to_string(root.join("identity"))?),
            panel_url: "https://panel.example.com".into(),
            token: "s".repeat(64),
            allow_http: false,
        };
        install(
            "127.0.0.1",
            "a94a6794-c60a-4c51-a094-b9164ddaaabc",
            &request,
        )
        .await?;
        ensure!(
            root.join("accepted").is_file(),
            "remote fixture installer did not run"
        );
        std::fs::remove_file(root.join("accepted"))?;
        request.fingerprint = "SHA256:wrong".into();
        ensure!(
            install(
                "127.0.0.1",
                "a94a6794-c60a-4c51-a094-b9164ddaaabc",
                &request
            )
            .await
            .is_err(),
            "changed host key accepted"
        );
        ensure!(
            !root.join("accepted").exists(),
            "ran remote command after fingerprint mismatch"
        );
        ensure!(
            !std::fs::read_dir(std::env::temp_dir())?.any(|item| item
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ssh-install-")),
            "credential directory leaked"
        );
        println!(
            "PASS real SSH key authentication, verified bundle transfer, private token handling, host-key mismatch rejection and credential cleanup; port={port}"
        );
        Ok(())
    }
}
