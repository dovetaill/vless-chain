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
        let live = crate::acme::paths(&dir.join("acme/config"), &tls.domain);
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
pub fn certificate_expiry(specs: &[Spec], dir: &Path) -> serde_json::Value {
    let mut result = BTreeMap::new();
    for spec in specs.iter().filter(|s| matches!(s.tls.mode.as_str(), "auto" | "manual")) {
        let expiry = (|| -> Result<u64> {
            let pem = if spec.tls.mode == "auto" {
                std::fs::read(crate::acme::paths(&dir.join("acme/config"), &spec.tls.domain).join("fullchain.pem"))?
            } else { spec.tls.cert.as_bytes().to_vec() };
            let cert = openssl::x509::X509::from_pem(&pem)?;
            let epoch = openssl::asn1::Asn1Time::from_unix(0)?;
            let difference = epoch.diff(cert.not_after())?;
            Ok((i64::from(difference.days) * 86400 + i64::from(difference.secs)).max(0) as u64)
        })();
        if let Ok(expires_at) = expiry {
            result.insert(spec.tls.domain.clone(), serde_json::json!({"name":spec.tls.domain,"expires_at":expires_at}));
        }
    }
    serde_json::json!(result.into_values().collect::<Vec<_>>())
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
    for spec in specs.iter().filter(|s| s.tls.mode == "auto") {
        ensure_certificate(
            &spec.tls,
            dir,
            existing_challenge,
            false,
            core_certificate(spec),
        )
        .await
        .with_context(|| format!("域名 {} 的证书准备失败", spec.tls.domain))?;
    }
    Ok(())
}

fn core_certificate(spec: &Spec) -> bool {
    spec.kind == Kind::Vless
        && matches!(
            spec.protocol,
            crate::model::Protocol::Vless
                | crate::model::Protocol::Hysteria2
                | crate::model::Protocol::Tuic
        )
}

pub(crate) fn core_cert_dir(dir: &Path, name: &str) -> PathBuf {
    dir.join("tls-live").join(name)
}

fn use_certificate(tls: &Tls, dir: &Path, core: bool) -> Result<()> {
    load_tls(tls, dir)?;
    if core {
        // Both native renewals and imported Certbot certificates can switch generations.
        let config = dir.canonicalize()?.join("acme/config");
        let source = crate::acme::paths(&config, &tls.domain);
        let link = core_cert_dir(dir, &tls.domain);
        if let Ok(previous) = std::fs::read_link(&link) {
            ensure!(
                [
                    config.join("live").join(&tls.domain),
                    config.join("rust/live").join(&tls.domain)
                ]
                .contains(&previous),
                "证书运行链接指向其他目录"
            );
            if previous == source {
                return Ok(());
            }
        }
        let parent = link.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        ensure!(
            !std::fs::symlink_metadata(parent)?.file_type().is_symlink(),
            "证书运行目录包含链接"
        );
        match std::fs::symlink_metadata(&link) {
            Ok(meta) => ensure!(meta.is_symlink(), "证书运行路径已有其他文件"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let temp = tempfile::tempdir_in(parent)?;
        let next = temp.path().join("next");
        std::os::unix::fs::symlink(source, &next)?;
        std::fs::rename(next, link)?;
    }
    Ok(())
}

async fn ensure_certificate(
    tls: &Tls,
    dir: &Path,
    existing_challenge: bool,
    renew: bool,
    core: bool,
) -> Result<()> {
    let config = dir.join("acme/config");
    let usable = || -> Result<bool> {
        let lifetime = crate::acme::remaining(&config, &tls.domain, &tls.domain)?;
        Ok(lifetime.is_some_and(|seconds| seconds > if renew { 30 * 86400 } else { 0 }))
    };
    if usable()? {
        return use_certificate(tls, dir, core);
    }
    // Renewal and a concurrent publish may need the same HTTP challenge port and certificate.
    static ISSUING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = ISSUING.lock().await;
    if !usable()? {
        let shared = std::env::var_os("CHAIN_ACME_WEBROOT").map(PathBuf::from);
        let webroot = shared.clone().unwrap_or_else(|| dir.join("acme/web"));
        std::fs::create_dir_all(&webroot)?;
        let issue = crate::acme::obtain(&config, &tls.domain, &tls.domain, &webroot);
        if existing_challenge || shared.is_some() {
            issue.await?;
        } else {
            let listener = TcpListener::bind("0.0.0.0:80")
                .await
                .context("无法监听证书验证端口 TCP 80")?;
            let app = Router::new()
                .fallback(any(|State(dir): State<PathBuf>, req: Request| async move {
                    challenge(req.uri().path(), &dir)
                        .await
                        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
                }))
                .with_state(dir.to_path_buf());
            tokio::select! {
                result = issue => { result?; }
                result = axum::serve(listener, app).into_future() => {
                    result?;
                    anyhow::bail!("证书验证监听提前退出");
                }
            }
        }
    }
    use_certificate(tls, dir, core)
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
    let existing_challenge = specs.iter().any(|s| s.kind == Kind::Website && s.port == 80);
    let mut tasks = vec![];
    let upgrades = TaskTracker::new();
    for (listener, sites) in listeners {
        let address = listener.local_addr()?;
        let mut listener = crate::gateway::Listener::new(listener, address, lanes[&sites[0].id].book.clone(), format!("HTTP {address}"));
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
                (stream,_)=listener.accept()=>{
                    if children.len()>=1024 {continue;}
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
    if specs.iter().any(|s| s.kind == Kind::Website) {
        let stop = cancel.clone();
        tasks.push(tokio::spawn(async move {
            stop.cancelled().await;
            upgrades.close();
            upgrades.wait().await;
        }));
    }
    if needs_challenge {
        let mut domains = BTreeMap::new();
        for spec in specs.iter().filter(|s| s.tls.mode == "auto") {
            let (_, core) = domains.entry(spec.tls.domain.clone()).or_insert((spec.tls.clone(), false));
            *core |= core_certificate(spec);
        }
        let dir = dir.to_path_buf();
        tasks.push(tokio::spawn(async move {
            loop {
                let mut errors = Vec::new();
                for (domain, (tls, core)) in &domains {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        result = ensure_certificate(tls, &dir, existing_challenge, true, *core) => {
                            if let Err(e) = result { errors.push(format!("{domain}: {e:#}")); }
                        }
                    }
                }
                let retry = !errors.is_empty();
                let status = serde_json::json!({"at":crate::model::now(),"ok":!retry,"error":retry.then(|| errors.join("\n"))});
                if let Err(e) = atomic_write(&dir.join("certificate-status.json"), &serde_json::to_vec(&status).unwrap()) {
                    eprintln!("证书续期状态写入失败: {e}");
                }
                let delay = Duration::from_secs(if retry { 3600 } else { 43200 });
                tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(delay) => {} }
            }
        }));
    }
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::{
        asn1::{Asn1Integer, Asn1Time},
        bn::BigNum,
        hash::MessageDigest,
        pkey::PKey,
        rsa::Rsa,
        x509::{X509, X509NameBuilder, extension::SubjectAlternativeName},
    };

    fn install_pair(dir: &Path, domain: &str, days: u32) -> Vec<u8> {
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut dn = X509NameBuilder::new().unwrap();
        dn.append_entry_by_text("CN", domain).unwrap();
        let dn = dn.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_serial_number(&Asn1Integer::from_bn(&BigNum::from_u32(days).unwrap()).unwrap())
            .unwrap();
        cert.set_subject_name(&dn).unwrap();
        cert.set_issuer_name(&dn).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(days).unwrap())
            .unwrap();
        let san = SubjectAlternativeName::new()
            .dns(domain)
            .build(&cert.x509v3_context(None, None))
            .unwrap();
        cert.append_extension(san).unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let pem = cert.build().to_pem().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("fullchain.pem"), &pem).unwrap();
        std::fs::write(
            dir.join("privkey.pem"),
            key.private_key_to_pem_pkcs8().unwrap(),
        )
        .unwrap();
        pem
    }

    #[tokio::test]
    async fn valid_imported_certificate_starts_offline_and_core_path_follows_renewal() {
        let dir = tempfile::tempdir().unwrap();
        let domain = "renewal.test";
        let tls = Tls {
            mode: "auto".into(),
            domain: domain.into(),
            ..Default::default()
        };
        let legacy = dir.path().join("acme/config/live").join(domain);
        let native = dir.path().join("acme/config/rust/live").join(domain);
        let cert = core_cert_dir(dir.path(), domain).join("fullchain.pem");
        let old = install_pair(&legacy, domain, 15);
        ensure_certificate(&tls, dir.path(), false, false, true)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&cert).unwrap(), old);
        let renewed = install_pair(&native, domain, 90);
        ensure_certificate(&tls, dir.path(), false, true, true)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&cert).unwrap(), renewed);
        assert_eq!(std::fs::read(legacy.join("fullchain.pem")).unwrap(), old);
        install_pair(&native, "wrong.test", 90);
        assert!(
            ensure_certificate(&tls, dir.path(), false, false, true)
                .await
                .is_err()
        );
    }
}
