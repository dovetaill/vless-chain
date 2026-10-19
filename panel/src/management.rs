//! Persisted control-plane settings. Secrets are never returned by the settings view.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub interval: u64,
    pub failures: u32,
    pub recoveries: u32,
    pub offline: u64,
    pub reminder: u64,
    pub url: String,
    pub quota: Vec<u32>,
    pub expiry_days: Vec<u32>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: 300,
            failures: 3,
            recoveries: 2,
            offline: 60,
            reminder: 86400,
            url: "https://example.com/".into(),
            quota: vec![80, 95, 100],
            expiry_days: vec![7, 3, 1, 0],
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (60..=86400).contains(&self.interval),
            "检测间隔须为 60～86400 秒"
        );
        ensure!(
            (1..=20).contains(&self.failures) && (1..=20).contains(&self.recoveries),
            "连续次数须为 1～20"
        );
        ensure!(
            (30..=3600).contains(&self.offline),
            "失联阈值须为 30～3600 秒"
        );
        ensure!(
            (3600..=604800).contains(&self.reminder),
            "持续故障提醒间隔须为 1 小时～7 天"
        );
        let url = url::Url::parse(&self.url)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && self.url.len() <= 2048,
            "检测网址无效"
        );
        ensure!(
            !self.quota.is_empty()
                && self.quota.len() <= 5
                && self.quota.iter().all(|n| (1..=100).contains(n)),
            "流量阈值须为 1～100，最多 5 档"
        );
        ensure!(
            !self.expiry_days.is_empty()
                && self.expiry_days.len() <= 8
                && self.expiry_days.iter().all(|n| *n <= 90),
            "到期提醒须为 0～90 天，最多 8 档"
        );
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Channels {
    pub telegram_token: String,
    pub telegram_chat: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_starttls: bool,
    pub smtp_user: String,
    pub smtp_password: String,
    pub mail_from: String,
    pub mail_to: String,
}
impl Channels {
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.telegram_token,
            &self.telegram_chat,
            &self.smtp_host,
            &self.smtp_user,
            &self.smtp_password,
            &self.mail_from,
            &self.mail_to,
        ] {
            ensure!(
                value.len() <= 1024 && !value.chars().any(|c| c.is_control()),
                "通知配置包含无效字符"
            );
        }
        if !self.telegram_token.is_empty() {
            ensure!(
                self.telegram_token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_:-".contains(c))
                    && !self.telegram_chat.is_empty(),
                "Telegram 配置不完整"
            );
        }
        if !self.smtp_host.is_empty() {
            ensure!(
                self.smtp_port > 0 && !self.mail_from.is_empty() && !self.mail_to.is_empty(),
                "邮件配置不完整"
            );
            for address in [&self.mail_from, &self.mail_to] {
                ensure!(
                    address.is_ascii()
                        && address.contains('@')
                        && !address.contains(['<', '>', ',', ' ', ';']),
                    "请填写单个邮件地址"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Condition {
    pub bad: u32,
    pub good: u32,
    pub active: bool,
    pub checked: u64,
    pub last_notice: u64,
    pub message: String,
}
impl Condition {
    pub fn observe(
        &mut self,
        failed: bool,
        message: String,
        at: u64,
        policy: &Policy,
        immediate: bool,
    ) -> Option<String> {
        if at.saturating_sub(self.checked) > policy.interval.saturating_mul(3) {
            self.bad = 0;
            self.good = 0;
        }
        self.checked = at;
        self.message = message.clone();
        if failed {
            self.good = 0;
            self.bad = self.bad.saturating_add(1);
            if (immediate || self.bad >= policy.failures)
                && (!self.active || at.saturating_sub(self.last_notice) >= policy.reminder)
            {
                self.active = true;
                self.last_notice = at;
                return Some(format!("故障：{message}"));
            }
        } else {
            self.bad = 0;
            self.good = self.good.saturating_add(1);
            if self.active && (immediate || self.good >= policy.recoveries) {
                self.active = false;
                self.last_notice = at;
                return Some(format!("恢复：{message}"));
            }
        }
        None
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub at: u64,
    pub message: String,
    pub telegram_done: bool,
    pub mail_done: bool,
    pub attempts: u32,
    pub retry_at: u64,
    pub error: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Monitoring {
    pub local: Policy,
    pub override_policy: Option<Policy>,
    pub channels: Channels,
    pub conditions: BTreeMap<String, Condition>,
    pub notices: Vec<Notice>,
    pub milestones: BTreeMap<String, u64>,
    pub checks: BTreeMap<String, Value>,
}
impl Monitoring {
    pub fn policy(&self) -> Policy {
        self.override_policy
            .clone()
            .unwrap_or_else(|| self.local.clone())
    }
    pub fn notice(&mut self, message: String, at: u64) {
        self.notices.push(Notice {
            id: crate::model::id(),
            at,
            message,
            telegram_done: false,
            mail_done: false,
            attempts: 0,
            retry_at: 0,
            error: String::new(),
        });
        if self.notices.len() > 500 {
            self.notices.remove(0);
        }
    }
    pub fn milestone(&mut self, key: String, message: String, at: u64) {
        if self.milestones.insert(key, at).is_none() {
            self.notice(message, at);
        }
        self.milestones
            .retain(|_, seen| at.saturating_sub(*seen) < 366 * 86400);
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Remote {
    pub id: String,
    pub name: String,
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub seen: u64,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub snapshot: Value,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fleet {
    pub instance_id: String,
    pub remotes: Vec<Remote>,
    pub controller_hash: String,
    pub controller_name: String,
    pub invitation_hash: String,
    pub invitation_expires: u64,
    pub policy: Option<Policy>,
    pub receipts: BTreeMap<String, Value>,
}

pub fn panel_url(value: &str) -> Result<String> {
    let url = url::Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none(),
        "远端面板须使用不含凭据的 HTTPS 根地址"
    );
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Machine authorization is deliberately narrower than an administrator session.
pub fn machine_route(method: &str, path: &str) -> bool {
    let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
    if parts.first() != Some(&"api") || path.contains('%') || path.contains("..") {
        return false;
    }
    match (method, parts.as_slice()) {
        ("GET", ["api", "federation", "state" | "document" | "task"]) => true,
        ("POST", ["api", "federation", "policy" | "ticket" | "revoke" | "task"]) => true,
        ("POST", ["api", "lines" | "externals" | "preview" | "publish"]) => true,
        ("POST", ["api", "lines", "operate" | "batch"]) => true,
        ("POST", ["api", "reality", "check"]) => true,
        ("POST" | "DELETE", ["api", "externals", id]) => uuid::Uuid::parse_str(id).is_ok(),
        ("DELETE", ["api", "lines", id]) => uuid::Uuid::parse_str(id).is_ok(),
        ("GET", ["api", "lines" | "externals", id, "export"]) => uuid::Uuid::parse_str(id).is_ok(),
        ("POST", ["api", "lines", id, "probe"]) => uuid::Uuid::parse_str(id).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_failures_recovery_and_restart_state() {
        let p = Policy::default();
        let mut c = Condition::default();
        assert!(c.observe(true, "test".into(), 300, &p, false).is_none());
        assert!(c.observe(true, "test".into(), 600, &p, false).is_none());
        assert!(c.observe(true, "test".into(), 900, &p, false).is_some());
        c = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(c.observe(true, "test".into(), 1200, &p, false).is_none());
        assert!(c.observe(false, "test".into(), 1500, &p, false).is_none());
        assert!(
            c.observe(false, "test".into(), 1800, &p, false)
                .unwrap()
                .starts_with("恢复")
        );
    }
    #[test]
    fn scope_and_url_reject_privilege_expansion() {
        assert!(!machine_route("POST", "/api/security/token"));
        assert!(!machine_route("GET", "/api/state"));
        assert!(!machine_route("POST", "/api/fleet/connect"));
        assert!(machine_route("POST", "/api/lines"));
        for url in [
            "http://panel.test",
            "https://a:b@panel.test",
            "https://panel.test/path",
            "https://panel.test/?x=1",
        ] {
            assert!(panel_url(url).is_err());
        }
        assert_eq!(
            panel_url("https://panel.test/").unwrap(),
            "https://panel.test"
        );
    }
    #[test]
    fn override_preserves_local_and_milestone_deduplicates() {
        let mut m = Monitoring::default();
        let mut p = Policy::default();
        p.interval = 600;
        m.override_policy = Some(p);
        assert_eq!(m.policy().interval, 600);
        m.override_policy = None;
        assert_eq!(m.policy().interval, 300);
        m.milestone("a".into(), "quota".into(), 1);
        m.milestone("a".into(), "quota".into(), 2);
        assert_eq!(m.notices.len(), 1);
    }
}
