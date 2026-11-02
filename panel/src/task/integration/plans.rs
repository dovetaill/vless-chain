//! Shared customer-plan acceptance: rights, weighted metering, finance and restart durability.
use super::{
    super::workspace::Workspace,
    harness::{Harness, map},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub async fn socks(
    line: &Value,
    target: u16,
    command: u8,
    password: Option<&str>,
) -> Result<(TcpStream, std::net::SocketAddr)> {
    let mut stream = tokio::time::timeout(
        Duration::from_secs(5),
        TcpStream::connect((
            "127.0.0.1",
            line["hops"][0]["port"].as_u64().context("缺少监听端口")? as u16,
        )),
    )
    .await??;
    stream.write_all(&[5, 1, 2]).await?;
    let mut reply = [0; 2];
    stream.read_exact(&mut reply).await?;
    ensure!(reply == [5, 2], "SOCKS 未要求认证");
    let user = line["access"]["username"]
        .as_str()
        .context("缺少用户名")?
        .as_bytes();
    let secret = password
        .unwrap_or(line["access"]["password"].as_str().context("缺少密码")?)
        .as_bytes();
    let mut auth = vec![1, user.len() as u8];
    auth.extend(user);
    auth.push(secret.len() as u8);
    auth.extend(secret);
    stream.write_all(&auth).await?;
    let result = stream.read_exact(&mut reply).await;
    if password.is_some() {
        ensure!(result.is_err() || reply != [1, 0], "错误凭据被接受");
        return Ok((stream, "127.0.0.1:0".parse()?));
    }
    result?;
    ensure!(reply == [1, 0], "SOCKS 认证失败");
    let mut request = vec![5, command, 0, 1, 127, 0, 0, 1];
    request.extend(target.to_be_bytes());
    stream.write_all(&request).await?;
    let mut response = [0; 10];
    stream.read_exact(&mut response).await?;
    ensure!(
        response[..2] == [5, 0] && response[3] == 1,
        "SOCKS 连接失败：{response:?}"
    );
    let port = u16::from_be_bytes([response[8], response[9]]);
    let address = std::net::Ipv4Addr::new(response[4], response[5], response[6], response[7]);
    Ok((
        stream,
        std::net::SocketAddr::from((
            if address.is_unspecified() {
                std::net::Ipv4Addr::LOCALHOST
            } else {
                address
            },
            port,
        )),
    ))
}
pub async fn echo(line: &Value, target: u16, payload: &[u8]) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(8), async {
        let (mut stream, _) = socks(line, target, 1, None).await?;
        stream.write_all(payload).await?;
        let mut received = vec![0; payload.len()];
        stream.read_exact(&mut received).await?;
        ensure!(received == payload, "TCP 回显不匹配");
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
pub fn database(path: &Path, query: &str) -> Result<Vec<Value>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = db.prepare(query)?;
    let columns = statement.column_count();
    Ok(statement
        .query_map([], |row| {
            let mut result = vec![];
            for index in 0..columns {
                result.push(match row.get_ref(index)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(v) => json!(v),
                    rusqlite::types::ValueRef::Real(v) => json!(v),
                    rusqlite::types::ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                    rusqlite::types::ValueRef::Blob(v) => json!(v),
                });
            }
            Ok(json!(result))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}
pub fn stored(h: &Harness<'_>) -> Result<Value> {
    Ok(serde_json::from_str(
        database(
            &h.root.join("control/panel.db"),
            "SELECT body FROM state WHERE id=1",
        )?[0][0]
            .as_str()
            .context("状态内容无效")?,
    )?)
}
fn consumption(h: &Harness<'_>) -> Result<Vec<Vec<Value>>> {
    (0..2)
        .map(|i| {
            database(
                &h.root.join(format!("agent-{i}/resources.db")),
                "SELECT resource,cycle,used FROM consumption ORDER BY resource",
            )
        })
        .collect()
}
pub async fn create(h: &Harness<'_>, nodes: &[Value], name: &str, path: &[usize]) -> Result<Value> {
    let hops = path
        .iter()
        .map(|i| Ok(json!({"node_id":nodes[*i]["id"],"port":h.port()?})))
        .collect::<Result<Vec<_>>>()?;
    h.request(
        "/api/lines",
        "POST",
        Some(&json!({"name":name,"kind":"vless","hops":hops,"access":{"protocol":"socks"}})),
        200,
    )
    .await
}
async fn pay(h: &Harness<'_>, key: &str, request_id: &str) -> Result<(Value, Value)> {
    let count = h.state().await?["payments"].as_array().unwrap().len();
    let value = json!({"line_ids":[key],"term":"monthly","request_id":request_id,"note":"隔离验收模拟人工到账"});
    let order = h
        .request("/api/rentals/orders", "POST", Some(&value), 200)
        .await?;
    ensure!(
        h.request("/api/rentals/orders", "POST", Some(&value), 200)
            .await?["id"]
            == order["id"]
            && h.request(
                &format!("/api/rentals/orders/by-request/{request_id}"),
                "GET",
                None,
                200
            )
            .await?["id"]
                == order["id"],
        "订单请求未保持幂等"
    );
    ensure!(
        h.state().await?["payments"].as_array().unwrap().len() == count,
        "订单提前记账"
    );
    let path = format!(
        "/api/rentals/orders/{}/confirm",
        order["id"].as_str().unwrap()
    );
    let receipt = h.request(&path, "POST", Some(&json!({})), 200).await?["payments"].clone();
    ensure!(
        h.request(&path, "POST", Some(&json!({})), 200).await?["payments"] == receipt
            && receipt.as_array().unwrap().len() == 1
            && receipt[0]["line_id"] == key,
        "重复确认改变支付记录"
    );
    ensure!(
        h.state().await?["payments"].as_array().unwrap().len() == count + 1,
        "确认支付次数错误"
    );
    Ok((receipt[0].clone(), order))
}
async fn link(h: &Harness<'_>, path: &str) -> Result<String> {
    Ok(
        h.request(path, "POST", Some(&json!({})), 200).await?["path"]
            .as_str()
            .context("查询地址缺失")?
            .split_once("#token=")
            .context("令牌缺失")?
            .1
            .into(),
    )
}
async fn public(h: &Harness<'_>, token: &str) -> Result<Value> {
    h.anonymous(
        "/api/public/lookup",
        "POST",
        Some(&json!({"token":token})),
        200,
    )
    .await
}
fn ids(value: &Value) -> std::collections::BTreeSet<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().into())
        .collect()
}

pub async fn run(work: &Workspace, stage: &Path) -> Result<()> {
    let mut h = Harness::new(work, stage, "customer-plans")?;
    let echo_port = h.echo().await?;
    h.allow_loopback(&[echo_port])?;
    let (tls, cert, _) = h.tls("localhost").await?;
    let control_env = map(&[
        ("CHAIN_APPLY_TIMEOUT", "15".into()),
        ("CHAIN_XRAY_LOG_LEVEL", "warning".into()),
        ("SSL_CERT_FILE", cert.display().to_string()),
    ]);
    let controller = h.panel(control_env.clone()).await?;
    let admin_token = fs::read_to_string(h.root.join("control/admin-token"))?;
    h.preflight(tls, true).await?;
    let mut nodes = vec![];
    let mut agents = vec![];
    let mut agent_envs = vec![];
    for i in 0..2 {
        let node=h.request("/api/nodes","POST",Some(&json!({"name":format!("套餐入口 {i}"),"host":"127.0.0.1","target":format!("127.0.0.1:{tls}"),"server_name":"localhost"})),200).await?;
        let (pid, env) = h
            .agent(
                i,
                &node,
                map(&[("SSL_CERT_FILE", cert.display().to_string())]),
            )
            .await?;
        nodes.push(node);
        agents.push(pid);
        agent_envs.push(env);
    }
    h.wait_state("具备套餐能力的 Agent", 30, |s| {
        s["nodes"].as_array().is_some_and(|a| {
            a.len() == 2
                && a.iter().all(|n| {
                    n["online"] == true && n["report"]["system"]["customer_plan_version"] == 1
                })
        })
    })
    .await?;
    let customer=h.request("/api/customers","POST",Some(&json!({"name":"private-customer","contact":"private-contact","notes":"private-notes"})),200).await?;
    let prices = json!({"monthly":2000,"quarterly":null,"yearly":null});
    let old = create(&h, &nodes, "原独立租赁", &[0]).await?;
    h.request(
        &format!("/api/rentals/{}", old["id"].as_str().unwrap()),
        "POST",
        Some(&json!({"customer_id":customer["id"],"prices":prices})),
        200,
    )
    .await?;
    h.request(
        "/api/rentals/renew",
        "POST",
        Some(&json!({"line_ids":[old["id"]],"term":"monthly","request_id":"independent-opening"})),
        200,
    )
    .await?;
    h.publish("applied").await?;
    h.synced(&[old.clone()]).await?;
    let old_rental = h.current(&old).await?["rental"].clone();
    let old_export = h
        .request(
            &format!("/api/lines/{}/export", old["id"].as_str().unwrap()),
            "GET",
            None,
            200,
        )
        .await?;
    let lines = vec![
        create(&h, &nodes, "成员 A · 双倍率", &[0, 1]).await?,
        create(&h, &nodes, "成员 B · 单倍率", &[1]).await?,
    ];
    for line in &lines {
        ensure!(
            h.current(line).await?["plan_eligible"] == true,
            "新线路不能加入套餐"
        );
    }
    let input = json!({"id":"","name":"真实共享套餐","customer_id":customer["id"],"monthly_gib":0.125,"members":[{"id":"","name":"成员 0","line_ids":[lines[0]["id"]],"monthly_gib":0.0625},{"id":"","name":"成员 1","line_ids":[lines[1]["id"]],"monthly_gib":null}],"prices":prices});
    let mut invalid = input.clone();
    invalid["members"] =
        json!([{"id":"","name":"不可迁入","line_ids":[old["id"]],"monthly_gib":null}]);
    h.request("/api/customer-plans", "POST", Some(&invalid), 400)
        .await?;
    ensure!(
        h.state().await?["customer_plans"] == json!([])
            && h.current(&old).await?["rental"] == old_rental,
        "拒绝迁入改变独立权益"
    );
    h.request("/api/customer-plans", "POST", Some(&input), 200)
        .await?;
    let plan = h.state().await?["customer_plans"][0].clone();
    let plan_id = plan["id"].as_str().unwrap();
    let plan_key = format!("plan:{plan_id}");
    for line in &lines {
        h.request("/api/rentals/renew","POST",Some(&json!({"line_ids":[line["id"]],"term":"monthly","request_id":format!("cannot-bypass-{}",line["id"])})),400).await?;
        h.request(
            "/api/lines/operate",
            "POST",
            Some(&json!({"line_ids":[line["id"]],"action":"unlimited"})),
            400,
        )
        .await?;
    }
    let finance = h
        .request(
            "/api/security/members",
            "POST",
            Some(&json!({"token":admin_token,"name":"验收财务","role":"finance"})),
            200,
        )
        .await?;
    let ops = h
        .request(
            "/api/security/members",
            "POST",
            Some(&json!({"token":admin_token,"name":"验收运维","role":"ops"})),
            200,
        )
        .await?;
    let admin = h.save_cookie();
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":finance["token"]})),
        200,
    )
    .await?;
    let (opening, opening_order) = pay(&h, &plan_key, "plan-opening").await?;
    ensure!(
        opening_order["created_by"]["id"] == finance["member"]["id"],
        "订单未记录财务身份"
    );
    ensure!(
        h.state().await?["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |l| l["access"]["password"].as_str().is_none_or(str::is_empty)
                    && l["share"].is_null()
            ),
        "财务泄漏代理凭据"
    );
    h.request(
        "/api/lines/operate",
        "POST",
        Some(&json!({"line_ids":[old["id"]],"action":"disable"})),
        403,
    )
    .await?;
    h.restore_cookie(admin.clone());
    for line in &lines {
        let current = h.current(line).await?;
        ensure!(
            current["rental"]["started_at"].as_u64().unwrap() > 0 && current["published"] == false,
            "开通未保存权益或提前发布"
        );
    }
    h.publish("applied").await?;
    let all = vec![old.clone(), lines[0].clone(), lines[1].clone()];
    h.synced(&all).await?;
    ensure!(
        h.current(&lines[0]).await?["multiplier"] == 2
            && h.current(&lines[1]).await?["multiplier"] == 1,
        "套餐倍率错误"
    );
    let payload = b"customer-plan-payload".repeat(128);
    for line in &lines {
        echo(line, echo_port, &payload).await?;
    }
    let expected = [4 * payload.len() as u64, 2 * payload.len() as u64];
    for (i, rows) in consumption(&h)?.iter().enumerate() {
        let used = rows
            .iter()
            .map(|r| (r[0].as_str().unwrap(), r[2].as_u64().unwrap()))
            .collect::<BTreeMap<_, _>>();
        ensure!(
            used[plan_key.as_str()] == expected[i]
                && used[format!(
                    "member:{plan_id}:{}",
                    plan["members"][i]["id"].as_str().unwrap()
                )
                .as_str()]
                    == expected[i],
            "共享/成员账本流量错误：{used:?}"
        );
    }
    tokio::time::sleep(Duration::from_secs(10)).await;
    for line in &lines {
        echo(line, echo_port, b"x").await?;
    }
    let view = h
        .wait_state("共享用量上报", 30, |s| {
            s["customer_plans"][0]["usage"]["used"] == expected.iter().sum::<u64>()
        })
        .await?["customer_plans"][0]
        .clone();
    ensure!(
        view["members"][0]["usage"]["used"] == expected[0]
            && view["members"][1]["usage"]["used"] == expected[1],
        "成员上报错误"
    );
    let reserved = view["usage"]["reserved"].as_u64().context("缺少预留额度")?;
    ensure!(
        reserved > expected.iter().sum::<u64>() && reserved <= 128 * 1024 * 1024,
        "预留额度越界"
    );
    let mut exports = vec![];
    for line in &lines {
        exports.push(
            h.request(
                &format!("/api/lines/{}/export", line["id"].as_str().unwrap()),
                "GET",
                None,
                200,
            )
            .await?,
        );
    }
    let plan_path = format!("/api/customer-plans/{plan_id}/query-link");
    let member_path = format!(
        "/api/customer-plans/{plan_id}/members/{}/query-link",
        plan["members"][0]["id"].as_str().unwrap()
    );
    let plan_token = link(&h, &plan_path).await?;
    let member_token = link(&h, &member_path).await?;
    let whole = public(&h, &plan_token).await?;
    let member = public(&h, &member_token).await?;
    ensure!(
        ids(&whole["lines"]) == ids(&json!(lines))
            && whole["customer_plans"][0]["members"]
                .as_array()
                .unwrap()
                .len()
                == 2,
        "套餐查询范围错误"
    );
    ensure!(
        ids(&member["lines"]) == ids(&json!([lines[0]]))
            && ids(&member["customer_plans"][0]["members"]) == ids(&json!([plan["members"][0]])),
        "成员查询泄漏其他成员"
    );
    let serialized = serde_json::to_string(&member)?;
    for forbidden in [
        customer["id"].as_str().unwrap(),
        "private-customer",
        "private-contact",
        "private-notes",
        plan["members"][1]["id"].as_str().unwrap(),
        "line_ids",
        "password",
        "token_hash",
    ] {
        ensure!(!serialized.contains(forbidden), "公开查询泄漏 {forbidden}");
    }
    let lookup = h
        .anonymous(
            "/api/public/lookup",
            "POST",
            Some(&json!({"config":exports[0]["share"]})),
            200,
        )
        .await?;
    ensure!(
        lookup["customer_plans"][0]["members"]
            .as_array()
            .unwrap()
            .len()
            == 1,
        "配置查询泄漏套餐范围"
    );
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":member_token})),
        401,
    )
    .await?;
    let previous_token = member_token;
    let member_token = link(&h, &member_path).await?;
    h.anonymous(
        "/api/public/lookup",
        "POST",
        Some(&json!({"token":previous_token})),
        404,
    )
    .await?;
    ensure!(
        public(&h, &member_token).await?["lines"][0]["id"] == lines[0]["id"],
        "轮换令牌改变成员范围"
    );
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":ops["token"]})),
        200,
    )
    .await?;
    let view = h.state().await?;
    ensure!(
        view["customers"] == json!([])
            && view["payments"] == json!([])
            && view["customer_plans"] == json!([]),
        "运维看到财务记录"
    );
    h.request(
        "/api/rentals/orders",
        "POST",
        Some(&json!({"line_ids":[plan_key],"term":"monthly","request_id":"ops-denied"})),
        403,
    )
    .await?;
    h.restore_cookie(admin.clone());
    let before = stored(&h)?;
    let counters = consumption(&h)?;
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":finance["token"]})),
        200,
    )
    .await?;
    let (renewal, _) = pay(&h, &plan_key, "plan-renewal").await?;
    h.restore_cookie(admin.clone());
    h.synced(&lines).await?;
    let after = stored(&h)?;
    ensure!(
        after["rentals"][&plan_key]["started_at"] == before["rentals"][&plan_key]["started_at"]
            && after["rentals"][&plan_key]["expires_at"].as_u64()
                > before["rentals"][&plan_key]["expires_at"].as_u64(),
        "续费改变起点或未延长到期"
    );
    ensure!(
        after["customer_ledgers"] == before["customer_ledgers"]
            && consumption(&h)? == counters
            && renewal["previous_expires_at"] == before["rentals"][&plan_key]["expires_at"]
            && h.current(&old).await?["rental"] == old_rental,
        "续费改变用量或独立权益"
    );
    let refund = json!({"payment_id":opening["id"],"kind":"refund","amount_fen":500,"expected_net_fen":2000,"note":"验收模拟线下已退 5 元","disable_line":false,"request_id":"refund-keep"});
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":finance["token"]})),
        200,
    )
    .await?;
    let first = h
        .request("/api/rentals/adjustments", "POST", Some(&refund), 200)
        .await?;
    ensure!(
        h.request("/api/rentals/adjustments", "POST", Some(&refund), 200)
            .await?
            == first
            && h.request(
                "/api/rentals/adjustments/by-request/refund-keep",
                "GET",
                None,
                200
            )
            .await?
                == first,
        "退款未保持幂等"
    );
    h.restore_cookie(admin.clone());
    for line in &lines {
        ensure!(
            h.current(line).await?["enabled"] == true,
            "部分退款擅自停用成员"
        );
    }
    echo(&lines[1], echo_port, b"after-keep").await?;
    let before = stored(&h)?;
    let lease = before["rentals"][&plan_key].clone();
    let ledger = before["customer_ledgers"].clone();
    let mut final_refund = refund.clone();
    final_refund["amount_fen"] = json!(1500);
    final_refund["expected_net_fen"] = json!(1500);
    final_refund["note"] = json!("验收模拟退还其余 15 元并停用全部成员");
    final_refund["disable_line"] = json!(true);
    final_refund["expected_line_ids"] = json!([lines[0]["id"], lines[1]["id"]]);
    final_refund["request_id"] = json!("refund-disable");
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":finance["token"]})),
        200,
    )
    .await?;
    let mut invalid = final_refund.clone();
    invalid["expected_line_ids"] = json!([lines[0]["id"]]);
    h.request("/api/rentals/adjustments", "POST", Some(&invalid), 400)
        .await?;
    let last = h
        .request("/api/rentals/adjustments", "POST", Some(&final_refund), 200)
        .await?;
    ensure!(
        h.request("/api/rentals/adjustments", "POST", Some(&final_refund), 200)
            .await?
            == last
            && h.request("/api/rentals/adjustments", "POST", Some(&refund), 200)
                .await?
                == first,
        "停用退款破坏幂等"
    );
    let mut excess = refund.clone();
    excess["request_id"] = json!("excess-refund");
    excess["expected_net_fen"] = json!(0);
    h.request("/api/rentals/adjustments", "POST", Some(&excess), 400)
        .await?;
    let finance_cookie = h.save_cookie();
    h.restore_cookie(admin.clone());
    h.synced(&lines).await?;
    let affected = last["affected_line_ids"]
        .as_array()
        .context("缺少影响范围")?;
    ensure!(
        affected.len() == 2 && lines.iter().all(|l| affected.contains(&l["id"])),
        "退款停用范围错误"
    );
    ensure!(
        stored(&h)?["rentals"][&plan_key] == lease
            && stored(&h)?["customer_ledgers"] == ledger
            && h.state().await?["payment_adjustments"]
                .as_array()
                .unwrap()
                .len()
                == 2,
        "退款改写权益或用量"
    );
    for line in &lines {
        ensure!(
            h.current(line).await?["enabled"] == false,
            "完整退款未停用成员"
        );
        let mut connection = TcpStream::connect((
            "127.0.0.1",
            line["hops"][0]["port"].as_u64().unwrap() as u16,
        ))
        .await?;
        let _ = connection.write_all(&[5, 1, 2]).await;
        let mut bytes = [0; 2];
        let result = connection.read(&mut bytes).await;
        ensure!(result.is_err() || result? == 0, "退款成员仍接受连接");
    }
    echo(&old, echo_port, b"independent-rights").await?;
    h.request(
        &format!(
            "/api/security/members/{}/revoke",
            finance["member"]["id"].as_str().unwrap()
        ),
        "POST",
        Some(&json!({"token":admin_token})),
        200,
    )
    .await?;
    h.restore_cookie(finance_cookie);
    h.request("/api/state", "GET", None, 401).await?;
    h.restore_cookie(admin.clone());
    h.anonymous(
        "/api/login",
        "POST",
        Some(&json!({"token":finance["token"]})),
        401,
    )
    .await?;
    let before = stored(&h)?;
    let counters = consumption(&h)?;
    let epochs = (0..2)
        .map(|i| {
            database(
                &h.root.join(format!("agent-{i}/resources.db")),
                "SELECT epoch FROM identity",
            )
        })
        .collect::<Result<Vec<_>>>()?;
    for pid in agents {
        work.stop(pid).await?;
    }
    work.stop(controller).await?;
    let mut env = control_env;
    env.insert(
        "CHAIN_LISTEN".into(),
        h.base.trim_start_matches("http://").into(),
    );
    env.insert(
        "CHAIN_DATA_DIR".into(),
        h.root.join("control").display().to_string(),
    );
    env.insert("CHAIN_XRAY_BIN".into(), h.xray.display().to_string());
    h.launch("control-restarted", &h.bin.join("chain-panel"), env, &[])?;
    h.ready().await?;
    h.login(&h.root.join("control")).await?;
    let after = stored(&h)?;
    for field in [
        "customers",
        "customer_plans",
        "customer_ledgers",
        "customer_query_tokens",
        "rentals",
        "payments",
        "rental_orders",
        "payment_adjustments",
        "operator_members",
        "subscriptions",
        "lines",
        "applied_lines",
    ] {
        ensure!(after[field] == before[field], "重启改变 {field}");
    }
    let restarted_at = chrono::Utc::now().timestamp();
    for (i, env) in agent_envs.into_iter().enumerate() {
        h.launch(
            &format!("agent-{i}-restarted"),
            &h.bin.join("chain-agent"),
            env,
            &[],
        )?;
    }
    h.wait_state("重启 Agent", 30, |s| {
        s["nodes"].as_array().unwrap().iter().all(|n| {
            n["seen"].as_i64().is_some_and(|v| v > restarted_at) && n["report"]["running"] == true
        })
    })
    .await?;
    h.synced(&all).await?;
    ensure!(consumption(&h)? == counters, "重启改变流量账本");
    for i in 0..2 {
        ensure!(
            database(
                &h.root.join(format!("agent-{i}/resources.db")),
                "SELECT epoch FROM identity"
            )? == epochs[i],
            "重启改变流量身份"
        );
    }
    for (line, export) in lines.iter().zip(exports) {
        let lookup = h
            .anonymous(
                "/api/public/lookup",
                "POST",
                Some(&json!({"config":export["share"]})),
                200,
            )
            .await?;
        ensure!(
            lookup["lines"].as_array().unwrap().len() == 1
                && lookup["lines"][0]["id"] == line["id"],
            "重启改变原始凭据"
        );
        ensure!(
            h.current(line).await?["enabled"] == false,
            "重启恢复已停用成员"
        );
    }
    ensure!(
        h.request(
            &format!("/api/lines/{}/export", old["id"].as_str().unwrap()),
            "GET",
            None,
            200
        )
        .await?
            == old_export,
        "重启改变独立出口凭据"
    );
    for token in [&plan_token, &member_token] {
        ensure!(
            public(&h, token).await?["lines"]
                .as_array()
                .unwrap()
                .iter()
                .all(|l| l["status"] == "disabled"),
            "重启查询状态错误"
        );
    }
    h.request(
        &format!(
            "/api/rentals/orders/{}/confirm",
            opening_order["id"].as_str().unwrap()
        ),
        "POST",
        Some(&json!({})),
        200,
    )
    .await?;
    ensure!(
        stored(&h)?["payments"] == before["payments"],
        "重启后重复确认再次记账"
    );
    echo(&old, echo_port, b"after-restart").await?;
    h.close().await?;
    println!("PASS 套餐真实加权流量、成员隔离、财务订单/退款幂等、权益与凭据重启持久性");
    Ok(())
}
