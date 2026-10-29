use super::{
    nginx, platform,
    upgrade::{Manager, atomic, command, ordinary, output},
};
use anyhow::{Context, Result, ensure};
use std::{fs, time::Duration};
pub const OWNER: &str = "# chain-panel HTTPS managed site v1";
pub async fn packages(names: &[&str]) -> Result<()> {
    let missing = names
        .iter()
        .copied()
        .filter(|s| platform::executable(s).is_none())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let user = std::env::var("SUDO_USER").context("macOS 请从普通管理员账户 sudo 安装")?;
        ensure!(user != "root", "Homebrew 需要普通管理员账户");
        let brew = platform::executable("brew").context("请先安装 Homebrew")?;
        let mut args = vec!["-H", "-u", &user, brew.to_str().unwrap(), "install"];
        args.extend(missing);
        command("sudo", &args, 300).await?;
    } else if platform::executable("apt-get").is_some() {
        command("apt-get", &["update"], 180).await?;
        let mut args = vec!["install", "-y"];
        args.extend(missing);
        command("apt-get", &args, 300).await?;
    } else {
        let (program, mut args) = if platform::executable("apk").is_some() {
            ("apk", vec!["add", "--no-cache"])
        } else if platform::executable("dnf").is_some() {
            ("dnf", vec!["install", "-y"])
        } else if platform::executable("yum").is_some() {
            ("yum", vec!["install", "-y"])
        } else if platform::executable("pacman").is_some() {
            ("pacman", vec!["-S", "--needed", "--noconfirm"])
        } else if platform::executable("zypper").is_some() {
            ("zypper", vec!["--non-interactive", "install"])
        } else {
            anyhow::bail!("请先安装所需组件：{}", missing.join(", "))
        };
        args.extend(missing);
        command(program, &args, 300).await?;
    }
    Ok(())
}
pub async fn public_ipv4() -> Result<String> {
    if cfg!(target_os = "linux") {
        let out = output(
            "ip",
            &["-j", "-4", "address", "show", "scope", "global"],
            10,
        )
        .await?;
        ensure!(out.status.success(), "无法读取本机地址");
        let links: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        for link in links.as_array().context("接口结果无效")? {
            if let Some(addrs) = link["addr_info"].as_array() {
                for addr in addrs {
                    if let Some(ip) = addr["local"].as_str() {
                        if ip.parse().is_ok_and(chain_panel::rules::public_ip) {
                            return Ok(ip.into());
                        }
                    }
                }
            }
        }
    } else {
        let out = output("ifconfig", &[], 10).await?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let words = line.split_whitespace().collect::<Vec<_>>();
            if words.first() == Some(&"inet") {
                if let Some(ip) = words.get(1) {
                    if ip.parse().is_ok_and(chain_panel::rules::public_ip) {
                        return Ok((*ip).into());
                    }
                }
            }
        }
    }
    anyhow::bail!("未找到网卡公网 IPv4，请明确填写 --local-host")
}
pub fn proxy_acme_config(manager: &Manager) -> String {
    format!(
        "{OWNER}\nserver {{\n listen 80;\n server_name ~^.+$;\n location ^~ /.well-known/acme-challenge/ {{ root {}; default_type text/plain; try_files /agent$uri /panel$uri =404; }}\n location / {{ return 404; }}\n}}\n",
        nginx::quote(manager.path("/var/lib/chain-acme").to_str().unwrap())
    )
}
pub async fn proxy_acme(manager: &Manager) -> Result<()> {
    if !manager.live() {
        return Ok(());
    }
    packages(&["nginx"]).await?;
    let main = nginx::main_path(manager);
    let directory = nginx::site_directory(&main)?;
    let path = directory.join("chain-proxy-acme.conf");
    ordinary(&path)?;
    let old = fs::read_to_string(&path).ok();
    nginx::replace_owned(&path, old.as_deref(), &proxy_acme_config(manager), OWNER)?;
    if let Err(e) = nginx::reload(manager).await {
        if let Some(old) = old {
            nginx::replace_owned(&path, None, &old, OWNER)?;
        } else {
            fs::remove_file(&path)?;
        }
        return Err(e);
    }
    Ok(())
}
pub fn site(manager: &Manager, domain: &str, port: u16, tls: bool) -> String {
    let cert = manager.path(&format!("/etc/letsencrypt/live/chain-panel-{domain}"));
    let root = manager.path("/var/lib/chain-acme");
    let mut text = format!(
        "{OWNER}\nserver {{\n listen 80;\n server_name {domain};\n location ^~ /.well-known/acme-challenge/ {{ root {}; default_type text/plain; try_files $uri /agent$uri /panel$uri =404; }}\n location / {{ {} }}\n}}\n",
        nginx::quote(root.to_str().unwrap()),
        if tls {
            format!("return 301 https://{domain}$request_uri;")
        } else {
            "return 404;".into()
        }
    );
    if tls {
        text.push_str(&format!("server {{\n listen 443 ssl;\n server_name {domain};\n ssl_certificate {};\n ssl_certificate_key {};\n ssl_protocols TLSv1.2 TLSv1.3;\n client_max_body_size 2m;\n location / {{ proxy_pass http://127.0.0.1:{port}; proxy_http_version 1.1; proxy_set_header Host $http_host; proxy_set_header X-Real-IP $remote_addr; proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for; proxy_set_header X-Forwarded-Proto https; proxy_read_timeout 120s; }}\n}}\n",nginx::quote(cert.join("fullchain.pem").to_str().unwrap()),nginx::quote(cert.join("privkey.pem").to_str().unwrap())));
    }
    text
}
async fn existing_certificate(manager: &Manager, domain: &str) -> Result<bool> {
    let dir = manager.path(&format!("/etc/letsencrypt/live/chain-panel-{domain}"));
    let cert = dir.join("fullchain.pem");
    let key = dir.join("privkey.pem");
    certificate(&cert, &key, domain).await
}
async fn certificate(cert: &std::path::Path, key: &std::path::Path, domain: &str) -> Result<bool> {
    if !cert.exists() && !key.exists() {
        return Ok(false);
    }
    ensure!(
        cert.is_file() && key.is_file(),
        "已有证书或私钥缺失，拒绝重新申请覆盖"
    );
    let cert_bytes = fs::read(&cert)?;
    let key_bytes = fs::read(&key)?;
    let chain = openssl::x509::X509::stack_from_pem(&cert_bytes)?;
    let leaf = chain.first().context("证书链为空")?;
    let private = openssl::pkey::PKey::private_key_from_pem(&key_bytes)?;
    ensure!(
        leaf.public_key()?.public_eq(&private),
        "已有证书与私钥不匹配"
    );
    let expiry = output(
        "openssl",
        &[
            "x509",
            "-in",
            cert.to_str().unwrap(),
            "-noout",
            "-checkend",
            "0",
        ],
        15,
    )
    .await?;
    if !expiry.status.success() {
        return Ok(false);
    }
    command(
        "openssl",
        &[
            "verify",
            "-trusted",
            cert.to_str().unwrap(),
            "-partial_chain",
            "-verify_hostname",
            domain,
            cert.to_str().unwrap(),
        ],
        15,
    )
    .await?;
    Ok(true)
}
pub async fn activate(
    manager: &Manager,
    name: &str,
    port: u16,
    host: &str,
) -> Result<Option<String>> {
    let domain = nginx::domain(name)?;
    ensure!(manager.live(), "离线安装树不申请证书或操作 Nginx");
    let expected: std::net::IpAddr = host
        .parse()
        .context("HTTPS DNS 校验需要 --local-host 指定本机公网 IP")?;
    let addresses = tokio::net::lookup_host((domain.as_str(), 443))
        .await?
        .map(|v| v.ip())
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        !addresses.is_empty() && addresses.contains(&expected),
        "域名没有解析到本机地址"
    );
    for ip in &addresses {
        ensure!(
            *ip == expected
                || std::net::TcpListener::bind(std::net::SocketAddr::new(*ip, 0)).is_ok(),
            "域名包含非本机地址 {ip}，请修正 A/AAAA"
        );
    }
    packages(&["nginx", "certbot", "openssl"]).await?;
    let main = nginx::main_path(manager);
    let files = nginx::files(&main)?;
    let directory = nginx::site_directory(&main)?;
    let path = directory.join("chain-panel.conf");
    ordinary(&path)?;
    for (file, text) in &files {
        if file == &path {
            continue;
        }
        for d in nginx::parse(text)? {
            if d.words.first().is_some_and(|s| s == "server_name") {
                ensure!(
                    !d.words.iter().skip(1).any(|n| n == &domain
                        || n.starts_with('~')
                        || n.starts_with('*') && domain.ends_with(n.trim_start_matches('*'))),
                    "域名与已有站点冲突：{}",
                    file.display()
                );
            }
        }
    }
    let previous = if path.exists() {
        let text = fs::read_to_string(&path)?;
        ensure!(text.starts_with(OWNER), "面板站点不归本项目管理");
        Some(text)
    } else {
        None
    };
    let webroot = manager.path("/var/lib/chain-acme");
    ordinary(&webroot)?;
    fs::create_dir_all(webroot.join(".well-known/acme-challenge"))?;
    let reusable = existing_certificate(manager, &domain).await?;
    let result: Result<()> = async {
        if !reusable {
            nginx::replace_owned(
                &path,
                previous.as_deref(),
                &site(manager, &domain, port, false),
                OWNER,
            )?;
            nginx::reload(manager).await?;
            let marker = uuid::Uuid::new_v4().to_string();
            let challenge = webroot.join(".well-known/acme-challenge").join(&marker);
            fs::write(&challenge, &marker)?;
            let response = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()?
                .get(format!(
                    "http://{domain}/.well-known/acme-challenge/{marker}"
                ))
                .send()
                .await;
            let checked: Result<String> =
                async { Ok(response?.error_for_status()?.text().await?) }.await;
            fs::remove_file(challenge)?;
            let checked = checked?;
            ensure!(checked == marker, "ACME HTTP 路径验证失败");
            command(
                "certbot",
                &[
                    "certonly",
                    "--webroot",
                    "-w",
                    webroot.to_str().unwrap(),
                    "--cert-name",
                    &format!("chain-panel-{domain}"),
                    "-d",
                    &domain,
                    "--non-interactive",
                    "--agree-tos",
                    "--register-unsafely-without-email",
                    "--keep-until-expiring",
                ],
                180,
            )
            .await?;
        }
        let original_site = site(manager, &domain, port, true);
        let mut final_site = original_site.clone();
        let ingress = manager.path("/var/lib/chain-ingress/state.json");
        if ingress.exists() {
            let state: serde_json::Value = serde_json::from_slice(&fs::read(ingress)?)?;
            final_site = super::ingress::migrate(
                &final_site,
                state["port"].as_u64().context("统一入口端口无效")? as u16,
            )?;
        }
        nginx::replace_owned(&path, None, &final_site, OWNER)?;
        nginx::reload(manager).await?;
        super::ingress::refresh_panel_site(manager, &path, &original_site, &final_site)?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        if let Some(body) = previous {
            nginx::replace_owned(&path, None, &body, OWNER)?;
        } else if path.exists() {
            fs::remove_file(&path)?;
        }
        let restored = nginx::reload(manager).await;
        return Err(e.context(format!(
            "HTTPS 未完成，站点文件已恢复；Nginx 恢复结果：{restored:?}"
        )));
    }
    Ok(previous)
}
pub async fn renewal(manager: &Manager, domain: &str) -> Result<()> {
    let hook = manager.path("/etc/letsencrypt/renewal-hooks/deploy/chain-panel-nginx");
    ordinary(&hook)?;
    if hook.exists() {
        ensure!(
            fs::read_to_string(&hook)?.contains(OWNER),
            "续期 hook 归属不匹配"
        );
    }
    fs::create_dir_all(hook.parent().unwrap())?;
    let ctl = manager.path("/usr/local/bin/chainctl");
    let body = format!(
        "#!/bin/sh\n{OWNER}\nset -eu\n[ \"${{RENEWED_LINEAGE:-}}\" = {} ] || exit 0\nexec {} internal-nginx-reload\n",
        platform::shell(
            manager
                .path(&format!("/etc/letsencrypt/live/chain-panel-{domain}"))
                .to_str()
                .unwrap()
        ),
        platform::shell(ctl.to_str().unwrap())
    );
    atomic(&hook, body.as_bytes(), 0o755)?;
    if manager.kind() == "systemd" {
        let service = manager.path("/etc/systemd/system/chain-panel-cert-renew.service");
        let timer = manager.path("/etc/systemd/system/chain-panel-cert-renew.timer");
        for p in [&service, &timer] {
            ordinary(p)?;
            if p.exists() {
                ensure!(fs::read_to_string(p)?.contains(OWNER), "续期服务归属不匹配");
            }
        }
        atomic(&service,format!("{OWNER}\n[Unit]\nDescription=Chain certificate renewal\n[Service]\nType=oneshot\nExecStart={} renew --cert-name chain-panel-{domain} --quiet\n",platform::executable("certbot").context("缺少 Certbot")?.display()).as_bytes(),0o644)?;
        atomic(&timer,format!("{OWNER}\n[Unit]\nDescription=Chain certificate renewal timer\n[Timer]\nOnCalendar=*-*-* 03,15:00:00\nRandomizedDelaySec=3600\nPersistent=true\n[Install]\nWantedBy=timers.target\n").as_bytes(),0o644)?;
        command("systemctl", &["daemon-reload"], 30).await?;
        command(
            "systemctl",
            &["enable", "--now", "chain-panel-cert-renew.timer"],
            30,
        )
        .await?;
    } else {
        let path = if manager.kind() == "openrc" {
            manager.path("/etc/init.d/chain-panel-cert-renew")
        } else {
            manager.path("/Library/LaunchDaemons/org.vless-chain.chain-panel-cert-renew.plist")
        };
        ordinary(&path)?;
        if path.exists() {
            let text = fs::read_to_string(&path)?;
            ensure!(
                text.contains(OWNER) || text.contains(super::upgrade::OWNER),
                "续期服务归属不匹配"
            );
        }
        let text = platform::definition(
            manager,
            "chain-panel-cert-renew",
            &[
                ctl.to_string_lossy().into(),
                "internal-cert-renew".into(),
                "--domain".into(),
                domain.into(),
            ],
            None,
            &Default::default(),
        )?;
        atomic(
            &path,
            text.as_bytes(),
            if manager.kind() == "openrc" {
                0o755
            } else {
                0o644
            },
        )?;
        platform::enable(manager, "chain-panel-cert-renew").await?;
        platform::related(manager, "restart", "chain-panel-cert-renew").await?;
    }
    Ok(())
}

pub async fn manual(
    manager: &Manager,
    domain: &str,
    upstream: u16,
    settings: &serde_json::Value,
) -> Result<()> {
    use std::{
        ffi::{CStr, CString},
        os::unix::fs::MetadataExt,
        path::Path,
    };
    ensure!(manager.live(), "离线安装不配置 HTTPS 服务");
    let domain = nginx::domain(domain)?;
    let port = settings["port"]
        .as_u64()
        .filter(|p| (1..=65535).contains(p))
        .context("HTTPS 端口无效")?;
    let cert = Path::new(settings["cert"].as_str().context("缺少证书路径")?);
    let key = Path::new(settings["key"].as_str().context("缺少私钥路径")?);
    ensure!(cert.is_absolute() && key.is_absolute(), "证书需要绝对路径");
    packages(&["nginx", "openssl"]).await?;
    ensure!(certificate(cert, key, &domain).await?, "证书已过期或不可用");
    let name = CString::new("nobody")?;
    let user = unsafe { libc::getpwnam(name.as_ptr()) };
    ensure!(!user.is_null(), "无法确认独立 HTTPS 工作账户");
    let (uid, gid) = unsafe { ((*user).pw_uid, (*user).pw_gid) };
    ensure!(uid != 0 && gid != 0, "HTTPS 工作账户必须非特权");
    let group = unsafe { libc::getgrgid(gid) };
    ensure!(!group.is_null(), "HTTPS 工作组不存在");
    let group = unsafe { CStr::from_ptr((*group).gr_name) }
        .to_str()?
        .to_owned();
    let runtime = manager.path("/var/lib/chain-panel-https");
    ordinary(&runtime)?;
    if runtime.exists() {
        ensure!(
            fs::read_to_string(runtime.join(".chain-owner"))?.trim() == OWNER,
            "HTTPS 临时目录归属不符"
        );
    } else {
        super::install::owned_dir(&runtime, 0, 0, 0o755)?;
        atomic(&runtime.join(".chain-owner"), OWNER.as_bytes(), 0o600)?;
    }
    let mut temporary = String::new();
    for name in ["client_body", "proxy", "fastcgi", "uwsgi", "scgi"] {
        let path = runtime.join(name);
        super::install::owned_dir(&path, uid, gid, 0o700)?;
        temporary.push_str(&format!(
            "{name}_temp_path {};\n",
            nginx::quote(path.to_str().unwrap())
        ));
    }
    let config = manager.path("/etc/chain-panel-https/nginx.conf");
    let unit = if manager.kind() == "systemd" {
        manager.path("/etc/systemd/system/chain-panel-https.service")
    } else if manager.kind() == "openrc" {
        manager.path("/etc/init.d/chain-panel-https")
    } else {
        manager.path("/Library/LaunchDaemons/org.vless-chain.chain-panel-https.plist")
    };
    let mut previous = vec![];
    for path in [&config, &unit] {
        ordinary(path)?;
        previous.push(if path.exists() {
            let text = fs::read_to_string(path)?;
            ensure!(
                text.contains(OWNER) || text.contains(super::upgrade::OWNER),
                "独立 HTTPS 配置归属不符"
            );
            Some((text, fs::metadata(path)?.mode() & 0o777))
        } else {
            None
        });
    }
    let body = format!(
        "{OWNER}\nuser nobody {};\npid {}/nginx.pid;\nerror_log {}/error.log;\nlock_file {}/nginx.lock;\nevents {{}}\nhttp {{ access_log off;\n{temporary}server {{ listen {port} ssl; server_name {domain};\nssl_certificate {}; ssl_certificate_key {}; ssl_protocols TLSv1.2 TLSv1.3; client_max_body_size 2m;\nlocation / {{ proxy_pass http://127.0.0.1:{upstream}; proxy_http_version 1.1; proxy_set_header Host $http_host; proxy_set_header X-Forwarded-Proto https; proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for; proxy_read_timeout 120s; }} }} }}\n",
        nginx::quote(&group),
        runtime.display(),
        runtime.display(),
        runtime.display(),
        nginx::quote(cert.to_str().unwrap()),
        nginx::quote(key.to_str().unwrap())
    );
    fs::create_dir_all(config.parent().unwrap())?;
    let nginx = platform::executable("nginx").context("缺少 Nginx")?;
    let args = vec![
        nginx.display().to_string(),
        "-c".into(),
        config.display().to_string(),
        "-g".into(),
        "daemon off;".into(),
    ];
    let definition = if manager.kind() == "systemd" {
        format!(
            "{OWNER}\n[Unit]\nDescription=Chain Panel HTTPS\nAfter=network.target\n[Service]\nExecStart={}\nExecReload={} -c {} -s reload\nRestart=on-failure\n[Install]\nWantedBy=multi-user.target\n",
            args.iter()
                .map(|v| nginx::quote(v))
                .collect::<Vec<_>>()
                .join(" "),
            nginx.display(),
            config.display()
        )
    } else {
        platform::definition(
            manager,
            "chain-panel-https",
            &args,
            None,
            &Default::default(),
        )?
        .replace(&format!("# {}", super::upgrade::OWNER), OWNER)
    };
    let was_active = match manager.kind() {
        "systemd" => output(
            "systemctl",
            &["is-active", "--quiet", "chain-panel-https"],
            15,
        )
        .await?
        .status
        .success(),
        "openrc" => output("rc-service", &["chain-panel-https", "status"], 15)
            .await?
            .status
            .success(),
        _ => output(
            "launchctl",
            &["print", "system/org.vless-chain.chain-panel-https"],
            15,
        )
        .await?
        .status
        .success(),
    };
    if !was_active {
        ensure!(
            std::net::TcpListener::bind(("0.0.0.0", port as u16)).is_ok(),
            "HTTPS 端口已占用"
        );
    }
    let mut started = false;
    let result: Result<()> = async {
        atomic(&config, body.as_bytes(), 0o600)?;
        nginx::validate_config(manager, &config).await?;
        atomic(
            &unit,
            definition.as_bytes(),
            if manager.kind() == "openrc" {
                0o755
            } else {
                0o644
            },
        )?;
        platform::enable(manager, "chain-panel-https").await?;
        started = true;
        platform::related(manager, "restart", "chain-panel-https").await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        if started {
            platform::related(manager, "stop", "chain-panel-https").await?;
        }
        for (path, old) in [&config, &unit].into_iter().zip(previous) {
            if let Some((text, mode)) = old {
                atomic(path, text.as_bytes(), mode)?;
            } else if path.exists() {
                fs::remove_file(path)?;
            }
        }
        if manager.kind() == "systemd" {
            command("systemctl", &["daemon-reload"], 30).await?;
        }
        if was_active {
            platform::related(manager, "start", "chain-panel-https").await?;
        }
        return Err(error.context("独立 HTTPS 配置未完成，原配置已恢复"));
    }
    println!("独立 HTTPS 已启动；证书继续由原工具续期，续期后需重载 chain-panel-https。");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_site_is_parseable_and_keeps_acme() {
        let r = tempfile::tempdir().unwrap();
        let m = Manager {
            root: r.path().into(),
            base: r.path().join("opt/chain"),
        };
        let s = site(&m, "panel.example", 12345, true);
        assert!(nginx::parse(&s).is_ok());
        assert!(s.contains("try_files $uri /agent$uri /panel$uri =404"));
        assert!(s.contains("proxy_pass http://127.0.0.1:12345"));
    }
}
