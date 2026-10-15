//! Owned HTTP(S) reverse proxies and ACME webroot challenges.
use crate::{
    gateway::{Lane, Spec, relay},
    model::{Kind, Tls},
    runtime::atomic_write,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::StreamExt;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use std::{
    collections::BTreeMap,
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

fn certified(cert: &str, key: &str) -> Result<rustls::sign::CertifiedKey> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(cert.as_bytes()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(!certs.is_empty(), "证书 PEM 无有效证书");
    let key = rustls_pemfile::private_key(&mut BufReader::new(key.as_bytes()))?
        .context("私钥 PEM 无效")?;
    let signer = rustls::crypto::ring::sign::any_supported_type(&key)?;
    let result = rustls::sign::CertifiedKey::new(certs, signer);
    result.keys_match().context("证书和私钥不匹配")?;
    Ok(result)
}
pub fn tls_config(cert: &str, key: &str) -> Result<Arc<rustls::ServerConfig>> {
    let cert = certified(cert, key)?;
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(cert)));
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}
fn certificate(tls: &Tls, dir: &Path) -> Result<(String, String)> {
    if tls.mode == "auto" {
        let live = dir.join("acme/config/live").join(&tls.domain);
        Ok((
            std::fs::read_to_string(live.join("fullchain.pem"))?,
            std::fs::read_to_string(live.join("privkey.pem"))?,
        ))
    } else {
        Ok((tls.cert.clone(), tls.key.clone()))
    }
}
pub fn load_tls(tls: &Tls, dir: &Path) -> Result<Arc<rustls::ServerConfig>> {
    let (cert, key) = certificate(tls, dir)?;
    tls_config(&cert, &key)
}
fn site_tls(specs: &[Spec], dir: &Path) -> Result<Arc<rustls::ServerConfig>> {
    let mut resolver = rustls::server::ResolvesServerCertUsingSni::new();
    for s in specs {
        let (cert, key) = certificate(&s.tls, dir)?;
        resolver.add(&s.tls.domain, certified(&cert, &key)?)?;
    }
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(resolver));
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

async fn challenge(path: &str, dir: &Path) -> Option<Response> {
    let token = path.strip_prefix("/.well-known/acme-challenge/")?;
    if token.is_empty()
        || token.len() > 256
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some(StatusCode::NOT_FOUND.into_response());
    }
    let p = dir.join("acme/web/.well-known/acme-challenge").join(token);
    Some(match tokio::fs::read(p).await {
        Ok(v) => ([("content-type", "text/plain")], v).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    })
}

/// Obtain certificates before switching the running configuration. Never kill a port owner.
pub async fn prepare(specs: &[Spec], dir: &Path, existing_challenge: bool) -> Result<()> {
    let automatic: Vec<_> = specs.iter().filter(|s| s.tls.mode == "auto").collect();
    if automatic.is_empty() {
        return Ok(());
    }
    for sub in ["config", "work", "logs", "web"] {
        std::fs::create_dir_all(dir.join("acme").join(sub))?;
    }
    let shared_webroot = std::env::var_os("CHAIN_ACME_WEBROOT").map(PathBuf::from);
    let webroot = shared_webroot
        .clone()
        .unwrap_or_else(|| dir.join("acme/web"));
    std::fs::create_dir_all(&webroot)?;
    let temporary = if !existing_challenge && shared_webroot.is_none() {
        Some(
            challenge_server(dir.to_path_buf(), CancellationToken::new())
                .await
                .context("无法监听证书验证端口 TCP 80（权限不足或端口占用），请使用手动证书")?,
        )
    } else {
        None
    };
    let result = async {
        for spec in automatic {
            let tls = &spec.tls;
            let mut command = tokio::process::Command::new(
                std::env::var("CHAIN_CERTBOT_BIN").unwrap_or_else(|_| "certbot".into()),
            );
            command
                .args([
                    "certonly",
                    "--webroot",
                    "--non-interactive",
                    "--agree-tos",
                    "--register-unsafely-without-email",
                    "--keep-until-expiring",
                    "--cert-name",
                    &tls.domain,
                    "-d",
                    &tls.domain,
                    "--webroot-path",
                ])
                .arg(&webroot)
                .arg("--config-dir")
                .arg(dir.join("acme/config"))
                .arg("--work-dir")
                .arg(dir.join("acme/work"))
                .arg("--logs-dir")
                .arg(dir.join("acme/logs"))
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            let output = tokio::time::timeout(Duration::from_secs(150), command.output())
                .await
                .context("证书申请超时")??;
            ensure!(
                output.status.success(),
                "域名 {} 的证书申请失败：{}",
                tls.domain,
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(1600)
                    .collect::<String>()
            );
            load_tls(tls, dir)?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Some((cancel, mut task)) = temporary {
        cancel.cancel();
        if tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
    result
}
async fn challenge_server(
    dir: PathBuf,
    cancel: CancellationToken,
) -> Result<(CancellationToken, JoinHandle<()>)> {
    let listener = TcpListener::bind("0.0.0.0:80").await?;
    let app = Router::new()
        .fallback(any(|State(dir): State<PathBuf>, req: Request| async move {
            challenge(req.uri().path(), &dir)
                .await
                .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
        }))
        .with_state(dir);
    let stop = cancel.clone();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    });
    Ok((cancel, task))
}

#[derive(Clone)]
struct Websites {
    upgrades: TaskTracker,
    sites: Arc<BTreeMap<String, (Spec, Arc<Lane>)>>,
    dir: PathBuf,
    client: reqwest::Client,
    cancel: CancellationToken,
}
fn clean_headers(headers: &mut axum::http::HeaderMap, upgrade: bool) {
    if let Some(connection) = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    {
        for name in connection.split(',').map(str::trim) {
            if !upgrade || !name.eq_ignore_ascii_case("upgrade") {
                headers.remove(name);
            }
        }
    }
    for name in [
        "proxy-authenticate",
        "proxy-authorization",
        "keep-alive",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        headers.remove(name);
    }
    if !upgrade {
        headers.remove("connection");
        headers.remove("upgrade");
    }
}
async fn proxy(State(state): State<Websites>, mut req: Request) -> Response {
    if let Some(r) = challenge(req.uri().path(), &state.dir).await {
        return r;
    }
    let hostname = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().authority().map(|a| a.as_str()))
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some((spec, lane)) = state.sites.get(&hostname).cloned() else {
        return (StatusCode::NOT_FOUND, "未配置此网站域名").into_response();
    };
    if !lane.available() {
        return (StatusCode::TOO_MANY_REQUESTS, "本站本月流量额度已用尽").into_response();
    }
    if spec.kind == Kind::Vless && spec.target == "builtin" {
        return axum::response::Html("<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>欢迎访问</title><style>body{max-width:46rem;margin:12vh auto;padding:24px;font:18px/1.8 system-ui;color:#334155;background:#f8fafc}h1{color:#0f172a}</style><h1>欢迎访问</h1><p>网站已上线，更多内容正在准备中。</p></html>").into_response();
    }
    // Public website fallbacks are not authenticated proxy traffic.
    let account = spec.kind == Kind::Website;
    let result = async {
        let mut upstream = url::Url::parse(&spec.target)?;
        let path = format!(
            "{}{}",
            upstream.path().trim_end_matches('/'),
            req.uri().path()
        );
        upstream.set_path(&path);
        upstream.set_query(req.uri().query());
        let websocket = req
            .headers()
            .get("upgrade")
            .is_some_and(|h| h.as_bytes().eq_ignore_ascii_case(b"websocket"));
        let upgrade = if websocket {
            Some(hyper::upgrade::on(&mut req))
        } else {
            None
        };
        let method = req.method().clone();
        let mut headers = req.headers().clone();
        clean_headers(&mut headers, websocket);
        headers.remove("host");
        headers.remove("forwarded");
        headers.remove("x-forwarded-for");
        headers.insert("x-forwarded-host", HeaderValue::from_str(&hostname)?);
        headers.insert(
            "x-forwarded-proto",
            HeaderValue::from_static(if matches!(spec.tls.mode.as_str(), "auto" | "manual") {
                "https"
            } else {
                "http"
            }),
        );
        let upload_lane = lane.clone();
        let input = req.into_body().into_data_stream().then(move |chunk| {
            let lane = upload_lane.clone();
            async move {
                let bytes = chunk.map_err(std::io::Error::other)?;
                let n = if account {
                    lane.charge(bytes.len(), true, false).await?
                } else {
                    bytes.len()
                };
                if n != bytes.len() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "额度已用尽",
                    ));
                }
                Ok(bytes)
            }
        });
        let response = state
            .client
            .request(method, upstream)
            .headers(headers)
            .body(reqwest::Body::wrap_stream(input))
            .send()
            .await?;
        let status = response.status();
        let mut headers = response.headers().clone();
        clean_headers(&mut headers, status == StatusCode::SWITCHING_PROTOCOLS);
        let body = if status == StatusCode::SWITCHING_PROTOCOLS {
            let incoming = upgrade.context("后端返回了未请求的协议升级")?;
            let cancel = state.cancel.clone();
            state.upgrades.spawn(async move {
                let transfer = async {
                    let (a, mut b) = tokio::try_join!(
                        async { Ok::<_, anyhow::Error>(incoming.await?) },
                        async { Ok::<_, anyhow::Error>(response.upgrade().await?) }
                    )?;
                    if account {
                        relay(TokioIo::new(a), b, lane).await?;
                    } else {
                        tokio::io::copy_bidirectional(&mut TokioIo::new(a), &mut b).await?;
                    }
                    Ok::<(), anyhow::Error>(())
                };
                tokio::select! {_=cancel.cancelled()=>{},_=transfer=>{}}
            });
            Body::empty()
        } else {
            let connection = account.then(|| lane.connection());
            let output = response.bytes_stream().then(move |chunk| {
                let lane = lane.clone();
                let _keep = &connection;
                async move {
                    let bytes = chunk.map_err(std::io::Error::other)?;
                    let n = if account {
                        lane.charge(bytes.len(), false, false).await?
                    } else {
                        bytes.len()
                    };
                    if n != bytes.len() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "额度已用尽",
                        ));
                    }
                    Ok(bytes)
                }
            });
            Body::from_stream(output)
        };
        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        Ok::<_, anyhow::Error>(response)
    }
    .await;
    result.unwrap_or_else(|_| {
        (StatusCode::BAD_GATEWAY, "后端连接失败或线路额度已用尽").into_response()
    })
}

pub async fn serve(
    specs: &[Spec],
    lanes: &BTreeMap<String, Arc<Lane>>,
    dir: &Path,
    cancel: CancellationToken,
) -> Result<Vec<JoinHandle<()>>> {
    let mut groups: BTreeMap<u16, Vec<Spec>> = BTreeMap::new();
    for s in specs {
        if s.kind == Kind::Website {
            groups.entry(s.port).or_default().push(s.clone());
        }
        if let Some(port) = s.fallback_port {
            let mut fallback = s.clone();
            fallback.port = port;
            fallback.listen = "127.0.0.1".into();
            fallback.tls.mode = "off".into();
            fallback.target = s.fallback_target.clone();
            groups.entry(port).or_default().push(fallback);
        }
    }
    // Bind all before spawning so a conflict cannot leave a half-created server behind.
    let mut listeners = vec![];
    for (port, sites) in groups {
        if sites
            .iter()
            .any(|s| matches!(s.tls.mode.as_str(), "manual" | "auto"))
        {
            site_tls(&sites, dir)?;
        }
        listeners.push((
            TcpListener::bind((sites[0].listen.as_str(), port))
                .await
                .with_context(|| format!("网站端口 {port} 已占用"))?,
            sites,
        ));
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let needs_challenge = specs.iter().any(|s| s.tls.mode == "auto");
    let challenge_listener = if needs_challenge
        && std::env::var_os("CHAIN_ACME_WEBROOT").is_none()
        && !specs
            .iter()
            .any(|s| s.kind == Kind::Website && s.port == 80)
    {
        Some(
            TcpListener::bind("0.0.0.0:80")
                .await
                .context("证书续期需要独立 TCP 80 入口")?,
        )
    } else {
        None
    };
    let mut tasks = vec![];
    let upgrades = TaskTracker::new();
    for (listener, sites) in listeners {
        let tls = sites
            .iter()
            .any(|s| matches!(s.tls.mode.as_str(), "manual" | "auto"));
        let state = Websites {
            upgrades: upgrades.clone(),
            sites: Arc::new(
                sites
                    .iter()
                    .map(|s| {
                        (
                            s.tls.domain.to_ascii_lowercase(),
                            (s.clone(), lanes[&s.id].clone()),
                        )
                    })
                    .collect(),
            ),
            dir: dir.into(),
            client: client.clone(),
            cancel: cancel.clone(),
        };
        let app = Router::new().fallback(any(proxy)).with_state(state);
        let cancel = cancel.clone();
        let dir = dir.to_path_buf();
        tasks.push(tokio::spawn(async move {
            let mut children=JoinSet::new();
            loop {tokio::select!{
                _=cancel.cancelled()=>break,
                Some(_)=children.join_next(),if !children.is_empty()=>{},
                accepted=listener.accept()=>{
                    let Ok((stream,_))=accepted else {break};if children.len()>=1024 {continue;}
                    let app=app.clone();let sites=sites.clone();let dir=dir.clone();
                    children.spawn(async move {
                        let builder=Builder::new(TokioExecutor::new());let service=TowerToHyperService::new(app);
                        if tls {
                            if let Ok(config)=site_tls(&sites,&dir) {if let Ok(Ok(stream))=tokio::time::timeout(Duration::from_secs(15),tokio_rustls::TlsAcceptor::from(config).accept(stream)).await {let _=builder.serve_connection_with_upgrades(TokioIo::new(stream),service).await;}}
                        } else {let _=builder.serve_connection_with_upgrades(TokioIo::new(stream),service).await;}
                    });
                }
            }}
            children.abort_all();while children.join_next().await.is_some(){}
        }));
    }
    if let Some(listener) = challenge_listener {
        let app = Router::new()
            .fallback(any(|State(dir): State<PathBuf>, req: Request| async move {
                challenge(req.uri().path(), &dir)
                    .await
                    .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
            }))
            .with_state(dir.to_path_buf());
        let cancel = cancel.clone();
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await;
        }));
    }
    if specs.iter().any(|s| s.kind == Kind::Website) {
        let stop = cancel.clone();
        tasks.push(tokio::spawn(async move {
            stop.cancelled().await;
            upgrades.close();
            upgrades.wait().await;
        }));
    }
    if needs_challenge {
        let specs = specs.to_vec();
        let dir = dir.to_path_buf();
        tasks.push(tokio::spawn(async move {
            loop {tokio::select!{_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(43200))=>{}};
                tokio::select!{_=cancel.cancelled()=>break,result=prepare(&specs,&dir,true)=>{
                    let status=serde_json::json!({"at":crate::model::now(),"ok":result.is_ok(),"error":result.err().map(|e|e.to_string())});
                    let _=atomic_write(&dir.join("certificate-status.json"),&serde_json::to_vec(&status).unwrap_or_default());
                }}
            }
        }));
    }
    Ok(tasks)
}
