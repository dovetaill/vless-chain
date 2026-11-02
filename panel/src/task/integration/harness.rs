use super::super::{Options, strings, workspace::Workspace};
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::IntoResponse,
    routing::any,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, UdpSocket},
    task::JoinHandle,
};

pub struct Preflight {
    pub enabled: Arc<AtomicBool>,
    pub intercepted: Arc<AtomicUsize>,
    pub port: u16,
    pub disconnect: Arc<std::sync::Mutex<Option<(String, u32)>>>,
}
pub struct Harness<'a> {
    pub work: &'a Workspace,
    pub root: PathBuf,
    pub bin: PathBuf,
    pub xray: PathBuf,
    original_xray: PathBuf,
    pub base: String,
    cookie: RefCell<String>,
    ports: RefCell<BTreeSet<u16>>,
    tasks: RefCell<Vec<JoinHandle<()>>>,
    children: RefCell<Vec<u32>>,
    pub preflight: Option<Preflight>,
    client: reqwest::Client,
}
impl<'a> Harness<'a> {
    pub fn new(work: &'a Workspace, stage: &Path, name: &str) -> Result<Self> {
        let root = work.root.join(name);
        fs::create_dir(&root)?;
        let bin = if stage.join("bin").is_dir() {
            stage.join("bin")
        } else {
            PathBuf::from(std::env::var_os("CHAIN_TEST_BIN_DIR").context("缺少测试二进制目录")?)
        };
        let xray = std::env::var_os("CHAIN_TEST_XRAY")
            .map(PathBuf::from)
            .unwrap_or_else(|| bin.join("xray"));
        Ok(Self {
            work,
            root,
            bin,
            original_xray: xray.clone(),
            xray,
            base: String::new(),
            cookie: RefCell::new(String::new()),
            ports: RefCell::new(BTreeSet::new()),
            tasks: RefCell::new(vec![]),
            children: RefCell::new(vec![]),
            preflight: None,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(48))
                .build()?,
        })
    }
    pub fn port(&self) -> Result<u16> {
        use rand::Rng;
        // Public fixture ports must not collide with the core's 50000+ backends or outbound ephemeral sockets.
        for _ in 0..200 {
            let port = rand::thread_rng().gen_range(20000..30000);
            if self.ports.borrow().contains(&port) {
                continue;
            }
            if let (Ok(_tcp), Ok(_udp)) = (
                std::net::TcpListener::bind(("127.0.0.1", port)),
                std::net::UdpSocket::bind(("127.0.0.1", port)),
            ) {
                self.ports.borrow_mut().insert(port);
                self.record()?;
                return Ok(port);
            }
        }
        anyhow::bail!("200 次尝试后仍无空闲测试 TCP/UDP 端口")
    }
    pub fn remember(&self, port: u16) -> Result<()> {
        self.ports.borrow_mut().insert(port);
        self.record()
    }
    pub fn task(&self, task: JoinHandle<()>) {
        self.tasks.borrow_mut().push(task);
    }
    fn record(&self) -> Result<()> {
        fs::write(
            self.root.join("listeners.json"),
            serde_json::to_vec(
                &json!({"ports":*self.ports.borrow(),"root":self.root,"pid":std::process::id()}),
            )?,
        )?;
        Ok(())
    }
    pub fn launch(
        &self,
        name: &str,
        binary: &Path,
        mut env: BTreeMap<String, String>,
        args: &[String],
    ) -> Result<u32> {
        if let Some(preflight) = &self.preflight {
            if env.get("CHAIN_PANEL_URL") == Some(&self.base) {
                env.insert(
                    "CHAIN_PANEL_URL".into(),
                    format!("http://127.0.0.1:{}", preflight.port),
                );
            }
        }
        env.insert("TMPDIR".into(), self.root.display().to_string());
        let mut command = vec![binary.display().to_string()];
        command.extend(args.iter().cloned());
        let pid = self.work.spawn(
            &self.root,
            &command,
            &env,
            &self.root.join(format!("{name}.log")),
        )?;
        self.children.borrow_mut().push(pid);
        Ok(pid)
    }
    pub fn launch_core(&self, name: &str, config: &Value) -> Result<u32> {
        let path = self.root.join(format!("{name}.json"));
        fs::write(&path, serde_json::to_vec(config)?)?;
        self.launch(
            name,
            &self.xray,
            BTreeMap::new(),
            &strings(&["run", "-config", path.to_str().unwrap()]),
        )
    }
    pub async fn request(
        &self,
        path: &str,
        method: &str,
        data: Option<&Value>,
        status: u16,
    ) -> Result<Value> {
        self.request_headers(path, method, data, status, &[]).await
    }
    pub async fn request_headers(
        &self,
        path: &str,
        method: &str,
        data: Option<&Value>,
        status: u16,
        headers: &[(&str, &str)],
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(method.parse()?, format!("{}{path}", self.base))
            .header("Content-Type", "application/json")
            .header("X-Chain-Request", "1");
        let cookie = self.cookie.borrow().clone();
        let mut request_headers = reqwest::header::HeaderMap::new();
        if !cookie.is_empty() {
            request_headers.insert(reqwest::header::COOKIE, cookie.parse()?);
        }
        for (key, value) in headers {
            request_headers.insert(
                reqwest::header::HeaderName::from_bytes(key.as_bytes())?,
                value.parse()?,
            );
        }
        request = request.headers(request_headers);
        if let Some(data) = data {
            request = request.json(data);
        }
        let response = request.send().await?;
        let actual = response.status().as_u16();
        if let Some(value) = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
        {
            self.cookie
                .replace(value.split(';').next().unwrap_or("").into());
        }
        let body = response.bytes().await?;
        ensure!(
            actual == status,
            "{method} {path}：预期 {status}，实际 {actual}：{}",
            String::from_utf8_lossy(&body)
        );
        Ok(serde_json::from_slice(&body).unwrap_or_else(|_| json!(String::from_utf8_lossy(&body))))
    }
    pub async fn state(&self) -> Result<Value> {
        self.request("/api/state", "GET", None, 200).await
    }
    pub async fn anonymous(
        &self,
        path: &str,
        method: &str,
        data: Option<&Value>,
        status: u16,
    ) -> Result<Value> {
        self.request_headers(path, method, data, status, &[("Cookie", "")])
            .await
    }
    pub async fn current(&self, line: &Value) -> Result<Value> {
        self.state().await?["lines"]
            .as_array()
            .context("线路缺失")?
            .iter()
            .find(|v| v["id"] == line["id"])
            .cloned()
            .context("线路不存在")
    }
    pub async fn synced(&self, lines: &[Value]) -> Result<()> {
        self.wait_state("策略确认", 30, |s| {
            lines.iter().all(|l| {
                s["lines"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|v| v["id"] == l["id"] && v["sync_status"] == "synced")
                })
            })
        })
        .await?;
        self.remember_ports()?;
        Ok(())
    }
    pub fn remember_ports(&self) -> Result<()> {
        fn scan(h: &Harness<'_>, root: &Path) -> Result<()> {
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    scan(h, &entry.path())?;
                } else if entry.file_name() == "active.json" {
                    let value: Value = serde_json::from_slice(&fs::read(entry.path())?)?;
                    let config = value.get("config").unwrap_or(&value);
                    for path in ["inbounds", "_chain"] {
                        if let Some(a) = config[path].as_array() {
                            for v in a {
                                for key in ["port", "backend", "fallback_port"] {
                                    if let Some(port) =
                                        v[key].as_u64().filter(|p| *p > 0 && *p <= 65535)
                                    {
                                        h.ports.borrow_mut().insert(port as u16);
                                    }
                                }
                                for key in ["port", "backend"] {
                                    if let Some(port) =
                                        v["meter"][key].as_u64().filter(|p| *p > 0 && *p <= 65535)
                                    {
                                        h.ports.borrow_mut().insert(port as u16);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(a) = config["_mihomo"]["listeners"].as_array() {
                        for v in a {
                            if let Some(p) = v["port"].as_u64() {
                                h.ports.borrow_mut().insert(p as u16);
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        scan(self, &self.root)?;
        self.record()
    }
    pub async fn login(&self, data: &Path) -> Result<()> {
        self.request(
            "/api/login",
            "POST",
            Some(&json!({"token":fs::read_to_string(data.join("admin-token"))?})),
            200,
        )
        .await?;
        Ok(())
    }
    pub fn save_cookie(&self) -> String {
        self.cookie.borrow().clone()
    }
    pub fn restore_cookie(&self, value: String) {
        self.cookie.replace(value);
    }
    pub async fn ready(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self
                .client
                .get(&self.base)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "面板启动超时");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub async fn wait_state(
        &self,
        description: &str,
        seconds: u64,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            let state = self.state().await?;
            if predicate(&state) {
                return Ok(state);
            }
            ensure!(
                Instant::now() < deadline,
                "等待 {description} 超时：{state}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    pub async fn publish(&self, expected: &str) -> Result<Value> {
        let id = self
            .request("/api/publish", "POST", Some(&json!({})), 200)
            .await?["job_id"]
            .clone();
        let state = self
            .wait_state("发布完成", 60, |s| {
                s["jobs"].as_array().is_some_and(|a| {
                    a.iter().any(|j| {
                        j["id"] == id
                            && !matches!(j["state"].as_str(), Some("publishing" | "rolling_back"))
                    })
                })
            })
            .await?;
        let job = state["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == id)
            .unwrap();
        ensure!(job["state"] == expected, "发布结果：{job}");
        Ok(job.clone())
    }
    pub async fn probe(&self, line: &Value, url: &str) -> Result<Value> {
        let value = self
            .request(
                &format!(
                    "/api/lines/{}/probe",
                    line["id"].as_str().context("线路缺少 ID")?
                ),
                "POST",
                Some(&json!({"mode":"http","url":url})),
                200,
            )
            .await?;
        ensure!(value["ok"] == true, "代理探测失败：{value}");
        Ok(value)
    }
    pub async fn panel(&mut self, env: BTreeMap<String, String>) -> Result<u32> {
        let port = self.port()?;
        self.base = format!("http://127.0.0.1:{port}");
        let mut env = env;
        env.insert("CHAIN_LISTEN".into(), format!("127.0.0.1:{port}"));
        env.entry("CHAIN_DATA_DIR".into())
            .or_insert(self.root.join("control").display().to_string());
        env.entry("CHAIN_XRAY_BIN".into())
            .or_insert(self.xray.display().to_string());
        let pid = self.launch("panel", &self.bin.join("chain-panel"), env, &[])?;
        self.ready().await?;
        self.login(&self.root.join("control")).await?;
        Ok(pid)
    }
    pub async fn agent(
        &self,
        index: usize,
        node: &Value,
        extra: BTreeMap<String, String>,
    ) -> Result<(u32, BTreeMap<String, String>)> {
        let token = self.root.join(format!("node-{index}.token"));
        fs::write(&token, node["token"].as_str().context("节点令牌缺失")?)?;
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600))?;
        let mut env = map(&[
            (
                "CHAIN_AGENT_DIR",
                self.root
                    .join(format!("agent-{index}"))
                    .display()
                    .to_string(),
            ),
            ("CHAIN_PANEL_URL", self.base.clone()),
            (
                "CHAIN_NODE_ID",
                node["id"].as_str().context("节点 ID 缺失")?.into(),
            ),
            ("CHAIN_NODE_TOKEN_FILE", token.display().to_string()),
            ("CHAIN_XRAY_BIN", self.xray.display().to_string()),
        ]);
        env.extend(extra);
        Ok((
            self.launch(
                &format!("agent-{index}"),
                &self.bin.join("chain-agent"),
                env.clone(),
                &[],
            )?,
            env,
        ))
    }
    pub async fn http(&self, body: Vec<u8>) -> Result<u16> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        self.ports.borrow_mut().insert(port);
        self.record()?;
        let body = Arc::new(body);
        self.tasks.borrow_mut().push(tokio::spawn(async move {
            let app = Router::new().fallback(any(move || {
                let body = body.clone();
                async move { ([("content-type", "text/plain")], body.as_ref().clone()) }
            }));
            let _ = axum::serve(listener, app).await;
        }));
        Ok(port)
    }
    pub async fn website(&self) -> Result<u16> {
        use base64::Engine;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        self.remember(port)?;
        self.tasks.borrow_mut().push(tokio::spawn(async move {
            let mut children=tokio::task::JoinSet::new();while let Ok((mut stream,_))=listener.accept().await {children.spawn(async move {let _=tokio::time::timeout(Duration::from_secs(10),async {
                let mut header=vec![];while !header.ends_with(b"\r\n\r\n") {let mut byte=[0];stream.read_exact(&mut byte).await?;header.push(byte[0]);ensure!(header.len()<16384,"请求头过大");}
                let header=String::from_utf8(header)?;
                if header.lines().any(|l|l.eq_ignore_ascii_case("Upgrade: websocket")) {
                    let key=header.lines().find_map(|l|l.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case("Sec-WebSocket-Key")).map(|(_,v)|v.trim())).context("WebSocket key 缺失")?;
                    let digest=openssl::hash::hash(openssl::hash::MessageDigest::sha1(),format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes())?;let accept=base64::engine::general_purpose::STANDARD.encode(digest);
                    stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await?;
                    let mut head=[0;2];stream.read_exact(&mut head).await?;let mut mask=[0;4];stream.read_exact(&mut mask).await?;let mut data=vec![0;(head[1]&127) as usize];stream.read_exact(&mut data).await?;for (i,v) in data.iter_mut().enumerate(){*v^=mask[i%4];}stream.write_all(&[0x81,data.len() as u8]).await?;stream.write_all(&data).await?;
                }else{let path=header.split_whitespace().nth(1).context("缺少请求地址")?;stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{path}",path.len()).as_bytes()).await?;}
                Ok::<_,anyhow::Error>(())
            }).await;});}
        }));
        Ok(port)
    }
    pub async fn echo(&self) -> Result<u16> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        self.ports.borrow_mut().insert(port);
        let udp = UdpSocket::bind(("127.0.0.1", port)).await?;
        self.tasks.borrow_mut().push(tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            while let Ok((mut stream, _)) = listener.accept().await {
                children.spawn(async move {
                    let mut bytes = [0u8; 65536];
                    while let Ok(n) = stream.read(&mut bytes).await {
                        if n == 0 {
                            break;
                        }
                        if stream.write_all(&bytes[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        }));
        self.tasks.borrow_mut().push(tokio::spawn(async move {
            let mut bytes = vec![0u8; 65536];
            while let Ok((n, peer)) = udp.recv_from(&mut bytes).await {
                let _ = udp.send_to(&bytes[..n], peer).await;
            }
        }));
        self.record()?;
        Ok(port)
    }
    pub async fn tls(&self, domain: &str) -> Result<(u16, PathBuf, PathBuf)> {
        use hyper_util::{
            rt::{TokioExecutor, TokioIo},
            server::conn::auto::Builder,
            service::TowerToHyperService,
        };
        let cert = self.root.join(format!("{domain}-cert.pem"));
        let key = self.root.join(format!("{domain}-key.pem"));
        self.work
            .run(
                &self.root,
                &strings(&[
                    "openssl",
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:prime256v1",
                    "-nodes",
                    "-days",
                    "90",
                    "-subj",
                    &format!("/CN={domain}"),
                    "-addext",
                    &format!("subjectAltName=DNS:{domain}"),
                    "-addext",
                    "basicConstraints=critical,CA:FALSE",
                    "-out",
                    cert.to_str().unwrap(),
                    "-keyout",
                    key.to_str().unwrap(),
                ]),
            )
            .await?;
        let config =
            (*crate::website::tls_config(&fs::read_to_string(&cert)?, &fs::read_to_string(&key)?)?)
                .clone();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        self.ports.borrow_mut().insert(port);
        self.tasks.borrow_mut().push(tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                children.spawn(async move {
                    if let Ok(Ok(stream)) =
                        tokio::time::timeout(Duration::from_secs(5), acceptor.accept(stream)).await
                    {
                        let app = Router::new().fallback(any(|| async { "chain-integration-ok" }));
                        let _ = Builder::new(TokioExecutor::new())
                            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
                            .await;
                    }
                });
            }
        }));
        self.record()?;
        Ok((port, cert, key))
    }
    pub fn allow_loopback(&mut self, ports: &[u16]) -> Result<()> {
        for port in ports {
            ensure!(
                self.ports.borrow().contains(port),
                "回环例外只能用于当前夹具端口"
            );
        }
        let real = self.original_xray.canonicalize()?;
        let wrapper = self.root.join("fixture-xray");
        let mihomo = real.parent().context("内核目录缺失")?.join("mihomo");
        if mihomo.is_file() && !self.root.join("mihomo").exists() {
            std::os::unix::fs::symlink(mihomo, self.root.join("mihomo"))?;
        }
        let executable = std::env::current_exe()?;
        let config = self.root.join("fixture-xray-settings.json");
        fs::write(
            &config,
            serde_json::to_vec(&json!({"root":self.root,"binary":real,"ports":ports}))?,
        )?;
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec {} fixture-xray --input {} -- \"$@\"\n",
                crate::ctl::platform::shell(executable.to_str().unwrap()),
                crate::ctl::platform::shell(config.to_str().unwrap())
            ),
        )?;
        super::executable(&wrapper)?;
        self.xray = wrapper;
        Ok(())
    }
    pub async fn preflight(&mut self, target: u16, enabled: bool) -> Result<()> {
        self.preflight_named(target, "localhost", enabled).await
    }
    pub async fn preflight_named(
        &mut self,
        target: u16,
        server_name: &str,
        enabled: bool,
    ) -> Result<()> {
        ensure!(
            self.ports.borrow().contains(&target),
            "预检夹具必须使用本任务端口"
        );
        #[derive(Clone)]
        struct Proxy {
            base: String,
            target: u16,
            server_name: String,
            enabled: Arc<AtomicBool>,
            intercepted: Arc<AtomicUsize>,
            client: reqwest::Client,
            disconnect: Arc<std::sync::Mutex<Option<(String, u32)>>>,
        }
        async fn proxy(State(state): State<Proxy>, request: Request) -> impl IntoResponse {
            let path = request.uri().path().to_string();
            if !path.starts_with("/api/agent/") {
                return (StatusCode::FORBIDDEN, Vec::new());
            }
            let authorization = request.headers().get("authorization").cloned();
            let body = match to_bytes(request.into_body(), 2 * 1024 * 1024).await {
                Ok(v) => v,
                Err(_) => return (StatusCode::BAD_REQUEST, vec![]),
            };
            let forward = |body: Vec<u8>| {
                let client = state.client.clone();
                let url = format!("{}{path}", state.base);
                let authorization = authorization.clone();
                async move {
                    let mut req = client
                        .post(url)
                        .header("content-type", "application/json")
                        .body(body);
                    if let Some(v) = authorization {
                        req = req.header("authorization", v);
                    }
                    req.send().await
                }
            };
            let result=async {
                let response=forward(body.to_vec()).await?;let mut status=response.status();let mut body=response.bytes().await?.to_vec();
                if status.is_success()&&path.ends_with("/diagnostics")&&state.enabled.load(Ordering::SeqCst) {
                    let task:Value=serde_json::from_slice(&body)?;
                    if task["request"]["operation"]=="reality-preflight" {
                        anyhow::ensure!(task["request"]["request"]==json!({"target":format!("127.0.0.1:{}",state.target),"server_name":state.server_name}),"意外预检目标");
                        {let mut disconnect=state.disconnect.lock().unwrap();
                        if disconnect.as_ref().is_some_and(|(token,_)|authorization.as_ref().and_then(|v|v.to_str().ok())==Some(token.as_str())) {
                            let (_,pid)=disconnect.take().unwrap();unsafe{libc::kill(-(pid as i32),libc::SIGTERM);}
                        }
                        }
                        let response=forward(serde_json::to_vec(&json!({"id":task["id"],"result":{"ok":true,"cloudflare_checked":true,"fallback":{"status":"verified"}}}))?).await?;
                        status=response.status();body=response.bytes().await?.to_vec();state.intercepted.fetch_add(1,Ordering::SeqCst);
                    }
                }Ok::<_,anyhow::Error>((status,body))
            }.await;
            result.unwrap_or((
                StatusCode::SERVICE_UNAVAILABLE,
                br#"{"error":"fixture upstream unavailable"}"#.to_vec(),
            ))
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        self.ports.borrow_mut().insert(port);
        let enabled = Arc::new(AtomicBool::new(enabled));
        let intercepted = Arc::new(AtomicUsize::new(0));
        let disconnect = Arc::new(std::sync::Mutex::new(None));
        let state = Proxy {
            base: self.base.clone(),
            target,
            server_name: server_name.into(),
            enabled: enabled.clone(),
            intercepted: intercepted.clone(),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(20))
                .build()?,
            disconnect: disconnect.clone(),
        };
        self.tasks.borrow_mut().push(tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                Router::new().fallback(any(proxy)).with_state(state),
            )
            .await;
        }));
        self.preflight = Some(Preflight {
            enabled,
            intercepted,
            port,
            disconnect,
        });
        self.record()?;
        Ok(())
    }
    pub async fn close(&self) -> Result<()> {
        self.remember_ports()?;
        let children = self.children.borrow_mut().drain(..).collect::<Vec<_>>();
        for pid in children.into_iter().rev() {
            self.work.stop(pid).await?;
        }
        for task in self.tasks.borrow_mut().drain(..) {
            task.abort();
            let _ = task.await;
        }
        for port in self.ports.borrow().iter() {
            let tcp = rebind(*port);
            let udp = std::net::UdpSocket::bind(("127.0.0.1", *port));
            ensure!(tcp.is_ok() && udp.is_ok(), "测试端口未释放：{port}");
        }
        println!("CLEANUP ports={:?}", self.ports.borrow());
        Ok(())
    }
}
fn rebind(port: u16) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    ensure!(fd >= 0, "无法检查端口");
    let socket = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let enabled: libc::c_int = 1;
    ensure!(
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                (&enabled as *const libc::c_int).cast(),
                std::mem::size_of_val(&enabled) as _,
            )
        } == 0,
        "无法配置检查套接字"
    );
    let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    address.sin_family = libc::AF_INET as _;
    address.sin_port = port.to_be();
    address.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
    #[cfg(target_os = "macos")]
    {
        address.sin_len = std::mem::size_of_val(&address) as _;
    }
    ensure!(
        unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_in).cast(),
                std::mem::size_of_val(&address) as _,
            )
        } == 0,
        "端口 {port} 仍被占用"
    );
    Ok(())
}
impl Drop for Harness<'_> {
    fn drop(&mut self) {
        if !self.children.borrow().is_empty() {
            fn logs(root: &Path, path: &Path) {
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if entry.file_type().is_ok_and(|v| v.is_dir()) {
                            logs(root, &path);
                        } else if path.extension().is_some_and(|v| v == "log") {
                            if let Ok(body) = fs::read_to_string(&path) {
                                let tail = body
                                    .chars()
                                    .rev()
                                    .take(6000)
                                    .collect::<String>()
                                    .chars()
                                    .rev()
                                    .collect::<String>();
                                eprintln!(
                                    "FAILURE_LOG {}\n{tail}",
                                    path.strip_prefix(root).unwrap().display()
                                );
                            }
                        }
                    }
                }
            }
            logs(&self.root, &self.root);
        }
        for task in self.tasks.borrow_mut().drain(..) {
            task.abort();
        }
    }
}
pub fn map(values: &[(&str, String)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

pub fn fixture_xray(args: &Options) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let settings: Value = serde_json::from_slice(&fs::read(args.value("--input")?)?)?;
    let root = Path::new(settings["root"].as_str().context("夹具目录缺失")?).canonicalize()?;
    let mut arguments = args.trailing.clone();
    if let Some(index) = arguments.iter().position(|s| s == "-config") {
        let source = Path::new(arguments.get(index + 1).context("配置路径缺失")?).canonicalize()?;
        ensure!(source.starts_with(&root), "夹具不能修改外部配置");
        let mut value: Value = serde_json::from_slice(&fs::read(&source)?)?;
        let ports = settings["ports"]
            .as_array()
            .context("夹具端口缺失")?
            .iter()
            .map(|v| v.as_u64().context("端口无效").map(|v| v.to_string()))
            .collect::<Result<Vec<_>>>()?
            .join(",");
        for outbound in value["outbounds"].as_array_mut().context("出站缺失")? {
            if outbound["protocol"] == "freedom" {
                if outbound["settings"].is_null() {
                    outbound["settings"] = json!({});
                }
                if outbound["settings"]["finalRules"].is_null() {
                    outbound["settings"]["finalRules"] = json!([]);
                }
                outbound["settings"]["finalRules"].as_array_mut().context("规则无效")?.insert(0,json!({"action":"allow","network":"tcp,udp","ip":["127.0.0.1"],"port":ports}));
            }
        }
        if let Some(cert) = std::env::var_os("SSL_CERT_FILE") {
            ensure!(
                Path::new(&cert).canonicalize()?.starts_with(&root),
                "夹具证书必须在独占目录"
            );
            fn trust(value: &mut Value) {
                match value {
                    Value::Object(map) => {
                        if let Some(tls) = map.get_mut("tlsSettings") {
                            tls["useSystemCA"] = json!(true);
                        }
                        for value in map.values_mut() {
                            trust(value);
                        }
                    }
                    Value::Array(array) => {
                        for value in array {
                            trust(value);
                        }
                    }
                    _ => (),
                }
            }
            trust(&mut value);
        }
        let temporary = root.join(format!("fixture-core-{}.json", uuid::Uuid::new_v4()));
        fs::write(&temporary, serde_json::to_vec(&value)?)?;
        arguments[index + 1] = temporary.display().to_string();
    }
    let binary = settings["binary"].as_str().context("实际 Xray 缺失")?;
    Err(std::process::Command::new(binary)
        .args(arguments)
        .exec()
        .into())
}
