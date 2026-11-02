use super::*;
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use serde_json::{Value, json};
use std::{
    os::unix::fs::MetadataExt,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Ca {
    base: String,
    root: PathBuf,
    jwk: Arc<Mutex<Option<Value>>>,
    certificate: Arc<Mutex<Option<String>>>,
    ready: Arc<Mutex<bool>>,
    accounts: Arc<Mutex<usize>>,
    bad: Arc<Mutex<bool>>,
}
impl Ca {
    async fn request(&self, request: Request) -> Result<Response> {
        let path = request.uri().path().to_string();
        let mut headers = vec![("replay-nonce".to_string(), "fixture-nonce".to_string())];
        let mut result = Value::Null;
        let mut status = StatusCode::OK;
        if path == "/directory" {
            result = json!({"newNonce":format!("{}/nonce",self.base),"newAccount":format!("{}/account",self.base),"newOrder":format!("{}/new-order",self.base)});
        } else if path != "/nonce" {
            let jws: Value =
                serde_json::from_slice(&to_bytes(request.into_body(), 1024 * 1024).await?)?;
            let protected: Value = serde_json::from_slice(
                &B64.decode(jws["protected"].as_str().context("JWS protected 缺失")?)?,
            )?;
            ensure!(
                protected["url"] == format!("{}{path}", self.base)
                    && protected["nonce"] == "fixture-nonce"
                    && protected["alg"] == "ES256",
                "JWS 地址/nonce/算法无效"
            );
            let jwk = if path == "/account" {
                let jwk = protected["jwk"].clone();
                *self.jwk.lock().unwrap() = Some(jwk.clone());
                jwk
            } else {
                ensure!(
                    protected["kid"] == format!("{}/account/1", self.base),
                    "账户地址错误"
                );
                self.jwk.lock().unwrap().clone().context("账户缺失")?
            };
            let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
            let x = openssl::bn::BigNum::from_slice(&B64.decode(jwk["x"].as_str().unwrap())?)?;
            let y = openssl::bn::BigNum::from_slice(&B64.decode(jwk["y"].as_str().unwrap())?)?;
            let key =
                PKey::from_ec_key(EcKey::from_public_key_affine_coordinates(&group, &x, &y)?)?;
            let raw = B64.decode(jws["signature"].as_str().unwrap())?;
            ensure!(raw.len() == 64, "签名长度错误");
            let signature = openssl::ecdsa::EcdsaSig::from_private_components(
                openssl::bn::BigNum::from_slice(&raw[..32])?,
                openssl::bn::BigNum::from_slice(&raw[32..])?,
            )?;
            let mut verifier = openssl::sign::Verifier::new(MessageDigest::sha256(), &key)?;
            verifier.update(
                format!(
                    "{}.{}",
                    jws["protected"].as_str().unwrap(),
                    jws["payload"].as_str().unwrap()
                )
                .as_bytes(),
            )?;
            ensure!(verifier.verify(&signature.to_der()?)?, "JWS 签名无效");
            let payload = B64.decode(jws["payload"].as_str().unwrap())?;
            let payload = if payload.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&payload)?
            };
            match path.as_str() {
                "/account" => {
                    *self.accounts.lock().unwrap() += 1;
                    ensure!(payload["termsOfServiceAgreed"] == true, "未同意条款");
                    headers.push(("location".into(), format!("{}/account/1", self.base)));
                    result = json!({"status":"valid","orders":format!("{}/orders",self.base)});
                    status = StatusCode::CREATED;
                }
                "/new-order" => {
                    ensure!(
                        payload["identifiers"]
                            == json!([{"type":"dns","value":"certificate.test"}]),
                        "订单域名错误"
                    );
                    *self.ready.lock().unwrap() = false;
                    *self.certificate.lock().unwrap() = None;
                    headers.push(("location".into(), format!("{}/order/1", self.base)));
                    result = self.order();
                    status = StatusCode::CREATED;
                }
                "/auth/1" => {
                    result = json!({"identifier":{"type":"dns","value":"certificate.test"},"status":if *self.ready.lock().unwrap(){"valid"}else{"pending"},"challenges":[{"type":"http-01","url":format!("{}/challenge",self.base),"status":"pending","token":"fixture-token"}]});
                }
                "/challenge" => {
                    use sha2::{Digest, Sha256};
                    let thumb = B64.encode(Sha256::digest(serde_json::to_vec(
                        &json!({"crv":jwk["crv"],"kty":jwk["kty"],"x":jwk["x"],"y":jwk["y"]}),
                    )?));
                    ensure!(
                        fs::read_to_string(
                            self.root
                                .join("web/.well-known/acme-challenge/fixture-token")
                        )? == format!("fixture-token.{thumb}"),
                        "HTTP-01 内容错误"
                    );
                    let challenge = self
                        .root
                        .join("web/.well-known/acme-challenge/fixture-token");
                    ensure!(
                        fs::metadata(&challenge)?.mode() & 0o777 == 0o644,
                        "HTTP-01 文件不可由验证服务器读取"
                    );
                    ensure!(
                        fs::metadata(challenge.parent().unwrap())?.mode() & 0o777 == 0o755,
                        "HTTP-01 目录不可遍历"
                    );
                    *self.ready.lock().unwrap() = true;
                    result = json!({"type":"http-01","url":format!("{}/challenge",self.base),"status":"valid","token":"fixture-token"});
                }
                "/order/1" => result = self.order(),
                "/finalize" => {
                    ensure!(*self.ready.lock().unwrap(), "未验证域名即签发");
                    let csr = X509Req::from_der(
                        &B64.decode(payload["csr"].as_str().context("CSR 缺失")?)?,
                    )?;
                    let public = csr.public_key()?;
                    ensure!(csr.verify(&public)?, "CSR 签名错误");
                    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
                    let issuer = PKey::from_ec_key(EcKey::generate(&group)?)?;
                    let mut cert = X509::builder()?;
                    cert.set_version(2)?;
                    let serial = openssl::bn::BigNum::from_u32(1)?.to_asn1_integer()?;
                    cert.set_serial_number(&serial)?;
                    cert.set_subject_name(csr.subject_name())?;
                    cert.set_issuer_name(csr.subject_name())?;
                    cert.set_pubkey(&public)?;
                    let start = Asn1Time::days_from_now(0)?;
                    let end = Asn1Time::days_from_now(90)?;
                    cert.set_not_before(&start)?;
                    cert.set_not_after(&end)?;
                    let name = if *self.bad.lock().unwrap() {
                        "wrong.test"
                    } else {
                        "certificate.test"
                    };
                    let san = SubjectAlternativeName::new()
                        .dns(name)
                        .build(&cert.x509v3_context(None, None))?;
                    cert.append_extension(san)?;
                    cert.sign(&issuer, MessageDigest::sha256())?;
                    *self.certificate.lock().unwrap() =
                        Some(String::from_utf8(cert.build().to_pem()?)?);
                    result = self.order();
                }
                "/certificate" => {
                    return Ok((
                        [
                            ("content-type", "application/pem-certificate-chain"),
                            ("replay-nonce", "fixture-nonce"),
                        ],
                        self.certificate
                            .lock()
                            .unwrap()
                            .clone()
                            .context("未签发证书")?,
                    )
                        .into_response());
                }
                _ => anyhow::bail!("未知 ACME 请求：{path}"),
            }
        }
        let mut response = (status, serde_json::to_vec(&result)?).into_response();
        for (key, value) in headers {
            response.headers_mut().insert(
                axum::http::HeaderName::from_bytes(key.as_bytes())?,
                value.parse()?,
            );
        }
        Ok(response)
    }
    fn order(&self) -> Value {
        let issued = self.certificate.lock().unwrap().is_some();
        json!({"status":if issued{"valid"}else if *self.ready.lock().unwrap(){"ready"}else{"pending"},"identifiers":[{"type":"dns","value":"certificate.test"}],"authorizations":[format!("{}/auth/1",self.base)],"finalize":format!("{}/finalize",self.base),"certificate":if issued{json!(format!("{}/certificate",self.base))}else{Value::Null}})
    }
}
fn builder() -> AccountBuilder {
    Account::builder_with_http(Box::new(
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http::<instant_acme::BodyWrapper<bytes::Bytes>>(),
    ))
}

#[tokio::test]
async fn real_jws_http01_csr_issue_reuse_renew_and_reject_bad_certificate() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = Ca {
        base: format!("http://{address}"),
        root: root.path().into(),
        jwk: Arc::new(Mutex::new(None)),
        certificate: Arc::new(Mutex::new(None)),
        ready: Arc::new(Mutex::new(false)),
        accounts: Arc::new(Mutex::new(0)),
        bad: Arc::new(Mutex::new(false)),
    };
    let router_state = state.clone();
    async fn handle(State(state): State<Ca>, request: Request) -> Response {
        let path = request.uri().path().to_string();
        match state.request(request).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ACME fixture {path}: {e:#}");
                (StatusCode::BAD_REQUEST,axum::Json(json!({"type":"urn:ietf:params:acme:error:malformed","detail":e.to_string(),"status":400}))).into_response()
            }
        }
    }
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(handle)).with_state(router_state),
        )
        .await
        .unwrap()
    });
    struct Abort(Option<tokio::task::JoinHandle<()>>);
    impl Drop for Abort {
        fn drop(&mut self) {
            if let Some(task) = &self.0 {
                task.abort();
            }
        }
    }
    let mut server = Abort(Some(task));
    let config = root.path().join("config");
    let web = root.path().join("web");
    let url = format!("http://{address}/directory");
    assert!(
        issue(
            &config,
            "certificate.test",
            "certificate.test",
            &web,
            builder(),
            &url
        )
        .await
        .unwrap()
    );
    assert!(
        remaining(&config, "certificate.test", "certificate.test")
            .unwrap()
            .unwrap()
            > 89 * 86400
    );
    let old = paths(&config, "certificate.test").canonicalize().unwrap();
    let old_key = fs::read(old.join("privkey.pem")).unwrap();
    assert_eq!(
        fs::metadata(old.join("privkey.pem")).unwrap().mode() & 0o777,
        0o600
    );
    assert!(
        !web.join(".well-known/acme-challenge/fixture-token")
            .exists()
    );
    assert!(
        !obtain(&config, "certificate.test", "certificate.test", &web)
            .await
            .unwrap()
    );
    assert!(
        issue(
            &config,
            "certificate.test",
            "certificate.test",
            &web,
            builder(),
            &url
        )
        .await
        .unwrap()
    );
    let current = paths(&config, "certificate.test").canonicalize().unwrap();
    assert_ne!(current, old);
    assert_eq!(fs::read(old.join("privkey.pem")).unwrap(), old_key);
    assert_eq!(*state.accounts.lock().unwrap(), 1);
    assert!(remaining(&config, "certificate.test", "other.test").is_err());
    *state.bad.lock().unwrap() = true;
    assert!(
        issue(
            &config,
            "certificate.test",
            "certificate.test",
            &web,
            builder(),
            &url
        )
        .await
        .is_err()
    );
    assert_eq!(
        paths(&config, "certificate.test").canonicalize().unwrap(),
        current
    );
    assert!(
        !web.join(".well-known/acme-challenge/fixture-token")
            .exists()
    );
    fs::write(current.join("privkey.pem"), "corrupted").unwrap();
    assert!(
        obtain(&config, "certificate.test", "certificate.test", &web)
            .await
            .is_err()
    );
    let task = server.0.take().unwrap();
    task.abort();
    let _ = task.await;
}
#[test]
fn rejects_unsafe_names_and_foreign_write_links() {
    for name in ["", "../escape", "a/b", "a\nb", ".bad", "bad.", "a..b"] {
        assert!(identifier(name).is_err());
    }
    let root = tempfile::tempdir().unwrap();
    let foreign = root.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    symlink(&foreign, root.path().join("config")).unwrap();
    assert!(private_dir(&root.path().join("config")).is_err());
}
