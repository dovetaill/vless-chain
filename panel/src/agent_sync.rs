use anyhow::{Result, bail};
use chain_panel::{
    gateway::Book,
    model::{Desired, Report},
    traffic_sync::{Heartbeat, pages},
};
use reqwest::{Client, StatusCode};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub struct Sync {
    client: Client,
    base: String,
    token: String,
    book: Arc<Book>,
    session: uuid::Uuid,
    paged: AtomicBool,
    legacy_until: Mutex<Option<Instant>>,
    snapshot_error: Mutex<Option<String>>,
}

impl Sync {
    pub fn new(client: Client, base: String, token: String, book: Arc<Book>) -> Self {
        Self {
            client,
            base,
            token,
            book,
            session: uuid::Uuid::new_v4(),
            paged: AtomicBool::new(false),
            legacy_until: Mutex::default(),
            snapshot_error: Mutex::default(),
        }
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}/{path}", self.base))
            .bearer_auth(&self.token)
    }

    pub async fn report(&self, report: &mut Report) -> Result<Option<Desired>> {
        report.system["traffic_error"] =
            serde_json::json!(self.book.check_ledger().err().map(|e| e.to_string()));
        report.system["traffic_sync_error"] =
            serde_json::json!(*self.snapshot_error.lock().unwrap());
        {
            let errors = self.book.listener_errors.lock().unwrap();
            report.system["listener_error_count"] = serde_json::json!(errors.len());
            report.system["listener_errors"] = serde_json::json!(
                errors
                    .iter()
                    .take(16)
                    .collect::<std::collections::BTreeMap<_, _>>()
            );
        }
        let legacy = self
            .legacy_until
            .lock()
            .unwrap()
            .is_some_and(|at| at > Instant::now());
        if !legacy {
            let response = self
                .post("heartbeat")
                .json(&Heartbeat {
                    session: self.session,
                    report: &*report,
                })
                .send()
                .await?;
            if !matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ) {
                let desired = response.error_for_status()?.json().await?;
                self.paged.store(true, Ordering::Release);
                return Ok(desired);
            }
            self.paged.store(false, Ordering::Release);
            *self.legacy_until.lock().unwrap() = Some(Instant::now() + Duration::from_secs(60));
        }
        let full = Report {
            traffic: self.book.snapshot(),
            ..report.clone()
        };
        Ok(self
            .post("sync")
            .json(&full)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn statistics(self: Arc<Self>) {
        let mut sequence = 0;
        loop {
            let paged = self.paged.load(Ordering::Acquire);
            let book = self.book.clone();
            let result = async {
                let (saved, rows) = tokio::task::spawn_blocking(move || {
                    (book.save(), paged.then(|| book.snapshot()))
                })
                .await?;
                if let Some(rows) = rows {
                    sequence += 1;
                    for page in pages(self.session, sequence, rows) {
                        let page = page?;
                        let send = || self.post("traffic").json(&page).send();
                        let response = match send().await {
                            Ok(response) if !response.status().is_server_error() => response,
                            _ => send().await?,
                        };
                        let status = response.status();
                        if status != StatusCode::NO_CONTENT {
                            let message = response
                                .json::<serde_json::Value>()
                                .await
                                .ok()
                                .and_then(|v| {
                                    v["error"]
                                        .as_str()
                                        .map(|s| s.chars().take(256).collect::<String>())
                                })
                                .unwrap_or_default();
                            bail!(
                                "流量快照第 {} 页未确认，HTTP {status} {message}",
                                page.part + 1
                            );
                        }
                    }
                }
                saved
            }
            .await;
            *self.snapshot_error.lock().unwrap() = result.err().map(|e| format!("{e:#}"));
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}
