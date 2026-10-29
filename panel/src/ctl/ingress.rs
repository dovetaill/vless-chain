use super::{
    nginx, platform,
    upgrade::{Manager, atomic, command, ordinary},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};
const OWNER: &str = "# chain-ingress managed v1";
pub fn migrate(text: &str, port: u16) -> Result<String> {
    let mut result = text.to_owned();
    for d in nginx::parse(text)?.into_iter().rev() {
        if d.words[0] != "listen" || d.words.len() < 2 {
            continue;
        }
        let address = &d.words[1];
        if address.rsplit(':').next() != Some("443")
            || d.words.iter().any(|v| v == "quic" || v == "udp")
        {
            continue;
        }
        ensure!(
            ["443", "0.0.0.0:443", "*:443", "[::]:443"].contains(&address.as_str())
                && d.words.iter().any(|v| v == "ssl")
                && !d.words.iter().any(|v| v == "proxy_protocol"),
            "仅支持普通 HTTP TLS 443 监听迁移"
        );
        let mut words = d.words.clone();
        words[1] = format!(
            "{}:{port}",
            if address == "[::]:443" {
                "[::1]"
            } else {
                "127.0.0.1"
            }
        );
        words.push("proxy_protocol".into());
        result.replace_range(d.start..d.end, &format!("{};", words.join(" ")));
    }
    Ok(result)
}
fn wildcard(name: &str, pattern: &str) -> bool {
    if pattern.starts_with('~') {
        return true;
    }
    if let Some(tail) = pattern.strip_prefix('.') {
        return name == tail || name.ends_with(pattern);
    }
    if let Some((a, b)) = pattern.split_once('*') {
        return name.starts_with(a) && name.ends_with(b);
    }
    name == pattern
}
pub fn routes(value: &Value, blocked: &Value) -> Result<String> {
    let routes = value.as_array().context("路由不是列表")?;
    ensure!(routes.len() <= 256, "路由超过 256 条");
    let mut seen = std::collections::BTreeSet::new();
    let mut lines = vec![];
    for route in routes {
        ensure!(
            route
                .as_object()
                .is_some_and(|v| v.len() == 2 && v.contains_key("sni") && v.contains_key("port")),
            "路由字段无效"
        );
        let name = nginx::domain(route["sni"].as_str().context("缺少 SNI")?)?;
        let port = route["port"].as_u64().context("端口无效")?;
        ensure!(
            (30000..=65535).contains(&port) && seen.insert(name.clone()),
            "端口超出范围或 SNI 重复"
        );
        for p in blocked.as_array().context("站点冲突列表无效")? {
            ensure!(
                !wildcard(&name, p.as_str().context("站点名称无效")?),
                "SNI 与现有网站冲突：{name}"
            );
        }
        lines.push(format!("    {name} 127.0.0.1:{port};"));
    }
    lines.sort();
    Ok(format!("{OWNER}\n{}\n", lines.join("\n")))
}
fn directory(m: &Manager) -> PathBuf {
    m.path("/var/lib/chain-ingress")
}
fn config(m: &Manager) -> PathBuf {
    m.path("/etc/chain-ingress")
}
fn state(m: &Manager) -> Result<Value> {
    let p = directory(m).join("state.json");
    ordinary(&p)?;
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn status(m: &Manager, v: Value) -> Result<()> {
    atomic(
        &directory(m).join("status.json"),
        &serde_json::to_vec(&v)?,
        0o644,
    )
}
pub(super) fn write_existing(path: &Path, expected: &str, value: &str) -> Result<()> {
    ordinary(path)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    use std::io::{Read, Seek, Write};
    let meta = file.metadata()?;
    ensure!(meta.is_file() && meta.nlink() == 1, "配置不是独立普通文件");
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    ensure!(
        text == expected,
        "配置已被另行修改，未覆盖：{}",
        path.display()
    );
    file.rewind()?;
    if let Err(e) = (|| -> std::io::Result<()> {
        file.write_all(value.as_bytes())?;
        file.set_len(value.len() as u64)?;
        file.sync_all()
    })() {
        file.rewind()?;
        file.write_all(text.as_bytes())?;
        file.set_len(text.len() as u64)?;
        file.sync_all()?;
        return Err(e.into());
    }
    Ok(())
}
pub async fn operate(m: &Manager, action: &str) -> Result<()> {
    let dir = directory(m);
    ordinary(&dir)?;
    fs::create_dir_all(&dir)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("process.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "已有统一入口任务"
    );
    match action {
        "enable" => enable(m).await,
        "disable" => disable(m).await,
        "apply" => apply(m).await,
        _ => anyhow::bail!("统一入口操作无效"),
    }
}
pub async fn units(m: &Manager) -> Result<()> {
    let binary = m
        .base
        .join("current-agent/bin/chainctl")
        .display()
        .to_string();
    let definitions = if m.kind() == "systemd" {
        vec![
            (
                m.path("/etc/systemd/system/chain-ingress.service"),
                format!(
                    "{OWNER}\n[Unit]\nDescription=Chain native SNI routes\nStartLimitIntervalSec=0\n[Service]\nType=oneshot\nExecStart=\"{binary}\" internal-ingress-apply\n"
                ),
                0o644,
            ),
            (
                m.path("/etc/systemd/system/chain-ingress.path"),
                format!(
                    "{OWNER}\n[Path]\nPathChanged={}\nUnit=chain-ingress.service\n[Install]\nWantedBy=multi-user.target\n",
                    m.path("/var/lib/chain-agent/ingress-request.json")
                        .display()
                ),
                0o644,
            ),
        ]
    } else {
        vec![(
            if m.kind() == "openrc" {
                m.path("/etc/init.d/chain-ingress")
            } else {
                m.path("/Library/LaunchDaemons/org.vless-chain.chain-ingress.plist")
            },
            platform::definition(
                m,
                "chain-ingress",
                &[binary, "internal-ingress-watch".into()],
                None,
                &Default::default(),
            )?,
            if m.kind() == "openrc" { 0o755 } else { 0o644 },
        )]
    };
    for (p, _, _) in &definitions {
        ordinary(p)?;
        if p.exists() {
            let v = fs::read_to_string(p)?;
            ensure!(
                v.contains(OWNER) || v.contains(super::upgrade::OWNER),
                "统一入口服务归属不符"
            );
        }
    }
    for (p, text, mode) in definitions {
        fs::create_dir_all(p.parent().unwrap())?;
        atomic(&p, text.as_bytes(), mode)?;
    }
    if m.live() {
        if m.kind() == "systemd" {
            command("systemctl", &["daemon-reload"], 30).await?;
            command("systemctl", &["enable", "--now", "chain-ingress.path"], 30).await?;
        } else {
            platform::enable(m, "chain-ingress").await?;
            platform::related(m, "restart", "chain-ingress").await?;
        }
    }
    Ok(())
}
async fn enable(m: &Manager) -> Result<()> {
    ensure!(m.live(), "离线安装不修改 Nginx 入口");
    if directory(m).join("state.json").exists() {
        ensure!(
            serde_json::from_slice::<Value>(&fs::read(directory(m).join("status.json"))?)?["ready"]
                == true,
            "统一入口状态异常"
        );
        return Ok(());
    }
    ensure!(
        m.state()?["roles"]["agent"].is_object(),
        "请先安装本机 Agent"
    );
    let main = nginx::main_path(m).canonicalize()?;
    let sources = nginx::files(&main)?;
    let mut snapshots = serde_json::Map::new();
    let mut blocked = std::collections::BTreeSet::new();
    let mut ipv6 = false;
    let mut ipv4 = false;
    let mut stream = None;
    for (path, text) in &sources {
        for d in nginx::parse(text)? {
            if ["real_ip_header", "set_real_ip_from"].contains(&d.words[0].as_str()) {
                anyhow::bail!("已有真实 IP 信任配置，请先手动整合");
            }
            if d.words[0] == "stream" && d.block {
                ensure!(stream.is_none(), "已有多个 stream 块");
                stream = Some((path.clone(), d.end));
            }
            if d.words[0] == "listen" {
                if d.words
                    .get(1)
                    .is_some_and(|s| s == "443" || s == "0.0.0.0:443" || s == "*:443")
                {
                    ipv4 = true;
                }
                if d.words.get(1).is_some_and(|s| s == "[::]:443") {
                    ipv6 = true;
                }
            }
        }
    }
    ensure!(ipv4, "统一入口需要保留已有 IPv4 HTTPS 站点");
    let port = (14443..15000)
        .find(|p| {
            std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()
                && (!ipv6 || std::net::TcpListener::bind(("::1", *p)).is_ok())
        })
        .context("没有可用内部端口")?;
    for (path, text) in &sources {
        let changed = migrate(text, port)?;
        if changed != *text {
            for d in nginx::parse(text)? {
                if d.words[0] == "server_name" {
                    for n in d.words.iter().skip(1).filter(|n| n.as_str() != "_") {
                        blocked.insert(n.to_ascii_lowercase());
                    }
                }
            }
            snapshots.insert(
                path.display().to_string(),
                json!({"before":text,"after":changed,"mode":fs::metadata(path)?.mode()&0o777}),
            );
        }
    }
    ensure!(!snapshots.is_empty(), "没有可迁移 HTTPS 监听");
    let mut text = snapshots
        .get(&main.display().to_string())
        .and_then(|v| v["after"].as_str())
        .unwrap_or(&sources[&main])
        .to_owned();
    let http = nginx::parse(&text)?
        .into_iter()
        .find(|d| d.words[0] == "http" && d.block)
        .context("缺少 http 块")?;
    text.insert_str(
        http.end,
        "\nreal_ip_header proxy_protocol;\nset_real_ip_from 127.0.0.1;\nset_real_ip_from ::1;\n",
    );
    let build = output_nginx().await?;
    if !sources.values().any(|s| s.contains("ngx_stream_module.so"))
        && !build.split_whitespace().any(|s| s == "--with-stream")
    {
        let module = build
            .split_whitespace()
            .find_map(|s| s.strip_prefix("--modules-path="))
            .map(|s| Path::new(s).join("ngx_stream_module.so"))
            .unwrap_or_else(|| Path::new("/usr/lib/nginx/modules/ngx_stream_module.so").into());
        ensure!(module.is_file(), "请先安装 Nginx stream 模块");
        text = format!(
            "load_module {};\n{text}",
            nginx::quote(module.to_str().unwrap())
        );
    }
    let config = config(m);
    ordinary(&config)?;
    ensure!(!config.exists(), "统一入口配置目录已存在，未覆盖");
    if let Some((path, _)) = &stream {
        let original = &sources[path];
        let directives = nginx::parse(original)?;
        ensure!(
            !directives
                .iter()
                .any(|d| d.depth.iter().any(|s| s == "stream")
                    && ["server", "map", "upstream"].contains(&d.words[0].as_str())),
            "已有业务 stream 配置，不能自动接管"
        );
        let body = if path == &main { &text } else { original };
        let d = nginx::parse(body)?
            .into_iter()
            .find(|d| d.words[0] == "stream" && d.block)
            .unwrap();
        let mut changed = body.to_owned();
        changed.insert_str(
            d.end,
            &format!(
                "\ninclude {};\n",
                nginx::quote(config.join("stream.conf").to_str().unwrap())
            ),
        );
        if path == &main {
            text = changed;
        } else {
            snapshots.insert(
                path.display().to_string(),
                json!({"before":original,"after":changed,"mode":fs::metadata(path)?.mode()&0o777}),
            );
        }
    } else {
        text.push_str(&format!(
            "\n{OWNER}\ninclude {};\n",
            nginx::quote(config.join("stream.conf").to_str().unwrap())
        ));
    }
    snapshots.insert(
        main.display().to_string(),
        json!({"before":sources[&main],"after":text,"mode":fs::metadata(&main)?.mode()&0o777}),
    );
    fs::create_dir_all(&config)?;
    let inner = format!(
        "map $ssl_preread_server_name $chain_backend {{ hostnames; default 127.0.0.1:{port}; include {}; }}\nserver {{ listen 443; {} ssl_preread on; proxy_pass $chain_backend; proxy_protocol on; proxy_connect_timeout 10s; proxy_timeout 1h; }}\n",
        nginx::quote(config.join("routes.conf").to_str().unwrap()),
        if ipv6 { "listen [::]:443;" } else { "" }
    );
    atomic(
        &config.join("stream.conf"),
        format!(
            "{OWNER}\n{}",
            if stream.is_none() {
                format!("stream {{ {inner} }}\n")
            } else {
                inner
            }
        )
        .as_bytes(),
        0o644,
    )?;
    atomic(
        &config.join("routes.conf"),
        format!("{OWNER}\n").as_bytes(),
        0o644,
    )?;
    let state = json!({"files":snapshots,"blocked":blocked,"port":port});
    atomic(
        &directory(m).join("state.json"),
        &serde_json::to_vec(&state)?,
        0o600,
    )?;
    let mut written = vec![];
    let result: Result<()> = async {
        for (path, v) in state["files"].as_object().unwrap() {
            write_existing(
                Path::new(path),
                v["before"].as_str().unwrap(),
                v["after"].as_str().unwrap(),
            )?;
            written.push(path.clone());
        }
        nginx::reload(m).await?;
        units(m).await?;
        status(m, json!({"ready":true,"applied":true,"id":"","error":""}))?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        for path in written.iter().rev() {
            let v = &state["files"][path];
            write_existing(
                Path::new(path),
                v["after"].as_str().unwrap(),
                v["before"].as_str().unwrap(),
            )?;
        }
        let _ = nginx::reload(m).await;
        for name in ["routes.conf", "stream.conf"] {
            fs::remove_file(config.join(name))?;
        }
        fs::remove_dir(config)?;
        fs::remove_file(directory(m).join("state.json"))?;
        status(m, json!({"ready":false,"error":"启用失败，已恢复原配置"}))?;
        return Err(e);
    }
    Ok(())
}
async fn output_nginx() -> Result<String> {
    let o = super::upgrade::output("nginx", &["-V"], 10).await?;
    ensure!(o.status.success(), "无法获取 Nginx 构建信息");
    Ok(String::from_utf8(o.stderr)?)
}
async fn apply(m: &Manager) -> Result<()> {
    let s = state(m)?;
    let mut id = String::new();
    let result: Result<()> = async {
        let path = m.path("/var/lib/chain-agent/ingress-request.json");
        ordinary(&path)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let meta = file.metadata()?;
        let user = unsafe { libc::getpwnam(c"chain-agent".as_ptr()) };
        ensure!(
            !user.is_null()
                && meta.uid() == unsafe { (*user).pw_uid }
                && meta.is_file()
                && meta.len() <= 65536,
            "路由请求归属或大小无效"
        );
        let request: Value = serde_json::from_reader(file)?;
        id = uuid::Uuid::parse_str(request["id"].as_str().context("请求缺少 ID")?)?.to_string();
        let text = routes(&request["routes"], &s["blocked"])?;
        let target = config(m).join("routes.conf");
        let old = fs::read_to_string(&target)?;
        write_existing(&target, &old, &text)?;
        if let Err(e) = nginx::reload(m).await {
            write_existing(&target, &text, &old)?;
            let _ = nginx::reload(m).await;
            return Err(e);
        }
        Ok(())
    }
    .await;
    status(
        m,
        json!({"ready":true,"applied":result.is_ok(),"id":id,"error":result.as_ref().err().map(|e|format!("{e:#}")).unwrap_or_default()}),
    )?;
    result
}
pub fn refresh_panel_site(m: &Manager, path: &Path, before: &str, after: &str) -> Result<()> {
    let state_path = directory(m).join("state.json");
    if !state_path.exists() {
        return Ok(());
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory(m).join("process.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "入口配置忙，请重试"
    );
    let mut s = state(m)?;
    let key = path.canonicalize()?.to_string_lossy().into_owned();
    s["files"][&key] =
        json!({"before":before,"after":after,"mode":fs::metadata(path)?.mode()&0o777});
    let mut blocked = std::collections::BTreeSet::new();
    for v in s["files"].as_object().context("入口快照无效")?.values() {
        for d in nginx::parse(v["before"].as_str().context("入口快照缺少原文")?)? {
            if d.words[0] == "server_name" {
                blocked.extend(d.words.into_iter().skip(1).filter(|s| s != "_"));
            }
        }
    }
    s["blocked"] = json!(blocked);
    atomic(&state_path, &serde_json::to_vec(&s)?, 0o600)
}
async fn disable(m: &Manager) -> Result<()> {
    if !directory(m).join("state.json").exists() {
        return Ok(());
    }
    let s = state(m)?;
    let files = s["files"].as_object().context("入口快照无效")?;
    for (path, v) in files {
        ensure!(
            fs::read_to_string(path)? == v["after"].as_str().context("入口快照无效")?,
            "配置已被修改，未恢复旧快照：{path}"
        );
    }
    let mut written = vec![];
    let result: Result<()> = async {
        for (path, v) in files {
            write_existing(
                Path::new(path),
                v["after"].as_str().unwrap(),
                v["before"].as_str().unwrap(),
            )?;
            written.push(path);
        }
        nginx::reload(m).await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        for path in written.into_iter().rev() {
            let v = &files[path];
            write_existing(
                Path::new(path),
                v["before"].as_str().unwrap(),
                v["after"].as_str().unwrap(),
            )?;
        }
        let _ = nginx::reload(m).await;
        return Err(e);
    }
    if m.live() {
        if m.kind() == "systemd" {
            command("systemctl", &["disable", "--now", "chain-ingress.path"], 30).await?;
        } else {
            platform::related(m, "stop", "chain-ingress").await?;
            platform::related(m, "disable", "chain-ingress").await?;
        }
    }
    for name in ["routes.conf", "stream.conf"] {
        fs::remove_file(config(m).join(name))?;
    }
    fs::remove_dir(config(m))?;
    fs::remove_file(directory(m).join("state.json"))?;
    status(m, json!({"ready":false,"applied":true,"id":"","error":""}))?;
    Ok(())
}
pub async fn watch(m: &Manager) -> Result<()> {
    let mut previous = String::new();
    loop {
        let p = m.path("/var/lib/chain-agent/ingress-request.json");
        if p.exists() {
            let digest = super::package::digest(&p)?;
            if digest != previous {
                let _ = operate(m, "apply").await;
                previous = digest;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listeners_and_conflicts() {
        let s = "server { listen 443 ssl; listen [::]:443 ssl; listen 443 quic; }";
        let v = migrate(s, 14443).unwrap();
        assert!(v.contains("listen 127.0.0.1:14443 ssl proxy_protocol"));
        assert!(v.contains("listen 443 quic;"));
        assert!(migrate("server { listen 192.0.2.1:443 ssl; }", 14443).is_err());
        assert!(
            routes(
                &json!([{"sni":"a.example.com","port":30001}]),
                &json!(["*.example.com"])
            )
            .is_err()
        );
        assert!(
            routes(
                &json!([{"sni":"other.example","port":30001}]),
                &json!(["panel.example"])
            )
            .is_ok()
        );
    }
}
