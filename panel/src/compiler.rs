use crate::model::*;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

pub fn host_valid(host: &str) -> bool {
    !host.is_empty()
        && !host
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#' | '\\'))
        && (host.parse::<std::net::IpAddr>().is_ok()
            || (host.len() <= 253
                && host.split('.').all(|s| {
                    !s.is_empty()
                        && s.len() <= 63
                        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })))
}
pub fn target(value: &str) -> Result<(String, u16)> {
    let u = Url::parse(&format!("tcp://{value}"))?;
    ensure!(
        u.username().is_empty()
            && u.password().is_none()
            && u.path().is_empty()
            && u.query().is_none()
            && u.fragment().is_none(),
        "目标必须为 host:port"
    );
    let host = u
        .host_str()
        .context("目标缺少地址")?
        .trim_matches(['[', ']'])
        .to_string();
    ensure!(host_valid(&host), "目标地址无效");
    let port = u.port().context("目标缺少端口")?;
    ensure!(port > 0, "端口必须大于 0");
    Ok((host, port))
}
pub fn address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
pub fn external_outbound(link: &str, tag: &str) -> Result<Value> {
    if !link.starts_with("vless://") {
        return crate::access::external(link, tag);
    }
    let u = Url::parse(link).context("VLESS 链接格式错误")?;
    ensure!(
        u.scheme() == "vless" && u.password().is_none(),
        "仅支持 vless:// 链接"
    );
    let uuid = uuid::Uuid::parse_str(u.username())
        .context("VLESS UUID 无效")?
        .to_string();
    let host = u
        .host_str()
        .context("链接缺少地址")?
        .trim_matches(['[', ']']);
    ensure!(host_valid(host), "链接地址无效");
    let port = u.port().context("链接缺少端口")?;
    ensure!(port > 0, "链接端口无效");
    let q: BTreeMap<String, String> = u.query_pairs().into_owned().collect();
    let get = |key: &str, default: &str| q.get(key).cloned().unwrap_or_else(|| default.to_string());
    ensure!(
        get("encryption", "none") == "none",
        "仅支持 encryption=none"
    );
    let network = get("type", "tcp");
    ensure!(
        matches!(network.as_str(), "tcp" | "raw" | "xhttp"),
        "首版外部落地支持 TCP/raw 或 XHTTP"
    );
    let security = get("security", "");
    let mut stream = json!({"network": network, "security": security});
    match security.as_str() {
        "reality" => {
            let key = get("pbk", "");
            ensure!(
                !key.is_empty() && !get("sni", "").is_empty(),
                "REALITY 链接缺少 pbk 或 sni"
            );
            stream["realitySettings"] = json!({"serverName": get("sni", ""), "publicKey": key,
                "shortId": get("sid", ""), "fingerprint": get("fp", "chrome"), "spiderX": get("spx", "/")});
            if !get("pqv", "").is_empty() {
                stream["realitySettings"]["mldsa65Verify"] = json!(get("pqv", ""));
            }
        }
        "tls" => {
            stream["tlsSettings"] =
                json!({"serverName": get("sni", host), "fingerprint": get("fp", "chrome")});
            crate::access::external_tls(&mut stream, &u)?;
        }
        _ => bail!("外部落地必须使用 TLS 或 REALITY"),
    }
    if network == "xhttp" {
        stream["xhttpSettings"] =
            json!({"path": get("path", "/"), "host": get("host", ""), "mode": get("mode", "auto")});
        if let Some(extra) = q.get("extra") {
            let extra: Value =
                serde_json::from_str(extra).context("XHTTP extra 必须是 JSON 对象")?;
            ensure!(extra.is_object(), "XHTTP extra 必须是 JSON 对象");
            stream["xhttpSettings"]["extra"] = extra;
        }
    }
    if let Some(tfo) = q.get("tfo") {
        ensure!(
            matches!(tfo.as_str(), "true" | "false" | "1" | "0"),
            "tfo 参数无效"
        );
        stream["sockopt"] = json!({"tcpFastOpen": matches!(tfo.as_str(), "true" | "1")});
    }
    let flow = get("flow", "");
    ensure!(
        flow.is_empty() || flow == "xtls-rprx-vision",
        "不支持此 VLESS flow"
    );
    ensure!(
        network != "xhttp" || flow.is_empty(),
        "XHTTP 不应设置 Vision flow"
    );
    Ok(
        json!({"tag":tag,"protocol":"vless","settings":{"vnext":[{"address":host,"port":port,
        "users":[{"id":uuid,"encryption":"none","flow":flow}]}]},"streamSettings":stream}),
    )
}
pub fn share(line: &Line, doc: &Document) -> Result<String> {
    ensure!(
        line.kind == Kind::Vless,
        "端口转发使用入口地址，无 VLESS 链接"
    );
    let h = line.hops.first().context("线路没有入口")?;
    let node = doc
        .nodes
        .iter()
        .find(|n| n.id == h.node_id)
        .context("入口节点不存在")?;
    if line.access.protocol != Protocol::Vless {
        return crate::access::share(line, node);
    }
    let host = if !line.access.host.is_empty() {
        &line.access.host
    } else if line.access.public_host.is_empty() {
        if matches!(line.access.tls.mode.as_str(), "auto" | "manual") {
            &line.access.tls.domain
        } else {
            &node.host
        }
    } else {
        &line.access.public_host
    };
    let mut u = Url::parse(&format!(
        "vless://{}@{}",
        h.uuid,
        address(host, line.access.public_port.unwrap_or(h.port))
    ))?;
    let tls = matches!(line.access.tls.mode.as_str(), "auto" | "manual");
    u.query_pairs_mut().extend_pairs([
        ("type", "tcp"),
        ("security", if tls { "tls" } else { "reality" }),
        ("flow", "xtls-rprx-vision"),
        ("encryption", "none"),
        (
            "sni",
            if tls {
                &line.access.tls.domain
            } else {
                &node.reality.server_name
            },
        ),
        ("fp", "chrome"),
    ]);
    if !tls {
        u.query_pairs_mut().extend_pairs([
            ("pbk", &node.reality.public_key),
            ("sid", &node.reality.short_id),
        ]);
    }
    u.set_fragment(Some(&line.name));
    Ok(u.to_string())
}
pub fn empty_config() -> Value {
    json!({"log":{"loglevel":log_level()}, "inbounds":[], "outbounds":[{"tag":"blocked","protocol":"blackhole"}], "routing":{"rules":[]}})
}
fn vless_in(node: &Node, hop: &Hop, tag: &str) -> Value {
    json!({"tag":tag,"listen":"0.0.0.0","port":hop.port,"protocol":"vless", "settings":{"clients":[{"id":hop.uuid,"flow":"xtls-rprx-vision"}],"decryption":"none"},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{"show":false,"target":node.reality.target,
            "serverNames":[node.reality.server_name],"privateKey":node.reality.private_key,"shortIds":[node.reality.short_id]}}})
}
fn vless_out(node: &Node, hop: &Hop, tag: &str) -> Value {
    json!({"tag":tag,"protocol":"vless","settings":{"vnext":[{"address":node.host,"port":hop.port,
        "users":[{"id":hop.uuid,"encryption":"none","flow":"xtls-rprx-vision"}]}]},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{"serverName":node.reality.server_name,
            "publicKey":node.reality.public_key,"shortId":node.reality.short_id,"fingerprint":"chrome"}}})
}
pub fn compile(doc: &Document) -> Result<BTreeMap<String, Value>> {
    fn check_chain(line: &Line, doc: &Document, visited: &mut BTreeSet<String>) -> Result<()> {
        ensure!(
            visited.len() < 16 && visited.insert(line.id.clone()),
            "链式代理不能循环引用"
        );
        let chain = line.proxy_chain();
        ensure!(chain.len() <= 8, "最多选择 8 个链式代理");
        let mut unique = BTreeSet::new();
        for reference in chain {
            ensure!(
                unique.insert(reference.clone()),
                "链式代理不能重复选择同一个代理"
            );
            if let Some(id) = reference.strip_prefix("line:") {
                let next = doc
                    .lines
                    .iter()
                    .find(|l| l.id == id && l.kind == Kind::Vless)
                    .context("引用的代理不存在")?;
                ensure!(
                    !line.enabled || next.enabled,
                    "请先移除对已停用代理的链式引用"
                );
                check_chain(next, doc, visited)?;
            } else {
                ensure!(reference.starts_with("external:"), "链式代理引用无效");
            }
        }
        visited.remove(&line.id);
        Ok(())
    }
    for line in &doc.lines {
        check_chain(line, doc, &mut BTreeSet::new())?;
    }
    let mut configs: BTreeMap<String, Value> = doc
        .nodes
        .iter()
        .map(|n| (n.id.clone(), empty_config()))
        .collect();
    let mut ports = BTreeSet::new();
    let mut websites: BTreeMap<(String, u16), (bool, BTreeSet<String>)> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut reserved: BTreeSet<(String, u16)> = doc
        .lines
        .iter()
        .flat_map(|l| l.hops.iter().map(|h| (h.node_id.clone(), h.port)))
        .collect();
    let mut ordered: Vec<_> = doc.lines.iter().collect();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    for line in ordered {
        ensure!(ids.insert(&line.id), "线路 ID 重复");
        ensure!(
            !line.name.trim().is_empty() && line.name.len() <= 120,
            "线路名称不能为空或超过 120 字节"
        );
        ensure!(
            !line.hops.is_empty() && line.hops.len() <= 8,
            "每条线路需要 1–8 个受管节点"
        );
        ensure!(
            matches!(line.network.as_str(), "tcp" | "udp" | "tcp,udp"),
            "转发网络无效"
        );
        let mut visited = BTreeSet::new();
        for h in &line.hops {
            ensure!(
                visited.insert(&h.node_id),
                "线路 {} 包含循环或重复节点",
                line.name
            );
            ensure!(h.port > 0, "端口必须大于 0");
            uuid::Uuid::parse_str(&h.uuid).context("节点接入 UUID 无效")?;
            ensure!(configs.contains_key(&h.node_id), "线路引用了不存在的节点");
            // Reserve disabled lines too: enabling must not silently steal another line's port.
            let fresh = ports.insert((&h.node_id, h.port));
            if line.kind == Kind::Website {
                let key = (h.node_id.clone(), h.port);
                let tls = matches!(line.access.tls.mode.as_str(), "auto" | "manual");
                ensure!(
                    fresh || websites.contains_key(&key),
                    "网站端口 {} 被其他类型的线路占用",
                    h.port
                );
                let (mode, domains) = websites.entry(key).or_insert((tls, BTreeSet::new()));
                ensure!(*mode == tls, "同一网站监听端口不能混用 HTTP 和 HTTPS");
                ensure!(
                    domains.insert(line.access.tls.domain.to_ascii_lowercase()),
                    "同一监听端口的网站域名重复"
                );
            } else {
                ensure!(fresh, "同一节点端口 {} 被多条线路占用", h.port);
            }
        }
        let forward = if line.kind == Kind::Forward {
            Some(target(&line.target)?)
        } else {
            None
        };
        let chain = line.proxy_chain();
        let mut external = Vec::new();
        for (index, reference) in chain.iter().enumerate() {
            let link = if let Some(id) = reference.strip_prefix("external:") {
                doc.externals
                    .iter()
                    .find(|e| e.id == id)
                    .context("外部代理不存在")?
                    .url
                    .clone()
            } else {
                let next = doc
                    .lines
                    .iter()
                    .find(|l| Some(l.id.as_str()) == reference.strip_prefix("line:"))
                    .context("引用的代理不存在")?;
                share(next, doc)?
            };
            let tag = if index + 1 == chain.len() {
                format!("line-{}-out", line.id)
            } else {
                format!("line-{}-chain-{index}", line.id)
            };
            let mut outbound = external_outbound(&link, &tag)?;
            if index > 0 {
                outbound["streamSettings"]["sockopt"]["dialerProxy"] =
                    json!(format!("line-{}-chain-{}", line.id, index - 1));
            }
            external.push(outbound);
        }
        crate::access::validate_line(line)?;
        if !line.enabled {
            continue;
        }
        for (i, hop) in line.hops.iter().enumerate() {
            let node = doc.nodes.iter().find(|n| n.id == hop.node_id).unwrap();
            let in_tag = format!("line-{}-in", line.id);
            let out_tag = format!("line-{}-out", line.id);
            let mut inbound = if i == 0 && line.kind == Kind::Forward {
                let (host, port) = forward.as_ref().unwrap();
                json!({"tag":in_tag,"listen":"0.0.0.0","port":hop.port,"protocol":"dokodemo-door",
                    "settings":{"address":host,"port":port,"network":line.network}})
            } else if i == 0 && line.kind == Kind::Vless && line.access.protocol != Protocol::Vless
            {
                crate::access::inbound(line, hop, &in_tag)
            } else {
                vless_in(node, hop, &in_tag)
            };
            if i == 0
                && line.kind == Kind::Vless
                && line.access.protocol == Protocol::Vless
                && matches!(line.access.tls.mode.as_str(), "auto" | "manual")
            {
                let tls = &line.access.tls;
                let certificates = if tls.mode == "manual" {
                    json!([{"certificate":tls.cert.lines().collect::<Vec<_>>(),"key":tls.key.lines().collect::<Vec<_>>() }])
                } else {
                    json!([])
                };
                inbound["streamSettings"] = json!({"network":"tcp","security":"tls",
                    "tlsSettings":{"minVersion":"1.2","certificates":certificates}});
            }
            let outbound = if let Some(next) = line.hops.get(i + 1) {
                vless_out(
                    doc.nodes.iter().find(|n| n.id == next.node_id).unwrap(),
                    next,
                    &out_tag,
                )
            } else if let Some(last) = external.last() {
                last.clone()
            } else {
                json!({"tag":out_tag,"protocol":"freedom"})
            };
            let config = configs.get_mut(&node.id).unwrap();
            if i == 0 {
                let backend = (50000..65535)
                    .find(|p| !reserved.iter().any(|(_, used)| used == p))
                    .context("没有可供内部接入使用的端口")?;
                reserved.insert((node.id.clone(), backend));
                inbound["listen"] = json!("127.0.0.1");
                inbound["port"] = json!(backend);
                if config.get("_chain").is_none() {
                    config["_chain"] = json!([]);
                }
                config["_chain"].as_array_mut().unwrap().push(json!({
                    "id":line.id,"port":hop.port,"backend":backend,"kind":line.kind,
                    "protocol":line.access.protocol,"network":if line.kind == Kind::Forward {line.network.as_str()} else if matches!(line.access.protocol,Protocol::Socks|Protocol::Shadowsocks) {"tcp,udp"} else {"tcp"},
                    "limits":line.limits,"tls":line.access.tls,"target":line.target,
                }));
                if line.kind == Kind::Website {
                    continue;
                }
            }
            config["inbounds"].as_array_mut().unwrap().push(inbound);
            config["outbounds"].as_array_mut().unwrap().push(outbound);
            if i + 1 == line.hops.len() && external.len() > 1 {
                config["outbounds"]
                    .as_array_mut()
                    .unwrap()
                    .extend_from_slice(&external[..external.len() - 1]);
            }
            config["routing"]["rules"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"field","inboundTag":[in_tag],"outboundTag":out_tag}));
        }
    }
    Ok(configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Document {
        let mut d = Document::default();
        for n in ["a", "b", "c"] {
            d.nodes.push(Node {
                id: n.into(),
                name: n.into(),
                host: format!("{n}.example.com"),
                reality: Reality::new("example.com:443".into(), "example.com".into()),
                token_hash: String::new(),
                seen: 0,
                report: Report::default(),
                desired: None,
            });
        }
        d.lines.push(Line {
            id: id(),
            name: "线路一".into(),
            kind: Kind::Vless,
            hops: ["a", "b", "c"]
                .iter()
                .map(|n| Hop {
                    node_id: n.to_string(),
                    port: 24443,
                    uuid: id(),
                })
                .collect(),
            external_id: None,
            chain: vec![],
            target: String::new(),
            network: "tcp".into(),
            enabled: true,
            access: Access::default(),
            limits: Limits::default(),
        });
        d
    }
    #[test]
    fn ordered_proxy_chain_uses_each_transport_and_rejects_cycles() {
        let mut d = fixture();
        d.lines[0].hops.truncate(1);
        for name in ["one", "two"] {
            d.externals.push(External {
                id: name.into(),
                name: name.into(),
                url: format!(
                    "vless://{}@{name}.example.com:443?security=tls&type=tcp",
                    id()
                ),
            });
        }
        d.lines[0].chain = vec!["external:one".into(), "external:two".into()];
        let c = compile(&d).unwrap();
        let out = &c["a"]["outbounds"];
        assert_eq!(out[1]["settings"]["vnext"][0]["address"], "two.example.com");
        assert_eq!(out[2]["settings"]["vnext"][0]["address"], "one.example.com");
        assert_eq!(
            out[1]["streamSettings"]["sockopt"]["dialerProxy"],
            out[2]["tag"]
        );
        d.lines[0].chain = vec![format!("line:{}", d.lines[0].id)];
        assert!(compile(&d).unwrap_err().to_string().contains("循环"));
        d.lines[0].chain = vec!["external:one".into(), "external:one".into()];
        assert!(compile(&d).is_err());
    }
    #[test]
    fn legacy_documents_keep_vless_defaults_and_credentials() {
        let document = fixture();
        let original = share(&document.lines[0], &document).unwrap();
        let mut value = serde_json::to_value(document).unwrap();
        value.as_object_mut().unwrap().remove("security");
        value.as_object_mut().unwrap().remove("subscriptions");
        for line in value["lines"].as_array_mut().unwrap() {
            line.as_object_mut().unwrap().remove("access");
            line.as_object_mut().unwrap().remove("limits");
        }
        for node in value["nodes"].as_array_mut().unwrap() {
            node["report"].as_object_mut().unwrap().remove("traffic");
            node["report"].as_object_mut().unwrap().remove("system");
        }
        let restored: Document = serde_json::from_value(value).unwrap();
        assert_eq!(share(&restored.lines[0], &restored).unwrap(), original);
        assert_eq!(restored.lines[0].access.protocol, Protocol::Vless);
        assert!(restored.lines[0].limits.monthly_gib.is_none());
        compile(&restored).unwrap();
    }
    #[test]
    fn three_hops_route_through_actual_next_node() {
        let d = fixture();
        let c = compile(&d).unwrap();
        assert_eq!(
            c["a"]["outbounds"][1]["settings"]["vnext"][0]["address"],
            "b.example.com"
        );
        assert_eq!(
            c["b"]["outbounds"][1]["settings"]["vnext"][0]["address"],
            "c.example.com"
        );
        assert_eq!(c["c"]["outbounds"][1]["protocol"], "freedom");
        assert_eq!(c["a"]["outbounds"][0]["protocol"], "blackhole");
    }
    #[test]
    fn reject_cycles_missing_nodes_and_port_collisions() {
        let mut d = fixture();
        d.lines[0].hops[2].node_id = "a".into();
        assert!(compile(&d).is_err());
        d.lines[0].hops[2].node_id = "missing".into();
        assert!(compile(&d).is_err());
        d = fixture();
        let mut second = d.lines[0].clone();
        second.id = id();
        d.lines.push(second);
        assert!(compile(&d).is_err());
        for h in &mut d.lines[1].hops {
            h.port += 1;
        }
        assert!(compile(&d).is_ok());
    }
    #[test]
    fn forwarding_keeps_destination_across_chain() {
        let mut d = fixture();
        d.lines[0].kind = Kind::Forward;
        d.lines[0].target = "[::1]:5353".into();
        d.lines[0].network = "tcp,udp".into();
        let c = compile(&d).unwrap();
        assert_eq!(c["a"]["inbounds"][0]["settings"]["address"], "::1");
        assert_eq!(c["a"]["inbounds"][0]["settings"]["network"], "tcp,udp");
        assert_eq!(c["b"]["inbounds"][0]["protocol"], "vless");
    }
    #[test]
    fn exported_share_roundtrips_and_entry_credentials_stay_stable() {
        let mut d = fixture();
        let before = share(&d.lines[0], &d).unwrap();
        d.lines[0].hops.swap(1, 2);
        assert_eq!(share(&d.lines[0], &d).unwrap(), before);
        let parsed = external_outbound(&before, "test").unwrap();
        assert_eq!(
            parsed["settings"]["vnext"][0]["users"][0]["id"],
            d.lines[0].hops[0].uuid
        );
    }
    #[test]
    fn tls_vision_entry_keeps_internal_reality_chain_and_exports_tls() {
        let mut d = fixture();
        d.lines[0].access.tls = Tls {
            mode: "auto".into(),
            domain: "entry.example.com".into(),
            ..Tls::default()
        };
        let configs = compile(&d).unwrap();
        assert_eq!(
            configs["a"]["inbounds"][0]["streamSettings"]["security"],
            "tls"
        );
        assert_eq!(
            configs["b"]["inbounds"][0]["streamSettings"]["security"],
            "reality"
        );
        assert_eq!(
            configs["a"]["outbounds"][1]["streamSettings"]["security"],
            "reality"
        );
        let link = share(&d.lines[0], &d).unwrap();
        let outbound = external_outbound(&link, "test").unwrap();
        assert_eq!(outbound["streamSettings"]["security"], "tls");
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["serverName"],
            "entry.example.com"
        );
        assert!(!link.contains("pbk=") && !link.contains("sid="));
        let node = crate::access::mihomo_node(&d.lines[0], &d).unwrap();
        assert_eq!(node["servername"], "entry.example.com");
        assert_eq!(node["flow"], "xtls-rprx-vision");
        assert!(node.get("reality-opts").is_none());
        d.lines[0].access.tls.domain = "127.0.0.1".into();
        assert!(compile(&d).is_err());
        d.lines[0].access.tls.mode = "manual".into();
        assert!(compile(&d).is_err());
    }
    #[test]
    fn rejects_plaintext_external_and_bad_destination() {
        assert!(
            external_outbound(
                &format!("vless://{}@example.com:443?security=none", id()),
                "x"
            )
            .is_err()
        );
        for s in [
            "example.com",
            "user@example.com:80",
            "example.com:80/path",
            "example.com:0",
        ] {
            assert!(target(s).is_err(), "{s}");
        }
    }
}
