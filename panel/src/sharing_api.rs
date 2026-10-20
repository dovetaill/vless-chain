use super::*;
use chain_panel::{resources, rules};

pub fn routes(router: Router<App>) -> Router<App> {
    router
        .route("/api/resources", get(view).post(save_resource))
        .route("/api/resources/{id}", delete(remove_resource))
        .route("/api/resources/groups", post(groups))
        .route("/api/resources/candidates", post(candidates))
        .route("/api/rules", get(rule_view).post(save_source))
        .route("/api/rules/{id}/{action}", post(rule_action))
        .route("/api/access/{id}", get(access_view).post(save_access))
        .route("/api/agent/{id}/resources", post(grant))
}
async fn view(State(app): State<App>) -> Json<Value> {
    let s = app.store.lock().unwrap();
    let mut state = s.doc.resources.clone();
    for resource in &state.resources {
        if let Ok((cycle, end)) = resource.cycle(now()) {
            let ledger = state.ledgers.entry(resource.id.clone()).or_default();
            if ledger.cycle != cycle {
                *ledger = resources::Ledger {
                    cycle,
                    end,
                    ..Default::default()
                };
            }
        }
    }
    Json(json!(state))
}
fn schedule(doc: &mut Document) -> Result<()> {
    let mut updated = doc.clone();
    schedule_inner(&mut updated)?;
    doc.nodes = updated.nodes;
    Ok(())
}
fn schedule_inner(doc: &mut Document) -> Result<()> {
    let mut active = doc.clone();
    active.lines = doc.applied_lines.clone();
    active.externals = doc.applied_externals.clone();
    for node in &mut doc.nodes {
        let Some(desired) = node.desired.as_mut() else {
            continue;
        };
        let Some(specs) = desired.config["_chain"].as_array_mut() else {
            continue;
        };
        let before = specs.clone();
        for spec in specs.iter_mut() {
            let Some(line) = active.lines.iter().find(|l| spec["id"] == l.id) else {
                continue;
            };
            let pools = resources::pools(&active, line);
            let policy = rules::compile(&active, &line.id)?;
            spec["resource_ids"] = json!(pools);
            spec["access_policy"] = json!(policy);
            spec["multiplier"] = json!(resources::multiplier(&active, line)?);
        }
        if *specs != before {
            ensure!(
                node.report.system["sharing_version"] == 1,
                "请先升级入口 Agent，再应用资源或访问策略"
            );
            desired.revision = format!("sharing-{}", id());
        }
    }
    Ok(())
}
fn validate_members(doc: &Document, r: &resources::Resource) -> Result<()> {
    ensure!(
        r.members.iter().collect::<BTreeSet<_>>().len() == r.members.len(),
        "资源成员有重复"
    );
    for member in &r.members {
        ensure!(
            doc.nodes
                .iter()
                .any(|n| member == &format!("node:{}", n.id))
                || doc
                    .externals
                    .iter()
                    .any(|e| member == &format!("external:{}", e.id)),
            "资源成员不存在"
        );
        ensure!(
            !doc.resources
                .resources
                .iter()
                .any(|other| other.id != r.id && other.members.contains(member)),
            "成员已属于其他组，请先移出原组"
        );
    }
    Ok(())
}
async fn save_resource(
    State(app): State<App>,
    Json(mut r): Json<resources::Resource>,
) -> ApiResult<Json<Value>> {
    r.validate()?;
    edit(&app, |doc| {
        if r.id.is_empty() {
            r.id = id();
        } else {
            ensure!(
                doc.resources.resources.iter().any(|old| old.id == r.id),
                "资源不存在"
            );
        }
        validate_members(doc, &r)?;
        if let Some(old) = doc.resources.resources.iter().find(|old| old.id == r.id) {
            if let Some(limit) = r.monthly_gib {
                let cycle = r.cycle(now())?.0;
                let granted = doc
                    .resources
                    .ledgers
                    .get(&r.id)
                    .filter(|l| l.cycle == cycle)
                    .map(|l| {
                        l.allocations
                            .values()
                            .fold(0u64, |n, a| n.saturating_add(a.granted))
                    })
                    .unwrap_or(0);
                ensure!(
                    (limit * 1073741824.0) as u64 >= granted,
                    "额度不能低于本周期已分配的流量；已分配额度可能仍在离线入口使用"
                );
            }
            if (old.reset_day, old.reset_hour, &old.timezone)
                != (r.reset_day, r.reset_hour, &r.timezone)
            {
                ensure!(
                    doc.resources
                        .ledgers
                        .get(&r.id)
                        .is_none_or(|l| l.allocations.is_empty()),
                    "已有额度分配，不能在本周期修改账期；请在新周期尚未使用时调整"
                );
            }
        }
        doc.resources.resources.retain(|old| old.id != r.id);
        doc.resources.resources.push(r.clone());
        schedule(doc)?;
        Ok(())
    })?;
    Ok(Json(json!(r)))
}
async fn remove_resource(
    State(app): State<App>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        let r = doc
            .resources
            .resources
            .iter()
            .find(|r| r.id == key)
            .context("资源不存在")?;
        ensure!(r.members.is_empty(), "请先移出资源成员");
        doc.resources.resources.retain(|r| r.id != key);
        // Keep this cycle's allocations until their leases expire; deleting a group never
        // revokes bytes already issued to an offline entry.
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
struct Selection {
    #[serde(default)]
    ids: Vec<String>,
}
fn host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
}
async fn candidates(State(app): State<App>, Json(input): Json<Selection>) -> Json<Value> {
    let s = app.store.lock().unwrap();
    let doc = &s.doc;
    let mut candidates: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in &doc.externals {
        if !input.ids.is_empty() && !input.ids.contains(&e.id) {
            continue;
        }
        if doc
            .resources
            .resources
            .iter()
            .any(|r| r.members.contains(&format!("external:{}", e.id)))
        {
            continue;
        }
        if let Some(h) = host(&e.url) {
            candidates.entry(h).or_default().push(e.id.clone());
        }
    }
    Json(json!(
        candidates
            .into_iter()
            .map(|(h, ids)| {
                let matching: Vec<_> = doc
                    .resources
                    .resources
                    .iter()
                    .filter(|r| {
                        r.members.iter().any(|m| {
                            doc.externals.iter().any(|e| {
                                m == &format!("external:{}", e.id)
                                    && host(&e.url).as_deref() == Some(&h)
                            })
                        })
                    })
                    .map(|r| json!({"id":r.id,"name":r.name}))
                    .collect();
                json!({"host":h,"ids":ids,"matching":matching})
            })
            .collect::<Vec<_>>()
    ))
}
#[derive(Deserialize)]
struct GroupInput {
    ids: Vec<String>,
    group: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    automatic: bool,
}
async fn groups(State(app): State<App>, Json(input): Json<GroupInput>) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        ensure!(
            !input.ids.is_empty()
                && input
                    .ids
                    .iter()
                    .all(|id| doc.externals.iter().any(|e| &e.id == id)),
            "请选择已有链接"
        );
        let members: Vec<_> = input
            .ids
            .iter()
            .map(|id| format!("external:{id}"))
            .collect();
        if input.automatic {
            ensure!(
                !doc.resources
                    .resources
                    .iter()
                    .any(|r| r.members.iter().any(|m| members.contains(m))),
                "链接已分组，请刷新后重试"
            );
        }
        let target = if let Some(group) = &input.group {
            ensure!(
                doc.resources.resources.iter().any(|r| &r.id == group),
                "分组不存在"
            );
            Some(group.clone())
        } else if !input.name.trim().is_empty() {
            let r = resources::Resource {
                id: id(),
                name: input.name.clone(),
                ..Default::default()
            };
            r.validate()?;
            let key = r.id.clone();
            doc.resources.resources.push(r);
            Some(key)
        } else {
            None
        };
        for r in &mut doc.resources.resources {
            r.members.retain(|m| !members.contains(m));
            if Some(&r.id) == target.as_ref() {
                r.members.extend(members.clone());
            }
        }
        schedule(doc)?;
        Ok(())
    })?;
    Ok(Json(json!({"ok":true})))
}
async fn grant(
    State(app): State<App>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(input): Json<resources::Request>,
) -> ApiResult<Json<resources::Grant>> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut s = app.store.lock().unwrap();
    if !s
        .doc
        .nodes
        .iter()
        .any(|n| n.id == node && equal(&digest(token), &n.token_hash))
    {
        return Err(error(StatusCode::UNAUTHORIZED, "节点身份无效"));
    }
    let before = s.doc.resources.clone();
    let grant = resources::allocate(&mut s.doc, &node, &input, now())?;
    if let Err(e) = s.save() {
        s.doc.resources = before;
        return Err(e.into());
    }
    Ok(Json(grant))
}

fn source_summary(s: &rules::Source) -> Value {
    let summary = |r: &rules::Rules| json!({"count":r.exact.len()+r.suffix.len(),"excluded_count":r.excluded_count,"excluded":r.excluded});
    let added = s.pending.as_ref().map(|p| {
        p.exact
            .iter()
            .filter(|d| s.active.as_ref().is_none_or(|a| !a.exact.contains(*d)))
            .count()
            + p.suffix
                .iter()
                .filter(|d| s.active.as_ref().is_none_or(|a| !a.suffix.contains(*d)))
                .count()
    });
    let removed = s.pending.as_ref().map(|p| {
        s.active
            .as_ref()
            .map(|a| a.exact.difference(&p.exact).count() + a.suffix.difference(&p.suffix).count())
            .unwrap_or(0)
    });
    json!({"id":s.id,"name":s.name,"category":s.category,"url":s.url,"auto_apply":s.auto_apply,"checked_at":s.checked_at,"applied_at":s.applied_at,"error":s.error,"active":s.active.as_ref().map(summary),"pending":s.pending.as_ref().map(summary),"added":added,"removed":removed})
}
async fn rule_view(State(app): State<App>) -> Json<Value> {
    let s = app.store.lock().unwrap();
    Json(json!(
        s.doc
            .rule_sources
            .iter()
            .map(source_summary)
            .collect::<Vec<_>>()
    ))
}
#[derive(Deserialize)]
struct SourceInput {
    #[serde(default)]
    id: String,
    name: String,
    url: String,
    #[serde(default)]
    auto_apply: bool,
}
async fn save_source(
    State(app): State<App>,
    Json(input): Json<SourceInput>,
) -> ApiResult<Json<Value>> {
    let key = edit(&app, |doc| {
        ensure!(
            !input.name.trim().is_empty() && input.name.len() <= 120 && input.url.len() <= 2048,
            "名称或 URL 无效"
        );
        let url = url::Url::parse(&input.url)?;
        ensure!(url.scheme() == "https", "规则源须为 HTTPS");
        let key = if input.id.is_empty() {
            id()
        } else {
            input.id.clone()
        };
        if let Some(s) = doc.rule_sources.iter_mut().find(|s| s.id == key) {
            if s.url != input.url {
                s.pending = None;
                s.checked_at = 0;
                s.error.clear();
            }
            s.name = input.name;
            s.url = input.url;
            s.auto_apply = input.auto_apply;
        } else {
            ensure!(input.id.is_empty(), "规则源不存在");
            doc.rule_sources.push(rules::Source {
                id: key.clone(),
                name: input.name,
                url: input.url,
                auto_apply: input.auto_apply,
                category: "自定义".into(),
                ..Default::default()
            });
        }
        Ok(key)
    })?;
    Ok(Json(json!({"id":key})))
}
async fn update(app: &App, key: &str) -> Result<()> {
    let url = {
        let s = app.store.lock().unwrap();
        s.doc
            .rule_sources
            .iter()
            .find(|s| s.id == key)
            .context("规则源不存在")?
            .url
            .clone()
    };
    let result = if url.is_empty() {
        rules::presets().into_iter().find(|s| s.id == key).and_then(|s| s.pending)
            .context("没有可用的内置规则，请配置 HTTPS 来源")
    } else {
        rules::fetch(&url).await
    };
    edit(app, |doc| {
        let s = doc
            .rule_sources
            .iter_mut()
            .find(|s| s.id == key && s.url == url)
            .context("规则源已改变，请重试")?;
        s.checked_at = now();
        match result {
            Ok(r) => {
                s.pending = Some(r);
                s.error.clear();
            }
            Err(e) => {
                s.error = format!("{e:#}");
                return Ok(());
            }
        }
        if s.auto_apply && !url.is_empty() {
            let active = s.active.clone();
            let pending = s.pending.clone();
            let applied_at = s.applied_at;
            s.active = s.pending.take();
            s.applied_at = now();
            if let Err(e) = schedule(doc) {
                let s = doc.rule_sources.iter_mut().find(|s| s.id == key).unwrap();
                s.active = active;
                s.pending = pending;
                s.applied_at = applied_at;
                s.error = format!("下载成功，自动应用失败：{e:#}");
            }
        }
        Ok(())
    })
    .map_err(|e| anyhow::anyhow!("{}", e.1))?;
    Ok(())
}
async fn rule_action(
    State(app): State<App>,
    Path((key, action)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    match action.as_str() {
        "update" => update(&app, &key).await?,
        "apply" => {
            edit(&app, |doc| {
                let s = doc
                    .rule_sources
                    .iter_mut()
                    .find(|s| s.id == key)
                    .context("规则源不存在")?;
                s.active = Some(s.pending.take().context("没有待应用版本")?);
                s.applied_at = now();
                s.error.clear();
                schedule(doc)
            })?;
        }
        _ => return Err(error(StatusCode::BAD_REQUEST, "操作无效")),
    }
    Ok(Json(json!({"ok":true})))
}
async fn access_view(State(app): State<App>, Path(key): Path<String>) -> Json<Value> {
    let s = app.store.lock().unwrap();
    Json(json!(
        s.doc.access_policies.get(&key).cloned().unwrap_or_default()
    ))
}
async fn save_access(
    State(app): State<App>,
    Path(key): Path<String>,
    Json(mut policy): Json<rules::Policy>,
) -> ApiResult<Json<Value>> {
    edit(&app, |doc| {
        ensure!(
            doc.lines
                .iter()
                .any(|l| l.id == key && l.kind == Kind::Vless),
            "代理不存在"
        );
        policy.domains = policy
            .domains
            .iter()
            .map(|d| rules::domain(d))
            .collect::<Result<Vec<_>>>()?;
        doc.access_policies.insert(key.clone(), policy);
        rules::compile(doc, &key)?;
        schedule(doc)
    })?;
    Ok(Json(json!({"ok":true})))
}
pub async fn run(app: App, stop: tokio_util::sync::CancellationToken) {
    loop {
        let ids = {
            let s = app.store.lock().unwrap();
            s.doc
                .rule_sources
                .iter()
                .filter(|s| !s.url.is_empty() && now().saturating_sub(s.checked_at) >= 86400)
                .map(|s| s.id.clone())
                .collect::<Vec<_>>()
        };
        for key in ids {
            tokio::select! {_=stop.cancelled()=>return,_=update(&app,&key)=>{}}
        }
        tokio::select! {_=stop.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(60))=>{}}
    }
}
