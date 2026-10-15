use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use x25519_dalek::{PublicKey, StaticSecret};

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn log_level() -> String {
    let level = std::env::var("CHAIN_XRAY_LOG_LEVEL").unwrap_or_else(|_| "warning".into());
    if matches!(
        level.as_str(),
        "none" | "error" | "warning" | "info" | "debug"
    ) {
        level
    } else {
        "warning".into()
    }
}
pub fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Reality {
    pub target: String,
    pub server_name: String,
    pub private_key: String,
    pub public_key: String,
    pub short_id: String,
}
impl Reality {
    pub fn new(target: String, server_name: String) -> Self {
        let private = StaticSecret::random_from_rng(rand::rngs::OsRng);
        Self {
            target,
            server_name,
            private_key: URL_SAFE_NO_PAD.encode(private.to_bytes()),
            public_key: URL_SAFE_NO_PAD.encode(PublicKey::from(&private).as_bytes()),
            short_id: secret()[..16].into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub host: String,
    pub reality: Reality,
    pub token_hash: String,
    #[serde(default)]
    pub seen: u64,
    #[serde(default)]
    pub report: Report,
    #[serde(default)]
    pub desired: Option<Desired>,
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Report {
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub traffic: BTreeMap<String, Traffic>,
    #[serde(default)]
    pub system: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Desired {
    pub revision: String,
    pub config: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct External {
    pub id: String,
    pub name: String,
    pub url: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Vless,
    Forward,
    Website,
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Vless,
    Http,
    Socks,
    Shadowsocks,
    Trojan,
    Vmess,
    Hysteria2,
    Tuic,
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
pub struct Tls {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub cert: String,
    #[serde(default)]
    pub key: String,
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
pub struct Access {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub public_host: String,
    #[serde(default)]
    pub public_port: Option<u16>,
    #[serde(default)]
    pub tls: Tls,
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
pub struct Limits {
    #[serde(default)]
    pub upload_mbps: Option<f64>,
    #[serde(default)]
    pub download_mbps: Option<f64>,
    #[serde(default)]
    pub monthly_gib: Option<f64>,
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
pub struct Traffic {
    pub epoch: String,
    pub month: String,
    pub upload: u64,
    pub download: u64,
    pub total_upload: u64,
    pub total_download: u64,
    pub upload_bps: f64,
    pub download_bps: f64,
    pub connections: u64,
    pub sampled_at: u64,
    pub blocked: bool,
    #[serde(default)]
    pub days: BTreeMap<String, [u64; 2]>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Hop {
    pub node_id: String,
    pub port: u16,
    pub uuid: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Line {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub hops: Vec<Hop>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub chain: Vec<String>,
    #[serde(default)]
    pub target: String,
    #[serde(default = "default_network")]
    pub network: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub access: Access,
    #[serde(default)]
    pub limits: Limits,
}

impl Line {
    pub fn proxy_chain(&self) -> Vec<String> {
        if self.chain.is_empty() {
            self.external_id
                .iter()
                .map(|id| format!("external:{id}"))
                .collect()
        } else {
            self.chain.clone()
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Security {
    #[serde(default)]
    pub totp_secret: String,
    #[serde(default)]
    pub totp_step: u64,
    #[serde(default)]
    pub recovery_hashes: Vec<String>,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub passkeys: Vec<Value>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub token_hash: String,
    pub line_ids: Vec<String>,
    pub created: u64,
}
fn default_network() -> String {
    "tcp".into()
}
fn yes() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub at: u64,
    pub message: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub state: String,
    pub started: u64,
    pub events: Vec<Event>,
    pub before: BTreeMap<String, Option<Desired>>,
    pub target: BTreeMap<String, Desired>,
    pub order: Vec<String>,
    pub lines: Vec<Line>,
    pub nodes: Vec<Node>,
    pub externals: Vec<External>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub externals: Vec<External>,
    pub lines: Vec<Line>,
    pub applied_lines: Vec<Line>,
    pub applied_nodes: Vec<Node>,
    pub applied_externals: Vec<External>,
    pub checks: BTreeMap<String, Value>,
    pub jobs: Vec<Job>,
    #[serde(default)]
    pub security: Security,
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
}
impl Document {
    pub fn busy(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| matches!(j.state.as_str(), "publishing" | "rolling_back"))
    }
}
