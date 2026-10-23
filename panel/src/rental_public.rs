use super::*;

const MAX_BODY: usize = 256 * 1024;
const MAX_PROXIES: usize = 100;
static REQUESTS: Mutex<BTreeMap<std::net::IpAddr, (u64, u32)>> = Mutex::new(BTreeMap::new());

#[derive(Deserialize)]
struct Lookup {
    config: Option<String>,
    token: Option<String>,
}

fn missing() -> ApiError {
    error(
        StatusCode::NOT_FOUND,
        "未找到可查询的租赁代理，请检查配置或查询链接",
    )
}

fn fingerprint(node: &Value) -> Result<String> {
    let text = |key: &str| {
        node[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .with_context(|| format!("缺少 {key}"))
    };
    let protocol = text("type")?;
    let host = text("server")?.trim_matches(['[', ']']);
    let host = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => url::Host::parse(host.trim_end_matches('.'))?.to_string(),
    };
    let port = node["port"]
        .as_u64()
        .filter(|p| *p > 0 && *p <= 65535)
        .context("缺少有效端口")?;
    let uuid = || -> Result<String> { Ok(uuid::Uuid::parse_str(text("uuid")?)?.to_string()) };
    let credentials = match protocol {
        "vless" => {
            let reality = node.get("reality-opts").filter(|v| !v.is_null());
            let reality = reality
                .map(|v| -> Result<Value> {
                    Ok(json!([
                        v["public-key"].as_str().context("缺少 Reality 公钥")?,
                        v["short-id"]
                            .as_str()
                            .context("缺少 Reality short-id")?
                            .to_ascii_lowercase()
                    ]))
                })
                .transpose()?;
            json!([uuid()?, reality])
        }
        "vmess" => json!([uuid()?]),
        "tuic" => json!([uuid()?, text("password")?]),
        "trojan" | "hysteria2" => json!([text("password")?]),
        "ss" => json!([text("cipher")?, text("password")?]),
        "http" | "socks5" => json!([text("username")?, text("password")?, node["tls"] == true]),
        _ => anyhow::bail!("不支持此代理协议"),
    };
    Ok(digest(&serde_json::to_string(&json!([
        protocol,
        host,
        port,
        credentials
    ]))?))
}

fn fingerprints(config: &str) -> Result<Vec<Option<String>>> {
    let config = config.trim();
    ensure!(
        !config.is_empty() && config.len() <= MAX_BODY,
        "配置大小无效"
    );
    if [
        "vless://",
        "vmess://",
        "hysteria2://",
        "hy2://",
        "tuic://",
        "ss://",
        "trojan://",
        "http://",
        "https://",
        "socks://",
        "socks5://",
    ]
    .iter()
    .any(|prefix| config.starts_with(prefix))
    {
        return Ok(vec![
            chain_panel::external::mihomo(config, "")
                .and_then(|node| fingerprint(&node))
                .ok(),
        ]);
    }
    let mut value: Value = serde_yaml_ng::from_str(config)?;
    if let Some(proxies) = value.get_mut("proxies") {
        value = proxies.take();
    }
    // Provider URLs are never fetched; only credentials present in this request can grant access.
    let nodes = match value {
        Value::Array(nodes) => nodes,
        node @ Value::Object(_) => vec![node],
        _ => anyhow::bail!("配置中没有代理"),
    };
    ensure!(
        !nodes.is_empty() && nodes.len() <= MAX_PROXIES,
        "单次最多查询 100 个代理"
    );
    Ok(nodes.iter().map(|node| fingerprint(node).ok()).collect())
}

fn matched<'a>(doc: &'a Document, supplied: &[Option<String>]) -> (Vec<&'a Line>, Vec<String>) {
    let applied = Document {
        nodes: doc.applied_nodes.clone(),
        lines: doc.applied_lines.clone(),
        externals: doc.applied_externals.clone(),
        ..Default::default()
    };
    let candidates: Vec<_> = doc
        .applied_lines
        .iter()
        .filter(|line| line.kind == Kind::Vless && !line.hops.is_empty())
        .filter_map(|line| {
            chain_panel::access::mihomo_node(line, &applied)
                .and_then(|node| fingerprint(&node))
                .ok()
                .map(|identity| (line, identity))
        })
        .collect();
    let mut lines = Vec::new();
    let mut unmatched = Vec::new();
    for (index, supplied) in supplied.iter().enumerate() {
        let matches: Vec<_> = candidates
            .iter()
            .filter(|(_, candidate)| supplied.as_ref().is_some_and(|s| equal(s, candidate)))
            .map(|(line, _)| *line)
            .collect();
        if matches.len() == 1 && chain_panel::plans::lease(doc, &matches[0].id).is_some() {
            if !lines.iter().any(|line: &&Line| line.id == matches[0].id) {
                lines.push(matches[0]);
            }
        } else {
            unmatched.push(format!("第 {} 个代理", index + 1));
        }
    }
    (lines, unmatched)
}

fn lookup_result(doc: &Document, input: Lookup, at: u64) -> ApiResult<Value> {
    let (lines, unmatched) = match (input.config, input.token) {
        (Some(config), None) => {
            let supplied = fingerprints(&config).map_err(|_| {
                error(
                    StatusCode::BAD_REQUEST,
                    "配置格式无效；单次最多查询 100 个代理",
                )
            })?;
            matched(doc, &supplied)
        }
        (None, Some(token))
            if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            let hash = digest(&token);
            let scope = doc.customer_query_tokens.iter().find(|(_, saved)| equal(saved, &hash)).map(|(key,_)| key.as_str());
            let lines: Vec<_> = doc
                .applied_lines
                .iter()
                .filter(|line| line.kind == Kind::Vless)
                .filter(|line| {
                    let customer_scope = scope.is_some_and(|scope| doc.customer_plans.iter().any(|p|
                        p.lines().any(|id| id == &line.id) && (p.key() == scope || p.members.iter().any(|m| chain_panel::plans::member_key(p,m)==scope && m.line_ids.contains(&line.id)))));
                    customer_scope || doc.rentals
                        .get(&line.id)
                        .is_some_and(|r| equal(&r.query_token_hash, &hash))
                })
                .collect();
            if lines.is_empty() || scope.is_none() && lines.len() != 1 {
                return Err(missing());
            }
            (lines, Vec::new())
        }
        (None, Some(_)) => return Err(missing()),
        _ => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "请提供代理配置或查询凭据中的一种",
            ));
        }
    };
    if lines.is_empty() {
        return Err(missing());
    }
    let authorized: BTreeSet<_> = lines.iter().map(|l| l.id.as_str()).collect();
    let customer_plans: Vec<_> = chain_panel::plans::view(doc, at).into_iter().filter_map(|mut plan| {
        let members = plan["members"].as_array_mut()?;
        members.retain(|m| m["line_ids"].as_array().is_some_and(|ids| ids.iter().any(|id| id.as_str().is_some_and(|id| authorized.contains(id)))));
        if members.is_empty() { return None; }
        for member in members { member.as_object_mut().unwrap().remove("line_ids"); }
        plan.as_object_mut().unwrap().remove("customer_id");
        if let Some(rental) = plan["rental"].as_object_mut() { rental.remove("customer_id"); }
        Some(plan)
    }).collect();
    let lines: Vec<_> = lines
        .into_iter()
        .map(|line| {
            let view = chain_panel::rental::view(doc, line, at);
            let rental = &view["rental"];
            json!({"id":line.id,"name":line.name,"status":view["rental_status"],"customer_plan_id":view["customer_plan_id"],
            "multiplier":view["multiplier"],"traffic":view["traffic"],"limits":line.limits,"sync_status":view["sync_status"],
            "rental":{"prices":rental["prices"],"started_at":rental["started_at"],
                "expires_at":rental["expires_at"],"next_reset_at":rental["next_reset_at"],"permanent":rental["permanent"]}})
        })
        .collect();
    Ok(json!({"lines":lines,"customer_plans":customer_plans,"unmatched":unmatched,"renewal_contact":doc.renewal_contact,"time":at}))
}

pub async fn lookup(State(app): State<App>, request: Request) -> ApiResult<Json<Value>> {
    let at = now();
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|peer| peer.0.ip())
        .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED.into());
    // Only a local reverse proxy may supply the client address; remote headers are untrusted.
    let peer = if peer.is_loopback() {
        request
            .headers()
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(peer)
    } else {
        peer
    };
    {
        let mut requests = REQUESTS.lock().unwrap();
        requests.retain(|_, (since, _)| at.saturating_sub(*since) < 60);
        if requests.len() >= 4096 && !requests.contains_key(&peer) {
            return Err(error(StatusCode::TOO_MANY_REQUESTS, "查询繁忙，请稍后重试"));
        }
        let bucket = requests.entry(peer).or_insert((at, 0));
        if bucket.1 >= 120 {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "查询过于频繁，请稍后重试",
            ));
        }
        bucket.1 += 1;
    }
    let body = axum::body::to_bytes(request.into_body(), MAX_BODY)
        .await
        .map_err(|_| error(StatusCode::PAYLOAD_TOO_LARGE, "查询内容不能超过 256 KiB"))?;
    let input: Lookup = serde_json::from_slice(&body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "查询请求格式无效"))?;
    let store = app.store.lock().unwrap();
    Ok(Json(lookup_result(&store.doc, input, at)?))
}

pub async fn query_link(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let token = secret();
    edit(&app, |doc| {
        ensure!(
            doc.applied_lines
                .iter()
                .any(|l| l.id == id && l.kind == Kind::Vless),
            "请先应用此租赁代理"
        );
        doc.rentals
            .get_mut(&id)
            .context("此代理尚未纳入租赁管理")?
            .query_token_hash = digest(&token);
        Ok(())
    })?;
    Ok(Json(json!({"path":format!("/query#token={token}")})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn customer_member_link_cannot_read_another_member_or_backend_identity() {
        let mut doc=document();
        doc.rentals.clear();
        let plan=chain_panel::plans::Plan { id:id(),name:"Team".into(),customer_id:"private-customer".into(),monthly_gib:10.0,
            members:["one","two"].into_iter().map(|line| chain_panel::plans::Member {id:id(),name:line.into(),line_ids:vec![line.into()],monthly_gib:None}).collect() };
        let owner="b".repeat(64); let member="c".repeat(64);
        doc.customer_query_tokens.insert(plan.key(),digest(&owner));
        doc.customer_query_tokens.insert(chain_panel::plans::member_key(&plan,&plan.members[0]),digest(&member));
        doc.rentals.insert(plan.key(),Rental {customer_id:plan.customer_id.clone(),started_at:1,..Default::default()});
        doc.customer_plans.push(plan.clone());
        let owner_view=lookup_result(&doc,Lookup {config:None,token:Some(owner)},2).unwrap();
        assert_eq!(owner_view["lines"].as_array().unwrap().len(),2);
        let member_view=lookup_result(&doc,Lookup {config:None,token:Some(member.clone())},2).unwrap();
        assert_eq!(member_view["lines"].as_array().unwrap().len(),1);
        assert_eq!(member_view["customer_plans"][0]["members"].as_array().unwrap().len(),1);
        let serialized=member_view.to_string();
        for forbidden in ["private-customer","private-contact","private-agent-token","password","token_hash","two"] { assert!(!serialized.contains(forbidden),"{forbidden}"); }
        doc.customer_query_tokens.insert(chain_panel::plans::member_key(&plan,&plan.members[0]),digest(&"d".repeat(64)));
        assert!(lookup_result(&doc,Lookup {config:None,token:Some(member)},2).is_err());
        assert!(doc.operator_members.is_empty());
    }

    fn node(protocol: &str) -> Value {
        json!({"name":"label", "type":protocol, "server":"proxy.example", "port":443,
            "uuid":"00000000-0000-4000-8000-000000000001", "password":"secret", "tls":true})
    }

    fn document() -> Document {
        let server: Node = serde_json::from_value(json!({"id":"server", "name":"server", "host":"proxy.example",
            "token_hash":"private-agent-token", "reality":{"target":"origin.example:443", "server_name":"origin.example",
            "private_key":"private", "public_key":"public", "short_id":"abcd"}})).unwrap();
        let lines: Vec<Line> = ["one", "two"].iter().enumerate().map(|(index, id)| {
            serde_json::from_value(json!({"id":id,"name":id,"kind":"vless",
                "hops":[{"node_id":"server","port":443 + index,"uuid":"00000000-0000-4000-8000-000000000001"}],
                "access":{"protocol":"trojan","password":"secret","tls":{"mode":"manual","domain":"proxy.example"}}})).unwrap()
        }).collect();
        Document {
            nodes: vec![server.clone()],
            applied_nodes: vec![server],
            lines: lines.clone(),
            applied_lines: lines,
            rentals: ["one", "two"]
                .into_iter()
                .map(|id| {
                    (
                        id.into(),
                        Rental {
                            customer_id: "private-customer".into(),
                            started_at: 1,
                            expires_at: 2,
                            query_token_hash: digest(&"a".repeat(64)),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            customers: vec![Customer {
                id: "private-customer".into(),
                name: "private-name".into(),
                contact: "private-contact".into(),
                notes: "private-notes".into(),
            }],
            renewal_contact: "运营联系入口".into(),
            ..Default::default()
        }
    }

    #[test]
    fn secrets_endpoints_and_protocols_all_must_match() {
        let original = node("trojan");
        let expected = fingerprint(&original).unwrap();
        for (key, value) in [
            ("password", json!("wrong")),
            ("server", json!("other.example")),
            ("port", json!(8443)),
            ("type", json!("hysteria2")),
        ] {
            let mut wrong = original.clone();
            wrong[key] = value;
            assert!(!equal(&expected, &fingerprint(&wrong).unwrap()));
        }
        let mut renamed = original.clone();
        renamed["name"] = json!("any label");
        assert!(equal(&expected, &fingerprint(&renamed).unwrap()));
        renamed["server"] = json!("PROXY.EXAMPLE.");
        assert!(equal(&expected, &fingerprint(&renamed).unwrap()));
    }

    #[test]
    fn tuic_requires_both_uuid_and_password() {
        let original = node("tuic");
        let expected = fingerprint(&original).unwrap();
        for (key, value) in [
            ("password", json!("wrong")),
            ("uuid", json!("00000000-0000-4000-8000-000000000002")),
        ] {
            let mut wrong = original.clone();
            wrong[key] = value;
            assert!(!equal(&expected, &fingerprint(&wrong).unwrap()));
            wrong.as_object_mut().unwrap().remove(key);
            assert!(fingerprint(&wrong).is_err());
        }
    }

    #[test]
    fn shares_and_local_mihomo_nodes_have_the_same_identity() {
        let input = "trojan://secret@proxy.example:443?sni=proxy.example";
        let expected = fingerprint(&node("trojan")).unwrap();
        assert_eq!(fingerprints(input).unwrap(), vec![Some(expected.clone())]);
        for value in [
            node("trojan"),
            json!([node("trojan")]),
            json!({"proxies":[node("trojan")],"proxy-providers":{"other":{"url":"https://invalid.example/private"}}}),
        ] {
            assert_eq!(
                fingerprints(&value.to_string()).unwrap(),
                vec![Some(expected.clone())]
            );
        }
        let provider = r#"{"proxy-providers":{"private":{"url":"https://127.0.0.1/secret"}}}"#;
        assert_eq!(fingerprints(provider).unwrap(), vec![None]);
        assert_eq!(
            fingerprints("https://127.0.0.1/private").unwrap(),
            vec![None]
        );
        assert!(fingerprints(&json!(vec![node("trojan"); MAX_PROXIES + 1]).to_string()).is_err());
    }

    #[test]
    fn multiple_nodes_reveal_only_individually_authorized_rentals() {
        let mut doc = document();
        let mut wrong = node("trojan");
        wrong["port"] = json!(444);
        wrong["password"] = json!("wrong");
        let input = Lookup {
            config: Some(json!({"proxies":[node("trojan"),wrong]}).to_string()),
            token: None,
        };
        let result = lookup_result(&doc, input, 100).ok().unwrap();
        assert_eq!(result["lines"].as_array().unwrap().len(), 1);
        assert_eq!(result["lines"][0]["id"], "one");
        assert_eq!(result["lines"][0]["status"], "expired");
        assert_eq!(result["unmatched"], json!(["第 2 个代理"]));
        let public = result.to_string();
        for secret in [
            "private-customer",
            "private-name",
            "private-contact",
            "private-notes",
            "token_hash",
            "password",
        ] {
            assert!(!public.contains(secret));
        }
        doc.applied_lines[0].enabled = false;
        assert_eq!(
            lookup_result(
                &doc,
                Lookup {
                    config: Some(node("trojan").to_string()),
                    token: None
                },
                100
            )
            .ok()
            .unwrap()["lines"][0]["status"],
            "disabled"
        );
        doc.rentals.remove("one");
        assert!(
            lookup_result(
                &doc,
                Lookup {
                    config: Some(node("trojan").to_string()),
                    token: None
                },
                100
            )
            .is_err()
        );
    }

    #[test]
    fn ambiguous_credentials_and_rotated_tokens_are_rejected() {
        let mut doc = document();
        doc.applied_lines[1].hops[0].port = 443;
        assert!(
            lookup_result(
                &doc,
                Lookup {
                    config: Some(node("trojan").to_string()),
                    token: None
                },
                100
            )
            .is_err()
        );
        doc.rentals.get_mut("two").unwrap().query_token_hash = digest(&"b".repeat(64));
        let token = || Lookup {
            config: None,
            token: Some("a".repeat(64)),
        };
        assert_eq!(
            lookup_result(&doc, token(), 100).ok().unwrap()["lines"][0]["id"],
            "one"
        );
        doc.rentals.get_mut("one").unwrap().query_token_hash = digest(&"c".repeat(64));
        assert!(lookup_result(&doc, token(), 100).is_err());
    }
}
