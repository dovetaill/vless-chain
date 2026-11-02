//! Standalone installer regressions: every external command is a native fixture.
use super::super::{repository, strings, workspace::Workspace};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::{
        fs::{PermissionsExt, symlink},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
const UUID: &str = "0bb1bf54-d087-4931-8baf-a560e28de527";
const URI: &str = "vless://0bb1bf54-d087-4931-8baf-a560e28de527@landing.example.com:8443?security=tls&type=raw&flow=xtls-rprx-vision";

pub fn command_fixture() -> Result<Option<i32>> {
    let Some(root) = std::env::var_os("NATIVE_MANAGER_FIXTURE") else {
        return Ok(None);
    };
    let root = PathBuf::from(root);
    let executable = std::env::args().next().context("缺少命令")?;
    let name = Path::new(&executable)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        [
            "id",
            "flock",
            "systemctl",
            "rc-service",
            "rc-update",
            "xray",
            "ss",
            "curl",
            "getent",
            "dpkg",
            "sleep",
            "timeout",
            "nginx",
            "rpm",
            "journalctl",
            "apt-get",
            "dnf",
            "yum",
            "apk",
            "cert-obtain"
        ]
        .contains(&name),
        "未登记的模拟命令：{name}"
    );
    let mut event = vec![name.to_string()];
    event.extend(args.clone());
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("events.jsonl"))?;
    writeln!(log, "{}", serde_json::to_string(&event)?)?;
    let flag = |key: &str| std::env::var(key).is_ok_and(|v| !v.is_empty());
    let value = |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.into());
    let mut code = 0;
    match name {
        "flock" => {
            ensure!(args.len() == 2 && args[0] == "-n", "不支持的测试锁参数");
            let fd = args[1].parse::<i32>()?;
            code = if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                0
            } else {
                1
            };
        }
        "id" => {
            if args == ["-u"] {
                println!("0")
            } else {
                code = std::process::Command::new("/usr/bin/id")
                    .args(&args)
                    .status()?
                    .code()
                    .unwrap_or(1)
            }
        }
        "systemctl" | "rc-service" | "rc-update" => {
            let path = root.join("services.json");
            let mut state: Value = if path.exists() {
                serde_json::from_slice(&fs::read(&path)?)?
            } else {
                json!({})
            };
            let (action, service) = if name == "systemctl" {
                (
                    args.first().map(String::as_str).unwrap_or(""),
                    args.last().map(String::as_str).unwrap_or(""),
                )
            } else if name == "rc-service" {
                (
                    args.get(1).map(String::as_str).unwrap_or(""),
                    args.first().map(String::as_str).unwrap_or(""),
                )
            } else {
                (
                    match args.first().map(String::as_str) {
                        Some("add") => "enable",
                        Some("del") => "disable",
                        _ => "show",
                    },
                    args.get(1).map(String::as_str).unwrap_or(""),
                )
            };
            if action == "show" {
                for (service, status) in state.as_object().unwrap() {
                    if status["enabled"] == true {
                        println!("{service} | default")
                    }
                }
                return Ok(Some(0));
            }
            if action == "daemon-reload" {
                return Ok(Some(0));
            }
            if action == "cat" {
                let path =
                    PathBuf::from(std::env::var("SYSTEMD_DIR")?).join(format!("{service}.service"));
                return Ok(Some(match fs::read_to_string(path) {
                    Ok(s) => {
                        print!("{s}");
                        0
                    }
                    Err(_) => 1,
                }));
            }
            if state.get(service).is_none() {
                state[service] = json!({"active":false,"enabled":false});
            }
            let status = &mut state[service];
            match action {
                "is-active" | "status" => {
                    println!(
                        "{}",
                        if status["active"] == true {
                            "active"
                        } else {
                            "inactive"
                        }
                    );
                    code = if status["active"] == true { 0 } else { 3 };
                }
                "is-enabled" => code = if status["enabled"] == true { 0 } else { 1 },
                "start" | "restart" => {
                    let marker = root.join("restart-failed");
                    if action == "restart" && flag("TEST_FAIL_RESTART") && !marker.exists() {
                        fs::write(marker, "")?;
                        return Ok(Some(1));
                    }
                    status["active"] = json!(true);
                }
                "reload" => {
                    let marker = root.join("reload-failed");
                    if service == "nginx" && flag("TEST_FAIL_NGINX_RELOAD") && !marker.exists() {
                        fs::write(marker, "")?;
                        return Ok(Some(1));
                    }
                }
                "stop" => status["active"] = json!(false),
                "enable" => status["enabled"] = json!(true),
                "disable" => status["enabled"] = json!(false),
                _ => {}
            }
            fs::write(path, serde_json::to_vec(&state)?)?;
        }
        "xray" => match args.first().map(String::as_str) {
            Some("version") => println!("Xray mock"),
            Some("uuid") => println!("{}", uuid::Uuid::new_v4()),
            Some("x25519") => {
                use base64::Engine;
                use sha2::{Digest, Sha256};
                let private = args
                    .iter()
                    .position(|s| s == "-i")
                    .and_then(|i| args.get(i + 1))
                    .cloned()
                    .unwrap_or_else(|| {
                        base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .encode(rand::random::<[u8; 32]>())
                    });
                let public = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(Sha256::digest(private.as_bytes()));
                println!("PrivateKey: {private}\nPassword (PublicKey): {public}");
            }
            Some("run") => {
                let path = Path::new(
                    args.get(
                        args.iter()
                            .position(|s| s == "-config")
                            .context("缺少配置参数")?
                            + 1,
                    )
                    .context("缺少配置")?,
                );
                if !args.iter().any(|s| s == "-format")
                    && path.extension().and_then(|s| s.to_str()) != Some("json")
                {
                    return Ok(Some(23));
                }
                if flag("TEST_FAIL_CONFIG") {
                    println!("mock configuration rejected");
                    return Ok(Some(1));
                }
                let config: Value = serde_json::from_slice(&fs::read(path)?)?;
                let ids = config["inbounds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|i| i["settings"]["clients"].as_array().unwrap())
                    .map(|c| c["id"].as_str().unwrap())
                    .collect::<std::collections::BTreeSet<_>>();
                ensure!(ids.len() == 1, "入站 UUID 不一致");
            }
            _ => {}
        },
        "ss" => {
            println!("State Recv-Q Send-Q Local Address:Port Peer Address:Port");
            for port in value("TEST_LISTEN_PORTS", "")
                .split(',')
                .filter(|s| !s.is_empty())
            {
                println!("LISTEN 0 128 0.0.0.0:{port} 0.0.0.0:*");
            }
        }
        "timeout" | "rpm" => code = 1,
        "curl" => {
            if let Some(url) = args.iter().find(|s| s.starts_with("http://")) {
                let url = url::Url::parse(url)?;
                let local = args.iter().any(|s| s == "--resolve");
                let status = value(
                    if local {
                        "TEST_HTTP_LOCAL_STATUS"
                    } else {
                        "TEST_HTTP_PUBLIC_STATUS"
                    },
                    "200",
                );
                let mut bytes = if status == "200" {
                    fs::read(
                        PathBuf::from(std::env::var("MANAGER_WEB_ROOT")?)
                            .join("domains")
                            .join(url.host_str().context("缺少域名")?)
                            .join(url.path().trim_start_matches('/')),
                    )?
                } else {
                    b"not found".to_vec()
                };
                if flag("TEST_HTTP_WRONG_BODY") {
                    bytes = b"another site".to_vec();
                }
                if let Some(i) = args.iter().position(|s| s == "-o") {
                    fs::write(&args[i + 1], bytes)?;
                }
                print!("{status}");
                code = value("TEST_HTTP_CURL_EXIT", "0").parse()?;
            } else {
                anyhow::bail!("夹具禁止外网 curl");
            }
        }
        "journalctl" => println!("mock service log"),
        "cert-obtain" => {
            if flag("TEST_CERT_FAIL") {
                return Ok(Some(1));
            }
            let lineage = root.join("acme-lineage");
            fs::create_dir_all(&lineage)?;
            fs::write(lineage.join("fullchain.pem"), "fake certificate")?;
            fs::write(lineage.join("privkey.pem"), "fake key")?;
            println!("renewed\n{}", lineage.display());
        }
        _ => {}
    }
    Ok(Some(code))
}

struct Case<'a> {
    work: &'a Workspace,
    root: PathBuf,
    env: BTreeMap<String, String>,
}
impl<'a> Case<'a> {
    fn new(work: &'a Workspace, bin: &Path, name: &str) -> Result<Self> {
        let root = work.root.join(format!("ssh-{name}"));
        let mocks = root.join("bin");
        fs::create_dir_all(&mocks)?;
        // A concurrent Cargo build may replace the running executable's path.
        let fixture = root.join("command-fixture");
        fs::copy(std::env::current_exe()?, &fixture)
            .context("无法保存独立 SSH 命令夹具，拒绝回退到宿主机命令")?;
        for name in [
            "id",
            "flock",
            "systemctl",
            "rc-service",
            "rc-update",
            "xray",
            "ss",
            "curl",
            "getent",
            "dpkg",
            "sleep",
            "timeout",
            "nginx",
            "rpm",
            "journalctl",
            "apt-get",
            "dnf",
            "yum",
            "apk",
            "cert-obtain",
        ] {
            symlink(&fixture, mocks.join(name))?;
        }
        let helper = root.join("chainctl");
        fs::write(
            &helper,
            "#!/bin/sh\nif [ \"$1\" = internal-cert-obtain ]; then shift; exec \"$TEST_RUNTIME/bin/cert-obtain\" \"$@\"; fi\nexec \"$REAL_CHAINCTL\" \"$@\"\n",
        )?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
        let mut env = BTreeMap::from([
            ("NATIVE_MANAGER_FIXTURE".into(), root.display().to_string()),
            ("TEST_RUNTIME".into(), root.display().to_string()),
            (
                "MANAGER_DIR".into(),
                root.join("manager").display().to_string(),
            ),
            (
                "MANAGER_WEB_ROOT".into(),
                root.join("web").display().to_string(),
            ),
            (
                "SYSTEMD_DIR".into(),
                root.join("units").display().to_string(),
            ),
            (
                "NGINX_CONF_DIR".into(),
                root.join("nginx").display().to_string(),
            ),
            (
                "RENEW_HOOK_DIR".into(),
                root.join("hooks").display().to_string(),
            ),
            (
                "OPENRC_DIR".into(),
                root.join("init.d").display().to_string(),
            ),
            ("TMPDIR".into(), root.display().to_string()),
            ("XRAY_BIN".into(), mocks.join("xray").display().to_string()),
            ("CHAINCTL_BIN".into(), helper.display().to_string()),
            (
                "REAL_CHAINCTL".into(),
                bin.join("chainctl").canonicalize()?.display().to_string(),
            ),
            ("INIT_SYSTEM".into(), "systemd".into()),
            ("DOMAIN".into(), "edge.example.com".into()),
            ("SECURITY_MODE".into(), "reality".into()),
            ("REALITY_TARGET_MODE".into(), "remote".into()),
            ("REALITY_TARGET".into(), "www.example.com:443".into()),
            ("REALITY_TARGET_EXPLICIT".into(), "1".into()),
            ("CERT_MODE".into(), "none".into()),
        ]);
        env.insert(
            "PATH".into(),
            format!("{}:{}", mocks.display(), std::env::var("PATH")?),
        );
        Ok(Self { work, root, env })
    }
    async fn command(
        &self,
        args: &[String],
        extra: &Value,
        input: &str,
        success: bool,
    ) -> Result<String> {
        let log = self
            .root
            .join(format!("output-{}.log", uuid::Uuid::new_v4()));
        let output = fs::File::create(&log)?;
        let mut command = std::process::Command::new(&args[0]);
        command
            .args(&args[1..])
            .envs(&self.work.env)
            .envs(&self.env)
            .env_remove("UUID")
            .env_remove("INSTANCE_ID")
            .env_remove("INSTANCE_NAME")
            .env_remove("UPSTREAM_URI")
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(output.try_clone()?)
            .stderr(output)
            .process_group(0);
        for (key, value) in extra.as_object().context("夹具环境必须为对象")? {
            command.env(
                key,
                value
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| value.to_string()),
            );
        }
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        let pid = self.work.register(child)?;
        stdin.write_all(input.as_bytes())?;
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.work.poll(pid)? {
                let body = String::from_utf8_lossy(&fs::read(&log)?).into_owned();
                ensure!(
                    status.success() == success,
                    "SSH 验收退出不符：{status}\n{body}"
                );
                return Ok(body);
            }
            ensure!(
                Instant::now() < deadline,
                "SSH 验收超时：{}",
                args.join(" ")
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn cli(&self, args: &[&str], extra: Value, input: &str, success: bool) -> Result<String> {
        let mut command = strings(&[
            "bash",
            repository()
                .join("xray-vless-native-v3.sh")
                .to_str()
                .unwrap(),
        ]);
        command.extend(strings(args));
        self.command(&command, &extra, input, success).await
    }
    async fn shell(&self, code: &str, extra: Value, input: &str, success: bool) -> Result<String> {
        self.command(
            &strings(&[
                "bash",
                "-c",
                &format!(
                    "source {}; trap cleanup EXIT; {code}",
                    crate::ctl::platform::shell(
                        repository()
                            .join("xray-vless-native-v3.sh")
                            .to_str()
                            .unwrap()
                    )
                ),
            ]),
            &extra,
            input,
            success,
        )
        .await
    }
    async fn install(
        &self,
        name: &str,
        port: u16,
        mode: &str,
        mut extra: Value,
    ) -> Result<PathBuf> {
        extra["REALITY_PORT"] = json!(port.to_string());
        if mode == "relay" && extra.get("UPSTREAM_URI").is_none() {
            extra["UPSTREAM_URI"] = json!(URI);
        }
        self.cli(&["install", mode, name], extra, "", true).await?;
        Ok(self.root.join("manager/instances").join(name))
    }
    fn state(&self, app: &Path) -> Result<Value> {
        Ok(serde_json::from_slice(&fs::read(app.join("state.json"))?)?)
    }
    fn services(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&fs::read(
            self.root.join("services.json"),
        )?)?)
    }
    fn snapshot(&self, app: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut files = vec!["state.json", "config.json", "client.txt", "install.env"];
        if self.state(app)?["SECURITY_MODE"] != "tls" {
            files.push("reality.env");
        }
        files
            .into_iter()
            .map(|s| {
                let path = app.join(s);
                Ok((
                    s.into(),
                    fs::read(&path).with_context(|| path.display().to_string())?,
                ))
            })
            .collect()
    }
    fn events(&self) -> Result<Vec<Value>> {
        Ok(fs::read_to_string(self.root.join("events.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?)
    }
    fn existing_cert(&self, port: u16, security: &str) -> Result<Value> {
        let cert = self.root.join("cert.pem");
        let key = self.root.join("key.pem");
        fs::write(&cert, "fake certificate")?;
        fs::write(&key, "fake key")?;
        Ok(
            json!({"SECURITY_MODE":security,"TLS_PORT":port.to_string(),"CERT_MODE":"existing","CERT_FILE":cert.display().to_string(),"KEY_FILE":key.display().to_string()}),
        )
    }
}

pub async fn run(work: &Workspace, bin: &Path) -> Result<()> {
    parsing(work, bin).await.context("SSH 节点解析及工具准备")?;
    println!("PASS SSH 节点解析及工具准备");
    instances(work, bin).await.context("SSH 多租户及上游更新")?;
    println!("PASS SSH 多租户及上游更新");
    certificates(work, bin)
        .await
        .context("SSH 证书及验证路径")?;
    println!("PASS SSH 证书及验证路径");
    editing(work, bin).await.context("SSH 凭据、编辑及删除")?;
    println!("PASS SSH 凭据、编辑及删除");
    terminal(work, bin).await.context("SSH 交互中断")?;
    println!("PASS 独立 SSH 管理器解析、多租户隔离、凭据、端口、删除及证书失败回滚");
    Ok(())
}
async fn terminal(work: &Workspace, bin: &Path) -> Result<()> {
    let c = Case::new(work, bin, "terminal")?;
    let app = c
        .install("alpine", 20701, "relay", json!({"INIT_SYSTEM":"openrc"}))
        .await?;
    fs::write(app.join("logs/service.log"), "live-log-marker\n")?;
    let mut env = c.env.clone();
    env.insert("INIT_SYSTEM".into(), "openrc".into());
    let command = strings(&[
        "bash",
        repository()
            .join("xray-vless-native-v3.sh")
            .to_str()
            .unwrap(),
        "manage",
        "alpine",
    ]);
    let (code, body) = super::fixtures::terminal_command(
        work,
        &command,
        &c.root,
        &env,
        &[
            ("选择", b"3\n"),
            ("live-log-marker", b"\x03"),
            ("选择", b"0\n"),
        ],
    )
    .await?;
    ensure!(
        code == 0 && body.contains("live-log-marker"),
        "Ctrl+C 没有返回实例菜单：{body}"
    );
    ensure!(
        c.cli(&[], json!({}), "0\n", true)
            .await?
            .contains("VLESS 实例管理"),
        "主菜单缺失"
    );
    Ok(())
}
async fn parsing(work: &Workspace, bin: &Path) -> Result<()> {
    let c = Case::new(work, bin, "parse")?;
    let path = c.root.join("node.input");
    let parse = async |text: &str, success: bool| -> Result<Value> {
        fs::write(&path, text)?;
        let body = c
            .shell(
                &format!(
                    "native_helper parse {}",
                    crate::ctl::platform::shell(path.to_str().unwrap())
                ),
                json!({}),
                "",
                success,
            )
            .await?;
        if success {
            Ok(serde_json::from_str(&body)?)
        } else {
            Ok(json!(body))
        }
    };
    let extra = json!({"headers":{"X-Test":"a\"b"},"xmux":{"maxConcurrency":"16-32"}});
    let encoded =
        url::form_urlencoded::byte_serialize(extra.to_string().as_bytes()).collect::<String>();
    let node=parse(&format!("vless://{UUID}@[2001:db8::1]:443?security=tls&type=xhttp&path=%2Ftenant%20a&mode=packet-up&extra={encoded}#%E8%90%BD%E5%9C%B0"),true).await?;
    ensure!(
        node["UPSTREAM_ADDRESS"] == "2001:db8::1"
            && node["UPSTREAM_XHTTP_PATH"] == "/tenant a"
            && node["UPSTREAM_NAME"] == "落地",
        "URI 转义错误"
    );
    let outbound: Value =
        serde_json::from_str(&c.shell("native_helper outbound", node, "", true).await?)?;
    ensure!(
        outbound["settings"].get("flow").is_none()
            && outbound["streamSettings"]["xhttpSettings"]["extra"] == extra,
        "XHTTP 参数丢失"
    );
    let node=parse(&format!("{{name: 'reality xhttp-Beta', type: vless, server: landing.example.com, port: 443, uuid: '{UUID}', tls: true, network: xhttp, servername: www.example.com, reality-opts: {{public-key: '{}', short-id: abcd}}, xhttp-opts: {{path: /tenant, mode: auto, reuse-settings: {{max-concurrency: '16-32'}}}}}}","B".repeat(43)),true).await?;
    ensure!(
        node["UPSTREAM_SECURITY"] == "reality"
            && node["UPSTREAM_TRANSPORT"] == "xhttp"
            && serde_json::from_str::<Value>(node["UPSTREAM_XHTTP_EXTRA"].as_str().unwrap())?["xmux"]
                ["maxConcurrency"]
                == "16-32",
        "YAML 参数丢失"
    );
    ensure!(
        parse(
            &json!({"type":"vless","server":"example.com","port":443,"uuid":UUID}).to_string(),
            true
        )
        .await?["UPSTREAM_UUID"]
            == UUID,
        "JSON 节点解析错误"
    );
    let node = parse(
        "{name: 'reality xhttp-Beta', server: landing.example.com}",
        true,
    )
    .await?;
    for key in [
        "UPSTREAM_PORT",
        "UPSTREAM_UUID",
        "UPSTREAM_SECURITY",
        "UPSTREAM_TRANSPORT",
    ] {
        ensure!(node[key] == "", "从名称猜测字段 {key}");
    }
    c.shell(
        "INTERACTIVE=1; complete_upstream; native_helper validate",
        node,
        &format!(
            "443\n{UUID}\nxhttp\nreality\nwww.example.com\n{}\nabcd\n/tenant\n",
            "B".repeat(43)
        ),
        true,
    )
    .await?;
    for text in [
        URI.replace("type=raw", "type=ws"),
        format!(
            "!!python/object/apply:os.system ['touch {}']",
            c.root.join("executed").display()
        ),
        "proxies: [{server: one}, {server: two}]".into(),
    ] {
        parse(&text, false).await?;
    }
    ensure!(!c.root.join("executed").exists(), "YAML 执行了代码");
    let node = parse(&(URI.replace("type=raw", "type=xhttp") + "&path=%2F"), true).await?;
    c.shell("native_helper validate", node, "", false).await?;
    let manual = json!({"UPSTREAM_ADDRESS":"landing.example.com","UPSTREAM_PORT":"443","UPSTREAM_UUID":UUID,"UPSTREAM_TRANSPORT":"xhttp","UPSTREAM_SECURITY":"tls","UPSTREAM_XHTTP_PATH":"/tenant"});
    let outbound: Value =
        serde_json::from_str(&c.shell("native_helper outbound", manual, "", true).await?)?;
    ensure!(
        outbound["streamSettings"]["xhttpSettings"]["extra"] == json!({}),
        "手动 XHTTP 默认值错误"
    );
    let ports=c.shell("SECURITY_MODE=tls; TLS_PORT=18080; INTERNAL_HTTP_PORT=18080; INTERNAL_HTTPS_PORT=18080; choose_internal_ports; printf '%s %s' \"$INTERNAL_HTTP_PORT\" \"$INTERNAL_HTTPS_PORT\"",json!({}),"",true).await?;
    ensure!(ports == "18081 18082", "内部端口冲突：{ports}");
    for (manager, command) in [
        ("apt", vec!["apt-get", "install", "-y", "example-package"]),
        ("dnf", vec!["dnf", "install", "-y", "example-package"]),
        ("yum", vec!["yum", "install", "-y", "example-package"]),
        ("apk", vec!["apk", "add", "--no-cache", "example-package"]),
    ] {
        c.shell(
            &format!("PACKAGE_MANAGER={manager}; package_install example-package"),
            json!({}),
            "",
            true,
        )
        .await?;
        ensure!(c.events()?.contains(&json!(command)), "包管理器适配丢失");
    }
    for (binary, manager) in [
        ("apt-get", "apt"),
        ("dnf", "dnf"),
        ("yum", "yum"),
        ("apk", "apk"),
    ] {
        let result=c.shell(&format!("need_cmd() {{ [[ \"$1\" == {binary} || \"$1\" == rc-service || \"$1\" == rc-update ]]; }}; INIT_SYSTEM=openrc; detect_platform; printf '%s' \"$PACKAGE_MANAGER\""),json!({}),"",true).await?;
        ensure!(result == manager, "包管理器检测错误");
    }
    let before = c.events()?.len();
    c.shell("detect_platform() { PACKAGE_MANAGER=dnf; }; repair_dpkg_state() { :; }; install_base_tools",json!({}),"",true).await?;
    ensure!(
        c.events()?[before..]
            .iter()
            .all(|event| !event.as_array().unwrap().iter().any(|v| [
                "python3",
                "python3-pyyaml",
                "curl",
                "coreutils"
            ]
            .iter()
            .any(|s| v == s))),
        "重装已有工具或引入 Python"
    );
    Ok(())
}
async fn instances(work: &Workspace, bin: &Path) -> Result<()> {
    let c = Case::new(work, bin, "instances")?;
    let landing = c.install("landing", 20001, "direct", json!({})).await?;
    let first = c.install("tenantA", 20002, "relay", json!({})).await?;
    let second = c
        .install(
            "tenantB",
            20003,
            "relay",
            json!({"UPSTREAM_URI":URI.replace("landing.example.com","other.example.com")}),
        )
        .await?;
    let states = [c.state(&landing)?, c.state(&first)?, c.state(&second)?];
    for key in ["UUID", "SERVICE_NAME"] {
        ensure!(
            states
                .iter()
                .map(|s| s[key].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 3,
            "多租户身份冲突"
        );
    }
    let second_before = c.snapshot(&second)?;
    c.cli(&["stop", "tenantA"], json!({}), "", true).await?;
    ensure!(c.snapshot(&second)? == second_before, "停止影响其他租户");
    c.cli(
        &["install", "direct", "duplicate"],
        json!({"REALITY_PORT":"20002"}),
        "",
        false,
    )
    .await?;
    ensure!(
        !first.parent().unwrap().join("duplicate").exists(),
        "端口冲突留下实例"
    );
    ensure!(
        c.cli(&["list"], json!({}), "", true)
            .await?
            .contains("other.example.com"),
        "列表缺少上游"
    );
    ensure!(
        fs::metadata(&first)?.permissions().mode() & 0o777 == 0o700
            && fs::metadata(first.join("state.json"))?.permissions().mode() & 0o777 == 0o600,
        "实例权限不正确"
    );
    c.cli(
        &["install", "direct", "bad"],
        json!({"REALITY_PORT":"20102","TEST_FAIL_CONFIG":"1"}),
        "",
        false,
    )
    .await?;
    ensure!(
        c.snapshot(&second)? == second_before
            && !first.parent().unwrap().join("bad").exists()
            && !c.root.join("units/xray-chain-bad.service").exists()
            && !c.root.join("web/instances/bad").exists(),
        "创建失败未回滚"
    );
    c.cli(&["start", "tenantA"], json!({}), "", true).await?;
    let before = c.snapshot(&first)?;
    c.cli(&["upstream","tenantA"],json!({"UPSTREAM_URI":URI.replace("landing.example.com","new.example.com"),"TEST_FAIL_RESTART":"1"}),"",false).await?;
    ensure!(
        c.snapshot(&first)? == before && c.snapshot(&second)? == second_before,
        "更新失败未回滚"
    );
    c.cli(&["stop", "tenantA"], json!({}), "", true).await?;
    c.cli(
        &["upstream", "tenantA"],
        json!({"UPSTREAM_URI":URI.replace("landing.example.com","new.example.com")}),
        "",
        true,
    )
    .await?;
    let after = c.state(&first)?;
    ensure!(
        after["UUID"] == states[1]["UUID"]
            && after["REALITY_PORT"] == states[1]["REALITY_PORT"]
            && after["UPSTREAM_ADDRESS"] == "new.example.com"
            && c.services()?["xray-chain-tenantA"]["active"] == false
            && c.services()?["xray-chain-tenantA"]["enabled"] == true,
        "更新改变身份或停止状态"
    );
    c.cli(&["disable", "tenantA"], json!({}), "", true).await?;
    c.cli(&["start", "tenantA"], json!({}), "", true).await?;
    ensure!(
        c.services()?["xray-chain-tenantA"]["active"] == true
            && c.services()?["xray-chain-tenantA"]["enabled"] == false,
        "运行状态与自启耦合"
    );
    let source = c.root.join("legacy");
    fs::create_dir(&source)?;
    for name in ["config.json", "install.env", "reality.env", "client.txt"] {
        fs::copy(landing.join(name), source.join(name))?;
    }
    let original = fs::read(source.join("config.json"))?;
    fs::write(
        c.root.join("units/old-xray.service"),
        format!(
            "ExecStart=xray run -config {}",
            source.join("config.json").display()
        ),
    )?;
    fs::remove_file(landing.join("state.json"))?;
    c.cli(
        &["import", "legacy", source.to_str().unwrap(), "old-xray"],
        json!({}),
        "",
        true,
    )
    .await?;
    let imported = c.state(&first.parent().unwrap().join("legacy"))?;
    ensure!(
        imported["UUID"] == states[0]["UUID"]
            && imported["APP_DIR"] == source.to_str().unwrap()
            && imported["SERVICE_NAME"] == "old-xray"
            && fs::read(source.join("config.json"))? == original,
        "导入重写原部署"
    );
    let alpine = c
        .install("alpine", 20501, "relay", json!({"INIT_SYSTEM":"openrc"}))
        .await?;
    let service = fs::read_to_string(c.root.join("init.d/xray-chain-alpine"))?;
    ensure!(
        service.contains("supervisor=\"supervise-daemon\"")
            && service.contains(alpine.join("config.json").to_str().unwrap()),
        "OpenRC 服务错误"
    );
    fs::write(alpine.join("logs/service.log"), "selected instance log\n")?;
    ensure!(
        c.cli(
            &["logs", "alpine"],
            json!({"INIT_SYSTEM":"openrc"}),
            "",
            true
        )
        .await?
        .contains("selected instance log"),
        "实例日志错误"
    );
    c.cli(
        &["disable", "alpine"],
        json!({"INIT_SYSTEM":"openrc"}),
        "",
        true,
    )
    .await?;
    ensure!(
        c.events()?
            .contains(&json!(["rc-update", "del", "xray-chain-alpine", "default"])),
        "OpenRC 关闭自启失败"
    );
    Ok(())
}
async fn certificates(work: &Workspace, bin: &Path) -> Result<()> {
    let c = Case::new(work, bin, "certificates")?;
    let first = c
        .install("tlsA", 20601, "relay", c.existing_cert(20601, "tls")?)
        .await?;
    let second = c
        .install("tlsB", 20602, "relay", c.existing_cert(20602, "tls")?)
        .await?;
    ensure!(
        fs::read_dir(c.root.join("nginx"))?
            .filter_map(|v| v.ok())
            .filter(|v| v.path().extension().is_some_and(|s| s == "conf"))
            .count()
            == 2
            && c.state(&first)?["INTERNAL_HTTP_PORT"] != c.state(&second)?["INTERNAL_HTTP_PORT"],
        "同域名实例配置冲突"
    );
    for name in ["tlsA", "tlsB"] {
        c.shell(
            &format!("load_instance {name}; CERT_MODE=http; setup_cert_renew_hook"),
            json!({}),
            "",
            true,
        )
        .await?;
    }
    let hooks = fs::read_dir(c.root.join("hooks"))?.collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(hooks.len() == 2, "续期钩子覆盖");
    for name in ["tlsA", "tlsB"] {
        ensure!(
            c.root
                .join(format!("units/xray-chain-{name}-cert-renew.timer"))
                .is_file(),
            "未安装原生续期定时器"
        );
    }
    let credentials = c.root.join("manager/credentials/edge.example.com.ini");
    fs::create_dir_all(credentials.parent().unwrap())?;
    fs::write(&credentials, "dns_cloudflare_api_token = existing-token\n")?;
    let before = fs::read(&credentials)?;
    c.shell("INSTALL_PENDING=1; CERT_MODE=cloudflare; SECURITY_MODE=tls; CF_API_TOKEN=new-token; obtain_or_copy_cert",json!({"TEST_CERT_FAIL":"1"}),"",false).await?;
    ensure!(fs::read(&credentials)? == before, "申请失败覆盖共享凭据");
    let before = c.events()?.len();
    c.shell(
        "CERT_MODE=http; write_nginx_challenge_config; verify_http_challenge",
        json!({}),
        "",
        true,
    )
    .await?;
    let calls = c.events()?[before..]
        .iter()
        .filter(|v| v[0] == "curl")
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        calls.len() == 2
            && calls[0]
                .as_array()
                .unwrap()
                .contains(&json!("edge.example.com:80:127.0.0.1"))
            && !calls[1].as_array().unwrap().contains(&json!("--resolve")),
        "HTTP 本机和域名自检丢失"
    );
    for call in calls {
        let args = call.as_array().unwrap();
        let index = args
            .iter()
            .position(|s| s == "--noproxy")
            .context("自检没有绕过环境代理")?;
        ensure!(args[index + 1] == "*", "HTTP 自检使用环境代理");
    }
    for (extra, message, count) in [
        (
            json!({"TEST_HTTP_LOCAL_STATUS":"404"}),
            "本机 HTTP-01 自检失败",
            3,
        ),
        (
            json!({"TEST_HTTP_PUBLIC_STATUS":"404"}),
            "域名 HTTP-01 自检失败",
            4,
        ),
        (
            json!({"TEST_HTTP_WRONG_BODY":"1"}),
            "本机 HTTP-01 自检失败",
            3,
        ),
        (
            json!({"TEST_HTTP_CURL_EXIT":"28"}),
            "本机 HTTP-01 自检失败",
            3,
        ),
    ] {
        let before = c.events()?.len();
        let output=c.shell("CERT_MODE=http; SECURITY_MODE=tls; write_nginx_challenge_config; obtain_or_copy_cert",extra,"",false).await?;
        let events = c.events()?;
        ensure!(
            output.contains(message)
                && events[before..].iter().filter(|v| v[0] == "curl").count() == count
                && events[before..].iter().all(|v| v[0] != "cert-obtain"),
            "HTTP 自检失败仍申请证书：{output}"
        );
        ensure!(
            fs::read_dir(
                c.root
                    .join("web/domains/edge.example.com/.well-known/acme-challenge")
            )?
            .next()
            .is_none(),
            "HTTP 探针遗留"
        );
    }
    c.shell("CERT_MODE=http; write_nginx_challenge_config; trap 'exit 143' TERM; curl() { kill -TERM $$; return 1; }; verify_http_challenge",json!({}),"",false).await?;
    ensure!(
        fs::read_dir(
            c.root
                .join("web/domains/edge.example.com/.well-known/acme-challenge")
        )?
        .next()
        .is_none(),
        "中断未清理探针"
    );
    Ok(())
}
async fn editing(work: &Workspace, bin: &Path) -> Result<()> {
    let c = Case::new(work, bin, "editing")?;
    let app = c.install("first", 20803, "relay", json!({})).await?;
    let other = c.install("second", 20809, "relay", json!({})).await?;
    let original = c.state(&app)?;
    let before = c.snapshot(&app)?;
    c.cli(&["reset", "first", "all"], json!({}), "\n", true)
        .await?;
    ensure!(c.snapshot(&app)? == before, "取消仍重置凭据");
    c.cli(&["credentials", "first"], json!({}), "4\nfirst\n0\n", true)
        .await?;
    let new = c.state(&app)?;
    let config: Value = serde_json::from_slice(&fs::read(app.join("config.json"))?)?;
    let old_config: Value = serde_json::from_slice(&before["config.json"])?;
    for key in [
        "UUID",
        "REALITY_PRIVATE_KEY",
        "REALITY_PUBLIC_KEY",
        "REALITY_SHORT_ID",
    ] {
        ensure!(new[key] != original[key], "凭据没有轮换：{key}");
    }
    ensure!(
        config["inbounds"][0]["settings"]["clients"][0]["id"] == new["UUID"]
            && config["inbounds"][0]["streamSettings"]["realitySettings"]["privateKey"]
                == new["REALITY_PRIVATE_KEY"]
            && config["outbounds"] == old_config["outbounds"]
            && new["REALITY_PORT"] == original["REALITY_PORT"],
        "轮换影响上游、端口或未写入配置"
    );
    let links = c.cli(&["links", "first"], json!({}), "", true).await?;
    ensure!(
        links.contains(new["UUID"].as_str().unwrap())
            && !links.contains(original["UUID"].as_str().unwrap()),
        "导出凭据未更新"
    );
    let before = c.snapshot(&app)?;
    c.cli(
        &["reset", "first", "all"],
        json!({"TEST_FAIL_RESTART":"1"}),
        "first\n",
        false,
    )
    .await?;
    ensure!(c.snapshot(&app)? == before, "重置失败未恢复所有文件");
    let duplicate = c.state(&other)?["UUID"].as_str().unwrap().to_string();
    c.cli(
        &["reset", "first", "custom-uuid"],
        json!({}),
        &format!("{duplicate}\nfirst\n"),
        false,
    )
    .await?;
    ensure!(c.snapshot(&app)? == before, "重复 UUID 改写配置");
    c.cli(&["stop", "first"], json!({}), "", true).await?;
    c.cli(&["ports", "first"], json!({}), "20807\nfirst\n", true)
        .await?;
    let config: Value = serde_json::from_slice(&fs::read(app.join("config.json"))?)?;
    ensure!(
        config["inbounds"][0]["port"] == 20807
            && config["outbounds"] == old_config["outbounds"]
            && c.services()?["xray-chain-first"]["active"] == false,
        "端口编辑改变停止状态或上游"
    );
    let before = c.snapshot(&app)?;
    let other_before = c.snapshot(&other)?;
    c.cli(&["ports", "first"], json!({}), "20809\n", false)
        .await?;
    ensure!(
        c.snapshot(&app)? == before && c.snapshot(&other)? == other_before,
        "端口冲突影响原配置"
    );
    fs::remove_file(app.join("client.txt"))?;
    let actual = c.state(&app)?;
    let mut stale = actual.clone();
    stale["UUID"] = json!(UUID);
    stale["REALITY_PUBLIC_KEY"] = json!("stale");
    fs::write(app.join("state.json"), serde_json::to_vec(&stale)?)?;
    let links = c.cli(&[], json!({}), "6\n1\n0\n", true).await?;
    ensure!(
        links.contains("查看租户 VLESS 链接")
            && links.contains(actual["UUID"].as_str().unwrap())
            && links.contains(&format!(
                "pbk={}",
                actual["REALITY_PUBLIC_KEY"].as_str().unwrap()
            ))
            && !links.contains("pbk=stale")
            && !links.contains(actual["REALITY_PRIVATE_KEY"].as_str().unwrap())
            && !app.join("client.txt").exists(),
        "查看链接未从原配置恢复或产生写入"
    );
    let node_line = links
        .lines()
        .find(|s| s.starts_with("  - {"))
        .context("缺少 YAML 导出")?;
    let node: Value = serde_yaml_ng::from_str(node_line)?;
    ensure!(
        node[0]["uuid"] == actual["UUID"]
            && node[0]["reality-opts"]["short-id"] == actual["REALITY_SHORT_ID"],
        "恢复导出参数错误"
    );
    fs::write(app.join("state.json"), serde_json::to_vec(&actual)?)?;
    c.cli(&["refresh-client", "first"], json!({}), "", true)
        .await?;
    let before = fs::read(app.join("config.json"))?;
    let output = c
        .cli(
            &[],
            json!({}),
            "4\n1\n11\n1\n香港 \"一号\"\n0\n9\n0\n0\n",
            true,
        )
        .await?;
    ensure!(
        c.state(&app)?["INSTANCE_NAME"] == "香港 \"一号\""
            && output.contains("租户：香港 \"一号\" (first)")
            && fs::read(app.join("config.json"))? == before,
        "交互重命名未返回或改变配置"
    );
    let delete = c
        .install("delete", 20810, "relay", c.existing_cert(20810, "tls")?)
        .await?;
    let shared = c.root.join("nginx/xray-chain-http-edge.example.com.conf");
    fs::write(&shared, "shared certificate validation site\n")?;
    c.cli(&["delete", "delete"], json!({}), "wrong\n", true)
        .await?;
    ensure!(delete.exists(), "取消删除未生效");
    c.cli(&["delete", "delete"], json!({}), "delete\n", true)
        .await?;
    ensure!(
        !delete.exists()
            && !c.root.join("web/instances/delete").exists()
            && !c.root.join("units/xray-chain-delete.service").exists()
            && c.snapshot(&other)? == other_before
            && shared.exists()
            && c.root.join("cert.pem").exists()
            && c.services()?["xray-chain-second"]["active"] == true
            && c.services()?["xray-chain-delete"]["active"] == false
            && c.services()?["xray-chain-delete"]["enabled"] == false,
        "删除影响其他租户或共享证书"
    );
    let rollback = c
        .install(
            "rollback-delete",
            20812,
            "relay",
            c.existing_cert(20812, "tls")?,
        )
        .await?;
    let before = c.snapshot(&rollback)?;
    let unit = fs::read(c.root.join("units/xray-chain-rollback-delete.service"))
        .context("删除回滚测试缺少服务文件")?;
    let site = c
        .root
        .join("nginx/xray-chain-rollback-delete-edge.example.com.conf");
    let site_before = fs::read(&site).with_context(|| site.display().to_string())?;
    c.cli(
        &["delete", "rollback-delete"],
        json!({"TEST_FAIL_NGINX_RELOAD":"1"}),
        "rollback-delete\n",
        false,
    )
    .await?;
    ensure!(
        c.snapshot(&rollback)? == before
            && fs::read(c.root.join("units/xray-chain-rollback-delete.service"))? == unit
            && fs::read(&site)? == site_before
            && c.services()?["xray-chain-rollback-delete"]["active"] == true
            && c.services()?["xray-chain-rollback-delete"]["enabled"] == true,
        "删除失败未恢复文件或服务"
    );
    let imported = c.install("legacy", 20813, "relay", json!({})).await?;
    let mut state = c.state(&imported)?;
    let original = c.root.join("original");
    fs::create_dir(&original)?;
    let before = fs::read(imported.join("config.json"))?;
    fs::write(original.join("config.json"), &before)?;
    state["INSTANCE_IMPORTED"] = json!("true");
    state["APP_DIR"] = json!(original.display().to_string());
    fs::write(imported.join("state.json"), serde_json::to_vec(&state)?)?;
    c.cli(&["delete", "legacy"], json!({}), "legacy\n", true)
        .await?;
    ensure!(
        !imported.join("state.json").exists()
            && fs::read(original.join("config.json"))? == before
            && c.services()?["xray-chain-legacy"]["active"] == true,
        "注销导入实例停止或删除原部署"
    );
    let dual = c
        .install("dual", 20814, "relay", c.existing_cert(20815, "dual")?)
        .await?;
    c.cli(&["reset", "dual", "uuid"], json!({}), "dual\n", true)
        .await?;
    let config: Value = serde_json::from_slice(&fs::read(dual.join("config.json"))?)?;
    let ids = config["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["settings"]["clients"].as_array().unwrap())
        .map(|c| c["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    let links = c.cli(&["links", "dual"], json!({}), "", true).await?;
    ensure!(
        ids.len() == 1
            && links.matches("vless://").count() == 2
            && links.contains(":20814?")
            && links.contains(":20815?"),
        "双协议重置或导出身份不一致"
    );
    Ok(())
}
