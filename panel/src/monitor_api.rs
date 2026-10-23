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
        let idle = n.report.applied
            && n.desired.as_ref().is_none_or(|d| d.revision.is_empty())
            && n.report.revision.is_empty()
            && doc.lines.is_empty()
            && doc.legacy_configs.is_empty();
        observe(
            doc,
            format!("runtime:{}", n.id),
            (!n.report.running && !idle) || !n.report.error.is_empty(),
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
                // An explicit enabled state needs no repeated recovery samples.
                observe(doc, format!("ntp:{}", n.id), !enabled, format!("{}：自动时间同步未启用", n.name), &clock_policy, enabled);
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
            let ratio = t.used() as f64 / (limit * 1073741824.0) * 100.0;
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

fn prune_probe_conditions(doc: &mut Document) {
    let obsolete: Vec<_> = doc.monitoring.conditions.keys().filter(|key| {
        let Some((source, id)) = key.strip_prefix("probe:").and_then(|key| key.split_once(':')) else { return false; };
        if source == "local" {
            !doc.applied_lines.iter().any(|line| line.id == id
                && matches!(chain_panel::rental::status(doc, line, now()), "active" | "offline"))
        } else {
            match doc.fleet.remotes.iter().find(|r| r.id == source) {
                None => true,
                Some(remote) if remote.error.is_empty() => remote.snapshot["lines"].as_array().is_some_and(|lines|
                    !lines.iter().any(|line| line["id"] == id && matches!(line["rental_status"].as_str(), Some("active" | "offline")))),
                _ => false,
            }
        }
    }).cloned().collect();
    for key in obsolete {
        doc.monitoring.conditions.remove(&key);
        doc.monitoring.checks.remove(&key);
    }
}

fn record_probe(doc: &mut Document, key: String, name: &str, line: &Line, p: &Policy, local: bool, result: Value) {
    if doc.monitoring.checks.get(&key).is_some_and(|previous| previous["checked_at"].as_u64() > result["checked_at"].as_u64()) {
        return;
    }
    let failed = result["ok"] == false;
    let summary: String = result["detail"].as_str().unwrap_or("").lines().take(2).collect::<Vec<_>>().join("；").chars().take(600).collect();
    doc.monitoring.checks.insert(key.clone(), result);
    observe(doc, key, failed, format!("{name} / {}：{}检测{}。{summary}", line.name,
        if local { "本机发起的连接" } else { "总控发起的连接" }, if failed { "未通过" } else { "通过" }), p, false);
}

pub fn record_manual_probe(doc: &mut Document, line: &Line, request: &ProbeRequest, result: Value) {
    let policy = doc.monitoring.policy().clone();
    let key = format!("probe:local:{}", line.id);
    // A TCP-only check or a different destination cannot resolve the monitored HTTP failure.
    if (!policy.enabled && !doc.monitoring.conditions.contains_key(&key))
        || request.mode != "http" || request.url != policy.url || doc.busy()
        || !doc.applied_lines.iter().any(|current| current.id == line.id
            && chain_panel::rental::status(doc, current, now()) == "active"
            && serde_json::to_value(current).ok() == serde_json::to_value(line).ok()) {
        return;
    }
    record_probe(doc, key, "本机", line, &policy, true, result);
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

pub(super) async fn poll_remote(
    app: &App,
    remote: chain_panel::management::Remote,
    fetch_document: bool,
) -> Result<Option<Document>> {
    let result = fleet_api::request(&remote, "GET", "/api/federation/state", None).await;
    let policy = app.store.lock().unwrap().doc.fleet.policy.clone();
    let mut policy_error = String::new();
    if result.is_ok() {
        // Avoid an unnecessary disk write on the remote when its policy is already current.
        if result.as_ref().unwrap()["monitoring"]["override"] != json!(policy) {
            if let Err(error) = fleet_api::request(
                &remote,
                "POST",
                "/api/federation/policy",
                Some(json!(policy)),
            )
            .await {
                policy_error = format!("{error:#}");
            }
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
            s.doc.fleet.remotes[index].policy_error = policy_error;
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
    let mut result = match tokio::time::timeout(
        Duration::from_secs(40),
        probe(&app.xray, &line, &doc, &request, local_id.as_deref()),
    )
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            json!({"ok":false,"detail":chain_panel::diagnostics::redact(&format!("{e:#}")),"checked_at":now()})
        }
        Err(_) => json!({"ok":false,"detail":"整链检测超时","checked_at":now()}),
    };
    let key = format!("probe:{source}:{}", line.id);
    // Diagnose sustained local failures using the applied outbound, without restarting the live proxy.
    let diagnose = local && line.kind == Kind::Vless && result["ok"] == false && {
        let s = app.store.lock().unwrap();
        s.doc.monitoring.conditions.get(&key).is_some_and(|c| c.active || c.bad.saturating_add(1) >= p.failures)
            || p.failures == 1
    };
    if diagnose {
        if let Some(entry) = line.hops.first() {
            let diagnosis = tokio::time::timeout(Duration::from_secs(40), diagnostics_api::dispatch(&app, &entry.node_id,
                json!({"operation":"path","request":{"line_id":line.id,"url":p.url}}), 35)).await;
            let detail = match diagnosis {
                Ok(Ok(value)) => {
                    result["outbound"] = value["result"].clone();
                    format!("自动分段检查：{}\n{}", if value["result"]["ok"] == true {
                        "出口及后续链路通过；入口检测仍未通过，需结合上述错误区分握手、本机转发或检测程序问题；其他入口未受本次验证。"
                    } else { "出口及后续链路也未通过；不能仅归因于入口或 SNI。" }, value["result"]["detail"].as_str().unwrap_or(""))
                }
                Ok(Err(e)) => format!("自动分段检查暂未完成：{e:#}；下轮自动重查。"),
                Err(_) => "自动分段检查超时；下轮自动重查。".into(),
            };
            result["detail"] = json!(format!("{}\n{}", result["detail"].as_str().unwrap_or(""), chain_panel::diagnostics::redact(&detail)));
        }
    }
    let mut s = app.store.lock().unwrap();
    if local && (s.doc.busy() || !s.doc.applied_lines.iter().any(|current| current.id == line.id
        && chain_panel::rental::status(&s.doc, current, now()) == "active"
        && serde_json::to_value(current).ok() == serde_json::to_value(&line).ok())) {
        return;
    }
    if !local
        && s.doc
            .fleet
            .remotes
            .iter()
            .find(|r| r.id == source)
            .is_none_or(|r| !r.error.is_empty() || r.snapshot["lines"].as_array().is_some_and(|lines|
                !lines.iter().any(|current| current["id"] == line.id && current["rental_status"] == "active")))
    {
        return;
    }
    record_probe(&mut s.doc, key, &name, &line, &p, local, result);
    if let Err(e) = s.save() {
        eprintln!("保存监控结果失败：{e}");
    }
}

pub async fn run(app: App, stop: tokio_util::sync::CancellationToken) {
    let mut jobs = tokio::task::JoinSet::new();
    let mut running_checks = HashMap::new();
    let mut poll_at = BTreeMap::<String, u64>::new();
    let mut due = BTreeMap::<String, u64>::new();
    let documents = Arc::new(Mutex::new(BTreeMap::<String, (Document, u64)>::new()));
    let mut next_state = 0;
    let mut next_notice = 0;
    let mut notification_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut next_batch = 0;
    let mut batch_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut next_verification = 0;
    let mut verification_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! { _ = stop.cancelled() => break, _ = interval.tick() => {} }
        while let Some(result) = jobs.try_join_next_with_id() {
            let id = match &result { Ok((id, _)) => *id, Err(e) => e.id() };
            running_checks.retain(|_, task| *task != id);
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
            prune_probe_conditions(&mut s.doc);
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
        if verification_task.as_ref().is_some_and(|task| task.is_finished()) {
            let _ = verification_task.take().unwrap().await;
        }
        if now() >= next_verification && verification_task.is_none() {
            let app = app.clone();
            verification_task = Some(tokio::spawn(async move {
                if let Err(e) = fleet_api::verify_batches(&app).await {
                    eprintln!("维护后自动验证：{e}");
                }
            }));
            next_verification = now() + 10;
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
                if running_checks.contains_key(&key) || now() < *due.get(&key).unwrap_or(&0) {
                    continue;
                }
                due.insert(key.clone(), now() + p.interval);
                let (a, d, l, p) = (app.clone(), local.clone(), line.clone(), p.clone());
                running_checks.insert(key, jobs.spawn(check(a, "local".into(), "本机".into(), d, l, p, true)).id());
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
                if running_checks.contains_key(&key) || now() < *due.get(&key).unwrap_or(&0) {
                    continue;
                }
                due.insert(key.clone(), now() + rp.interval);
                running_checks.insert(key, jobs.spawn(check(
                    app.clone(),
                    remote.id.clone(),
                    remote.name.clone(),
                    d.clone(),
                    line.clone(),
                    rp.clone(),
                    false,
                )).id());
            }
        }
    }
    // Let bounded probes finish so their core processes are stopped and reaped before shutdown.
    while jobs.join_next().await.is_some() {}
    if let Some(task) = batch_task {
        let _ = task.await;
    }
    if let Some(task) = verification_task {
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
    fn empty_online_node_clears_runtime_alarm_without_hiding_failures() {
        let mut empty = Document::default();
        empty.nodes.push(serde_json::from_value(json!({"id":"node","name":"node","host":"192.0.2.1",
            "token_hash":"","reality":Reality::new("example.com:443".into(),"example.com".into()),
            "seen":now(),"report":{"running":false,"applied":true}})).unwrap());
        let policy = Policy::default();
        let key = "runtime:node";
        local_conditions(&mut empty, &policy);
        assert!(!empty.monitoring.conditions[key].active);
        assert!(empty.monitoring.notices.is_empty());
        empty.monitoring.conditions.get_mut(key).unwrap().active = true;
        local_conditions(&mut empty, &policy);
        assert!(!empty.monitoring.conditions[key].active);
        assert!(empty.monitoring.notices.last().unwrap().message.starts_with("恢复"));
        for case in 0..6 {
            let mut doc = empty.clone();
            match case {
                0 => doc.nodes[0].report.error = "fixture failure".into(),
                1 => doc.nodes[0].report.applied = false,
                2 => doc.nodes[0].report.revision = "previous-config".into(),
                3 => doc.nodes[0].desired = Some(Desired { revision: "pending-config".into(), config: json!({}) }),
                4 => doc.lines.push(line()),
                _ => doc.legacy_configs.push(serde_json::from_value(json!({
                    "id":"legacy","node_id":"node","name":"legacy","config":{}})).unwrap()),
            }
            local_conditions(&mut doc, &policy);
            assert!(doc.monitoring.conditions[key].active, "case {case}");
        }
        empty.nodes[0].seen = now() - policy.offline;
        local_conditions(&mut empty, &policy);
        assert!(empty.monitoring.conditions["agent:node"].active);
    }
    fn line() -> Line {
        serde_json::from_value(json!({"id":"line", "name":"REALITY", "kind":"vless",
            "hops":[{"node_id":"node", "port":20000, "uuid":id()}]})).unwrap()
    }
    #[test]
    fn automatic_probe_keeps_evidence_and_recovers_without_manual_action() {
        let mut doc = Document::default();
        let p = Policy::default();
        let line = line();
        let key = "probe:local:line";
        let failed = json!({"ok":false,"checked_at":now(),"detail":"入口 127.0.0.1:20000 · SNI example.com：Connection reset by peer",
            "outbound":{"ok":true,"detail":"HTTP 200"}});
        for _ in 0..p.failures {
            record_probe(&mut doc, key.into(), "本机", &line, &p, true, failed.clone());
        }
        assert!(doc.monitoring.conditions[key].active);
        assert_eq!(doc.monitoring.checks[key], failed);
        assert!(doc.monitoring.conditions[key].message.contains("Connection reset by peer"));
        assert_eq!(doc.monitoring.notices.len(), 1);
        for sample in 1..=p.recoveries {
            record_probe(&mut doc, key.into(), "本机", &line, &p, true,
                json!({"ok":true,"checked_at":now(),"detail":"入口 HTTP 200"}));
            assert_eq!(doc.monitoring.conditions[key].active, sample < p.recoveries);
        }
        assert_eq!(doc.monitoring.notices.len(), 2);
        assert!(doc.monitoring.notices[1].message.starts_with("恢复："));
        assert!(!doc.monitoring.checks[key]["outbound"].is_object(), "成功后不能沿用旧失败诊断");
    }
    #[test]
    fn manual_probe_updates_the_same_alert_without_accepting_other_or_stale_checks() {
        let line = line();
        let node: Node = serde_json::from_value(json!({"id":"node","name":"node","host":"127.0.0.1",
            "token_hash":"test","seen":now(),"reality":Reality::new("example.com:443".into(), "example.com".into()),
            "report":{"running":true}})).unwrap();
        let mut doc = Document { nodes: vec![node], applied_lines: vec![line.clone()], ..Default::default() };
        let p = Policy::default();
        let key = "probe:local:line";
        for _ in 0..p.failures {
            record_probe(&mut doc, key.into(), "本机", &line, &p, true,
                json!({"ok":false,"detail":"failed","checked_at":now()}));
        }
        let mut request = ProbeRequest { mode:"tcp".into(), url:p.url.clone() };
        let passed = json!({"ok":true,"detail":"HTTP 200","checked_at":now()});
        record_manual_probe(&mut doc, &line, &request, passed.clone());
        request.mode = "http".into(); request.url = "https://other.example/".into();
        record_manual_probe(&mut doc, &line, &request, passed.clone());
        request.url = p.url.clone();
        let mut changed = line.clone(); changed.name = "changed".into();
        record_manual_probe(&mut doc, &changed, &request, passed.clone());
        record_manual_probe(&mut doc, &line, &request, json!({"ok":true,"checked_at":1}));
        assert_eq!(doc.monitoring.conditions[key].good, 0);
        for n in 1..=p.recoveries {
            record_manual_probe(&mut doc, &line, &request, passed.clone());
            assert_eq!(doc.monitoring.conditions[key].active, n < p.recoveries);
        }
        assert_eq!(doc.monitoring.checks[key], passed);
        assert_eq!(doc.monitoring.notices.len(), 2);
        doc.monitoring.conditions.clear();
        record_manual_probe(&mut doc, &line, &request, json!({"ok":false,"checked_at":now()}));
        assert!(doc.monitoring.conditions.is_empty(), "手动诊断不能擅自开启监控告警");
    }
    #[test]
    fn obsolete_probes_clear_but_offline_nodes_keep_their_evidence() {
        let mut doc = Document::default();
        let mut line = line();
        let key = "probe:local:line";
        doc.applied_lines.push(line.clone());
        doc.monitoring.conditions.insert(key.into(), chain_panel::management::Condition { active: true, ..Default::default() });
        doc.monitoring.checks.insert(key.into(), json!({"ok":false}));
        doc.monitoring.notice("historical failure".into(), now());
        prune_probe_conditions(&mut doc);
        assert!(doc.monitoring.conditions[key].active, "节点离线不是检测恢复");
        line.enabled = false;
        doc.applied_lines[0] = line;
        prune_probe_conditions(&mut doc);
        assert!(!doc.monitoring.conditions.contains_key(key));
        assert!(!doc.monitoring.checks.contains_key(key));
        assert_eq!(doc.monitoring.notices.len(), 1, "保留历史通知，不能伪造恢复");
        doc.monitoring.conditions.insert("probe:removed:line".into(), Default::default());
        doc.monitoring.conditions.insert("ntp:node".into(), Default::default());
        prune_probe_conditions(&mut doc);
        assert!(!doc.monitoring.conditions.contains_key("probe:removed:line"));
        assert!(doc.monitoring.conditions.contains_key("ntp:node"));
        doc.monitoring.conditions.insert(key.into(), Default::default());
        doc.applied_lines.clear();
        prune_probe_conditions(&mut doc);
        assert!(!doc.monitoring.conditions.contains_key(key));
    }
    #[tokio::test]
    async fn sustained_failure_automatically_requests_outbound_diagnosis() {
        let dir = tempfile::tempdir().unwrap();
        let mut line = line();
        line.id = id();
        let node: Node = serde_json::from_value(json!({"id":"node", "name":"node", "host":"192.0.2.1",
            "token_hash":digest("test-agent"), "seen":now(), "reality":Reality::new("example.com:443".into(), "example.com".into()),
            "report":{"running":true,"system":{"diagnostics_version":1}}})).unwrap();
        std::fs::write(dir.path().join("local-agent.json"), br#"{"id":"node","token":"test-agent"}"#).unwrap();
        let doc = Document { nodes: vec![node.clone()], applied_nodes: vec![node], applied_lines: vec![line.clone()], ..Default::default() };
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY,body TEXT NOT NULL)").unwrap();
        let app = App {
            diagnostics: Arc::new(diagnostics_api::Broker::default()),
            store: Arc::new(Mutex::new(Store { conn, doc: doc.clone() })),
            sessions: Default::default(), tickets: Default::default(), delegated_sessions: Default::default(),
            admin_hash: Default::default(), token_file: dir.path().join("token"),
            auth: Arc::new(chain_panel::security::AuthState::default()), secure_cookie: true,
            xray: dir.path().join("deliberately-missing-core"), probe_lock: Arc::new(tokio::sync::Mutex::new(())),
            apply_timeout: 10, local_agent: Arc::new(tokio::sync::Mutex::new(chain_panel::local_agent::LocalAgent::default())),
            local_url: String::new(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = diagnostics_api::routes(Router::new()).with_state(app.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let key = format!("probe:local:{}", line.id);
        let policy = Policy { failures: 1, ..Default::default() };
        let checking = check(app.clone(), "local".into(), "本机".into(), doc, line.clone(), policy, true);
        let answering = async {
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let endpoint = format!("http://{address}/api/agent/node/diagnostics");
            loop {
                let task: Value = client.post(&endpoint).bearer_auth("test-agent").json(&json!({})).send().await.unwrap().json().await.unwrap();
                if !task.is_null() {
                    assert_eq!(task["request"]["operation"], "path");
                    assert_eq!(task["request"]["request"]["line_id"], line.id);
                    assert_eq!(task["request"]["request"]["url"], "https://example.com/");
                    client.post(&endpoint).bearer_auth("test-agent").json(&json!({"id":task["id"],
                        "result":{"result":{"ok":true,"detail":"HTTP 200"}}})).send().await.unwrap().error_for_status().unwrap();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let completed = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(checking, answering); }).await;
        server.abort();
        let _ = server.await;
        let _released = std::net::TcpListener::bind(address).unwrap();
        completed.unwrap();
        assert!(app.diagnostics.0.lock().unwrap().is_empty());
        let store = app.store.lock().unwrap();
        let result = &store.doc.monitoring.checks[&key];
        assert_eq!(result["ok"], false, "出口通过不能伪造入口恢复");
        assert_eq!(result["outbound"]["ok"], true);
        assert!(result["detail"].as_str().unwrap().contains("出口及后续链路通过"));
        assert!(store.doc.monitoring.conditions[&key].active);
    }
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
    fn ntp_recovers_on_first_enabled_sample_without_treating_correct_time_as_enabled() {
        let mut doc = Document::default();
        doc.nodes.push(serde_json::from_value(json!({"id":"ntp-node","name":"node","host":"192.0.2.1","token_hash":"","reality":Reality::new("example.com:443".into(),"example.com".into()),"seen":now(),"report":{"running":true}})).unwrap());
        let p = Policy::default();
        for sample in 1..=3 {
            doc.nodes[0].report.system = json!({"clock":{"checked_at":sample,"verified":true,"offset_seconds":0,"ntp_enabled":false}});
            local_conditions(&mut doc, &p);
            local_conditions(&mut doc, &p);
            assert_eq!(doc.monitoring.conditions["ntp:ntp-node"].active, sample == 3);
        }
        doc.nodes[0].report.system["clock"] = json!({"checked_at":4,"verified":true,"offset_seconds":0});
        local_conditions(&mut doc, &p);
        assert!(doc.monitoring.conditions["ntp:ntp-node"].active);
        doc.nodes[0].report.system["clock"] = json!({"checked_at":5,"ntp_enabled":true});
        local_conditions(&mut doc, &p);
        assert!(!doc.monitoring.conditions["ntp:ntp-node"].active);
        assert_eq!(doc.monitoring.notices.len(), 2);
        local_conditions(&mut doc, &p);
        assert_eq!(doc.monitoring.notices.len(), 2);
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
