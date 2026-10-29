use super::{
    package,
    upgrade::{Manager, OWNER, atomic, command, ordinary},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString},
    fs::{self, File},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

#[derive(Default, Clone, Debug)]
pub struct Options {
    pub listen: Option<String>,
    pub public_url: Option<String>,
    pub secure_cookie: Option<String>,
    pub panel_url: Option<String>,
    pub node_id: Option<String>,
    pub token_file: Option<PathBuf>,
    pub node_token: Option<String>,
    pub allow_http: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub panel_url: String,
    pub node_id: String,
    pub token: String,
}

pub fn url(value: &str) -> Result<url::Url> {
    ensure!(
        !value.chars().any(|c| c.is_whitespace() || c.is_control()),
        "控制台地址包含空白或控制字符"
    );
    let url = url::Url::parse(value)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none(),
        "控制台地址须为完整 HTTP/HTTPS 根地址且不含凭据"
    );
    Ok(url)
}
pub fn token(value: &str) -> Result<()> {
    ensure!(
        (32..=256).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "节点令牌格式无效"
    );
    Ok(())
}
pub fn connection(value: &str) -> Result<Connection> {
    ensure!(value.len() < 8192, "接入码过长");
    let encoded = value
        .strip_prefix("chain-agent-v1:")
        .context("接入码格式无效")?
        .trim_end_matches('=');
    let connection: Connection = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
    url(&connection.panel_url)?;
    uuid::Uuid::parse_str(&connection.node_id)?;
    token(&connection.token)?;
    Ok(connection)
}

pub fn configuration(
    manager: &Manager,
    role: &str,
    options: &Options,
    mut env: BTreeMap<String, String>,
) -> Result<(BTreeMap<String, String>, Option<String>)> {
    ensure!(matches!(role, "panel" | "agent"), "组件无效");
    env.insert(
        "CHAIN_XRAY_BIN".into(),
        manager
            .base
            .join(format!("current-{role}/bin/xray"))
            .to_string_lossy()
            .into(),
    );
    env.insert(
        "CHAIN_ACME_WEBROOT".into(),
        manager
            .path(&format!("/var/lib/chain-acme/{role}"))
            .to_string_lossy()
            .into(),
    );
    let mut secret = None;
    if role == "panel" {
        env.insert(
            "CHAIN_DATA_DIR".into(),
            manager
                .path("/var/lib/chain-panel")
                .to_string_lossy()
                .into(),
        );
        env.insert(
            "CHAIN_RELEASE_DIR".into(),
            manager.base.join("current-panel").to_string_lossy().into(),
        );
        let listen = options
            .listen
            .clone()
            .or_else(|| env.get("CHAIN_LISTEN").cloned())
            .unwrap_or_else(|| "127.0.0.1:8088".into());
        let addr: std::net::SocketAddr = listen.parse().context("监听地址须为 IP:端口")?;
        ensure!(addr.port() != 0, "监听端口不能为 0");
        env.insert("CHAIN_LISTEN".into(), listen);
        let secure = options
            .secure_cookie
            .clone()
            .or_else(|| env.get("CHAIN_SECURE_COOKIE").cloned())
            .unwrap_or_else(|| "0".into());
        ensure!(
            matches!(secure.as_str(), "0" | "1"),
            "secure-cookie 只能为 0 或 1"
        );
        env.insert("CHAIN_SECURE_COOKIE".into(), secure);
        if let Some(public) = &options.public_url {
            env.remove("CHAIN_PUBLIC_URL");
            if !public.is_empty() {
                url(public)?;
                env.insert(
                    "CHAIN_PUBLIC_URL".into(),
                    public.trim_end_matches('/').into(),
                );
            }
        }
    } else {
        env.insert(
            "CHAIN_AGENT_DIR".into(),
            manager
                .path("/var/lib/chain-agent")
                .to_string_lossy()
                .into(),
        );
        let token_path = manager.path("/etc/chain-agent/token");
        env.insert(
            "CHAIN_NODE_TOKEN_FILE".into(),
            token_path.to_string_lossy().into(),
        );
        let panel = options
            .panel_url
            .clone()
            .or_else(|| env.get("CHAIN_PANEL_URL").cloned())
            .context("Agent 需要控制台地址")?;
        let node = options
            .node_id
            .clone()
            .or_else(|| env.get("CHAIN_NODE_ID").cloned())
            .context("Agent 需要节点 UUID")?;
        let endpoint = url(&panel)?;
        let host = endpoint.host_str().unwrap().trim_matches(['[', ']']);
        let loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        let allow_http =
            options.allow_http || env.get("CHAIN_ALLOW_HTTP").is_some_and(|v| v == "1");
        ensure!(
            endpoint.scheme() == "https" || loopback || allow_http,
            "非本机控制台需要 HTTPS；可信私网可显式加 --allow-http"
        );
        uuid::Uuid::parse_str(&node)?;
        env.insert("CHAIN_PANEL_URL".into(), panel.trim_end_matches('/').into());
        env.insert("CHAIN_NODE_ID".into(), node);
        if allow_http {
            env.insert("CHAIN_ALLOW_HTTP".into(), "1".into());
        }
        if let Some(value) = &options.node_token {
            secret = Some(value.clone());
        } else if let Some(path) = &options.token_file {
            let info = fs::metadata(path)?;
            ensure!(
                info.is_file() && info.mode() & 0o077 == 0 && info.len() <= 1024,
                "令牌文件必须为私有普通文件"
            );
            secret = Some(fs::read_to_string(path)?.trim().into());
        }
        if let Some(value) = &secret {
            token(value)?;
        } else {
            ordinary(&token_path)?;
            let info = fs::metadata(&token_path).context("请指定节点令牌文件或接入码")?;
            ensure!(
                info.is_file() && info.mode() & 0o077 == 0 && info.len() <= 1024,
                "已有令牌文件权限或大小无效"
            );
            token(fs::read_to_string(token_path)?.trim())?;
        }
    }
    Ok((env, secret))
}

fn environment(env: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let mut text = String::new();
    for (key, value) in env {
        ensure!(
            key.starts_with("CHAIN_")
                && key.bytes().all(|c| c.is_ascii_uppercase() || c == b'_')
                && !value.contains(['\n', '\r', '\0']),
            "环境变量无效"
        );
        text.push_str(&format!("{key}={}\n", serde_json::to_string(value)?));
    }
    Ok(text.into_bytes())
}

fn account(name: &str) -> Result<Option<(u32, u32)>> {
    let c_name = CString::new(name)?;
    let record = unsafe { libc::getpwnam(c_name.as_ptr()) };
    if record.is_null() {
        return Ok(None);
    }
    let record = unsafe { &*record };
    let home = unsafe { CStr::from_ptr(record.pw_dir) }.to_str()?;
    let shell = unsafe { CStr::from_ptr(record.pw_shell) }.to_str()?;
    ensure!(
        record.pw_uid != 0
            && home == format!("/var/lib/{name}")
            && (shell.ends_with("nologin") || shell.ends_with("false")),
        "已有服务账户属性不匹配"
    );
    Ok(Some((record.pw_uid, record.pw_gid)))
}

async fn ensure_account(manager: &Manager, role: &str) -> Result<(u32, u32)> {
    if !manager.live() {
        return Ok((unsafe { libc::geteuid() }, unsafe { libc::getegid() }));
    }
    let name = format!("chain-{role}");
    if let Some(identity) = account(&name)? {
        return Ok(identity);
    }
    ensure!(
        unsafe { libc::getgrnam(CString::new(name.clone())?.as_ptr()) }.is_null(),
        "已有同名组，无法确认归属"
    );
    super::platform::create_account(&name).await?;
    account(&name)?.context("服务账户创建后无法读取")
}

pub(super) fn owned_dir(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    ordinary(path)?;
    if path.exists() {
        let info = fs::metadata(path)?;
        ensure!(
            info.is_dir() && (info.uid(), info.gid(), info.mode() & 0o777) == (uid, gid, mode),
            "已有目录属主或权限不符合服务契约，未调整：{}",
            path.display()
        );
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(mode)
        .create(path)?;
    let file = File::open(path)?;
    ensure!(
        unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } == 0,
        "无法设置新目录属主"
    );
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(())
}
pub(super) fn owned_write(path: &Path, body: &[u8], mode: u32, uid: u32, gid: u32) -> Result<()> {
    ordinary(path)?;
    if path.exists() {
        let info = fs::metadata(path)?;
        ensure!(
            (info.uid(), info.gid()) == (uid, gid),
            "已有文件归属不匹配，未覆盖：{}",
            path.display()
        );
    }
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    use std::io::Write;
    temporary.write_all(body)?;
    ensure!(
        unsafe { libc::fchown(temporary.as_file().as_raw_fd(), uid, gid) } == 0,
        "无法设置新文件属主"
    );
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

pub fn unit_with_env(
    manager: &Manager,
    role: &str,
    env: &BTreeMap<String, String>,
) -> Result<String> {
    ensure!(matches!(role, "panel" | "agent"), "组件无效");
    if manager.kind() != "systemd" {
        return super::platform::definition(
            manager,
            &format!("chain-{role}"),
            &[manager
                .base
                .join(format!("current-{role}/bin/chain-{role}"))
                .to_string_lossy()
                .into()],
            Some(&format!("chain-{role}")),
            env,
        );
    }
    let quote = |path: PathBuf| -> Result<String> {
        let value = path.to_str().context("服务路径必须为 UTF-8")?;
        ensure!(!value.contains(['\r', '\n', '\0']), "服务路径无效");
        Ok(format!(
            "\"{}\"",
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
        ))
    };
    Ok(format!(
        "# {OWNER}\n[Unit]\nDescription=Chain {role}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nUser=chain-{role}\nGroup=chain-{role}\nStateDirectory=chain-{role}\nStateDirectoryMode=0700\nUMask=0077\nEnvironmentFile={}\nExecStart={}\nRestart=on-failure\nRestartSec=5\nKillMode=control-group\nTimeoutStopSec=50\n{}NoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=true\nReadWritePaths={}\n\n[Install]\nWantedBy=multi-user.target\n",
        quote(manager.path(&format!("/etc/chain-{role}/{role}.env")))?,
        quote(
            manager
                .base
                .join(format!("current-{role}/bin/chain-{role}"))
        )?,
        if role == "agent" {
            "LimitNOFILE=65536\nAmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\n"
        } else {
            ""
        },
        quote(manager.path(&format!("/var/lib/chain-acme/{role}")))?
    ))
}

fn recoverable_openrc_data(manager: &Manager, role: &str) -> Result<bool> {
    if manager.kind() != "openrc" {
        return Ok(false);
    }
    let data = manager.path(&format!("/var/lib/chain-{role}"));
    let identity = if manager.live() {
        account(&format!("chain-{role}"))?
    } else {
        Some((unsafe { libc::geteuid() }, unsafe { libc::getegid() }))
    };
    let Some((uid, gid)) = identity else {
        return Ok(false);
    };
    let owned = |path: &Path, mode: u32, uid: u32, gid: u32| {
        fs::symlink_metadata(path).is_ok_and(|m| {
            !m.file_type().is_symlink() && (m.mode() & 0o777, m.uid(), m.gid()) == (mode, uid, gid)
        })
    };
    if !owned(
        &manager.base.join(".chain-install.json"),
        0o600,
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
    ) || !owned(&data, 0o700, uid, gid)
    {
        return Ok(false);
    }
    let marker = data.join(".chain-owner");
    if owned(&marker, 0o600, uid, gid) {
        return Ok(fs::read_to_string(marker)? == format!("chain-{role}"));
    }
    let entries = fs::read_dir(&data)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let log = data.join("service.log");
    Ok(entries == [log.clone()] && log.is_file() && owned(&log, 0o600, uid, gid))
}

pub async fn component(
    manager: &Manager,
    source: &Path,
    role: &str,
    options: &Options,
) -> Result<()> {
    let manifest = package::verify(source, true)?;
    package::compatible(&manifest)?;
    let old = manager.state()?;
    let prior = old["roles"][role].is_object();
    let retained = old["retained_roles"][role].is_object();
    let etc = manager.path(&format!("/etc/chain-{role}"));
    let data = manager.path(&format!("/var/lib/chain-{role}"));
    let service_path = manager.unit_path(role);
    ordinary(&service_path)?;
    if prior {
        ensure!(
            fs::read_to_string(&service_path)?.contains(OWNER),
            "服务文件归属不匹配"
        );
    } else {
        ensure!(!service_path.exists(), "已有其他来源的服务文件，拒绝覆盖");
    }
    for path in [&etc, &data] {
        ordinary(path)?;
        ensure!(
            prior
                || retained
                || !path.exists()
                || fs::read_dir(path)?.next().is_none()
                || (path == &data && recoverable_openrc_data(manager, role)?),
            "已有目录非空，拒绝接管：{}",
            path.display()
        );
    }
    let old_env = if etc.join(format!("{role}.env")).exists() {
        manager.environment(role)?
    } else {
        BTreeMap::new()
    };
    let (env, secret) = configuration(manager, role, options, old_env.clone())?;
    let definition = unit_with_env(manager, role, &env)?;
    let (uid, gid) = ensure_account(manager, role).await?;
    let _guard = manager.lock()?;
    ensure!(manager.state()? == old, "安装状态已经变化");
    for path in [&etc, &data] {
        owned_dir(path, uid, gid, 0o700)?;
    }
    if manager.kind() == "openrc" && !data.join(".chain-owner").exists() {
        ensure!(
            fs::read_dir(&data)?.next().is_none() || recoverable_openrc_data(manager, role)?,
            "数据目录归属未确认"
        );
        owned_write(
            &data.join(".chain-owner"),
            format!("chain-{role}").as_bytes(),
            0o600,
            uid,
            gid,
        )?;
    }
    owned_dir(
        &manager.path(&format!("/var/lib/chain-acme/{role}")),
        uid,
        gid,
        0o755,
    )?;
    let releases = manager.base.join("releases");
    ordinary(&releases)?;
    fs::create_dir_all(&releases)?;
    let id = format!(
        "{}-{}-{}-{}",
        manifest.version,
        manifest.os,
        manifest.arch,
        &package::digest(&source.join("release.json"))?[..12]
    );
    let destination = releases.join(&id);
    ordinary(&destination)?;
    if destination.exists() {
        package::verify(&destination, true)?;
    } else {
        let temporary = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&releases)?;
        let stage = temporary.path().join("release");
        package::copy(source, &stage, &manifest)?;
        fs::rename(stage, &destination)?;
    }
    let paths = [
        etc.join(format!("{role}.env")),
        etc.join("token"),
        service_path.clone(),
    ];
    let snapshots = paths
        .iter()
        .map(|path| -> Result<_> {
            ordinary(path)?;
            if path.exists() {
                let m = fs::metadata(path)?;
                Ok(Some((fs::read(path)?, m.uid(), m.gid(), m.mode() & 0o777)))
            } else {
                Ok(None)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let was_active = manager.active(role).await?;
    let result: Result<()> = async {
        if prior {
            manager.service("stop", role).await?;
        }
        if manager.live() && role == "panel" {
            let addr: std::net::SocketAddr = env["CHAIN_LISTEN"].parse()?;
            drop(std::net::TcpListener::bind(addr).context("控制台监听端口已被占用")?);
        }
        owned_write(&paths[0], &environment(&env)?, 0o600, uid, gid)?;
        if let Some(secret) = &secret {
            owned_write(&paths[1], format!("{secret}\n").as_bytes(), 0o600, uid, gid)?;
        }
        fs::create_dir_all(service_path.parent().unwrap())?;
        atomic(
            &service_path,
            definition.as_bytes(),
            if manager.kind() == "openrc" {
                0o755
            } else {
                0o644
            },
        )?;
        manager.link(role, &id)?;
        super::platform::enable(manager, &format!("chain-{role}")).await?;
        manager.service("start", role).await?;
        manager.healthy(role.into()).await?;
        let mut state = old.clone();
        let mut record = if prior {
            old["roles"][role].clone()
        } else if retained {
            old["retained_roles"][role].clone()
        } else {
            json!({})
        };
        record["current"] = json!(id);
        record["previous"] = Value::Null;
        record["version"] = json!(manifest.version);
        state["roles"][role] = record;
        if let Some(roles) = state["retained_roles"].as_object_mut() {
            roles.remove(role);
        }
        atomic(
            &manager.base.join(".chain-install.json"),
            &serde_json::to_vec_pretty(&state)?,
            0o600,
        )?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        manager.service("stop", role).await?;
        for (path, snapshot) in paths.iter().zip(&snapshots) {
            if let Some((bytes, uid, gid, mode)) = snapshot {
                owned_write(path, bytes, *mode, *uid, *gid)?;
            } else if path.exists() {
                fs::remove_file(path)?;
            }
        }
        if prior {
            manager.link(
                role,
                old["roles"][role]["current"]
                    .as_str()
                    .context("原发行版本缺失")?,
            )?;
        } else if manager.base.join(format!("current-{role}")).is_symlink() {
            fs::remove_file(manager.base.join(format!("current-{role}")))?;
        }
        if manager.live() && manager.kind() == "systemd" {
            command("systemctl", &["daemon-reload"], 30).await?;
        }
        if was_active {
            manager.service("start", role).await?;
        }
        return Err(error.context("安装未完成，已恢复原程序链接和配置；业务数据保留"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enrollment_rejects_credentials_paths_and_invalid_tokens() {
        for value in [
            "https://u:p@example.com",
            "https://example.com/path",
            "ftp://example.com",
            "https://example.com/?token=x",
            "https://example.com\n",
        ] {
            assert!(url(value).is_err());
        }
        let value = json!({"panel_url":"https://example.com","node_id":uuid::Uuid::new_v4(),"token":"a".repeat(32)});
        let code = format!(
            "chain-agent-v1:{}",
            URL_SAFE_NO_PAD.encode(value.to_string())
        );
        assert!(connection(&code).is_ok());
        assert!(connection("chain-agent-v1:bad").is_err());
    }
    #[test]
    fn remote_plaintext_requires_explicit_consent() {
        let root = tempfile::tempdir().unwrap();
        let manager = Manager {
            root: root.path().into(),
            base: root.path().join("opt/chain"),
        };
        let mut options = Options {
            panel_url: Some("http://example.com".into()),
            node_id: Some(uuid::Uuid::new_v4().to_string()),
            node_token: Some("a".repeat(32)),
            ..Default::default()
        };
        assert!(configuration(&manager, "agent", &options, BTreeMap::new()).is_err());
        options.allow_http = true;
        assert!(configuration(&manager, "agent", &options, BTreeMap::new()).is_ok());
        options.allow_http = false;
        options.panel_url = Some("http://[::1]:8088".into());
        assert!(configuration(&manager, "agent", &options, BTreeMap::new()).is_ok());
    }
    #[tokio::test]
    async fn offline_agent_install_writes_service_and_keeps_identity_on_repair() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("opt/chain");
        fs::create_dir_all(&base).unwrap();
        fs::write(
            base.join(".chain-install.json"),
            json!({"owner":OWNER,"roles":{}}).to_string(),
        )
        .unwrap();
        let manager = Manager::open(root.path()).unwrap();
        let source = root.path().join("source");
        super::super::package::tests::fixture(&source);
        let node = uuid::Uuid::new_v4().to_string();
        let options = Options {
            panel_url: Some("https://example.com".into()),
            node_id: Some(node.clone()),
            node_token: Some("a".repeat(32)),
            ..Default::default()
        };
        component(&manager, &source, "agent", &options)
            .await
            .unwrap();
        assert_eq!(manager.environment("agent").unwrap()["CHAIN_NODE_ID"], node);
        let unit = fs::read_to_string(manager.unit_path("agent")).unwrap();
        match manager.kind() {
            "launchd" => {
                assert!(unit.contains("<key>UserName</key><string>chain-agent</string>"));
                assert!(unit.contains("<key>GroupName</key><string>chain-agent</string>"));
                assert!(unit.contains(&format!("<key>CHAIN_NODE_ID</key><string>{node}</string>")));
            }
            "openrc" => assert!(unit.contains("rc_ulimit=\"-n 65536\"")),
            _ => assert!(unit.contains("LimitNOFILE=65536")),
        }
        let path = manager.path("/etc/chain-agent/token");
        let before = fs::metadata(&path).unwrap();
        component(&manager, &source, "agent", &Options::default())
            .await
            .unwrap();
        let after = fs::metadata(path).unwrap();
        assert_eq!(
            (before.ino(), before.uid(), before.gid(), before.mode()),
            (after.ino(), after.uid(), after.gid(), after.mode())
        );
    }
}
