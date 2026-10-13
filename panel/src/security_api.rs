use super::*;
use chain_panel::security::{self, Challenge};
use webauthn_rs::prelude::*;
macro_rules! ensure {($condition:expr,$($message:tt)*)=>{if !$condition {return Err(anyhow::anyhow!($($message)*).into());}};}

pub fn summary(s: &Security) -> Value {
    json!({"totp_enabled":!s.totp_secret.is_empty(),"recovery_remaining":s.recovery_hashes.len(),"origin":s.origin,
        "passkeys":s.passkeys.iter().map(|v|json!({"id":v["id"],"name":v["name"],"created":v["created"]})).collect::<Vec<_>>()})
}
pub async fn options(State(app): State<App>) -> Json<Value> {
    let s = &app.store.lock().unwrap().doc.security;
    Json(json!({"totp":!s.totp_secret.is_empty(),"passkey":!s.passkeys.is_empty()}))
}
#[derive(Deserialize)]
pub struct Proof {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub code: String,
}
fn reauthenticate(app: &App, proof: &Proof) -> ApiResult<()> {
    app.auth.rate_limit()?;
    if !equal(&digest(&proof.token), &app.admin_hash.lock().unwrap()) {
        return Err(error(StatusCode::UNAUTHORIZED, "当前管理员密钥不正确"));
    }
    let mut store = app.store.lock().unwrap();
    security::second_factor(&mut store.doc.security, &proof.code)?;
    store.save()?;
    Ok(())
}
#[derive(Deserialize)]
pub struct Rotate {
    #[serde(flatten)]
    proof: Proof,
    #[serde(default)]
    new_token: String,
}
pub async fn rotate(State(app): State<App>, Json(input): Json<Rotate>) -> ApiResult<Json<Value>> {
    reauthenticate(&app, &input.proof)?;
    let token = if input.new_token.is_empty() {
        secret()
    } else {
        input.new_token
    };
    if token.len() < 32 || token.len() > 512 || token.trim() != token {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "管理员密钥须为 32–512 字符，且两端无空白",
        ));
    }
    atomic_write(&app.token_file, token.as_bytes())?;
    *app.admin_hash.lock().unwrap() = digest(&token);
    app.sessions.lock().unwrap().clear();
    app.auth.challenges.lock().unwrap().clear();
    app.auth.enroll.lock().unwrap().clear();
    Ok(Json(
        json!({"token":token,"message":"旧密钥和全部登录会话已失效，请保存新密钥并重新登录"}),
    ))
}
pub async fn totp_start(
    State(app): State<App>,
    headers: HeaderMap,
    Json(proof): Json<Proof>,
) -> ApiResult<Json<Value>> {
    reauthenticate(&app, &proof)?;
    if !app
        .store
        .lock()
        .unwrap()
        .doc
        .security
        .totp_secret
        .is_empty()
    {
        return Err(error(StatusCode::CONFLICT, "请先关闭已有 TOTP 再重新绑定"));
    }
    let (secret, url) = security::new_totp()?;
    app.auth
        .enroll
        .lock()
        .unwrap()
        .retain(|_, (_, expires)| *expires > now());
    app.auth
        .enroll
        .lock()
        .unwrap()
        .insert(digest(&cookie(&headers)), (secret.clone(), now() + 300));
    Ok(Json(json!({"secret":secret,"url":url})))
}
#[derive(Deserialize)]
pub struct Code {
    code: String,
}
pub async fn totp_finish(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Code>,
) -> ApiResult<Json<Value>> {
    app.auth.rate_limit()?;
    let session = digest(&cookie(&headers));
    let (secret, expiry) = app
        .auth
        .enroll
        .lock()
        .unwrap()
        .get(&session)
        .cloned()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "请重新开始绑定"))?;
    if expiry <= now() {
        return Err(error(StatusCode::BAD_REQUEST, "绑定请求已过期"));
    }
    let mut setup = Security {
        totp_secret: secret,
        ..Default::default()
    };
    security::second_factor(&mut setup, &input.code)?;
    let codes: Vec<String> = (0..10).map(|_| secret_code()).collect();
    setup.recovery_hashes = codes.iter().map(|s| digest(s)).collect();
    {
        let mut store = app.store.lock().unwrap();
        ensure!(
            store.doc.security.totp_secret.is_empty(),
            "TOTP 已启用，请重新登录"
        );
        store.doc.security.totp_secret = setup.totp_secret;
        store.doc.security.totp_step = setup.totp_step;
        store.doc.security.recovery_hashes = setup.recovery_hashes;
        store.save()?;
    }
    app.auth.enroll.lock().unwrap().remove(&session);
    app.sessions.lock().unwrap().retain(|k, _| k == &session);
    Ok(Json(json!({"recovery_codes":codes})))
}
fn secret_code() -> String {
    let s = secret();
    format!("{}-{}-{}", &s[..6], &s[6..12], &s[12..18])
}
pub async fn totp_disable(
    State(app): State<App>,
    Json(proof): Json<Proof>,
) -> ApiResult<Json<Value>> {
    reauthenticate(&app, &proof)?;
    let mut store = app.store.lock().unwrap();
    store.doc.security.totp_secret.clear();
    store.doc.security.recovery_hashes.clear();
    store.doc.security.totp_step = 0;
    store.save()?;
    app.sessions.lock().unwrap().clear();
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
pub struct RegisterInput {
    #[serde(flatten)]
    proof: Proof,
    name: String,
    origin: String,
}
pub async fn register_start(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<RegisterInput>,
) -> ApiResult<Json<Value>> {
    reauthenticate(&app, &input.proof)?;
    ensure!(
        !input.name.trim().is_empty() && input.name.len() <= 120,
        "请输入 Passkey 名称"
    );
    let engine = security::engine(&input.origin)?;
    let origin = url::Url::parse(&input.origin)?
        .origin()
        .ascii_serialization();
    let excluded = {
        let store = app.store.lock().unwrap();
        let security = &store.doc.security;
        ensure!(
            security.passkeys.is_empty() || security.origin == origin,
            "已有 Passkey 绑定其他域名，请先删除旧凭据再更改域名"
        );
        security
            .passkeys
            .iter()
            .filter_map(|v| serde_json::from_value::<Passkey>(v["credential"].clone()).ok())
            .map(|p| p.cred_id().clone())
            .collect()
    };
    let user = uuid::Uuid::from_u128(0x6ad1249f0f034f88a4a7069b7f5466a1);
    let (options, state) = engine
        .start_passkey_registration(user, "administrator", "Chain Panel 管理员", Some(excluded))
        .map_err(anyhow::Error::from)?;
    let challenge = secret();
    app.auth.insert(
        challenge.clone(),
        Challenge::Register {
            state,
            session: digest(&cookie(&headers)),
            origin,
            name: input.name,
            expires: now() + 300,
        },
    )?;
    Ok(Json(json!({"challenge_id":challenge,"options":options})))
}
#[derive(Deserialize)]
pub struct CredentialInput {
    challenge_id: String,
    credential: Value,
}
pub async fn register_finish(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<CredentialInput>,
) -> ApiResult<Json<Value>> {
    let Challenge::Register {
        state,
        session,
        origin,
        name,
        ..
    } = app.auth.take(&input.challenge_id)?
    else {
        return Err(error(StatusCode::BAD_REQUEST, "验证类型不匹配"));
    };
    ensure!(
        equal(&session, &digest(&cookie(&headers))),
        "注册会话不匹配"
    );
    let credential: RegisterPublicKeyCredential = serde_json::from_value(input.credential)?;
    let passkey = security::engine(&origin)?
        .finish_passkey_registration(&credential, &state)
        .map_err(anyhow::Error::from)?;
    let mut store = app.store.lock().unwrap();
    ensure!(
        store.doc.security.passkeys.len() < 20,
        "最多保存 20 个 Passkey"
    );
    ensure!(
        store.doc.security.passkeys.is_empty() || store.doc.security.origin == origin,
        "认证域名已变化，请重新开始"
    );
    store.doc.security.origin = origin;
    store
        .doc
        .security
        .passkeys
        .push(json!({"id":id(),"name":name,"created":now(),"credential":passkey}));
    store.save()?;
    Ok(Json(json!({"ok":true})))
}
pub async fn login_start(State(app): State<App>) -> ApiResult<Json<Value>> {
    app.auth.rate_limit()?;
    let (origin, keys) = {
        let store = app.store.lock().unwrap();
        let s = &store.doc.security;
        (
            s.origin.clone(),
            s.passkeys
                .iter()
                .map(|v| serde_json::from_value::<Passkey>(v["credential"].clone()))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        )
    };
    ensure!(!keys.is_empty(), "尚未绑定 Passkey");
    let (options, state) = security::engine(&origin)?
        .start_passkey_authentication(&keys)
        .map_err(anyhow::Error::from)?;
    let challenge = secret();
    app.auth.insert(
        challenge.clone(),
        Challenge::Login {
            state,
            origin,
            expires: now() + 120,
        },
    )?;
    Ok(Json(json!({"challenge_id":challenge,"options":options})))
}
pub async fn login_finish(
    State(app): State<App>,
    Json(input): Json<CredentialInput>,
) -> ApiResult<Response> {
    let Challenge::Login { state, origin, .. } = app.auth.take(&input.challenge_id)? else {
        return Err(error(StatusCode::BAD_REQUEST, "验证类型不匹配"));
    };
    let credential: PublicKeyCredential = serde_json::from_value(input.credential)?;
    let result = security::engine(&origin)?
        .finish_passkey_authentication(&credential, &state)
        .map_err(anyhow::Error::from)?;
    {
        let mut store = app.store.lock().unwrap();
        ensure!(
            store.doc.security.origin == origin,
            "认证配置已变化，请重新登录"
        );
        let mut matched = false;
        for value in &mut store.doc.security.passkeys {
            let mut key: Passkey = serde_json::from_value(value["credential"].clone())?;
            if key.update_credential(&result).is_some() {
                value["credential"] = json!(key);
                matched = true;
            }
        }
        ensure!(matched, "该 Passkey 已被撤销");
        store.save()?;
    }
    Ok(session_response(&app))
}
#[derive(Deserialize)]
pub struct Remove {
    #[serde(flatten)]
    proof: Proof,
    id: String,
}
pub async fn remove_passkey(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Remove>,
) -> ApiResult<Json<Value>> {
    reauthenticate(&app, &input.proof)?;
    let mut store = app.store.lock().unwrap();
    store
        .doc
        .security
        .passkeys
        .retain(|v| v["id"].as_str() != Some(&input.id));
    store.save()?;
    let session = digest(&cookie(&headers));
    app.sessions.lock().unwrap().retain(|k, _| k == &session);
    app.auth.challenges.lock().unwrap().clear();
    Ok(Json(json!({"ok":true})))
}
