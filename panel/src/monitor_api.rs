use super::*;
use chain_panel::management::{Channels, Policy};

pub fn routes(router: Router<App>) -> Router<App> {
    router
        .route("/api/monitoring", get(view).post(save))
        .route("/api/monitoring/test", post(test))
}
async fn view(State(app): State<App>) -> Json<Value> {
    let s = app.store.lock().unwrap();
    let m = &s.doc.monitoring;
    let mut channels = m.channels.clone();
    let telegram_set = !channels.telegram_token.is_empty();
    let password_set = !channels.smtp_password.is_empty();
    channels.telegram_token.clear();
    channels.smtp_password.clear();
    Json(
        json!({"local":m.local,"effective":m.policy(),"override":m.override_policy,
        "controller":s.doc.fleet.controller_name,"channels":channels,"telegram_set":telegram_set,"password_set":password_set,
        "conditions":m.conditions,"checks":m.checks,"notices":m.notices.iter().rev().take(100).collect::<Vec<_>>()}),
    )
}
#[derive(Deserialize)]
struct Settings {
    local: Policy,
    channels: Channels,
    #[serde(default)]
    clear_telegram: bool,
    #[serde(default)]
    clear_password: bool,
}
async fn save(State(app): State<App>, Json(mut input): Json<Settings>) -> ApiResult<Json<Value>> {
    input.local.validate()?;
    edit(&app, |d| {
        if input.channels.telegram_token.is_empty() && !input.clear_telegram {
            input.channels.telegram_token = d.monitoring.channels.telegram_token.clone();
        }
        if input.channels.smtp_password.is_empty() && !input.clear_password {
            input.channels.smtp_password = d.monitoring.channels.smtp_password.clone();
        }
        input.channels.validate()?;
        d.monitoring.local = input.local;
        d.monitoring.channels = input.channels;
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn test(State(app): State<App>) -> ApiResult<Json<Value>> {
    edit(&app, |d| {
        ensure!(
            d.fleet.controller_hash.is_empty(),
            "已接入总控，请在总控测试通知"
        );
        ensure!(
            !d.monitoring.channels.telegram_token.is_empty()
                || !d.monitoring.channels.smtp_host.is_empty(),
            "请先配置通知渠道"
        );
        d.monitoring.notice("Chain Panel 测试通知".into(), now());
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
fn observe(
    doc: &mut Document,
    key: String,
    failed: bool,
    message: String,
    policy: &Policy,
    immediate: bool,
) {
    if let Some(message) = doc.monitoring.conditions.entry(key).or_default().observe(
        failed,
        message,
        now(),
        policy,
        immediate,
    ) {
        doc.monitoring.notice(message, now());
    }
}
fn local_conditions(doc: &mut Document, policy: &Policy) {
    let at = now();
    for n in doc.nodes.clone() {
        let offline = at.saturating_sub(n.seen) >= policy.offline;
        observe(
            doc,
            format!("agent:{}", n.id),
            offline,
            format!("{}：Agent 连接", n.name),
            policy,
            true,
        );
        if offline {
            continue;
        }
        observe(
            doc,
            format!("runtime:{}", n.id),
            !n.report.running || !n.report.error.is_empty(),
            format!("{}：代理运行组件，请查看节点应用结果", n.name),
            policy,
            true,
        );
        let clock = &n.report.system["clock"];
        let sample = clock["checked_at"].as_u64().unwrap_or(0);
        let key = format!("clock:{}", n.id);
        let previous = doc.monitoring.checks.get(&key).and_then(|v| v["checked_at"].as_u64()).unwrap_or(0);
        if sample != 0 && sample != previous {
            doc.monitoring.checks.insert(key.clone(), json!({"checked_at":sample,"ok":clock["verified"] == true && clock["offset_seconds"].as_i64().is_some_and(|v| v.abs() <= 60),"detail":clock["detail"]}));
            let mut clock_policy = policy.clone(); clock_policy.failures = 3;
            if clock["verified"] == true {
                if let Some(offset) = clock["offset_seconds"].as_i64() {
                    observe(doc, key, offset.unsigned_abs() > 60, format!("{}：系统时钟偏差 {} 秒，请启用时间同步", n.name, offset), &clock_policy, false);
                }
            } else if let Some(condition) = doc.monitoring.conditions.get_mut(&key) {
                condition.bad = 0; condition.good = 0;
            }
            if let Some(enabled) = clock["ntp_enabled"].as_bool() {
                observe(doc, format!("ntp:{}", n.id), !enabled, format!("{}：自动时间同步未启用", n.name), &clock_policy, false);
            }
        }
        let cert = &n.report.system["certificate"];
        if cert["ok"].is_boolean() {
            observe(
                doc,
                format!("cert:{}", n.id),
                cert["ok"] == false,
                format!("{}：证书续期", n.name),
                policy,
                true,
            );
        }
        if let Some(certs) = n.report.system["certificate_expiry"].as_array() {
            for cert in certs {
                let expiry = cert["expires_at"].as_u64().unwrap_or(0);
                if expiry == 0 {
                    continue;
                }
                for days in &policy.expiry_days {
                    if expiry.saturating_sub(at) <= u64::from(*days) * 86400 {
                        doc.monitoring.milestone(
                            format!("cert:{}:{}:{expiry}:{days}", n.id, cert["name"]),
                            format!(
                                "{}：证书 {} 距离到期不超过 {days} 天",
                                n.name,
                                cert["name"].as_str().unwrap_or("")
                            ),
                            at,
                        );
                    }
                }
            }
        }
    }
    for line in doc.lines.clone() {
        if let Some(rental) = doc.rentals.get(&line.id).cloned() {
            if rental.expires_at > 0 {
                for days in &policy.expiry_days {
                    if rental.expires_at.saturating_sub(at) <= u64::from(*days) * 86400 {
                        doc.monitoring.milestone(
                            format!("expiry:{}:{}:{days}", line.id, rental.expires_at),
                            format!("{}：租期到期提醒（{days} 天档）", line.name),
                            at,
                        );
                    }
                }
            }
        }
        if let (Some(t), Some(limit)) = (
            chain_panel::rental::safe_traffic(
                doc.rentals.get(&line.id),
                chain_panel::rental::traffic(doc, &line),
                at,
            ),
            line.limits.monthly_gib,
        ) {
            let ratio = t.upload.saturating_add(t.download) as f64 / (limit * 1073741824.0) * 100.0;
            for threshold in &policy.quota {
                if ratio >= f64::from(*threshold) {
                    doc.monitoring.milestone(
                        format!(
                            "quota:{}:{}:{}:{threshold}",
                            line.id, t.epoch, t.cycle_start
                        ),
                        format!(
                            "{}：流量已达到 {threshold}%（当前 {:.1}%）",
                            line.name, ratio
                        ),
                        at,
                    );
                }
            }
        }
    }
}

async fn notify(app: &App) -> Result<()> {
    let pending = {
        let s = app.store.lock().unwrap();
        if !s.doc.fleet.controller_hash.is_empty() {
            return Ok(());
        }
        let channels = s.doc.monitoring.channels.clone();
        s.doc
            .monitoring
            .notices
            .iter()
            .find(|n| {
                n.retry_at <= now()
                    && ((!n.telegram_done && !channels.telegram_token.is_empty())
                        || (!n.mail_done && !channels.smtp_host.is_empty()))
            })
            .map(|n| (n.clone(), channels))
    };
    let Some((mut notice, channels)) = pending else {
        return Ok(());
    };
    let mut failures = vec![];
    if !notice.telegram_done && !channels.telegram_token.is_empty() {
        match chain_panel::notifications::telegram(&channels, &notice.message).await {
            Ok(()) => notice.telegram_done = true,
            Err(e) => failures.push(e.to_string()),
        }
    }
    if !notice.mail_done && !channels.smtp_host.is_empty() {
        let channels = channels.clone();
        let message = notice.message.clone();
        match tokio::task::spawn_blocking(move || {
            chain_panel::notifications::email(&channels, &message)
        })
        .await?
        {
            Ok(()) => notice.mail_done = true,
            Err(_) => failures.push("邮件发送失败，请检查 SMTP、TLS 和账号配置".into()),
        }
    }
    notice.attempts += 1;
    notice.retry_at = now() + (30 * 2_u64.pow(notice.attempts.min(7))).min(3600);
    notice.error = failures.join("；");
    let mut s = app.store.lock().unwrap();
    if let Some(existing) = s
        .doc
        .monitoring
        .notices
        .iter_mut()
        .find(|n| n.id == notice.id)
    {
        *existing = notice;
    }
    s.save()?;
    Ok(())
}

async fn poll_remote(
    app: &App,
    remote: chain_panel::management::Remote,
    fetch_document: bool,
) -> Result<Option<Document>> {
    let result = fleet_api::request(&remote, "GET", "/api/federation/state", None).await;
    let policy = app.store.lock().unwrap().doc.fleet.policy.clone();
    if result.is_ok() {
        // Avoid an unnecessary disk write on the remote when its policy is already current.
        if result.as_ref().unwrap()["monitoring"]["override"] != json!(policy) {
            fleet_api::request(
                &remote,
                "POST",
                "/api/federation/policy",
                Some(json!(policy)),
            )
            .await?;
        }
    }
    let document = if fetch_document
        && result
            .as_ref()
            .is_ok_and(|v| v["monitoring"]["effective"]["enabled"] == true)
    {
        fleet_api::request(&remote, "GET", "/api/federation/document", None)
            .await
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
    } else {
        None
    };
    let mut s = app.store.lock().unwrap();
    let Some(index) = s.doc.fleet.remotes.iter().position(|r| r.id == remote.id) else {
        return Ok(None);
    };
    let p = s
        .doc
        .fleet
        .policy
        .clone()
        .unwrap_or_else(|| s.doc.monitoring.policy());
    match result {
        Ok(value) => {
            if let Some(notices) = value["monitoring"]["notices"].as_array() {
                for notice in notices.iter().rev() {
                    s.doc.monitoring.milestone(
                        format!("remote:{}:{}", remote.id, notice["id"]),
                        format!(
                            "{}：{}",
                            remote.name,
                            notice["message"].as_str().unwrap_or("告警")
                        ),
                        now(),
                    );
                }
            }
            s.doc.fleet.remotes[index].snapshot = value;
            s.doc.fleet.remotes[index].seen = now();
            s.doc.fleet.remotes[index].error.clear();
            if p.enabled {
                observe(
                    &mut s.doc,
                    format!("panel:{}", remote.id),
                    false,
                    format!("{}：远端面板连接", remote.name),
                    &p,
                    true,
                );
            }
        }
        Err(e) => {
            s.doc.fleet.remotes[index].error = format!("{e:#}");
            if p.enabled && now().saturating_sub(remote.seen) >= p.offline {
                observe(
                    &mut s.doc,
                    format!("panel:{}", remote.id),
                    true,
                    format!("{}：远端面板失联，线路状态未知", remote.name),
                    &p,
                    true,
                );
            }
        }
    }
    s.save()?;
    Ok(document)
}

async fn check(
    app: App,
    source: String,
    name: String,
    doc: Document,
    line: Line,
    p: Policy,
    local: bool,
) {
    let local_id = if local {
        chain_panel::local_agent::node_id(app.token_file.parent().unwrap())
    } else {
        None
    };
    let request = ProbeRequest {
        mode: "http".into(),
        url: p.url.clone(),
    };
    let result = match tokio::time::timeout(
        Duration::from_secs(40),
        probe(&app.xray, &line, &doc, &request, local_id.as_deref()),
    )
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => {
            json!({"ok":false,"detail":"整链 HTTPS 检测失败；可使用手动检查定位鉴权、路径或目标问题","checked_at":now()})
        }
        Err(_) => json!({"ok":false,"detail":"整链检测超时","checked_at":now()}),
    };
    let mut s = app.store.lock().unwrap();
    let key = format!("probe:{source}:{}", line.id);
    if !local
        && s.doc
            .fleet
            .remotes
            .iter()
            .find(|r| r.id == source)
            .is_none_or(|r| !r.error.is_empty())
    {
        return;
    }
    s.doc.monitoring.checks.insert(key.clone(), result.clone());
    observe(
        &mut s.doc,
        key,
        result["ok"] == false,
        format!(
            "{name} / {}：{}整链检测",
            line.name,
            if local { "本机" } else { "公网入口" }
        ),
        &p,
        false,
    );
    if let Err(e) = s.save() {
        eprintln!("保存监控结果失败：{e}");
    }
}

pub async fn run(app: App, stop: tokio_util::sync::CancellationToken) {
    let mut jobs = tokio::task::JoinSet::new();
    let mut poll_at = BTreeMap::<String, u64>::new();
    let mut due = BTreeMap::<String, u64>::new();
    let documents = Arc::new(Mutex::new(BTreeMap::<String, (Document, u64)>::new()));
    let mut next_state = 0;
    let mut next_notice = 0;
    let mut notification_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut next_batch = 0;
    let mut batch_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! { _ = stop.cancelled() => break, _ = interval.tick() => {} }
        while let Some(result) = jobs.try_join_next() {
            if let Err(e) = result {
                eprintln!("监控任务失败：{e}");
            }
        }
        let (doc, p) = {
            let s = app.store.lock().unwrap();
            (s.doc.clone(), s.doc.monitoring.policy())
        };
        if now() >= next_state && p.enabled {
            let mut s = app.store.lock().unwrap();
            local_conditions(&mut s.doc, &p);
            let _ = s.save();
            next_state = now() + 15;
        }
        if notification_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let _ = notification_task.take().unwrap().await;
        }
        if now() >= next_notice && notification_task.is_none() {
            let app = app.clone();
            notification_task = Some(tokio::spawn(async move {
                if let Err(e) = notify(&app).await {
                    eprintln!("通知任务：{e}");
                }
            }));
            next_notice = now() + 5;
        }
        if batch_task.as_ref().is_some_and(|task| task.is_finished()) {
            let _ = batch_task.take().unwrap().await;
        }
        if now() >= next_batch && batch_task.is_none() {
            let app = app.clone();
            batch_task = Some(tokio::spawn(async move {
                if let Err(e) = fleet_api::work_batch(&app).await {
                    eprintln!("批量任务：{e}");
                }
            }));
            next_batch = now() + 10;
        }
        for remote in &doc.fleet.remotes {
            if now() >= *poll_at.get(&remote.id).unwrap_or(&0) && jobs.len() < 8 {
                poll_at.insert(remote.id.clone(), now() + 30);
                let fetch = documents
                    .lock()
                    .unwrap()
                    .get(&remote.id)
                    .is_none_or(|(_, at)| now().saturating_sub(*at) >= 60);
                let app = app.clone();
                let remote = remote.clone();
                let documents = documents.clone();
                jobs.spawn(async move {
                    match poll_remote(&app, remote.clone(), fetch).await {
                        Ok(Some(d)) => {
                            documents.lock().unwrap().insert(remote.id, (d, now()));
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("远端同步：{e}"),
                    }
                });
            }
        }
        if p.enabled && !doc.busy() {
            let local = Document {
                nodes: doc.applied_nodes.clone(),
                lines: doc.applied_lines.clone(),
                externals: doc.applied_externals.clone(),
                ..Default::default()
            };
            for line in local
                .lines
                .iter()
                .filter(|l| chain_panel::rental::status(&doc, l, now()) == "active")
            {
                let key = format!("local:{}", line.id);
                if jobs.len() >= 8 {
                    break;
                }
                if now() < *due.get(&key).unwrap_or(&0) {
                    continue;
                }
                due.insert(key, now() + p.interval);
                let (a, d, l, p) = (app.clone(), local.clone(), line.clone(), p.clone());
                jobs.spawn(check(a, "local".into(), "本机".into(), d, l, p, true));
            }
        }
        // Remote policies apply to remote self-checks and the controller's external checks alike.
        for remote in &doc.fleet.remotes {
            if jobs.len() >= 8 {
                break;
            }
            if !remote.error.is_empty() || now().saturating_sub(remote.seen) >= p.offline {
                continue;
            }
            let rp: Policy =
                serde_json::from_value(remote.snapshot["monitoring"]["effective"].clone())
                    .unwrap_or_default();
            if !rp.enabled || remote.snapshot["busy"] == true {
                continue;
            }
            let cached = documents.lock().unwrap().get(&remote.id).cloned();
            let Some((d, at)) = cached else {
                continue;
            };
            if now().saturating_sub(at) > 120 {
                continue;
            }
            for line in d.lines.iter().filter(|l| {
                remote.snapshot["lines"].as_array().is_some_and(|lines| {
                    lines
                        .iter()
                        .any(|v| v["id"] == l.id && v["rental_status"] == "active")
                })
            }) {
                if jobs.len() >= 8 {
                    break;
                }
                let key = format!("{}:{}", remote.id, line.id);
                if now() < *due.get(&key).unwrap_or(&0) {
                    continue;
                }
                due.insert(key, now() + rp.interval);
                jobs.spawn(check(
                    app.clone(),
                    remote.id.clone(),
                    remote.name.clone(),
                    d.clone(),
                    line.clone(),
                    rp.clone(),
                    false,
                ));
            }
        }
    }
    // Let bounded probes finish so their core processes are stopped and reaped before shutdown.
    while jobs.join_next().await.is_some() {}
    if let Some(task) = batch_task {
        let _ = task.await;
    }
    if let Some(task) = notification_task {
        let _ = task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_clock_requires_distinct_samples_and_reports_recovery() {
        let mut doc = Document::default(); let at = now();
        doc.nodes.push(serde_json::from_value(json!({"id":"clock-node","name":"node","host":"192.0.2.1","token_hash":"","reality":Reality::new("example.com:443".into(),"example.com".into()),"seen":at,"report":{"running":true}})).unwrap());
        let p = Policy::default();
        for sample in 1..=3 {
            doc.nodes[0].report.system = json!({"clock":{"checked_at":sample,"verified":true,"offset_seconds":120,"detail":"late"}});
            local_conditions(&mut doc,&p); local_conditions(&mut doc,&p);
            assert_eq!(doc.monitoring.notices.len(),if sample==3 {1} else {0});
        }
        doc.nodes[0].report.system["clock"] = json!({"checked_at":4,"verified":false,"detail":"unverified"});
        local_conditions(&mut doc,&p); assert!(doc.monitoring.conditions["clock:clock-node"].active);
        for sample in 5..=6 {
            doc.nodes[0].report.system["clock"] = json!({"checked_at":sample,"verified":true,"offset_seconds":0,"detail":"ok"});
            local_conditions(&mut doc,&p);
        }
        assert_eq!(doc.monitoring.notices.len(),2); assert!(!doc.monitoring.conditions["clock:clock-node"].active);
    }
    #[test]
    fn offline_threshold_and_recovery_do_not_repeat_notices() {
        let at = now();
        let mut doc = Document::default();
        doc.nodes.push(
            serde_json::from_value(json!({"id":"node","name":"node","host":"192.0.2.1",
            "token_hash":"","reality":Reality::new("example.com:443".into(),"example.com".into()),
            "seen":at-59,"report":{"running":true}}))
            .unwrap(),
        );
        let policy = Policy::default();
        local_conditions(&mut doc, &policy);
        assert!(doc.monitoring.notices.is_empty());
        doc.nodes[0].seen = at - 61;
        local_conditions(&mut doc, &policy);
        local_conditions(&mut doc, &policy);
        assert_eq!(doc.monitoring.notices.len(), 1);
        doc.nodes[0].seen = now();
        local_conditions(&mut doc, &policy);
        assert_eq!(doc.monitoring.notices.len(), 2);
        assert!(doc.monitoring.notices[1].message.starts_with("恢复"));
    }
}
