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
        }
        "tls" => {
            stream["tlsSettings"] =
                json!({"serverName": get("sni", host), "fingerprint": get("fp", "chrome")});
        }
        _ => bail!("外部落地必须使用 TLS 或 REALITY"),
    }
    if network == "xhttp" {
        stream["xhttpSettings"] =
            json!({"path": get("path", "/"), "host": get("host", ""), "mode": get("mode", "auto")});
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
    let mut u = Url::parse(&format!(
        "vless://{}@{}",
        h.uuid,
        address(&node.host, h.port)
    ))?;
    u.query_pairs_mut().extend_pairs([
        ("type", "tcp"),
        ("security", "reality"),
        ("flow", "xtls-rprx-vision"),
        ("encryption", "none"),
        ("sni", &node.reality.server_name),
        ("pbk", &node.reality.public_key),
        ("sid", &node.reality.short_id),
        ("fp", "chrome"),
    ]);
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
    let mut configs: BTreeMap<String, Value> = doc
        .nodes
        .iter()
        .map(|n| (n.id.clone(), empty_config()))
        .collect();
    let mut ports = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for line in &doc.lines {
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
            ensure!(
                ports.insert((&h.node_id, h.port)),
                "同一节点端口 {} 被多条线路占用",
                h.port
            );
        }
        let forward = if line.kind == Kind::Forward {
            Some(target(&line.target)?)
        } else {
            None
        };
        let ext = if let Some(id) = &line.external_id {
            let e = doc
                .externals
                .iter()
                .find(|e| &e.id == id)
                .context("外部节点不存在")?;
            Some(external_outbound(&e.url, "external")?)
        } else {
            None
        };
        if !line.enabled {
            continue;
        }
        for (i, hop) in line.hops.iter().enumerate() {
            let node = doc.nodes.iter().find(|n| n.id == hop.node_id).unwrap();
            let in_tag = format!("line-{}-in", line.id);
            let out_tag = format!("line-{}-out", line.id);
            let inbound = if i == 0 && line.kind == Kind::Forward {
                let (host, port) = forward.as_ref().unwrap();
                json!({"tag":in_tag,"listen":"0.0.0.0","port":hop.port,"protocol":"dokodemo-door",
                    "settings":{"address":host,"port":port,"network":line.network}})
            } else {
                vless_in(node, hop, &in_tag)
            };
            let outbound = if let Some(next) = line.hops.get(i + 1) {
                vless_out(
                    doc.nodes.iter().find(|n| n.id == next.node_id).unwrap(),
                    next,
                    &out_tag,
                )
            } else if let Some(external) = &ext {
                let mut e = external.clone();
                e["tag"] = json!(out_tag);
                e
            } else {
                json!({"tag":out_tag,"protocol":"freedom"})
            };
            let config = configs.get_mut(&node.id).unwrap();
            config["inbounds"].as_array_mut().unwrap().push(inbound);
            config["outbounds"].as_array_mut().unwrap().push(outbound);
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
            target: String::new(),
            network: "tcp".into(),
            enabled: true,
        });
        d
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
