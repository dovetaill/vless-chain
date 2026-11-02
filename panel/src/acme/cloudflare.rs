use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::Path, time::Duration};

pub(super) struct Challenges {
    client: reqwest::Client,
    token: String,
    base: String,
    records: Vec<(String, String)>,
}
impl Challenges {
    pub fn new(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        ensure!(
            metadata.is_file() && metadata.mode() & 0o077 == 0,
            "Cloudflare 凭据必须仅属主可读"
        );
        let body = fs::read_to_string(path)?;
        let token = body
            .lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == "dns_cloudflare_api_token")
            .map(|(_, v)| v.trim())
            .context("缺少 Cloudflare API token")?;
        ensure!(
            !token.is_empty()
                && token
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            "Cloudflare API token 格式无效"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(12))
                .build()?,
            token: token.into(),
            base: "https://api.cloudflare.com/client/v4".into(),
            records: vec![],
        })
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("Cloudflare 请求失败")?;
        ensure!(
            response.status().is_success(),
            "Cloudflare 请求失败（HTTP {}）",
            response.status()
        );
        ensure!(
            response.content_length().is_none_or(|n| n <= 1024 * 1024),
            "Cloudflare 响应过大"
        );
        let bytes = response.bytes().await?;
        ensure!(bytes.len() <= 1024 * 1024, "Cloudflare 响应过大");
        let value: Value = serde_json::from_slice(&bytes).context("Cloudflare 响应无效")?;
        ensure!(
            value["success"] == true,
            "Cloudflare 操作失败，请检查 token 权限"
        );
        Ok(value["result"].clone())
    }
    pub async fn create(&mut self, domain: &str, value: &str) -> Result<()> {
        let mut zone_name = domain;
        let zone = loop {
            let zones = self
                .request(
                    reqwest::Method::GET,
                    &format!("/zones?name={zone_name}&status=active"),
                    None,
                )
                .await?;
            let zones = zones.as_array().context("Cloudflare zone 响应无效")?;
            ensure!(zones.len() <= 1, "Cloudflare zone 不唯一");
            if let Some(zone) = zones.first() {
                break zone["id"]
                    .as_str()
                    .context("Cloudflare zone 缺少 ID")?
                    .to_string();
            }
            zone_name = zone_name
                .split_once('.')
                .context("未找到域名对应的 Cloudflare zone")?
                .1;
        };
        ensure!(
            zone.len() == 32 && zone.bytes().all(|c| c.is_ascii_hexdigit()),
            "Cloudflare zone ID 无效"
        );
        let record=self.request(reqwest::Method::POST,&format!("/zones/{zone}/dns_records"),Some(json!({"type":"TXT","name":format!("_acme-challenge.{domain}"),"content":value,"ttl":120}))).await?;
        let id = record["id"]
            .as_str()
            .context("Cloudflare DNS 记录缺少 ID")?;
        ensure!(
            id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit()),
            "Cloudflare DNS 记录 ID 无效"
        );
        self.records.push((zone, id.into()));
        Ok(())
    }
    pub async fn clear(&mut self) -> Result<()> {
        while let Some((zone, id)) = self.records.last() {
            self.request(
                reqwest::Method::DELETE,
                &format!("/zones/{zone}/dns_records/{id}"),
                None,
            )
            .await?;
            self.records.pop();
        }
        Ok(())
    }
}
impl Drop for Challenges {
    fn drop(&mut self) {
        if self.records.is_empty() {
            return;
        }
        let mut pending = Self {
            client: self.client.clone(),
            token: self.token.clone(),
            base: self.base.clone(),
            records: std::mem::take(&mut self.records),
        };
        // Cancellation must also remove only the TXT records created by this order.
        let result = std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| runtime.block_on(pending.clear()));
            if result.is_err() {
                for (zone, id) in &pending.records {
                    eprintln!("DNS 挑战清理未完成：zone={zone}, record={id}");
                }
            }
            pending.records.clear();
            result
        })
        .join();
        if !matches!(result, Ok(Ok(()))) {
            eprintln!("Cloudflare DNS 挑战清理失败，请按上述记录 ID 检查");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path as ApiPath, Query, State},
        http::{HeaderMap, StatusCode},
        routing::{delete, get, post},
    };
    use std::{
        collections::BTreeMap,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex},
    };
    #[derive(Default)]
    struct Api {
        records: BTreeMap<String, Value>,
        deleted: Vec<String>,
    }
    async fn zones(
        headers: HeaderMap,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer isolated-token");
        Json(
            json!({"success":true,"result":if query["name"]=="example.test"{json!([{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}])}else{json!([])}}),
        )
    }
    async fn create(State(state): State<Arc<Mutex<Api>>>, Json(value): Json<Value>) -> Json<Value> {
        assert_eq!(value["type"], "TXT");
        assert_eq!(value["name"], "_acme-challenge.sub.example.test");
        assert_eq!(value["ttl"], 120);
        let mut state = state.lock().unwrap();
        let id = format!("{:032x}", state.records.len() + 1);
        state.records.insert(id.clone(), value);
        Json(json!({"success":true,"result":{"id":id}}))
    }
    async fn remove(
        State(state): State<Arc<Mutex<Api>>>,
        ApiPath((zone, id)): ApiPath<(String, String)>,
    ) -> (StatusCode, Json<Value>) {
        assert_eq!(zone, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let mut state = state.lock().unwrap();
        assert!(state.records.remove(&id).is_some());
        state.deleted.push(id.clone());
        (
            StatusCode::OK,
            Json(json!({"success":true,"result":{"id":id}})),
        )
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn creates_exact_txt_and_cleans_on_success_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let credential = root.path().join("token.ini");
        fs::write(&credential, "dns_cloudflare_api_token = isolated-token\n").unwrap();
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Challenges::new(&credential).is_err());
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
        let configured = Challenges::new(&credential).unwrap();
        assert!(configured.records.is_empty());
        drop(configured);
        let state = Arc::new(Mutex::new(Api::default()));
        state
            .lock()
            .unwrap()
            .records
            .insert("existing".into(), json!({"content":"preserve-other-order"}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/zones", get(zones))
            .route("/zones/{zone}/dns_records", post(create))
            .route("/zones/{zone}/dns_records/{id}", delete(remove))
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let new = || Challenges {
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            token: "isolated-token".into(),
            base: format!("http://127.0.0.1:{port}"),
            records: vec![],
        };
        let mut success = new();
        success
            .create("sub.example.test", "first-validation")
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().records.len(), 2);
        success.clear().await.unwrap();
        assert!(success.records.is_empty());
        drop(success);
        let mut cancelled = new();
        cancelled
            .create("sub.example.test", "cancelled-validation")
            .await
            .unwrap();
        drop(cancelled);
        let result = state.lock().unwrap();
        assert_eq!(result.deleted.len(), 2);
        assert_eq!(result.records.len(), 1);
        assert_eq!(
            result.records["existing"]["content"],
            "preserve-other-order"
        );
        drop(result);
        server.abort();
        let _ = server.await;
    }
}
