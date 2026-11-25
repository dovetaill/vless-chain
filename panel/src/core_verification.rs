use super::*;
use chain_panel::management::CoreVerification;

const WAIT_SECONDS: u64 = 180;

async fn inputs(app: &App, remote: Option<&Remote>) -> Result<(Value, Document)> {
    let (state, document) = if let Some(remote) = remote {
        (
            request(remote, "GET", "/api/federation/state", None).await?,
            request(remote, "GET", "/api/federation/document", None).await?,
        )
    } else {
        (
            remote_state(State(app.clone())).await.0,
            probe_document(State(app.clone())).await.0,
        )
    };
    ensure!(
        document["lines"].is_array() && document["nodes"].is_array(),
        "缺少已发布线路数据"
    );
    Ok((state, serde_json::from_value(document)?))
}

fn legacy_enabled(state: &Value) -> bool {
    state["published_legacy_enabled"].as_bool().unwrap_or_else(|| {
        state["legacy_configs"]
            .as_array()
            .is_some_and(|items| items.iter().any(|p| p["enabled"] != false && p["published"] != false))
    })
}

fn fingerprint(state: &Value, doc: &Document) -> Result<String> {
    let local = state["local_node_id"]
        .as_str()
        .context("本机 Agent 身份待确认")?;
    state["legacy_configs"]
        .as_array()
        .context("缺少原生配置状态")?;
    let mut links = BTreeMap::new();
    for line in &doc.lines {
        let endpoint = share(line, doc)?;
        ensure!(
            links.insert(&line.id, endpoint).is_none(),
            "已发布线路 ID 重复"
        );
    }
    // Store only a digest of the pre-upgrade links, never credentials in task receipts.
    Ok(digest(&serde_json::to_string(&(
        local,
        legacy_enabled(state),
        links,
    ))?))
}

pub(super) async fn prepare(app: &App, remote: Option<&Remote>) -> Result<CoreVerification> {
    let before = async {
        let (state, doc) = inputs(app, remote).await?;
        fingerprint(&state, &doc)
    }
    .await;
    let (baseline, state, detail) = match before {
        Ok(hash) => (Some(hash), "pending", "等待内核切换和新心跳".to_owned()),
        Err(e) => (None, "manual", format!("无法自动核对升级前的入口：{e:#}")),
    };
    Ok(CoreVerification {
        confirm_id: id(),
        baseline,
        state: state.into(),
        detail,
        created: now(),
        waiting_since: None,
    })
}

async fn task_request(app: &App, remote: Option<&Remote>, body: Value) -> Result<Value> {
    if let Some(remote) = remote {
        return request(remote, "POST", "/api/federation/task", Some(body)).await;
    }
    if let Some(original) = confirmation_guard(app, &body)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.1))?
    {
        return Ok(original);
    }
    maintenance(body).await
}

fn ready<'a>(state: &'a Value, task: &Value) -> Result<&'a Value> {
    let local = state["local_node_id"]
        .as_str()
        .context("本机 Agent 身份未知")?;
    let node = state["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|n| n["id"] == local))
        .context("等待本机 Agent")?;
    let seen = node["seen"].as_u64().context("缺少心跳时间")?;
    let at = state["time"].as_u64().context("缺少远端时间")?;
    ensure!(
        seen > task["finished"].as_u64().context("内核切换尚未结束")?
            && seen <= at
            && at - seen <= 30
            && node["online"] == true,
        "等待切换后的新 Agent 心跳"
    );
    let core = task["core"].as_str().context("缺少内核类型")?;
    let target = task["core_version"]
        .as_str()
        .context("缺少切换后的目标版本")?;
    ensure!(
        task["version"] == target && node["report"]["system"]["cores"][core] == target,
        "等待 Agent 上报目标 {core} {target}"
    );
    Ok(node)
}

enum Proof {
    Passed(String),
    Manual(String),
    Failed(String),
}

fn applied(node: &Value, doc: &Document) -> bool {
    let report = &node["report"];
    report["applied"] == true
        && report["error"] == ""
        && (report["running"] == true
            || (doc.lines.is_empty() && node["desired_revision"].is_null()))
        && report["revision"].as_str() == Some(node["desired_revision"].as_str().unwrap_or(""))
}

async fn verify(app: &App, remote: Option<&Remote>, task: &Value, baseline: &str) -> Result<Proof> {
    let (state, doc) = inputs(app, remote).await?;
    if fingerprint(&state, &doc)? != baseline {
        return Ok(Proof::Manual(
            "已发布入口与升级前的记录不同，请核对原链接".into(),
        ));
    }
    let node = ready(&state, task)?;
    let report = &node["report"];
    if let Some(error) = report["error"].as_str().filter(|s| !s.is_empty()) {
        return Ok(Proof::Failed(format!(
            "目标版本 Agent 上报配置错误：{}",
            chain_panel::diagnostics::redact(error)
        )));
    }
    ensure!(applied(node, &doc), "等待 Agent 应用目标配置");
    let has_legacy = legacy_enabled(&state);
    if doc.lines.is_empty() {
        if has_legacy {
            return Ok(Proof::Manual(
                "目标内核、新心跳和配置已自动核对；原生配置仍需使用其原客户端人工确认".into(),
            ));
        }
        return Ok(Proof::Passed(
            "目标内核、新心跳和配置已核对，当前无可用代理线路".into(),
        ));
    }
    let url = state["monitoring"]["effective"]["url"]
        .as_str()
        .context("缺少传输探测地址")?;
    let mut udp_count = 0;
    for line in &doc.lines {
        let udp = udp_supported(line, &doc)?;
        let result = tokio::time::timeout(
            Duration::from_secs(40),
            probe_core_link(&app.xray, line, &doc, url, udp),
        )
        .await
        .with_context(|| format!("{}：原入口探测超时", line.name))??;
        if result["ok"] != true {
            let reason = format!(
                "{}（{}探测）：{}",
                line.name,
                if remote.is_some() {
                    "总控"
                } else {
                    "本机公网入口"
                },
                result["detail"].as_str().unwrap_or("探测未完成")
            );
            return if matches!(result["stage"].as_str(), Some("http" | "udp")) {
                Ok(Proof::Failed(reason))
            } else {
                Err(anyhow::anyhow!(reason))
            };
        }
        udp_count += usize::from(udp);
    }
    let scope = format!(
        "目标内核、新心跳和配置已核对，{} 条升级前的入口通过 HTTP 传输，{udp_count} 条通过 UDP DNS 应答",
        doc.lines.len()
    );
    if has_legacy {
        return Ok(Proof::Manual(format!(
            "{scope}；原生配置仍需使用其原客户端人工确认"
        )));
    }
    Ok(Proof::Passed(scope))
}

pub(super) fn summary(check: &CoreVerification) -> Value {
    json!({"state":check.state,"detail":check.detail})
}

fn manual(check: &mut CoreVerification, detail: String) {
    check.state = "manual".into();
    check.detail = detail;
}

async fn advance(
    app: &App,
    remote: Option<&Remote>,
    task: &Value,
    check: &mut CoreVerification,
) -> Result<()> {
    let waiting = *check.waiting_since.get_or_insert_with(now);
    let expired = now().saturating_sub(waiting) >= WAIT_SECONDS;
    let resolution = task_request(
        app,
        remote,
        json!({"action":"status","id":check.confirm_id}),
    )
    .await?;
    match resolution["state"].as_str() {
        Some("failed" | "unknown") => {
            manual(
                check,
                format!(
                    "自动验收处理未完成：{}",
                    resolution["error"].as_str().unwrap_or("请核对原任务")
                ),
            );
            return Ok(());
        }
        Some("queued" | "running" | "succeeded") => {
            ensure!(
                resolution["task_id"] == task["id"] && resolution["core"] == task["core"],
                "验收回执与原任务不一致"
            );
            if expired {
                check.state = "reconciling".into();
                check.detail = "验收处理已受理，等待原任务回执".into();
            }
            return Ok(());
        }
        Some("not_found") => (),
        _ => anyhow::bail!("验收回执状态未知"),
    }
    let baseline = check.baseline.as_deref().context("缺少升级前的入口记录")?;
    let proof = tokio::time::timeout(
        Duration::from_secs(WAIT_SECONDS),
        verify(app, remote, task, baseline),
    )
    .await
    .context("原链接自动验收超时")??;
    let (action, detail) = match proof {
        Proof::Manual(detail) => {
            manual(check, detail);
            return Ok(());
        }
        Proof::Failed(detail) if !expired => {
            check.detail = detail;
            return Ok(());
        }
        Proof::Failed(detail) if check.detail != detail => {
            manual(check, format!("自动复查未能确认同一故障：{detail}"));
            return Ok(());
        }
        Proof::Failed(detail) => ("core-reject", detail),
        Proof::Passed(detail) => ("core-confirm", detail),
    };
    if !matches!(check.state.as_str(), "confirming" | "rejecting") {
        check.waiting_since = Some(now());
    }
    check.state = if action == "core-confirm" {
        "confirming"
    } else {
        "rejecting"
    }
    .into();
    check.detail = detail;
    let mut decision =
        json!({"id":check.confirm_id,"action":action,"task_id":task["id"],"core":task["core"]});
    if action == "core-reject" {
        decision["keep_agent_on_failure"] = json!(true);
    }
    let reply = task_request(app, remote, decision).await?;
    ensure!(
        reply["id"] == check.confirm_id
            && reply["task_id"] == task["id"]
            && matches!(
                reply["state"].as_str(),
                Some("queued" | "running" | "succeeded")
            ),
        "验收处理回执异常，正在重新查询"
    );
    Ok(())
}

pub(super) async fn update(
    app: &App,
    remote: Option<&Remote>,
    task: &Value,
    check: &mut CoreVerification,
) {
    if task["state"] == "succeeded" {
        if check.state != "succeeded" {
            let receipt = task_request(
                app,
                remote,
                json!({"action":"status","id":check.confirm_id}),
            )
            .await;
            let auto = receipt.is_ok_and(|v| {
                v["state"] == "succeeded"
                    && v["action"] == "core-confirm"
                    && v["core"] == task["core"]
                    && v["task_id"] == task["id"]
            });
            check.detail = if auto && check.state == "confirming" {
                format!("自动验收通过。{}", check.detail)
            } else if auto {
                "自动验收通过，原升级任务已完成".into()
            } else {
                "原升级任务已确认完成".into()
            };
            check.state = "succeeded".into();
        }
    } else if matches!(task["state"].as_str(), Some("failed" | "unknown")) {
        let detail = task["error"]
            .as_str()
            .or(task["detail"].as_str())
            .unwrap_or("原任务未成功");
        check.detail = if check.state == "rejecting" {
            format!("{}。{detail}", check.detail)
        } else {
            detail.into()
        };
        check.state = "failed".into();
    } else if task["state"] == "awaiting_verification"
        && !matches!(check.state.as_str(), "manual" | "reconciling")
    {
        if let Err(e) = advance(app, remote, task, check).await {
            let message = chain_panel::diagnostics::redact(&format!("{e:#}"));
            if now().saturating_sub(check.waiting_since.unwrap_or_else(now)) >= WAIT_SECONDS {
                manual(check, message);
            } else if !matches!(check.state.as_str(), "confirming" | "rejecting") {
                check.detail = message;
            }
        }
    }
}

pub(super) async fn register_local(app: &App, task: &str) -> Result<()> {
    if app
        .store
        .lock()
        .unwrap()
        .doc
        .fleet
        .local_core_checks
        .contains_key(task)
        || maintenance(json!({"id":task,"action":"status"})).await?["state"] != "not_found"
    {
        return Ok(());
    }
    let check = prepare(app, None).await?;
    edit(app, |doc| {
        let checks = &mut doc.fleet.local_core_checks;
        checks.retain(|_, c| now().saturating_sub(c.created) <= 30 * 86400);
        checks.entry(task.into()).or_insert(check);
        Ok(())
    })
    .map_err(|e| anyhow::anyhow!("{}", e.1))
}

pub(crate) async fn work_local(app: &App) -> Result<()> {
    let active: Vec<_> = app
        .store
        .lock()
        .unwrap()
        .doc
        .fleet
        .local_core_checks
        .iter()
        .filter(|(_, c)| {
            matches!(
                c.state.as_str(),
                "pending" | "confirming" | "rejecting" | "reconciling"
            )
        })
        .map(|(id, c)| (id.clone(), c.clone()))
        .collect();
    for (id, mut check) in active {
        let previous = summary(&check);
        let since = check.waiting_since;
        let task = maintenance(json!({"action":"status","id":id})).await;
        match task {
            Ok(task) if task["id"] == id && task["action"] == "core-upgrade" => {
                update(app, None, &task, &mut check).await
            }
            Ok(_) | Err(_) if now().saturating_sub(check.created) >= 300 => {
                manual(&mut check, "无法查询原升级任务，自动验收已暂停".into())
            }
            _ => continue,
        }
        if previous != summary(&check) || since != check.waiting_since {
            let mut s = app.store.lock().unwrap();
            s.doc.fleet.local_core_checks.insert(id, check);
            s.save()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_requires_a_new_heartbeat_with_the_exact_target_core() {
        let task =
            json!({"core":"mihomo","version":"1.19.34","core_version":"1.19.34","finished":120});
        let mut state = json!({"time":121,"local_node_id":"node","nodes":[{"id":"node","seen":120,"online":true,
            "report":{"system":{"cores":{"mihomo":"1.19.34"}}}}]});
        assert!(ready(&state, &task).is_err());
        state["nodes"][0]["seen"] = json!(121);
        assert!(ready(&state, &task).is_ok());
        state["nodes"][0]["report"]["system"]["cores"]["mihomo"] = json!("1.19.33");
        assert!(ready(&state, &task).is_err());
    }
}
