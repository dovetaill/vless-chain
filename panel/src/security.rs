use crate::model::{Security, now};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Mutex};
use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, Secret, TOTP};
use webauthn_rs::prelude::*;

pub fn digest(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
pub fn equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
pub fn totp(secret: &str) -> Result<TOTP> {
    Ok(TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.into()).to_bytes()?,
        Some("Chain Panel".into()),
        "administrator".into(),
    )?)
}
pub fn new_totp() -> Result<(String, String)> {
    let secret = Secret::generate_secret().to_encoded().to_string();
    Ok((secret.clone(), totp(&secret)?.get_url()))
}
pub fn second_factor(security: &mut Security, code: &str) -> Result<()> {
    if security.totp_secret.is_empty() {
        return Ok(());
    }
    let t = totp(&security.totp_secret)?;
    let step = now() / 30;
    if let Some(valid) = [step.saturating_sub(1), step, step + 1]
        .into_iter()
        .find(|s| *s > security.totp_step && equal(&t.generate(*s * 30), code))
    {
        security.totp_step = valid;
        return Ok(());
    }
    let hash = digest(code.trim());
    if let Some(index) = security
        .recovery_hashes
        .iter()
        .position(|v| equal(v, &hash))
    {
        security.recovery_hashes.remove(index);
        return Ok(());
    }
    anyhow::bail!("请输入未使用的动态验证码或恢复码")
}
pub fn engine(origin: &str) -> Result<Webauthn> {
    let url = url::Url::parse(origin).context("请输入完整 HTTPS 控制台地址")?;
    let host = url.host_str().context("控制台地址缺少域名")?;
    ensure!(
        url.scheme() == "https" || host == "localhost" && url.scheme() == "http",
        "Passkey 需要 HTTPS 域名（本机 localhost 除外）"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "" | "/"),
        "Passkey 地址不得含路径、认证信息或查询"
    );
    ensure!(
        matches!(url.host(), Some(url::Host::Domain(_))),
        "Passkey 请使用稳定的域名"
    );
    Ok(WebauthnBuilder::new(host, &url)?
        .rp_name("Chain Panel")
        .build()?)
}
pub enum Challenge {
    Register {
        state: PasskeyRegistration,
        session: String,
        origin: String,
        name: String,
        expires: u64,
    },
    Login {
        state: PasskeyAuthentication,
        origin: String,
        expires: u64,
    },
}
impl Challenge {
    pub fn expires(&self) -> u64 {
        match self {
            Self::Register { expires, .. } | Self::Login { expires, .. } => *expires,
        }
    }
}
#[derive(Default)]
pub struct AuthState {
    pub challenges: Mutex<HashMap<String, Challenge>>,
    pub enroll: Mutex<HashMap<String, (String, u64)>>,
    pub failures: Mutex<Vec<u64>>,
}
impl AuthState {
    pub fn rate_limit(&self) -> Result<()> {
        let mut recent = self.failures.lock().unwrap();
        recent.retain(|t| now().saturating_sub(*t) < 60);
        ensure!(recent.len() < 30, "登录尝试过于频繁，请一分钟后重试");
        recent.push(now());
        Ok(())
    }
    pub fn insert(&self, id: String, challenge: Challenge) -> Result<()> {
        let mut pending = self.challenges.lock().unwrap();
        pending.retain(|_, v| v.expires() > now());
        ensure!(pending.len() < 100, "验证请求过多，请稍后重试");
        pending.insert(id, challenge);
        Ok(())
    }
    pub fn take(&self, id: &str) -> Result<Challenge> {
        let c = self
            .challenges
            .lock()
            .unwrap()
            .remove(id)
            .context("验证已使用或不存在，请重新开始")?;
        ensure!(c.expires() > now(), "验证已过期，请重新开始");
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn totp_and_recovery_codes_cannot_be_replayed() {
        let (secret, _) = new_totp().unwrap();
        let code = totp(&secret).unwrap().generate_current().unwrap();
        let mut s = Security {
            totp_secret: secret,
            recovery_hashes: vec![digest("one-use")],
            ..Default::default()
        };
        second_factor(&mut s, &code).unwrap();
        assert!(second_factor(&mut s, &code).is_err());
        second_factor(&mut s, "one-use").unwrap();
        assert!(second_factor(&mut s, "one-use").is_err());
    }
    #[test]
    fn passkeys_require_stable_secure_origins() {
        assert!(engine("https://panel.example.com").is_ok());
        assert!(engine("http://panel.example.com").is_err());
        assert!(engine("https://192.0.2.1").is_err());
        assert!(engine("https://panel.example.com/path").is_err());
    }
}
