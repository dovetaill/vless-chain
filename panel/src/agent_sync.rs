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
        type StateFp = std::collections::BTreeMap<String, (u64, u64, bool, String)>;
        type FullFp =
            std::collections::BTreeMap<String, (u64, u64, Option<u64>, u64, u64, bool, u64, String)>;
        let mut sequence = 0;
        let mut last_saved_fp: Option<FullFp> = None;
        let mut last_saved_state: Option<StateFp> = None;
        let mut last_saved_at: Option<Instant> = None;
        let mut last_acked_fp: Option<FullFp> = None;
        let mut last_acked_had_rate = false;
        let mut last_acked_at: Option<Instant> = None;
        loop {
            let paged = self.paged.load(Ordering::Acquire);
            if !paged {
                last_acked_fp = None;
                last_acked_at = None;
            }
            let result = async {
                let rows = self.book.snapshot();
                let state_fp: StateFp = rows
                    .iter()
                    .map(|(k, v)| (k.clone(), (v.cycle_start, v.cycle_end, v.blocked, v.epoch.clone())))
                    .collect();
                let full_fp: FullFp = rows
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            (
                                v.total_upload,
                                v.total_download,
                                v.charged,
                                v.cycle_start,
                                v.cycle_end,
                                v.blocked,
                                v.connections,
                                v.epoch.clone(),
                            ),
                        )
                    })
                    .collect();
                let state_changed = last_saved_state.as_ref() != Some(&state_fp);
                let save_due = last_saved_fp.as_ref() != Some(&full_fp)
                    && (state_changed
                        || last_saved_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)));
                let saved = if save_due {
                    let book = self.book.clone();
                    let res = tokio::task::spawn_blocking(move || book.save()).await?;
                    if res.is_ok() {
                        last_saved_fp = Some(full_fp.clone());
                        last_saved_state = Some(state_fp);
                        last_saved_at = Some(Instant::now());
                    }
                    res
                } else {
                    self.book.check_ledger()
                };
                if paged {
                    let has_rate = rows
                        .values()
                        .any(|v| v.upload_bps > 0.0 || v.download_bps > 0.0);
                    let upload_due = last_acked_fp.as_ref() != Some(&full_fp)
                        || has_rate
                        || last_acked_had_rate
                        || last_acked_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(15));
                    if upload_due {
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
                        last_acked_fp = Some(full_fp);
                        last_acked_had_rate = has_rate;
                        last_acked_at = Some(Instant::now());
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
